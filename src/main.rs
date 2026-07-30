mod detect;
mod error;
mod extract;
mod fetch;
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
            let fetcher = fetch::client::Fetcher::new(BrowserProfile::host_default())
                .expect("fetcher init");
            let out = fetcher.fetch(&url).await.expect("fetch");

            // A challenge page is NOT content. An agent must
            // never quote "Just a moment…" as page content —
            // give it an actionable message instead.
            if !matches!(out.verdict, detect::walls::Verdict::ContentOk) {
                println!(
                    "# Blocked: {:?}\n\nThe page at {} returned a bot-wall challenge, not content. This needs tier 2 (JS challenge solving).\n\n*[verdict: {:?}, status: {}]*",
                    out.verdict, out.url, out.verdict, out.status
                );
                eprintln!("--- verdict={:?} status={}", out.verdict, out.status);
                return;
            }

            let ct = out
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            let t0 = std::time::Instant::now();
            let ex = match extract::extract(&out.body, &ct, &out.url, &opts) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
            };
            let extract_ms = t0.elapsed().as_secs_f64() * 1000.0;
            print!("{}", ex.markdown);
            eprintln!(
                "--- title={:?} kind={:?} verdict={:?} status={} fetch={:.0}ms extract={:.1}ms blocks={}/{} chars={}/{} tokens~{} next_offset={:?} thin={}",
                ex.title,
                ex.content_kind,
                out.verdict,
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
        _ => {
            eprintln!("donsetch — commands: fetch <url> | extract <url> [--focus q] [--max n] [--offset n] [--selector css] [--links] [--media] | fingerprint [url] | resume-test <url>");
        }
    }
}
