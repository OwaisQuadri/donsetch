mod detect;
mod error;
mod extract;
mod fetch;
mod ghost;
mod mcp;
mod memory;
mod profile;
mod transport;

use profile::BrowserProfile;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "fetch" => {
            let url = args.get(2).expect("usage: donsetch fetch <url>");
            let fetcher = fetch::client::Fetcher::new(BrowserProfile::host_default())
                .expect("fetcher init");
            match fetcher.fetch(url).await {
                Ok(out) => {
                    println!("status: {}", out.status);
                    println!("alpn: {}", out.alpn);
                    println!("redirects: {}", out.redirects);
                    println!("final: {}", out.url);
                    println!("elapsed: {:?}", out.elapsed);
                    println!("bytes: {}", out.body.len());
                    println!("cache: {:?}", out.cache);
                    println!("pooled-conn: {}", out.used_pool);
                    println!("verdict: {:?}", out.verdict);
                    println!("profile: {}", fetcher.profile().name);
                    println!("--- headers ---");
                    for (n, v) in &out.headers {
                        println!("{n}: {v}");
                    }
                    println!("--- body (first 800) ---");
                    let text = String::from_utf8_lossy(&out.body);
                    println!("{}", &text[..text.len().min(800)]);
                    if let Some(pos) = args.iter().position(|a| a == "--dump") {
                        if let Some(path) = args.get(pos + 1) {
                            let _ = std::fs::write(path, &out.body);
                            eprintln!("dumped {} bytes -> {path}", out.body.len());
                        }
                    }
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "extract" => {
            let mut url = String::new();
            let mut input_file: Option<String> = None;
            let mut opts = extract::ExtractOptions::default();
            let mut tier = "auto".to_string();
            let mut shot_path: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--focus" => {
                        i += 1;
                        opts.focus = args.get(i).cloned();
                    }
                    "--max" => {
                        i += 1;
                        opts.max_chars = args.get(i).and_then(|s| s.parse().ok());
                    }
                    "--offset" => {
                        i += 1;
                        opts.offset = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(0);
                    }
                    "--selector" => {
                        i += 1;
                        opts.selector = args.get(i).cloned();
                    }
                    "--links" => opts.include_links = true,
                    "--media" => opts.include_media = true,
                    "--toc" => opts.toc = true,
                    "--section" => {
                        i += 1;
                        opts.section = args.get(i).cloned();
                    }
                    "--tier" => {
                        i += 1;
                        tier = args.get(i).cloned().unwrap_or("auto".into());
                    }
                    "--shot" => {
                        i += 1;
                        shot_path = args.get(i).cloned();
                    }
                    "--input" => {
                        i += 1;
                        input_file = args.get(i).cloned();
                    }
                    other => url = other.to_string(),
                }
                i += 1;
            }
            if let Some(path) = input_file {
                let body = std::fs::read(&path).expect("read input");
                let t0 = std::time::Instant::now();
                let ex = match extract::extract(&body, "text/html", "https://local/", &opts) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(1);
                    }
                };
                eprintln!(
                    "--- extract={:.1}ms blocks={}/{} chars={}/{}",
                    t0.elapsed().as_secs_f64() * 1000.0,
                    ex.blocks_shown,
                    ex.blocks_total,
                    ex.markdown.len(),
                    ex.total_chars,
                );
                print!("{}", ex.markdown);
                return;
            }

            // Smart fetch: tier 1 first, auto-escalate to
            // DonGhost on wall (solve) or JS shell (render).
            let profile = BrowserProfile::host_default();
            let fetcher = fetch::client::Fetcher::new(profile.clone())
                .expect("fetcher init");
            let mut tier_used = "1";
            let mut ghost: Option<ghost::Ghost> = None;
            let mut state = ghost::cache::GhostState::load();
            let host = url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(|s| s.to_string()))
                .unwrap_or_default();

            // Warm tier 1 from a previous solve — the ghost
            // stays asleep until clearance actually fails.
            if tier != "2" {
                if let Some(rec) = state.solved_for(&host) {
                    eprintln!("--- warm start: {} solved cookies from state", rec.cookies.len());
                    fetcher.import_cookies(&rec.cookies).await;
                    tier_used = "1(warm)";
                }
            }

            let mut out = fetcher.fetch(&url).await.expect("fetch");
            let mut rendered_html: Option<Vec<u8>> = None;

            let walled = !matches!(
                out.verdict,
                detect::walls::Verdict::ContentOk
            );
            if walled && tier != "1" {
                // SOLVE mode: beat the wall, hand cookies
                // to tier 1, re-fetch at tier-1 speed.
                let mut g = ghost::Ghost::launch(&profile)
                    .await
                    .expect("ghost launch");
                match ghost::ops::solve(
                    &mut g,
                    &url,
                    std::time::Duration::from_secs(30),
                )
                .await
                .expect("ghost solve")
                {
                    ghost::ops::SolveOutcome::Solved(r) => {
                        eprintln!(
                            "--- ghost solved in {:?} ({} cookies, clearance={})",
                            r.took,
                            r.cookies.len(),
                            ghost::ops::has_clearance(&r.cookies)
                        );
                        fetcher.import_cookies(&r.cookies).await;
                        state.record_solved(&host, &r.cookies);
                        let retry =
                            fetcher.fetch(&url).await.expect("fetch");
                        if matches!(
                            retry.verdict,
                            detect::walls::Verdict::ContentOk
                        ) {
                            out = retry;
                            tier_used = "1+ghost-solve";
                        } else {
                            // Tier 1 still refused: use
                            // the ghost's own DOM.
                            rendered_html =
                                Some(r.html.into_bytes());
                            out = retry;
                            tier_used = "ghost-dom";
                        }
                    }
                    ghost::ops::SolveOutcome::CaptchaWalled => {
                        println!(
                            "# Blocked: interactive captcha\n\n{} requires a human-or-service captcha solve. No solving service by design.\n\n*[verdict: {:?}, status: {}]*",
                            url, out.verdict, out.status
                        );
                        if let Some(path) = &shot_path {
                            let _ = g.screenshot(path).await;
                            eprintln!("--- what the ghost saw -> {path}");
                        }
                        g.kill().await;
                        return;
                    }
                    ghost::ops::SolveOutcome::TimedOut => {
                        eprintln!("--- ghost solve timed out");
                    }
                }
                ghost = Some(g);
            }

            // Body source: tier-1 bytes or ghost DOM.
            let (body, ct, final_url) = if let Some(h) = &rendered_html
            {
                (h.clone(), "text/html".to_string(), out.url.clone())
            } else {
                if matches!(out.verdict, detect::walls::Verdict::ContentOk)
                {
                    let ct = out
                        .headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default();
                    (out.body.clone(), ct, out.url.clone())
                } else {
                    println!(
                        "# Blocked: {:?}\n\nThe page at {} returned a bot-wall challenge, not content.\n\n*[verdict: {:?}, status: {}]*",
                        out.verdict, out.url, out.verdict, out.status
                    );
                    if let Some(mut g) = ghost {
                        g.kill().await;
                    }
                    return;
                }
            };

            let t0 = std::time::Instant::now();
            let mut ex = match extract::extract(&body, &ct, &final_url, &opts) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };

            // RENDER mode: clean 200 but a JS shell.
            // Check the render cache first — repeat SPA
            // visits skip the browser entirely.
            if ex.thin && tier == "auto" {
                if let Some(rc) = state.render_for(&final_url) {
                    eprintln!(
                        "--- render cache hit ({}B, fresh)",
                        rc.html.len()
                    );
                    ex = extract::extract(
                        rc.html.as_bytes(),
                        "text/html",
                        &final_url,
                        &opts,
                    )
                    .expect("extract");
                    tier_used = "render-cache";
                } else {
                    let g = match ghost {
                        Some(g) => g,
                        None => ghost::Ghost::launch(&profile)
                            .await
                            .expect("ghost launch"),
                    };
                    let mut g = g;
                    match ghost::ops::render(
                        &mut g,
                        &final_url,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    {
                        Ok(html) => {
                            eprintln!(
                                "--- ghost rendered {} bytes (was thin)",
                                html.len()
                            );
                            state.record_render(&final_url, &html);
                            ex = extract::extract(
                                html.as_bytes(),
                                "text/html",
                                &final_url,
                                &opts,
                            )
                            .expect("extract");
                            tier_used = "ghost-render";
                        }
                        Err(e) => {
                            eprintln!("--- ghost render failed: {e}")
                        }
                    }
                    ghost = Some(g);
                }
            }

            let extract_ms = t0.elapsed().as_secs_f64() * 1000.0;
            print!("{}", ex.markdown);
            eprintln!(
                "--- tier={} title={:?} kind={:?} status={} fetch={:.0}ms extract={:.1}ms blocks={}/{} chars={}/{} tokens~{} next_offset={:?} thin={}",
                tier_used,
                ex.title,
                ex.content_kind,
                out.status,
                out.elapsed.as_secs_f64() * 1000.0,
                extract_ms,
                ex.blocks_shown,
                ex.blocks_total,
                ex.markdown.len(),
                ex.total_chars,
                ex.tokens_est,
                ex.next_offset,
                ex.thin,
            );
            if let Some(mut g) = ghost {
                g.kill().await;
            }
        }
        "mcp" => {
            if let Err(e) = mcp::server::run().await {
                eprintln!("mcp daemon: {e}");
                std::process::exit(1);
            }
        }
        "fingerprint" => {
            let url = args
                .get(2)
                .cloned()
                .unwrap_or_else(|| "https://tls.peet.ws/api/all".into());
            let fetcher = fetch::client::Fetcher::new(BrowserProfile::host_default())
                .expect("fetcher init");
            match fetcher.fetch(&url).await {
                Ok(out) => {
                    println!("status: {} alpn: {} elapsed: {:?}", out.status, out.alpn, out.elapsed);
                    println!("{}", String::from_utf8_lossy(&out.body));
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            }
        }
        "resume-test" => {
            let url = args.get(2).expect("usage: donsetch resume-test <url>");
            let fetcher = fetch::client::Fetcher::new(BrowserProfile::host_default())
                .expect("fetcher init");
            for i in 1..=3 {
                match fetcher.fetch(url).await {
                    Ok(out) => println!(
                        "fetch{i}: status={} alpn={} elapsed={:?} bytes={} cache={:?} pooled={}",
                        out.status,
                        out.alpn,
                        out.elapsed,
                        out.body.len(),
                        out.cache,
                        out.used_pool
                    ),
                    Err(e) => {
                        eprintln!("fetch{i} error: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
        "ghost" => {
            // donsetch ghost <solve|render|shot> <url> [path]
            let sub = args.get(2).map(|s| s.as_str()).unwrap_or("help");
            let profile = BrowserProfile::host_default();
            match sub {
                "solve" => {
                    let url = args.get(3).expect("usage: ghost solve <url>");
                    let t0 = std::time::Instant::now();
                    let mut g = ghost::Ghost::launch(&profile)
                        .await
                        .expect("launch");
                    eprintln!("launched in {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);
                    match ghost::ops::solve(
                        &mut g,
                        url,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .expect("solve")
                    {
                        ghost::ops::SolveOutcome::Solved(r) => {
                            println!("SOLVED in {:?}", r.took);
                            println!("clearance: {}", ghost::ops::has_clearance(&r.cookies));
                            println!("cookies: {}", r.cookies.len());
                            for (n, _v, d) in &r.cookies {
                                println!("  {n} (domain {d})");
                            }
                        }
                        ghost::ops::SolveOutcome::CaptchaWalled => {
                            println!("CAPTCHA-WALLED (honest dead end)")
                        }
                        ghost::ops::SolveOutcome::TimedOut => println!("TIMED OUT"),
                    }
                    g.kill().await;
                }
                "render" => {
                    let url = args.get(3).expect("usage: ghost render <url>");
                    let mut g = ghost::Ghost::launch(&profile)
                        .await
                        .expect("launch");
                    let html = ghost::ops::render(
                        &mut g,
                        url,
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .expect("render");
                    println!("rendered {} bytes", html.len());
                    let ex = extract::extract(
                        html.as_bytes(),
                        "text/html",
                        url,
                        &extract::ExtractOptions::default(),
                    )
                    .expect("extract");
                    print!("{}", &ex.markdown[..ex.markdown.len().min(2000)]);
                    eprintln!(
                        "--- thin={} kind={:?} blocks={}",
                        ex.thin, ex.content_kind, ex.blocks_total
                    );
                    g.kill().await;
                }
                "shot" => {
                    let url = args.get(3).expect("usage: ghost shot <url> [path]");
                    let path = args.get(4).cloned().unwrap_or("/tmp/ghost.png".into());
                    let mut g = ghost::Ghost::launch(&profile)
                        .await
                        .expect("launch");
                    g.navigate(url).await.expect("nav");
                    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                    g.screenshot(&path).await.expect("shot");
                    println!("shot -> {path}");
                    g.kill().await;
                }
                "selftest" => {
                    let mut g = ghost::Ghost::launch(&profile)
                        .await
                        .expect("launch");
                    match ghost::ops::selftest(&mut g).await {
                        Ok(j) => println!("{j}"),
                        Err(e) => eprintln!("selftest: {e}"),
                    }
                    g.kill().await;
                }
                "freeze-check" => {
                    // Lifecycle roundtrip: launch → render →
                    // freeze (0 CPU) → thaw → render again.
                    let mut g = ghost::Ghost::launch(&profile)
                        .await
                        .expect("launch");
                    let h1 = ghost::ops::render(
                        &mut g,
                        "https://example.com",
                        std::time::Duration::from_secs(15),
                    )
                    .await
                    .expect("render1");
                    println!("render1: {}B", h1.len());
                    g.freeze();
                    println!("frozen");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    let alive = g.thaw();
                    println!("thawed alive={alive}");
                    let h2 = ghost::ops::render(
                        &mut g,
                        "https://example.com",
                        std::time::Duration::from_secs(15),
                    )
                    .await
                    .expect("render2");
                    println!("render2 after thaw: {}B", h2.len());
                    g.kill().await;
                }
                _ => eprintln!("ghost subcommands: solve | render | shot | selftest | freeze-check"),
            }
        }
        _ => {
            eprintln!("donsetch — commands: fetch <url> | extract <url> [--focus q] [--max n] [--offset n] [--selector css] [--links] [--media] | fingerprint [url] | resume-test <url> | ghost <solve|render|shot> <url>");
        }
    }
}
