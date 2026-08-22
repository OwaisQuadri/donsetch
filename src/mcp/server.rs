//! The stdio server: read loop, dispatch, writer task,
//! and the fetch tool handler with full escalation.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use futures_util::FutureExt;

use crate::crawl::real as crawl_real;
use crate::crawl::{CrawlMode, CrawlOptions, Crawler};
use crate::detect::walls::{Vendor, Verdict};
use crate::error::FetchError;
use crate::extract::{self, ExtractOptions};
use crate::fetch::client::Fetcher;
use crate::ghost::cache::{CookieRecord, GhostState, RouteDecision};
use crate::ghost::manager::GhostManager;
use crate::ghost::ops;
use crate::profile::BrowserProfile;
use crate::search::byok::ByokSearcher;
use crate::search::egress::EgressPool;
use crate::search::intent::Intent;
use crate::search::{self, Searcher};

use super::tools;

/// Shared daemon state, built once, lives forever.
pub struct Daemon {
    fetcher: Arc<Fetcher>,
    profile: BrowserProfile,
    ghost_mgr: Arc<GhostManager>,
    state: Arc<Mutex<GhostState>>,
    searcher: Arc<Searcher>,
    byok: ByokSearcher,
    crawler: Crawler,
}

impl Daemon {
    pub async fn new() -> Result<Self, crate::error::FetchError> {
        let profile = BrowserProfile::host_default();
        let fetcher = Arc::new(Fetcher::new(profile.clone())?);
        let searcher = Arc::new(Searcher::new(
            Fetcher::new(profile.clone())?,
            EgressPool::from_env(),
        ));
        searcher.preflight();
        let proxies = crate::transport::proxy::load_all();
        let ghost_mgr = GhostManager::new().await;
        let state = Arc::new(Mutex::new(GhostState::load()));

        // Build ghost escalation hook for the crawl: renders
        // JS-only pages in the headless browser so SPA sites
        // yield real content instead of empty shells. Capped at
        // 3 per crawl by the orchestrator.
        let ghost_hook: crate::crawl::GhostHook = {
            let ghost_mgr = Arc::clone(&ghost_mgr);
            let profile = profile.clone();
            let fetcher = Arc::clone(&fetcher);
            let state = Arc::clone(&state);
            Arc::new(move |url: String| {
                let ghost_mgr = Arc::clone(&ghost_mgr);
                let profile = profile.clone();
                let fetcher = Arc::clone(&fetcher);
                let state = Arc::clone(&state);
                async move {
                    // Render cache shortcut.
                    {
                        let s = state.lock().await;
                        if let Some(rc) = s.render_for(&url) {
                            return Ok(crate::crawl::GhostRender {
                                html: rc.html.clone(),
                            });
                        }
                    }
                    let mut g = match ghost_mgr.acquire(&profile).await {
                        Ok(g) => g,
                        Err(e) => return Err(format!("browser launch: {e}")),
                    };
                    let page =
                        match ops::ghost_fetch(&mut g, &url, std::time::Duration::from_secs(20))
                            .await
                        {
                            Ok(p) => p,
                            Err(first) => {
                                // Retry once on transient timeout.
                                match ops::ghost_fetch(
                                    &mut g,
                                    &url,
                                    std::time::Duration::from_secs(20),
                                )
                                .await
                                {
                                    Ok(p) => p,
                                    Err(second) => {
                                        return Err(format!("render: {first}; retry: {second}"));
                                    }
                                }
                            }
                        };
                    if page.captcha {
                        return Err("interactive captcha (unsolvable by design)".to_string());
                    }
                    if !page.cookies.is_empty() {
                        fetcher.import_cookies(&page.cookies).await;
                    }
                    {
                        let mut s = state.lock().await;
                        s.record_render(&url, &page.html);
                    }
                    Ok(crate::crawl::GhostRender { html: page.html })
                }
                .boxed()
            })
        };

        let (crawler, _gov) = crawl_real::build(Arc::clone(&fetcher), proxies);
        let crawler = crawler.with_ghost(ghost_hook);
        Ok(Self {
            fetcher,
            profile,
            ghost_mgr,
            state,
            searcher,
            byok: ByokSearcher::new(),
            crawler,
        })
    }

    /// Shutdown: kill ghost browser + Xvfb (if owned).
    /// Called by the CLI before exit; by the MCP daemon on close.
    pub async fn shutdown(&self) {
        self.ghost_mgr.shutdown().await;
    }
}

/// Run the daemon until stdin closes. Never returns Err
/// on client garbage — only on fatal IO.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let daemon = Arc::new(Daemon::new().await?);
    let (tx, mut rx) = mpsc::channel::<String>(256);

    // Single writer: response lines can never interleave.
    let writer = tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(line) = rx.recv().await {
            // A broken stdout (client died, pipe closed) must not be
            // swallowed: every later response would be silently
            // dropped while the daemon pretends to serve. Log the
            // real cause and stop — the client is gone.
            if let Err(e) = out.write_all(line.as_bytes()).await {
                eprintln!("[mcp] stdout write failed, shutting down: {e}");
                std::process::exit(1);
            }
            if let Err(e) = out.write_all(b"\n").await {
                eprintln!("[mcp] stdout write failed, shutting down: {e}");
                std::process::exit(1);
            }
            if let Err(e) = out.flush().await {
                eprintln!("[mcp] stdout flush failed, shutting down: {e}");
                std::process::exit(1);
            }
        }
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let daemon = Arc::clone(&daemon);
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Some(resp) = handle(&daemon, &line).await {
                let _ = tx.send(resp).await;
            }
        });
    }

    // stdin EOF: graceful shutdown, no orphan browsers.
    drop(tx);
    daemon.ghost_mgr.shutdown().await;
    let _ = writer.await;
    Ok(())
}

/// Handle one line. Returns Some(response) for requests,
/// None for notifications.
async fn handle(daemon: &Arc<Daemon>, line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(
                json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32700, "message": "parse error" }
                })
                .to_string(),
            );
        }
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // Notifications (no id) that we recognize: stay silent.
    id.as_ref()?;
    let id = id.unwrap();

    let result: Result<Value, (i64, String)> = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list()),
        "tools/call" => call_tool(daemon, &params).await,
        "notifications/initialized" | "notifications/cancelled" => {
            return None;
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    let resp = match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }),
    };
    Some(resp.to_string())
}

fn initialize(params: &Value) -> Value {
    let asked = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    // Echo theirs if we speak it, else our max.
    let version = if tools::PROTOCOL_VERSIONS.contains(&asked) {
        asked
    } else {
        tools::PROTOCOL_VERSIONS[0]
    };
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": tools::SERVER_NAME,
            "version": tools::SERVER_VERSION
        }
    })
}

pub(crate) async fn call_tool(
    daemon: &Arc<Daemon>,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "web_fetch" => Ok(fetch_tool(daemon, &args).await),
        "web_search" => Ok(search_tool(daemon, &args).await),
        "web_crawl" => Ok(crawl_tool(daemon, &args).await),
        _ => Err((-32602, format!("unknown tool: {name}"))),
    }
}

/// The crawl tool: two-phase site walk. Phase 1 = sitemap
/// discovery (a map costs ~2 requests instead of N fetches);
/// Phase 2 = Governor-paced frontier walk riding DonShadow +
/// DonSift. Resume tokens make huge sites paginable.
#[allow(clippy::field_reassign_with_default)]
async fn crawl_tool(daemon: &Arc<Daemon>, args: &Value) -> Value {
    // Resume can work without a url (the seed is stored in the
    // resume state). If url is missing AND no resume token, error.
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.to_string(),
        // Empty string (the CLI's explicit resume-only positional) and
        // a missing key are the same case: the seed is loaded from
        // the resume state.
        None | Some("") => {
            if args.get("resume").and_then(Value::as_str).is_none() {
                return tool_error("crawl: url required (or provide resume token to continue)");
            }
            String::new()
        }
        Some(u) => return tool_error(format!("crawl: url must be http(s), got: {u}")),
    };
    let mut opts = CrawlOptions::default();
    opts.focus = args.get("focus").and_then(Value::as_str).map(String::from);
    opts.mode = match args.get("mode").and_then(Value::as_str).unwrap_or("full") {
        "map" => CrawlMode::Map,
        "content" => CrawlMode::Content,
        _ => CrawlMode::Full,
    };
    if let Some(n) = args.get("max_pages").and_then(Value::as_u64) {
        opts.max_pages = n.clamp(1, 200) as usize;
    }
    if let Some(n) = args.get("max_depth").and_then(Value::as_u64) {
        opts.max_depth = n.clamp(0, 8) as u32;
    }
    if let Some(n) = args.get("max_total_chars").and_then(Value::as_u64) {
        opts.max_total_chars = (n as usize).clamp(4_000, 500_000);
    }
    if let Some(n) = args.get("per_page_max").and_then(Value::as_u64) {
        opts.per_page_max = (n as usize).clamp(400, 40_000);
    }
    if let Some(a) = args.get("include_paths").and_then(Value::as_array) {
        opts.include_paths = a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
    }
    if let Some(a) = args.get("exclude_paths").and_then(Value::as_array) {
        opts.exclude_paths = a
            .iter()
            .filter_map(Value::as_str)
            .map(String::from)
            .collect();
    }
    if let Some(b) = args.get("same_host").and_then(Value::as_bool) {
        opts.same_host = b;
    }
    if let Some(b) = args.get("respect_robots").and_then(Value::as_bool) {
        opts.respect_robots = b;
    }
    if let Some(n) = args.get("deadline_s").and_then(Value::as_u64) {
        opts.deadline = std::time::Duration::from_secs(n.clamp(5, 600));
    }
    if let Some(q) = args.get("min_quality").and_then(Value::as_f64) {
        opts.min_quality = q.clamp(0.0, 1.0) as f32;
    }
    let resume = args.get("resume").and_then(Value::as_str).map(String::from);

    // SSRF guard on the seed — the fetch tool has one, the crawl
    // tool must too (it fetches just as hard).
    if !url.is_empty() {
        match url::Url::parse(&url) {
            Ok(u) => {
                if let Some(host) = u.host_str()
                    && crate::fetch::guards::is_ssrf_host(host)
                {
                    return tool_error(format!(
                        "blocked: {host} is a private/loopback address — SSRF guard"
                    ));
                }
            }
            Err(_) => return tool_error(format!("invalid URL: {url}")),
        }
    }

    // Ghost-warm: if this host was tier-2 solved recently, the
    // clearance cookies ride tier 1 from page one.
    if let Some(host) = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
    {
        let route = daemon.state.lock().await.route_for(&host);
        if let RouteDecision::Warm(cookies) = route {
            daemon.fetcher.import_cookies(&cookies).await;
        }
    }

    let crawl_t0 = std::time::Instant::now();
    let result = match daemon.crawler.crawl(&url, opts, resume.as_deref()).await {
        Ok(r) => r,
        Err(e) => {
            // Crawl failures are input errors (bad seed / expired
            // resume token) — permanent, not worth a blind retry.
            // Classify honestly so the agent doesn't burn calls.
            let msg = e.to_ascii_lowercase();
            let (kind, hint) = if msg.contains("resume token") {
                (
                    "permanent",
                    "the resume token is expired or unknown — start a fresh crawl (omit resume)",
                )
            } else if msg.contains("bad seed") || msg.contains("must have a host") {
                (
                    "permanent",
                    "check the seed URL format (full scheme + host, e.g. https://example.com/docs/)",
                )
            } else {
                (
                    "transient",
                    "safe to retry immediately; if repeated, lower max_pages or widen deadline_s",
                )
            };
            let mut trace = Trace::default();
            trace.step("crawl", "crawl", "error", crawl_t0.elapsed().as_millis());
            return tool_error_structured(
                format!("crawl: {e}"),
                kind,
                Some(json!({
                    "url": url,
                    "escalation": trace.value(),
                    "next_action": hint,
                })),
            );
        }
    };

    // Content text: the map (if any) + pages. Keep the lead-in
    // small; the pages are the payload.
    let mut text = String::new();
    text.push_str(&format!(
        "# crawl: {} ({} pages, stop={:?}, {:.1}s)\n\n",
        result.seed,
        result.pages.len(),
        result.stop,
        result.elapsed.as_secs_f64()
    ));
    // A crawl-delay-pace crawl looks hung without this note —
    // the site demanded the pace, we honored it, say so.
    if let Some(cd) = result.crawl_delay
        && cd > 2.0
    {
        text.push_str(&format!(
            "*robots crawl-delay: {cd:.0}s between requests (site-declared; pass respect_robots=false to override)*\n\n"
        ));
    }
    if !result.map.is_empty() {
        text.push_str("## map\n");
        for u in &result.map {
            text.push_str(&format!("- {u}\n"));
        }
        text.push('\n');
    }
    for p in &result.pages {
        if p.duplicate {
            continue;
        }
        text.push_str(&format!("## [{}] {}\n", p.title, p.url));
        text.push_str(&format!(
            "kind={:?} quality={:.2} {} chars\n\n",
            p.kind, p.quality, p.chars
        ));
        text.push_str(&p.markdown);
        text.push_str("\n\n---\n\n");
    }
    if !result.skipped.is_empty() {
        text.push_str("## skipped\n");
        for (u, why) in &result.skipped {
            text.push_str(&format!("- {u}: {why}\n"));
        }
    }
    if let Some(tok) = &result.resume {
        text.push_str(&format!(
            "\nresume: call crawl again with resume={tok} to continue.\n"
        ));
    }

    // Agent guidance: next_action tells the agent what to try
    // next when results are poor or empty. Computed from the
    // stop reason, skip reasons, and page count.
    let next_action = compute_crawl_next_action(&result);
    if !next_action.is_empty() {
        text.push_str(&format!("\n💡 {next_action}\n"));
    }

    let structured = json!({
        "seed": result.seed,
        "pages": result.pages.iter().filter(|p| !p.duplicate).map(|p| json!({
            "url": p.url,
            "title": p.title,
            "kind": format!("{:?}", p.kind),
            "chars": p.chars,
            "quality": p.quality,
            "parent": p.parent,
            "score": (p.score * 100.0).round() / 100.0,
            "lastmod": p.lastmod,
        })).collect::<Vec<_>>(),
        "map": result.map,
        "queued": result.queued,
        "filtered_out": result.filtered_out,
        "skipped": result.skipped.iter().map(|(u, w)| json!({"url": u, "reason": w})).collect::<Vec<_>>(),
        "stop": format!("{:?}", result.stop),
        "crawl_delay": result.crawl_delay,
        "elapsed_s": result.elapsed.as_secs_f64(),
        "resume": result.resume,
        "next_action": next_action,
    });
    let mut meta = json!({
        "seed": result.seed,
        "pages": result.pages.iter().filter(|p| !p.duplicate).count(),
        "stop": format!("{:?}", result.stop),
        "elapsed_s": (result.elapsed.as_secs_f64() * 10.0).round() / 10.0,
    });
    if let Some(tok) = &result.resume {
        meta["resume"] = json!(tok);
    }
    if !next_action.is_empty() {
        meta["next_action"] = json!(next_action);
    }
    json!({
        "content": [
            {"type": "text", "text": format!("[meta] {}", compact_json(&meta))},
            {"type": "text", "text": text},
        ],
        "structuredContent": structured
    })
}

/// Compute actionable guidance for the agent based on crawl
/// results. Returns an empty string when the crawl succeeded
/// normally (no guidance needed).
fn compute_crawl_next_action(result: &crate::crawl::CrawlResult) -> String {
    use crate::crawl::StopReason;

    // Resume available — always suggest it first.
    if let Some(tok) = &result.resume {
        return format!(
            "resume={tok} to continue crawling (stopped: {:?}).",
            result.stop
        );
    }

    // 0 pages — diagnose why.
    if result.pages.is_empty() {
        let skip_reasons: Vec<&str> = result.skipped.iter().map(|(_, w)| w.as_str()).collect();
        let all_scope = skip_reasons
            .iter()
            .all(|r| r.contains("out of scope") || r.contains("filtered"));
        let all_blocked = skip_reasons
            .iter()
            .all(|r| r.contains("Challenge") || r.contains("Blocked") || r.contains("wall"));
        let all_404 = skip_reasons
            .iter()
            .all(|r| r.contains("404") || r.contains("NotFound"));
        let has_sitemap = !result.map.is_empty();

        if all_404 {
            return "seed URL returned 404 — check the URL is correct.".into();
        }
        if all_blocked {
            return "the site blocked the crawler. Try respect_robots=false, or fetch the seed URL directly first to check access.".into();
        }
        if all_scope && result.filtered_out > 0 {
            return "all discovered URLs were outside the seed's path scope. Try broader include_paths, or same_host=false to crawl the whole host.".into();
        }
        if !has_sitemap && result.map.is_empty() && result.filtered_out == 0 {
            return "no sitemap found and no links discovered. Try mode=content to BFS from the seed, or check the seed URL is accessible.".into();
        }
        return "crawl returned 0 pages. Try mode=content, broader include_paths, or a different seed URL.".into();
    }

    // Pages found but stopped early.
    match result.stop {
        StopReason::MaxPages => {
            "crawl hit the page budget. Increase max_pages or use resume to continue.".into()
        }
        StopReason::CharBudget => {
            "crawl hit the character budget. Increase max_total_chars or use resume to continue."
                .into()
        }
        StopReason::Deadline => {
            "crawl hit the time deadline. Increase deadline_s or use resume to continue.".into()
        }
        StopReason::ThrottledOut => {
            "the host throttled the crawler. Wait a few minutes and resume.".into()
        }
        StopReason::DepthLimit => {
            "crawl hit the depth limit. Increase max_depth to discover more pages.".into()
        }
        StopReason::FrontierEmpty => String::new(), // normal completion
    }
}

/// Map a raw FetchError to a user-friendly diagnostic.
/// No Rust internals, no TLS jargon — clean, actionable.
fn friendly_fetch_error(e: &FetchError) -> String {
    match e {
        FetchError::Timeout => "request timed out (the server took too long to respond)".into(),
        FetchError::TooManyRedirects => "too many redirects (the URL loops)".into(),
        FetchError::InvalidUrl(u) => format!("invalid URL: {u}"),
        FetchError::Tls(msg) => {
            // TLS errors: strip the raw SSL/BoringSSL internals.
            let msg = msg.to_lowercase();
            if msg.contains("certificate") || msg.contains("handshake") {
                "TLS error: the server's certificate or handshake failed".into()
            } else if msg.contains("reset") || msg.contains("eof") {
                "connection reset by server".into()
            } else {
                "TLS connection failed".into()
            }
        }
        FetchError::Io(e) => {
            let msg = e.to_string();
            if msg.contains("refused") {
                "connection refused (the server is not accepting connections)".into()
            } else if msg.contains("timed out") {
                "connection timed out".into()
            } else if msg.contains("not found") || msg.contains("no address") {
                "host not found (DNS lookup failed)".into()
            } else if msg.contains("reset") {
                "connection reset by server".into()
            } else {
                format!("network error: {e}")
            }
        }
        FetchError::Http(msg) => {
            // h1/h2 protocol errors: strip raw parser messages.
            let msg = msg.to_lowercase();
            if msg.contains("eof before headers") {
                "server closed the connection before sending a response".into()
            } else if msg.contains("read_server_hello") {
                "TLS handshake failed (server rejected the connection)".into()
            } else {
                format!("HTTP protocol error: {e}")
            }
        }
        FetchError::Ghost(msg) => format!("browser automation error: {msg}"),
    }
}

/// Map a Verdict + status code to a clean, specific error message.
/// Distinguishes genuine blocks from upstream errors from SPAs.
fn verdict_error(verdict: Verdict, status: u16, url: &str) -> String {
    match verdict {
        Verdict::AuthWall => {
            format!("HTTP 401 at {url} — the server requires authentication")
        }
        Verdict::Paywall => format!("paywall: {url} requires payment to view content"),
        Verdict::SoftNotFound => format!("not found: {url} returned HTTP {status}"),
        Verdict::Blocked => {
            // 403/429 without challenge markers = upstream block, not a bot wall.
            match status {
                403 => format!("forbidden: {url} returned HTTP 403 (access denied)"),
                429 => format!("rate limited: {url} returned HTTP 429 (too many requests)"),
                503 => format!(
                    "service unavailable: {url} returned HTTP 503 (server overloaded or down)"
                ),
                _ => format!("blocked: {url} returned HTTP {status}"),
            }
        }
        Verdict::Challenge(v) => format!(
            "bot wall: {url} is protected by {:?} (try fetch with tier=2 for headless browser)",
            v
        ),
        Verdict::ContentOk => format!("unexpected error: {url} (status {status})"),
    }
}

/// The fetch tool: tier 1 → verdict → ghost solve/render
/// → DonSift. Ports the CLI escalation into the daemon,
/// with warm-start and render cache.
#[allow(clippy::field_reassign_with_default)]
async fn fetch_tool(daemon: &Arc<Daemon>, args: &Value) -> Value {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.to_string(),
        _ => return tool_error("fetch: url must be http(s)"),
    };
    // Full parse up front: an unparseable URL would otherwise flow
    // through the whole pipeline with host="" — poisoning domain
    // profiles and producing confusing late errors.
    let parsed_url = match url::Url::parse(&url) {
        Ok(u) => u,
        Err(e) => return tool_error(format!("fetch: invalid URL ({e})")),
    };
    let url_host = parsed_url.host_str().unwrap_or("").to_string();

    // Universal reddit optimization: rewrite all reddit.com
    // URLs to old.reddit.com — Reddit's legacy SSR domain
    // serves real content to plain HTTP clients. No JS shell,
    // no login overlay, no CAPTCHA. One cheap tier-1 request
    // beats a 60s ghost burn. The dedicated reddit extractor
    // in extract/reddit.rs formats the output.
    let url = if let Ok(mut u) = url::Url::parse(&url) {
        match u.host_str() {
            Some("www.reddit.com") | Some("reddit.com") => {
                let _ = u.set_host(Some("old.reddit.com"));
                u.to_string()
            }
            _ => url,
        }
    } else {
        url
    };

    // SSRF guard: never fetch private/loopback addresses.
    let parsed = match url::Url::parse(&url) {
        Ok(u) => u,
        Err(_) => return tool_error(format!("invalid URL: {url}")),
    };
    if let Some(host) = parsed.host_str()
        && crate::fetch::guards::is_ssrf_host(host)
    {
        return tool_error_structured(
            format!("blocked: {host} is a private/loopback address — SSRF guard"),
            "permanent",
            Some(json!({
                "url": url,
                "next_action": "private/loopback targets are blocked by design — use a public URL",
            })),
        );
    }
    let mut opts = ExtractOptions::default();
    opts.focus = args.get("focus").and_then(Value::as_str).map(String::from);
    opts.max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|n| (n as usize).clamp(200, 1_048_576));
    opts.offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(1_000_000_000) as usize;
    opts.section = args
        .get("section")
        .and_then(Value::as_str)
        .map(String::from);
    opts.selector = args
        .get("selector")
        .and_then(Value::as_str)
        .map(String::from);
    opts.toc = args.get("toc").and_then(Value::as_bool).unwrap_or(false);
    opts.include_links = args.get("links").and_then(Value::as_bool).unwrap_or(false);
    opts.include_media = args.get("media").and_then(Value::as_bool).unwrap_or(false);
    let tier = args.get("tier").and_then(Value::as_str).unwrap_or("auto");
    let shot = args.get("shot").and_then(Value::as_str);

    // === v2: fetch-actions — browser control INSIDE fetch ===
    // A non-empty `actions` array routes the whole call to the
    // ghost with an action executor: navigate → act → extract.
    // Parsing/validation happens before any browser time is
    // spent; a typo in step 5 must not burn a launch on step 1.
    let actions = match args.get("actions") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => match crate::ghost::actions::parse(v) {
            Ok(a) => a,
            Err(e) => return tool_error(format!("fetch: {e}")),
        },
    };
    if !actions.is_empty() {
        if is_pdf_url_like(&url) {
            return tool_error(
                "fetch: actions cannot run on PDFs — fetch the PDF directly instead",
            );
        }
        if tier == "1" {
            return tool_error(
                "fetch: actions need the browser — use tier=auto (default) or tier=2",
            );
        }
        return fetch_with_actions(daemon, &url, &url_host, &opts, &actions, shot).await;
    }

    let host = url_host;

    // === PDF early detection ===
    // Ghost can't render PDFs (Chrome's PDF viewer is a JS shell).
    // If the URL looks like a PDF, always fetch raw bytes (tier 1)
    // and route to the DonSheet engine. Never skip tier 1 for PDFs.
    // Uses the SAME helper as the actions guard — covers both the
    // `.pdf` suffix and the `/pdf/` path convention (arXiv serves
    // PDFs at /pdf/1706.03762 with no extension).
    let is_pdf_url = is_pdf_url_like(&url);

    // === Decision: how to route this fetch? ===
    // The self-improving loop: the domain profile decides
    // cold / warm / skip-to-solve / recheck-cold.
    // Reddit is always SSR (old.reddit.com rewrite) — never
    // needs a browser. Force Cold route even if a stale
    // profile says SkipToSolve (from a previous Xvfb failure
    // that poisoned the domain). old.reddit.com serves 80KB+
    // of server-rendered HTML to plain HTTP clients.
    let is_reddit = host.ends_with("reddit.com");
    let route = if tier == "2" && !is_pdf_url && !is_reddit {
        RouteDecision::SkipToSolve
    } else if tier == "1" || is_pdf_url || is_reddit {
        RouteDecision::Cold
    } else {
        daemon.state.lock().await.route_for(&host)
    };

    let warm_cookies: Vec<CookieRecord> = match &route {
        RouteDecision::Warm(c) => c.clone(),
        _ => Vec::new(),
    };
    let is_warm = !warm_cookies.is_empty();
    let is_recheck = matches!(route, RouteDecision::RecheckCold);
    let skip_tier1 = matches!(route, RouteDecision::SkipToSolve);

    let mut tier_used = "1";
    if is_warm {
        daemon.fetcher.import_cookies(&warm_cookies).await;
        tier_used = "1(warm)";
    } else if is_recheck {
        tier_used = "1(recheck)";
    } else if skip_tier1 {
        tier_used = "2-direct";
    }

    let mut trace = Trace::default();
    let route_name = match &route {
        RouteDecision::Cold => "cold",
        RouteDecision::Warm(_) => "warm",
        RouteDecision::SkipToSolve => "skip-to-solve",
        RouteDecision::RecheckCold => "recheck-cold",
    };
    trace.step("route", "domain-profile", route_name, 0);

    // === Fetch (tier 1, unless skipped) ===
    let mut out: Option<crate::fetch::client::FetchOutcome> = None;

    if !skip_tier1 {
        let t0 = std::time::Instant::now();
        let fetched = match daemon.fetcher.fetch(&url).await {
            Ok(o) => o,
            Err(e) => {
                return tool_error_structured(
                    friendly_fetch_error(&e),
                    fetch_error_kind(&e),
                    Some(json!({
                        "url": url,
                        "status": 0,
                        "next_action": next_action_for(None, 0, fetch_error_kind(&e)),
                        "escalation": trace.value(),
                    })),
                );
            }
        };
        let ms = t0.elapsed().as_millis();
        let verdict_str = format!("{:?}", fetched.verdict);
        trace.step(
            "1",
            "http-fetch",
            &format!("{} status={}", verdict_str, fetched.status),
            ms,
        );
        out = Some(fetched);

        // === Observe the outcome ===
        // Every fetch teaches the domain profile something — but
        // only CHALLENGES say anything about walls. A 404, 429,
        // paywall, or auth wall is an honest terminal answer from
        // the origin; recording it as "walled" used to poison easy
        // domains into permanent skip-to-solve (every later fetch
        // burned a 20s ghost launch on a 404).
        let o = out.as_ref().unwrap();
        {
            let mut state = daemon.state.lock().await;
            match o.verdict {
                Verdict::Challenge(_) => {
                    if is_warm {
                        // Warm cookies went stale — learn the real lifetime.
                        state.record_warm_stale(&host);
                    } else {
                        // Cold (or recheck) was challenged — domain needs tier 2.
                        let vendor = match &o.verdict {
                            Verdict::Challenge(v) => Some(format!("{v:?}").to_lowercase()),
                            _ => None,
                        };
                        state.record_cold_walled(&host, vendor.as_deref());
                    }
                }
                Verdict::ContentOk => {
                    if is_warm {
                        // Warm succeeded — refresh the cookie vault (write-back).
                        let snap = daemon.fetcher.jar_snapshot(&host);
                        state.record_warm_ok(&host, &snap);
                    } else {
                        // Cold (or recheck) succeeded — if was needs_tier2, wall is gone.
                        state.record_cold_ok(&host);
                    }
                }
                // Everything else (404, rate-limit, paywall, auth,
                // hard block): counters only, no wall inference.
                _ => state.record_fetch(&host),
            }
        }
    }

    // === Verdict gate: everything except ContentOk/Challenge ===
    // is a terminal, legitimate response — clean error, no ghost.
    // Challenge on an explicit tier=1 request is also terminal.
    if let Some(o) = &out {
        match o.verdict {
            Verdict::ContentOk => {}
            Verdict::Challenge(_) if tier != "1" => {}
            v => {
                let kind = verdict_kind(v, o.status);
                return tool_error_structured(
                    verdict_error(v, o.status, &o.url),
                    kind,
                    Some(json!({
                        "url": o.url,
                        "status": o.status,
                        "verdict": format!("{:?}", v),
                        "next_action": next_action_for(Some(v), o.status, kind),
                        "escalation": trace.value(),
                    })),
                );
            }
        }
    }

    // === Tier-1 extraction (when we have a body) ===
    let mut final_ex: Option<extract::Extracted> = None;
    let mut final_tier: &str = tier_used;
    let mut final_status: u16 = out.as_ref().map(|o| o.status).unwrap_or(0);
    let mut final_url: String = url.clone();
    let mut final_verdict: String = out
        .as_ref()
        .map(|o| format!("{:?}", o.verdict))
        .unwrap_or_else(|| "ContentOk".to_string());

    if let Some(o) = &out
        && matches!(o.verdict, Verdict::ContentOk)
    {
        let ct = o
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        // Binary content guard: images, video, audio, etc.
        // Don't pass binary bytes to extract (mojibake).
        if crate::fetch::guards::is_binary(&o.body, &ct) {
            let kind = ct.split(';').next().unwrap_or("unknown").trim();
            return tool_error_structured(
                format!(
                    "binary content: {url} returned {kind} ({} bytes) — not text, cannot extract",
                    o.body.len()
                ),
                "permanent",
                Some(json!({
                    "url": url,
                    "next_action": "this URL is a raw file, not a page — if it is a PDF, fetch it directly (DonSeTch parses PDFs); otherwise look for an HTML landing page via web_search",
                })),
            );
        }
        match extract::extract(&o.body, &ct, &o.url, &opts) {
            Ok(e) => {
                final_url = o.url.clone();
                final_ex = Some(e);
            }
            Err(e) => {
                return tool_error_structured(
                    format!("content extraction failed: {e}"),
                    "transient",
                    Some(json!({
                        "url": url,
                        "next_action": "retry with a narrow selector= or focus=; if the page is JS-heavy, tier=2 renders it in a browser",
                    })),
                );
            }
        }
    }

    let ex_thin = final_ex.as_ref().map(|e| e.thin).unwrap_or(false);
    let challenge = out
        .as_ref()
        .map(|o| matches!(o.verdict, Verdict::Challenge(_)))
        .unwrap_or(false);

    // Warm cookies that only buy a SHELL are stale cookies — but
    // the evidence must be a shell, not an extraction gap. A warm
    // ContentOk whose body is big yet nearly invisible-text-free
    // (JS shell) means the clearance bought nothing. A body with
    // rich visible text that extracts thin is a DonSift gap —
    // killing valid cookies for it is the gallery-page bug.
    let shell_warm = is_warm && ex_thin && {
        let o = out.as_ref().unwrap();
        o.body.len() > 20_000
            && (crate::detect::walls::visible_text_count(&o.body) as f64 / o.body.len() as f64)
                < 0.02
    };
    if shell_warm {
        daemon.state.lock().await.record_warm_stale(&host);
    }

    // Tier-1 links fallback: listing/feed pages over plain
    // HTTP (Hacker News, indexes) die in the prose pipeline
    // simply for being link-dense. Try links-keeping
    // extraction before any ghost work.
    if final_ex.as_ref().map(|e| e.thin).unwrap_or(false)
        && !opts.include_links
        && let Some(o) = &out
        && matches!(o.verdict, Verdict::ContentOk)
    {
        let ct = o
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let mut lopts = opts.clone();
        lopts.include_links = true;
        if let Ok(e3) = extract::extract(&o.body, &ct, &o.url, &lopts)
            && !e3.thin
        {
            final_ex = Some(e3);
            final_tier = "1(links)";
            trace.step("1", "links-extract", "ok", 0);
        }
    }

    // === Tier 2 via ghost (unified) ===
    // Triggers: explicit tier 2, profile skip-to-solve, challenge
    // wall, or tier 1 produced only a JS shell on auto tier.
    // (thin recomputed AFTER the tier-1 links fallback.)
    //
    // Exception: very small pages (< 5KB) that came back thin are
    // 404/error pages, not JS shells. JS shells are > 50KB (React
    // apps, SPAs). A 2KB page with no content is a 404 — don't
    // waste 20s launching a browser for it.
    let still_thin = final_ex.as_ref().map(|e| e.thin).unwrap_or(false);
    let page_size = out.as_ref().map(|o| o.body.len()).unwrap_or(0);
    // PDF detection: if the response is a PDF (content-type or magic
    // bytes), never escalate to ghost — Chrome's PDF viewer is a JS
    // shell with no extractable text. PDFs are handled by DonSheet.
    let is_pdf_content = out
        .as_ref()
        .map(|o| {
            let ct = o
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                .map(|(_, v)| v.clone())
                .unwrap_or_default();
            crate::fetch::guards::is_pdf(&o.body, &ct)
        })
        .unwrap_or(is_pdf_url);
    // Small 404 check: a small thin page is likely a 404/error.
    // But a small PDF is still a PDF — DonSheet handles it.
    let is_small_404 =
        page_size > 0 && page_size < 5_000 && still_thin && !challenge && !is_pdf_content;
    let need_ghost = !is_pdf_content
        && !is_reddit // old.reddit.com is SSR — never needs a browser
        && ((challenge && tier != "1" && !is_small_404)
            || skip_tier1
            || (still_thin && tier == "auto" && !is_small_404));

    if need_ghost {
        // Render-cache shortcut: a previously recovered DOM.
        // Verified non-thin AND non-challenge before serving — the
        // cache used to store shells and challenge interstitials,
        // re-serving them forever as ContentOk.
        if ex_thin
            && tier == "auto"
            && let Some(rc) = daemon.state.lock().await.render_for(&final_url).cloned()
            && let Ok(e2) = extract::extract(
                rc.html.as_bytes(),
                extract::charset::GHOST_TEXT_CT,
                &final_url,
                &opts,
            )
            && !e2.thin
        {
            // Defense in depth: even if a challenge page slipped into
            // the cache (pre-fix), don't serve it as ContentOk.
            let cached_verdict = crate::detect::walls::detect_dom_smart(rc.html.as_bytes());
            if !matches!(cached_verdict, crate::detect::walls::Verdict::Challenge(_)) {
                let vstr = format!("{:?}", cached_verdict);
                trace.step("cache", "render-hit", "ok", 0);
                let mut res =
                    finish_result(&e2, "render-cache", final_status, &vstr, &final_url, &trace);
                res["_meta"] = json!({ "ttlMs": 300_000, "cacheScope": "session" });
                return res;
            }
        }

        match ghost_escalate(
            daemon,
            &url,
            &host,
            &opts,
            challenge || shell_warm || skip_tier1,
            shot,
            &mut trace,
        )
        .await
        {
            Ok((e, tier2, status, furl)) => {
                final_ex = Some(e);
                final_tier = tier2;
                final_status = status;
                final_url = furl;
                // Ghost beat the challenge — the verdict should reflect
                // the actual content, not the tier-1 wall that was
                // bypassed. Without this, a successfully rendered page
                // shows "Challenge(DataDome)" in the verdict field.
                final_verdict = "ContentOk".to_string();
            }
            Err((msg, kind)) => {
                // A ghost failure on a warm-routed fetch means the
                // cookies no longer clear the wall — count it as the
                // second warm failure so the vault clears (first was
                // the tier-1 challenge that triggered escalation).
                if is_warm {
                    daemon.state.lock().await.record_warm_stale(&host);
                }
                return tool_error_structured(
                    msg,
                    kind,
                    Some(json!({
                        "url": url,
                        "status": final_status,
                        "verdict": final_verdict,
                        "next_action": next_action_for(out.as_ref().map(|o| o.verdict), final_status, kind),
                        "escalation": trace.value(),
                    })),
                );
            }
        }
    }

    let Some(ex) = final_ex else {
        return tool_error_structured(
            "all fetch tiers exhausted — no response received",
            "permanent",
            Some(json!({
                "url": url,
                "status": 0,
                "next_action": "retry — if repeated, the site may be down",
                "escalation": trace.value(),
            })),
        );
    };

    // Small 404 page: if we didn't escalate to ghost (is_small_404)
    // and the extraction is still thin/empty, return "not found".
    // This is honest — the page exists (HTTP 200) but has no content.
    // Could be a non-existent product, a deleted page, or a soft 404.
    if is_small_404 {
        return tool_error_structured(
            format!(
                "not found: {url} — page returned no content (may not exist or requires JavaScript)"
            ),
            "permanent",
            Some(json!({
                "url": url,
                "status": final_status,
                "verdict": "SoftNotFound",
                "next_action": next_action_for(Some(Verdict::SoftNotFound), final_status, "permanent"),
                "escalation": trace.value(),
            })),
        );
    }

    finish_result(
        &ex,
        final_tier,
        final_status,
        &final_verdict,
        &final_url,
        &trace,
    )
}

/// Unified tier-2: ghost render + cookie harvest + tier-1 retry,
/// then pick the candidate with the best content yield. Ok ONLY
/// when a candidate extracts as real content — a shell is a
/// failure, never a success. This is the loop the design always
/// promised: escalate, render, hand cookies back to tier 1.
///
/// `learn` = this escalation was WALL-DRIVEN (challenge seen, warm
/// cookies bought a shell, or the profile routed skip-to-solve).
/// A wall-driven success records the solve so the next fetch can
/// ride warm tier 1 — with `replay_ok` set from the tier-1 retry's
/// actual outcome. A pure SPA render (thin content, no wall) never
/// touches the domain profile: the site isn't walled, it's JS-only.
async fn ghost_escalate(
    daemon: &Arc<Daemon>,
    url: &str,
    host: &str,
    opts: &ExtractOptions,
    learn: bool,
    shot: Option<&str>,
    trace: &mut Trace,
) -> Result<(extract::Extracted, &'static str, u16, String), (String, &'static str)> {
    let t0 = std::time::Instant::now();
    let mut g = daemon
        .ghost_mgr
        .acquire(&daemon.profile)
        .await
        .map_err(|e| (format!("browser launch failed: {e}"), "permanent"))?;
    trace.step("2", "browser-launch", "ok", t0.elapsed().as_millis());
    let t1 = std::time::Instant::now();
    let page = match ops::ghost_fetch(&mut g, url, std::time::Duration::from_secs(20)).await {
        Ok(p) => p,
        Err(e) => {
            // CDP timeouts on first attempt are transient — the
            // browser was still warming up. Retry once before
            // conceding a permanent failure.
            if std::env::var_os("DONGHOST_DEBUG").is_some() {
                eprintln!("[ghost_escalate] first attempt failed: {e}, retrying...");
            }
            ops::ghost_fetch(&mut g, url, std::time::Duration::from_secs(20))
                .await
                .map_err(|e| (format!("browser automation error: {e}"), "permanent"))?
        }
    };
    trace.step(
        "2",
        "ghost-render",
        &format!("captcha={} dom={}KB", page.captcha, page.html.len() / 1024),
        t1.elapsed().as_millis(),
    );
    if std::env::var_os("DONGHOST_DEBUG").is_some() {
        let safe: String = host
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let p = std::env::temp_dir().join(format!("donsetch-dom-{safe}.html"));
        let _ = std::fs::write(&p, &page.html);
        eprintln!(
            "[ghost_escalate] dom={}B dumped to {}",
            page.html.len(),
            p.display()
        );
    }
    if page.captcha {
        if let Some(p) = shot {
            let _ = g.screenshot(p).await;
        }
        return Err((
            format!(
                "blocked at {url} — interactive captcha or challenge could not be solved automatically. Use an Agent browser to browse sites like these"
            ),
            "walled",
        ));
    }
    if !page.cookies.is_empty() {
        daemon.fetcher.import_cookies(&page.cookies).await;
    }
    // Retry tier 1 with fresh cookies — the cheap path back to
    // normal HTTP when the gate was cookie-driven.
    let t2 = std::time::Instant::now();
    let retry = if !page.cookies.is_empty() {
        let r = daemon.fetcher.fetch(url).await.ok();
        trace.step(
            "1",
            "http-retry-with-ghost-cookies",
            &format!(
                "cookies={} status={}",
                page.cookies.len(),
                r.as_ref().map(|o| o.status).unwrap_or(0)
            ),
            t2.elapsed().as_millis(),
        );
        r
    } else {
        None
    };

    // Replay verification: cookies are only "warm-worthy" when the
    // tier-1 retry returned real content with them. A walled or
    // shell retry means the vendor binds clearance to the browser
    // fingerprint — record replay_ok=false so route_for never
    // serves a doomed Warm roundtrip again.
    let mut replay_content_ok = false;

    // The retry is the oracle of record for TERMINAL verdicts: a
    // 404/paywall on tier 1 means the ghost spent its time
    // rendering a dead page (browsers render 404s too). The ghost's
    // pretty DOM must never launder a dead URL into ContentOk.
    //
    // AuthWall is deliberately excluded: an auth wall on the
    // retry means the HTTP path can't authenticate, but the
    // browser may have (Chromium handles userinfo/cookies/JS
    // auth natively). Discarding the ghost's content because the
    // tier-1 retry hit a wall the browser already cleared is
    // the core tier-2 regression in issue #15.
    if let Some(r) = &retry
        && matches!(r.verdict, Verdict::SoftNotFound | Verdict::Paywall)
    {
        let kind = verdict_kind(r.verdict, r.status);
        return Err((verdict_error(r.verdict, r.status, &r.url), kind));
    }

    // Candidates: retry bytes (cheap path) and the ghost's own
    // rendered DOM. Non-thin always beats thin; within a class,
    // bigger yield wins. The old code always preferred the retry
    // and discarded the browser's work — the core tier-2 bug.
    let mut best: Option<(bool, extract::Extracted, &'static str, u16, String)> = None;

    if let Some(r) = &retry
        && matches!(r.verdict, Verdict::ContentOk)
    {
        let ct = r
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        if !crate::fetch::guards::is_binary(&r.body, &ct)
            && let Ok(e) = extract::extract(&r.body, &ct, &r.url, opts)
        {
            let thin = e.thin;
            replay_content_ok = !thin;
            let better = match &best {
                None => true,
                Some((bt, be, ..)) => {
                    (!thin && *bt) || (thin == *bt && e.total_chars > be.total_chars)
                }
            };
            if better {
                best = Some((thin, e, "1+ghost-solve", r.status, r.url.clone()));
            }
        }
    }
    if let Ok(e2) = extract::extract(
        page.html.as_bytes(),
        extract::charset::GHOST_TEXT_CT,
        url,
        opts,
    ) {
        let thin = e2.thin;
        let better = match &best {
            None => true,
            Some((bt, be, ..)) => {
                (!thin && *bt) || (thin == *bt && e2.total_chars > be.total_chars)
            }
        };
        if better {
            best = Some((
                thin,
                e2,
                "ghost-dom",
                retry.as_ref().map(|r| r.status).unwrap_or(200),
                url.to_string(),
            ));
        }
    }

    // Links fallback: listing/feed pages (marketplaces, SERPs,
    // thread indexes) are link-dense by nature — the prose-tuned
    // pipeline kills them. Re-extract with links kept as a last
    // candidate before conceding.
    if best.as_ref().map(|(thin, ..)| *thin).unwrap_or(true) {
        let mut lopts = opts.clone();
        lopts.include_links = true;
        if let Ok(e3) = extract::extract(
            page.html.as_bytes(),
            extract::charset::GHOST_TEXT_CT,
            url,
            &lopts,
        ) {
            let thin = e3.thin;
            let better = match &best {
                None => true,
                Some((bt, be, ..)) => {
                    (!thin && *bt) || (thin == *bt && e3.total_chars > be.total_chars)
                }
            };
            if better {
                best = Some((
                    thin,
                    e3,
                    "ghost-dom(links)",
                    retry.as_ref().map(|r| r.status).unwrap_or(200),
                    url.to_string(),
                ));
            }
        }
    }

    if let Some((thin, e, t, s, u)) = best
        && !thin
    {
        // Learning is gated on WALL-DRIVEN escalation AND gated on
        // CONTENT — success is "we got content", not "we got HTTP
        // 200". The replay probe (or its absence) sets replay_ok.
        if learn {
            daemon.state.lock().await.record_solved(
                host,
                &page.cookies,
                page.vendor.as_deref(),
                replay_content_ok,
            );
        }
        // Don't cache challenge/wall DOMs — defense in depth alongside
        // the ghost_fetch timeout check. A challenge page that has
        // enough block structure to pass !thin would otherwise be
        // cached and re-served as ContentOk forever.
        let dom_verdict = crate::detect::walls::detect_dom_smart(page.html.as_bytes());
        if !matches!(dom_verdict, crate::detect::walls::Verdict::Challenge(_)) {
            daemon.state.lock().await.record_render(&u, &page.html);
        }
        return Ok((e, t, s, u));
    }

    // Last resort: raw text fallback. If the ghost DOM has real
    // visible text but DonSift's block extraction couldn't parse
    // it (complex DOM, non-standard structure), strip tags and
    // return the visible text. This makes "found DOM but failed
    // to extract content" IMPOSSIBLE when the DOM has real text.
    //
    // BUT: only return Ok when the fallback is non-thin (>= 800
    // chars of visible text). A captcha/challenge page with 300
    // chars of "Please verify you are a human" must NOT be
    // returned as ContentOk — the agent would trust it.
    if !page.captcha {
        let doc = scraper::Html::parse_document(&page.html);
        let meta = crate::extract::metadata::metadata(&doc);
        let max_chars = opts.max_chars.unwrap_or(16_000).max(200);
        if let Some(fb) = crate::extract::text_fallback(&page.html, &meta, url, opts, max_chars)
            && !fb.thin
        {
            return Ok((fb, "ghost-text", 200, url.to_string()));
        }
    }

    // Differentiate: small DOM with no content = not found / blocked.
    // Large DOM with no extractable content = genuine extraction failure.
    // A challenge page (captcha, bot wall) must ALWAYS return "blocked"
    // with kind="walled" (exit 3), regardless of DOM size — never "not
    // found" (exit 1). This fixes the Medium URL that gave different
    // verdicts across runs: sometimes the challenge page was < 5KB
    // (→ "not found"), sometimes larger (→ "blocked").
    let dom_verdict = crate::detect::walls::detect_dom_smart(page.html.as_bytes());
    if matches!(dom_verdict, Verdict::Challenge(_)) {
        return Err((
            format!(
                "blocked at {url} — interactive captcha or challenge could not be solved automatically. Use an Agent browser to browse sites like these"
            ),
            "walled",
        ));
    }
    if page.html.len() < 5_000 {
        return Err((
            format!(
                "not found: {url} — page returned no content (may not exist or requires JavaScript)"
            ),
            "permanent",
        ));
    }
    Err((
        format!(
            "blocked at {url} — tier 2 rendered a {}KB DOM but no real content was extractable. Use an Agent browser to browse sites like these",
            page.html.len() / 1024
        ),
        "walled",
    ))
}

/// PDF-shaped URL check for the actions guard (before the main
/// flow computes its own is_pdf_url). Covers both the .pdf
/// suffix convention and the /pdf/ path convention (arXiv:
/// arxiv.org/pdf/1706.03762 serves a PDF with no extension).
fn is_pdf_url_like(url: &str) -> bool {
    let path = url.split('?').next().unwrap_or(url).to_lowercase();
    if path.ends_with(".pdf") {
        return true;
    }
    // Path-segment "/pdf/" or trailing "/pdf" (arXiv, IACR,
    // many journal endpoints).
    let no_scheme = path
        .strip_prefix("https://")
        .or_else(|| path.strip_prefix("http://"))
        .unwrap_or(&path);
    let path_part = no_scheme.split_once('/').map(|(_, p)| p).unwrap_or("");
    let segs: Vec<&str> = path_part.split('/').filter(|s| !s.is_empty()).collect();
    segs.contains(&"pdf") || path_part.ends_with("/pdf")
}

/// v2: fetch with an action script — navigate, act (click /
/// type / press / scroll / wait), then run the NORMAL DonSift
/// extraction over the final DOM. focus/section/toc all work
/// on the interacted-with page. One call replaces hound's
/// navigate→act→act→read round-trips.
async fn fetch_with_actions(
    daemon: &Arc<Daemon>,
    url: &str,
    host: &str,
    opts: &ExtractOptions,
    actions: &[crate::ghost::actions::Action],
    shot: Option<&str>,
) -> Value {
    let mut trace = Trace::default();
    trace.step("route", "actions", "browser-script", 0);

    let t0 = std::time::Instant::now();
    let mut g = match daemon.ghost_mgr.acquire(&daemon.profile).await {
        Ok(g) => g,
        Err(e) => {
            return tool_error_structured(
                format!("browser launch failed: {e}"),
                "permanent",
                Some(json!({
                    "url": url,
                    "status": 0,
                    "next_action": "run `donsetch doctor` — the browser path is broken on this machine",
                    "escalation": trace.value(),
                })),
            );
        }
    };
    trace.step("2", "browser-launch", "ok", t0.elapsed().as_millis());

    // Initial render through the standard ghost oracle: navigate,
    // settle, challenge handling, content checks.
    let t1 = std::time::Instant::now();
    let page = match ops::ghost_fetch(&mut g, url, std::time::Duration::from_secs(25)).await {
        Ok(p) => p,
        Err(e) => {
            // One transient retry, same as ghost_escalate.
            match ops::ghost_fetch(&mut g, url, std::time::Duration::from_secs(25)).await {
                Ok(p) => p,
                Err(e2) => {
                    return tool_error_structured(
                        format!("browser automation error: {e} / {e2}"),
                        "permanent",
                        Some(json!({
                            "url": url,
                            "status": 0,
                            "escalation": trace.value(),
                        })),
                    );
                }
            }
        }
    };
    trace.step(
        "2",
        "ghost-render",
        &format!("captcha={} dom={}KB", page.captcha, page.html.len() / 1024),
        t1.elapsed().as_millis(),
    );
    if page.captcha {
        if let Some(p) = shot {
            let _ = g.screenshot(p).await;
        }
        return tool_error_structured(
            format!(
                "blocked at {url} — interactive captcha before actions could run. Use an Agent browser to browse sites like these"
            ),
            "walled",
            Some(json!({
                "url": url,
                "status": 200,
                "verdict": "Challenge",
                "next_action": next_action_for(Some(Verdict::Challenge(Vendor::Generic)), 200, "walled"),
                "escalation": trace.value(),
            })),
        );
    }

    // Run the script.
    let t2 = std::time::Instant::now();
    let outcomes = match crate::ghost::actions::run(&mut g, actions).await {
        Ok(o) => {
            trace.step(
                "2",
                "actions",
                &format!("{} steps ok", o.len()),
                t2.elapsed().as_millis(),
            );
            o
        }
        Err((step, reason, partial)) => {
            for o in &partial {
                trace.step("2", &format!("action[{}]", o.step), &o.outcome, o.ms);
            }
            if let Some(p) = shot {
                let _ = g.screenshot(p).await;
            }
            let steps_json: Vec<Value> = partial
                .iter()
                .map(|o| json!({"step": o.step, "action": o.action, "outcome": o.outcome, "ms": o.ms}))
                .collect();
            return tool_error_structured(
                format!(
                    "actions[{step}] failed: {reason} — steps before it succeeded (see structuredContent.actions); fix the step and re-run"
                ),
                "permanent",
                Some(json!({
                    "url": url,
                    "status": 200,
                    "actions": steps_json,
                    "escalation": trace.value(),
                    "next_action": "inspect the page with a plain fetch (no actions), correct the failing step's selector/text, re-run",
                })),
            );
        }
    };

    // Post-action DOM + optional screenshot for visual debugging.
    let html = match g.outer_html().await {
        Ok(h) => h,
        Err(e) => {
            return tool_error_structured(
                format!("post-action DOM read failed: {e}"),
                "transient",
                Some(json!({
                    "url": url,
                    "status": 200,
                    "escalation": trace.value(),
                })),
            );
        }
    };
    if let Some(p) = shot {
        let _ = g.screenshot(p).await;
    }

    // Cookie write-back — same discipline as ghost_escalate:
    // the browser's clearance cookies flow to tier 1 for future
    // plain-HTTP fetches of this domain. record_solved ONLY when
    // a challenge was actually cleared (page.vendor set) —
    // marking a never-walled domain needs_tier2 would poison its
    // route to skip-to-solve forever (the v1.1 reddit-poisoning
    // bug class). Replay is unverified in the actions flow (no
    // tier-1 retry happens) — false until the fetch path proves it.
    if let Ok(cookies) = g.cookies().await
        && !cookies.is_empty()
    {
        daemon.fetcher.import_cookies(&cookies).await;
        if page.vendor.is_some() {
            daemon
                .state
                .lock()
                .await
                .record_solved(host, &cookies, page.vendor.as_deref(), false);
        }
    }

    // Standard extraction over the final DOM, with the same
    // candidate ladder as ghost_escalate: prose → links-keeping
    // → raw text. A shell after actions is still a shell.
    let mut best: Option<extract::Extracted> = None;
    if let Ok(e) = extract::extract(html.as_bytes(), extract::charset::GHOST_TEXT_CT, url, opts)
        && !e.thin
    {
        best = Some(e);
    }
    if best.is_none() {
        let mut lopts = opts.clone();
        lopts.include_links = true;
        if let Ok(e2) = extract::extract(
            html.as_bytes(),
            extract::charset::GHOST_TEXT_CT,
            url,
            &lopts,
        ) && !e2.thin
        {
            best = Some(e2);
        }
    }
    let Some(ex) = best else {
        return tool_error_structured(
            format!(
                "actions succeeded but the resulting page yielded no extractable content ({}KB DOM) — the site may still be loading; add a wait step and re-run",
                html.len() / 1024
            ),
            "walled",
            Some(json!({
                "url": url,
                "status": 200,
                "escalation": trace.value(),
                "next_action": "add {\"do\":\"wait_text\",\"text\":\"<expected>\"} or {\"do\":\"wait\",\"ms\":2000} before extraction",
            })),
        );
    };

    // Cache the action-recovered DOM for future plain fetches.
    let dom_verdict = crate::detect::walls::detect_dom_smart(html.as_bytes());
    if !matches!(dom_verdict, crate::detect::walls::Verdict::Challenge(_)) {
        daemon.state.lock().await.record_render(url, &html);
    }

    let steps_json: Vec<Value> = outcomes
        .iter()
        .map(|o| json!({"step": o.step, "action": o.action, "outcome": o.outcome, "ms": o.ms}))
        .collect();
    let mut res = finish_result(&ex, "2-actions", 200, "ContentOk", url, &trace);
    res["structuredContent"]["actions"] = Value::Array(steps_json);
    res
}

/// Compact JSON string (no whitespace) for embedding in text
/// content blocks. Used for [meta] blocks that give clients
/// (Claude Code, VSCode) essential fields they'd otherwise
/// only get from structuredContent.
fn compact_json(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn finish_result(
    ex: &extract::Extracted,
    tier: &str,
    status: u16,
    verdict: &str,
    url: &str,
    trace: &Trace,
) -> Value {
    // PDF per-page stats: chars, ocr flag, per-page confidence.
    // Cap at 50 pages to avoid blowing up the response on large
    // PDFs (a 1000-page PDF produces 60K of per-page JSON alone).
    // The summary (total pages, ocr pages, mean confidence) is
    // always included; per_page detail is capped.
    let pdf = ex.pdf_pages.as_ref().map(|pages| {
        let ocr_pages = pages.iter().filter(|p| p.ocr).count();
        let mean_conf = if pages.is_empty() {
            0.0
        } else {
            pages.iter().map(|p| p.confidence).sum::<f32>() / pages.len() as f32
        };
        let capped: Vec<_> = pages.iter().take(50).collect();
        json!({
            "pages": pages.len(),
            "ocr_pages": ocr_pages,
            "mean_confidence": mean_conf,
            "per_page": capped,
            "per_page_capped": pages.len() > 50,
        })
    });
    let structured = json!({
        "status": status,
        "tier": tier,
        "verdict": verdict,
        "content_ok": !ex.thin && verdict == "ContentOk",
        "thin": ex.thin,
        "content_kind": format!("{:?}", ex.content_kind),
        "quality": ex.quality,
        "lang": ex.lang,
        "title": ex.title,
        "byline": ex.byline,
        "published": ex.published,
        "site": ex.site,
        "blocks_shown": ex.blocks_shown,
        "blocks_total": ex.blocks_total,
        "total_chars": ex.total_chars,
        "next_offset": ex.next_offset,
        "tokens_est": ex.tokens_est,
        "escalation": trace.value(),
        "pdf": pdf,
        "url": url,
    });
    // Compact metadata text block prepended for clients (Claude Code,
    // VSCode) that drop text content when structuredContent is present.
    let mut meta = json!({
        "url": url,
        "tier": tier,
        "verdict": verdict,
        "content_ok": !ex.thin && verdict == "ContentOk",
        "thin": ex.thin,
        "tokens_est": ex.tokens_est,
        "total_chars": ex.total_chars,
        "lang": ex.lang,
    });
    if let Some(n) = ex.next_offset {
        meta["next_offset"] = json!(n);
    }
    if let Some(t) = &ex.title {
        meta["title"] = json!(t);
    }
    if let Some(p) = &pdf {
        meta["pdf_pages"] = json!(p["pages"]);
        if p["ocr_pages"].as_u64().unwrap_or(0) > 0 {
            meta["pdf_ocr"] = json!(p["ocr_pages"]);
        }
    }
    json!({
        "content": [
            {"type": "text", "text": format!("[meta] {}", compact_json(&meta))},
            {"type": "text", "text": ex.markdown},
        ],
        "structuredContent": structured,
    })
}

async fn search_tool(daemon: &Arc<Daemon>, args: &Value) -> Value {
    let query = match args.get("query").and_then(Value::as_str) {
        Some(q) if !q.trim().is_empty() => q.to_string(),
        _ => return tool_error("search: query required"),
    };
    let max = args.get("max_results").and_then(Value::as_u64).unwrap_or(7) as usize;
    let intent = match args.get("intent").and_then(Value::as_str) {
        Some("web") => Some(Intent::Web),
        Some("code") => Some(Intent::Code),
        Some("paper") => Some(Intent::Paper),
        Some("news") => Some(Intent::News),
        Some("entity") => Some(Intent::Entity),
        _ => None,
    };

    // BYOK: if external search providers are configured,
    // try them first. The provider handles everything (IP,
    // rate limits, search). Falls back to local search if
    // all providers are exhausted (rate-limited, credits
    // depleted, invalid keys).
    //
    // If "local" is set as the default (donsetch keys default
    // local), the order is flipped: local search is tried
    // first, BYOK is the fallback. This lets users test the
    // local engine without removing their keys.
    //
    // Reload from disk first — picks up keys added/removed
    // via CLI while the daemon was running.
    daemon.byok.reload();
    let byok_configured = daemon.byok.is_configured();
    let local_first = daemon.byok.is_local_default();

    // BYOK-first mode: try providers, fall back to local.
    if byok_configured && !local_first {
        match daemon.byok.search(&query, max, intent).await {
            Ok(out) => {
                let md = search::render_markdown(&out, &query);
                let meta = search::render_meta(&out);
                return json!({
                    "content": [{ "type": "text", "text": md }],
                    "structuredContent": meta,
                });
            }
            Err(e) => {
                if std::env::var_os("DONSEEK_DEBUG").is_some() {
                    eprintln!("[byok] all providers exhausted, falling back to local: {e}");
                }
                // Fall through to local search.
            }
        }
    }

    // Local search (primary in local-first mode, fallback in BYOK-first).
    match daemon.searcher.search(&query, max, intent).await {
        Ok(out) => {
            let md = search::render_markdown(&out, &query);
            let meta = search::render_meta(&out);
            json!({
                "content": [{ "type": "text", "text": md }],
                "structuredContent": meta,
            })
        }
        Err(e) => {
            // Local failed — if BYOK is configured and we're in
            // local-first mode, try BYOK as a last resort.
            if byok_configured && local_first {
                if std::env::var_os("DONSEEK_DEBUG").is_some() {
                    eprintln!("[byok] local search failed, trying BYOK fallback: {e}");
                }
                match daemon.byok.search(&query, max, intent).await {
                    Ok(out) => {
                        let md = search::render_markdown(&out, &query);
                        let meta = search::render_meta(&out);
                        json!({
                            "content": [{ "type": "text", "text": md }],
                            "structuredContent": meta,
                        })
                    }
                    Err(e2) => search_error(&query, &format!("local ({e}); byok ({e2})"), true),
                }
            } else {
                search_error(&query, &e.to_string(), false)
            }
        }
    }
}

/// Search failure → structured error: every engine (and BYOK if
/// tried) failed. The agent needs to know retrying is safe and
/// what the levers are (BYOK keys, intent, simpler query).
fn search_error(query: &str, cause: &str, byok_tried: bool) -> Value {
    let mut trace = Trace::default();
    trace.step("search", "engines", "error", 0);
    if byok_tried {
        trace.step("byok", "providers", "error", 0);
    }
    let mut hint = String::from(
        "all engines failed — transient in most cases: retry once, then simplify the query",
    );
    if !byok_tried {
        hint.push_str(
            "; if repeated, add an API key provider (donsetch keys add) for a fallback path",
        );
    }
    tool_error_structured(
        format!("search: {cause}"),
        "transient",
        Some(json!({
            "query": query,
            "escalation": trace.value(),
            "next_action": hint,
        })),
    )
}

fn tool_error(message: impl Into<String>) -> Value {
    tool_error_kind(message, "permanent")
}

/// Like `tool_error` but with an explicit `errorKind` for CLI
/// exit-code mapping. `kind` is one of: "permanent", "transient",
/// "walled". MCP clients ignore the extra field; the CLI uses it
/// to choose exit 1 / 2 / 3.
fn tool_error_kind(message: impl Into<String>, kind: &str) -> Value {
    tool_error_structured(message, kind, None)
}

/// Error with structure: the 50-case report asked for honest
/// machine-readable failure state — status, verdict, url,
/// next_action, and the escalation trace — so an agent can
/// decide its fallback without parsing prose. Human message
/// stays in content[0].text exactly as before.
fn tool_error_structured(
    message: impl Into<String>,
    kind: &str,
    structured: Option<Value>,
) -> Value {
    let mut text = message.into();
    // Fold next_action from structured into the text for clients
    // (Claude Code, VSCode) that drop text when structuredContent
    // is present. next_action is critical for agent recovery.
    if let Some(ref s) = structured
        && let Some(action) = s.get("next_action").and_then(Value::as_str)
        && !action.is_empty()
    {
        text.push_str(&format!("\n\nNext action: {action}"));
    }
    let mut v = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true,
        "errorKind": kind
    });
    if let Some(s) = structured {
        v["structuredContent"] = s;
    }
    v
}

/// What should the agent DO next, given this failure? One line,
/// actionable, derived from verdict + kind. The report's core
/// ask: "make failures unambiguous."
fn next_action_for(verdict: Option<Verdict>, status: u16, kind: &str) -> String {
    match verdict {
        Some(Verdict::AuthWall) => {
            "requires login credentials — no keyless automated path; use an interactive browser with your session".into()
        }
        Some(Verdict::Paywall) => {
            "paid content — no automated path; look for an open preprint/copy via web_search".into()
        }
        Some(Verdict::SoftNotFound) => {
            "verify the URL (typo? deleted page?) — or web_search the page title to find the moved copy".into()
        }
        Some(Verdict::Challenge(_)) if kind == "walled" => {
            "tier 2 browser could not solve it — interactive verification needed; no automated path (by design DonSeTch does not solve captchas)".into()
        }
        Some(Verdict::Challenge(_)) => {
            "retry with tier=2 (or tier=auto) — the headless browser solves most JS/cookie challenges".into()
        }
        Some(Verdict::Blocked) => match status {
            429 => "rate limited — wait 30-60s and retry".into(),
            403 => "access denied — retry later or from a different network; this server refuses bots".into(),
            _ => "server rejected the request — retrying later sometimes works".into(),
        },
        _ if kind == "transient" => {
            "transient network failure — safe to retry immediately".into()
        }
        _ if kind == "walled" => {
            "no extractable content behind the wall — use an interactive agent browser for this site".into()
        }
        _ => "check the URL and retry; if repeated, the site may be down or blocking".into(),
    }
}

/// Escalation trace: the ordered record of what DonSeTch tried —
/// HTTP → browser → OCR-style fallbacks — with tier, action,
/// outcome and per-step latency. Surfaced as
/// structuredContent.escalation on successes AND errors, so the
/// agent sees exactly why a fetch took its path (and what a
/// 20s latency was spent on) without re-deriving it.
#[derive(Default)]
struct Trace {
    steps: Vec<Value>,
}

impl Trace {
    fn step(&mut self, tier: &str, action: &str, outcome: &str, ms: u128) {
        self.steps.push(json!({
            "tier": tier,
            "action": action,
            "outcome": outcome,
            "ms": ms,
        }));
    }

    fn value(&self) -> Value {
        Value::Array(self.steps.clone())
    }
}

/// Classify a wall verdict into an errorKind for CLI exit codes.
fn verdict_kind(v: Verdict, status: u16) -> &'static str {
    match v {
        Verdict::Challenge(_) | Verdict::AuthWall | Verdict::Paywall => "walled",
        Verdict::Blocked if status == 429 || status == 503 => "transient",
        _ => "permanent",
    }
}

/// Classify a network/fetch error into an errorKind.
fn fetch_error_kind(e: &FetchError) -> &'static str {
    match e {
        FetchError::Timeout | FetchError::Io(_) => "transient",
        _ => "permanent",
    }
}
