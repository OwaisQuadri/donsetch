mod detect;
mod error;
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
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(1);
                }
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
        _ => {
            eprintln!("donsetch — commands: fetch <url> | fingerprint [url] | resume-test <url>");
        }
    }
}
