mod cli;
mod crawl;
mod detect;
mod error;
mod extract;
mod fetch;
mod ghost;
mod mcp;
mod memory;
mod paths;
mod pdf;
mod profile;
mod search;
mod spec;
use fetch::client::Fetcher;
use transport::proxy;
mod transport;

use profile::BrowserProfile;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        // ── Agent tools (spec-driven, shared core, clap-parsed) ──
        "fetch" | "search" | "crawl" => {
            let code = cli::tool::run(cmd, &args[2..]).await;
            std::process::exit(code as i32);
        }

        // ── Discovery ──
        "tools" => cli::tool::print_tools_json(),

        // ── Management ──
        "mcp" => {
            if let Err(e) = mcp::server::run().await {
                eprintln!("mcp daemon: {e}");
                std::process::exit(1);
            }
        }
        "keys" => cli::keys::run(&args),
        "proxy" => cli::proxy::run(&args).await,
        "status" => cli::status::run().await,
        "doctor" | "--doctor" => cli::doctor::run().await,
        "update" | "-u" | "--update" => cli::update::run().await,
        "rollback" | "--rollback" => cli::rollback::run(),
        "version" | "-v" | "--version" => cli::version::run().await,

        // ── Dev/internal (hidden from --help) ──
        "dev" => dev::dispatch(&args[2..]).await,
        // Backward-compat: bare dev commands still work.
        "probe" | "fingerprint" | "resume-test" | "ghost" | "extract" => {
            dev::dispatch(&args[1..]).await;
        }

        "help" | "-h" | "--help" => {
            // Route `donsetch help <command>` to the command's help.
            if let Some(sub) = args.get(2).map(|s| s.as_str()) {
                route_help(sub).await;
            } else {
                cli::tool::print_top_help();
            }
        }
        _ => {
            eprintln!("donsetch: unknown command '{cmd}'\n");
            cli::tool::print_top_help();
            std::process::exit(1);
        }
    }
}

// ── Dev commands ─────────────────────────────────────────

/// Route `donsetch help <command>` to the command's own help.
/// Falls back to top-level help for unknown commands.
async fn route_help(cmd: &str) {
    match cmd {
        "fetch" | "search" | "crawl" => {
            // Re-invoke with --help (clap handles the output).
            let help_args = vec!["--help".to_string()];
            let _ = cli::tool::run(cmd, &help_args).await;
        }
        "keys" => {
            cli::keys::run(&["donsetch".into(), "keys".into(), "help".into()]);
        }
        "proxy" => {
            // proxy::run is async, but print_help is sync.
            // Just call the help directly.
            println!("Usage: donsetch proxy <subcommand> [args]");
            println!();
            println!("Subcommands:");
            println!(
                "  add <url> [url...] [--no-check]  Add proxies (validated, optionally probed)"
            );
            println!("  remove <id> [id...]              Remove proxies by index or host:port");
            println!("  list                             Show all configured proxies");
            println!(
                "  check                            Probe all proxies (connectivity + exit IP)"
            );
            println!("  clear                            Remove all proxies");
            println!("  test <url>                       Test a proxy without adding it");
            println!("  import <file>                    Import from file (one URL per line)");
            println!("  export [file]                    Export to file (default: stdout)");
            println!();
            println!("Proxy URL format:");
            println!("  socks5://user:pass@host:port     SOCKS5 with auth (remote DNS, no leak)");
            println!("  socks5://host:port               SOCKS5 without auth");
            println!("  http://user:pass@host:port       HTTP CONNECT with auth");
            println!("  http://host:port                 HTTP CONNECT without auth");
            println!("  user:pass@host:port              Bare = HTTP CONNECT (backward compat)");
            println!("  host:port                        No auth, HTTP CONNECT");
            println!();
            println!("Config: cache_dir/proxies.txt (one URL per line, # comments)");
            println!(
                "Env:   DONSEEK_PROXIES (comma-separated, overrides config for same host:port)"
            );
        }
        "status" => {
            println!("Usage: donsetch status");
            println!();
            println!("  Shows a quick overview: version, search config, proxies, cache, health.");
            println!("  No probes, no browser launch — fast.");
            println!("  For full diagnostics, run `donsetch doctor`.");
        }
        "doctor" => {
            println!("Usage: donsetch doctor");
            println!();
            println!("  13 health checks: binary, network, TLS, browser, Xvfb, ghost profile,");
            println!("  cache, permissions, PDFium, OCR models, rerank model, ghost state.");
            println!("  Auto-fixes what it can. Reports issues with instructions.");
        }
        "update" => {
            println!("Usage: donsetch update");
            println!();
            println!("  Checks for a new release and downloads the platform-correct binary");
            println!("  from GitHub Releases. Verifies SHA256, replaces in place, saves backup.");
        }
        "rollback" => {
            println!("Usage: donsetch rollback");
            println!();
            println!("  Reverts to the previous binary version (saved by `donsetch update`).");
            println!("  Can be run again to roll forward.");
        }
        "version" => {
            println!("Usage: donsetch version");
            println!();
            println!("  Shows build info and checks for updates.");
        }
        "mcp" => {
            println!("Usage: donsetch mcp");
            println!();
            println!("  Starts the MCP server on stdio (JSON-RPC). Connect from your MCP client.");
        }
        "tools" => {
            println!("Usage: donsetch tools");
            println!();
            println!("  Prints the tool schemas as JSON (same as MCP tools/list).");
        }
        _ => {
            cli::tool::print_top_help();
        }
    }
}

// ── Dev commands ─────────────────────────────────────────────

mod dev {
    use super::*;

    pub async fn dispatch(args: &[String]) {
        let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
        match cmd {
            "extract" => extract_cmd(&args[1..]).await,
            "probe" => probe_cmd(&args[1..]).await,
            "fingerprint" => fingerprint_cmd(&args[1..]).await,
            "resume-test" => resume_test_cmd(&args[1..]).await,
            "ghost" => ghost_cmd(&args[1..]).await,
            _ => {
                eprintln!("dev subcommands: extract | probe | fingerprint | ghost | resume-test");
                eprintln!();
                eprintln!("  extract --input <file>     Extract from a local HTML/PDF file");
                eprintln!("  probe <url> [proxy] [dump]  Raw fetch with diagnostics");
                eprintln!("  fingerprint [url]            TLS/HTTP fingerprint check");
                eprintln!("  resume-test <url>            3x fetch to test connection reuse");
                eprintln!("  ghost <solve|render|shot> <url>  Ghost browser ops");
            }
        }
    }

    async fn extract_cmd(args: &[String]) {
        let mut input_file: Option<String> = None;
        let mut opts = extract::ExtractOptions::default();
        let mut i = 0;
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
                "--input" => {
                    i += 1;
                    input_file = args.get(i).cloned();
                }
                _ => {}
            }
            i += 1;
        }
        let Some(path) = input_file else {
            eprintln!(
                "usage: donsetch dev extract --input <file> [--focus q] [--max n] [--offset n] [--selector css]"
            );
            return;
        };
        let body = std::fs::read(&path).expect("read input");
        let sniff_ct = if body.starts_with(b"%PDF-") {
            "application/pdf"
        } else {
            "text/html"
        };
        let t0 = std::time::Instant::now();
        let ex = match extract::extract(&body, sniff_ct, "https://local/", &opts) {
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
    }

    async fn probe_cmd(args: &[String]) {
        let u = args.first().map(String::as_str).unwrap_or("");
        let px = args.get(1).and_then(|s| proxy::Proxy::parse(s).ok());
        let f = Fetcher::new(BrowserProfile::host_default()).expect("fetcher");
        match f.fetch_once_via(u, &[], px.as_ref(), false, None).await {
            Ok(out) => {
                println!(
                    "status={} alpn={} bytes={} verdict={:?} t={:.2}s",
                    out.status,
                    out.alpn,
                    out.body.len(),
                    out.verdict,
                    out.elapsed.as_secs_f64()
                );
                if let Some(p) = args.get(2) {
                    std::fs::write(p, &out.body).ok();
                    eprintln!("dumped -> {p}");
                }
            }
            Err(e) => println!("error: {e}"),
        }
    }

    async fn fingerprint_cmd(args: &[String]) {
        let url = args
            .first()
            .cloned()
            .unwrap_or_else(|| "https://tls.peet.ws/api/all".into());
        let fetcher = Fetcher::new(BrowserProfile::host_default()).expect("fetcher init");
        match fetcher.fetch(&url).await {
            Ok(out) => {
                println!(
                    "status: {} alpn: {} elapsed: {:?}",
                    out.status, out.alpn, out.elapsed
                );
                println!("{}", String::from_utf8_lossy(&out.body));
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }

    async fn resume_test_cmd(args: &[String]) {
        let url = args.first().expect("usage: donsetch dev resume-test <url>");
        let fetcher = Fetcher::new(BrowserProfile::host_default()).expect("fetcher init");
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

    async fn ghost_cmd(args: &[String]) {
        let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
        let profile = BrowserProfile::host_default();
        match sub {
            "solve" => {
                let url = args.get(1).expect("usage: ghost solve <url>");
                let t0 = std::time::Instant::now();
                let mut g = ghost::Ghost::launch(&profile, None).await.expect("launch");
                eprintln!("launched in {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);
                match ghost::ops::solve(&mut g, url, std::time::Duration::from_secs(30))
                    .await
                    .expect("solve")
                {
                    ghost::ops::SolveOutcome::Solved(r) => {
                        println!("SOLVED in {:?}", r.took);
                        println!("clearance: {}", ghost::ops::has_clearance(&r.cookies));
                        println!("cookies: {}", r.cookies.len());
                        for c in &r.cookies {
                            println!("  {} (domain {})", c.name, c.domain);
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
                let url = args.get(1).expect("usage: ghost render <url>");
                let mut g = ghost::Ghost::launch(&profile, None).await.expect("launch");
                let html = ghost::ops::render(&mut g, url, std::time::Duration::from_secs(30))
                    .await
                    .expect("render");
                println!("rendered {} bytes", html.len());
                let ex = extract::extract(
                    html.as_bytes(),
                    extract::charset::GHOST_TEXT_CT,
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
                let url = args.get(1).expect("usage: ghost shot <url> [path]");
                let path = args.get(2).cloned().unwrap_or_else(|| {
                    std::env::temp_dir()
                        .join("ghost.png")
                        .to_string_lossy()
                        .into_owned()
                });
                let mut g = ghost::Ghost::launch(&profile, None).await.expect("launch");
                g.navigate(url).await.expect("nav");
                tokio::time::sleep(std::time::Duration::from_secs(4)).await;
                g.screenshot(&path).await.expect("shot");
                println!("shot -> {path}");
                g.kill().await;
            }
            "selftest" => {
                let mut g = ghost::Ghost::launch(&profile, None).await.expect("launch");
                match ghost::ops::selftest(&mut g).await {
                    Ok(j) => println!("{j}"),
                    Err(e) => eprintln!("selftest: {e}"),
                }
                g.kill().await;
            }
            "freeze-check" => {
                let mut g = ghost::Ghost::launch(&profile, None).await.expect("launch");
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
            _ => {
                eprintln!("ghost subcommands: solve | render | shot | selftest | freeze-check")
            }
        }
    }
}
