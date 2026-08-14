//! The stdio server: read loop, dispatch, writer task,
//! and the fetch tool handler with full escalation.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use futures_util::FutureExt;

use crate::crawl::real as crawl_real;
use crate::crawl::{CrawlMode, CrawlOptions, Crawler};
use crate::detect::walls::Verdict;
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
                            return Some(crate::crawl::GhostRender {
                                html: rc.html.clone(),
                            });
                        }
                    }
                    let mut g = match ghost_mgr.acquire(&profile).await {
                        Ok(g) => g,
                        Err(_) => return None,
                    };
                    let page =
                        match ops::ghost_fetch(&mut g, &url, std::time::Duration::from_secs(20))
                            .await
                        {
                            Ok(p) => p,
                            Err(_) => {
                                // Retry once on transient timeout.
                                match ops::ghost_fetch(
                                    &mut g,
                                    &url,
                                    std::time::Duration::from_secs(20),
                                )
                                .await
                                {
                                    Ok(p) => p,
                                    Err(_) => return None,
                                }
                            }
                        };
                    if page.captcha {
                        return None;
                    }
                    if !page.cookies.is_empty() {
                        fetcher.import_cookies(&page.cookies).await;
                    }
                    {
                        let mut s = state.lock().await;
                        s.record_render(&url, &page.html);
                    }
                    Some(crate::crawl::GhostRender { html: page.html })
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
            let _ = out.write_all(line.as_bytes()).await;
            let _ = out.write_all(b"\n").await;
            let _ = out.flush().await;
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
        Some(u) => return tool_error(format!("crawl: url must be http(s), got: {u}")),
        None => {
            if args.get("resume").and_then(Value::as_str).is_none() {
                return tool_error("crawl: url required (or provide resume token to continue)");
            }
            String::new()
        }
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

    let result = match daemon.crawler.crawl(&url, opts, resume.as_deref()).await {
        Ok(r) => r,
        Err(e) => return tool_error_kind(format!("crawl: {e}"), "transient"),
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
        "elapsed_s": result.elapsed.as_secs_f64(),
        "resume": result.resume,
    });
    json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": structured
    })
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
        return tool_error(format!(
            "blocked: {host} is a private/loopback address — SSRF guard"
        ));
    }
    let mut opts = ExtractOptions::default();
    opts.focus = args.get("focus").and_then(Value::as_str).map(String::from);
    opts.max_chars = args
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|n| n as usize);
    opts.offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
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

    let host = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();

    // === PDF early detection ===
    // Ghost can't render PDFs (Chrome's PDF viewer is a JS shell).
    // If the URL looks like a PDF, always fetch raw bytes (tier 1)
    // and route to the DonSheet engine. Never skip tier 1 for PDFs.
    let is_pdf_url = url
        .split('?')
        .next()
        .unwrap_or(&url)
        .to_lowercase()
        .ends_with(".pdf");

    // === Decision: how to route this fetch? ===
    // The self-improving loop: the domain profile decides
    // cold / warm / skip-to-solve / recheck-cold.
    let route = if tier == "2" && !is_pdf_url {
        RouteDecision::SkipToSolve
    } else if tier == "1" || is_pdf_url {
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

    // === Fetch (tier 1, unless skipped) ===
    let mut out: Option<crate::fetch::client::FetchOutcome> = None;

    if !skip_tier1 {
        out = Some(match daemon.fetcher.fetch(&url).await {
            Ok(o) => o,
            Err(e) => return tool_error_kind(friendly_fetch_error(&e), fetch_error_kind(&e)),
        });

        // === Observe the outcome ===
        // Every fetch teaches the domain profile something.
        let o = out.as_ref().unwrap();
        let walled = !matches!(o.verdict, Verdict::ContentOk);
        {
            let mut state = daemon.state.lock().await;
            if walled {
                if is_warm {
                    // Warm cookies went stale — learn the real lifetime.
                    state.record_warm_stale(&host);
                } else {
                    // Cold (or recheck) was walled — domain needs tier 2.
                    let vendor = match &o.verdict {
                        Verdict::Challenge(v) => Some(format!("{v:?}").to_lowercase()),
                        _ => None,
                    };
                    state.record_cold_walled(&host, vendor.as_deref());
                }
            } else if is_warm {
                // Warm succeeded — refresh the cookie vault (write-back).
                let snap = daemon.fetcher.jar_snapshot(&host);
                state.record_warm_ok(&host, &snap);
            } else {
                // Cold (or recheck) succeeded — if was needs_tier2, wall is gone.
                state.record_cold_ok(&host);
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
                return tool_error_kind(verdict_error(v, o.status, &o.url), kind);
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
            return tool_error(format!(
                "binary content: {url} returned {kind} ({} bytes) — not text, cannot extract",
                o.body.len()
            ));
        }
        match extract::extract(&o.body, &ct, &o.url, &opts) {
            Ok(e) => {
                final_url = o.url.clone();
                final_ex = Some(e);
            }
            Err(e) => return tool_error(format!("content extraction failed: {e}")),
        }
    }

    let ex_thin = final_ex.as_ref().map(|e| e.thin).unwrap_or(false);
    let challenge = out
        .as_ref()
        .map(|o| matches!(o.verdict, Verdict::Challenge(_)))
        .unwrap_or(false);

    // Warm cookies that only buy a shell are stale cookies:
    // kill the warm route and let a ghost success re-teach it.
    let shell_warm = is_warm && ex_thin;
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
            && let Ok(e2) = extract::extract(rc.html.as_bytes(), "text/html", &final_url, &opts)
            && !e2.thin
        {
            // Defense in depth: even if a challenge page slipped into
            // the cache (pre-fix), don't serve it as ContentOk.
            let cached_verdict = crate::detect::walls::detect_dom_smart(rc.html.as_bytes());
            if !matches!(cached_verdict, crate::detect::walls::Verdict::Challenge(_)) {
                let vstr = format!("{:?}", cached_verdict);
                let mut res = finish_result(&e2, "render-cache", final_status, &vstr, &final_url);
                res["_meta"] = json!({ "ttlMs": 300_000, "cacheScope": "session" });
                return res;
            }
        }

        match ghost_escalate(daemon, &url, &host, &opts, challenge || shell_warm, shot).await {
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
                return tool_error_kind(msg, kind);
            }
        }
    }

    let Some(ex) = final_ex else {
        return tool_error("all fetch tiers exhausted — no response received");
    };

    // Small 404 page: if we didn't escalate to ghost (is_small_404)
    // and the extraction is still thin/empty, return "not found".
    // This is honest — the page exists (HTTP 200) but has no content.
    // Could be a non-existent product, a deleted page, or a soft 404.
    if is_small_404 {
        return tool_error(format!(
            "not found: {url} — page returned no content (may not exist or requires JavaScript)"
        ));
    }

    finish_result(&ex, final_tier, final_status, &final_verdict, &final_url)
}

/// Unified tier-2: ghost render + cookie harvest + tier-1 retry,
/// then pick the candidate with the best content yield. Ok ONLY
/// when a candidate extracts as real content — a shell is a
/// failure, never a success. This is the loop the design always
/// promised: escalate, render, hand cookies back to tier 1.
async fn ghost_escalate(
    daemon: &Arc<Daemon>,
    url: &str,
    host: &str,
    opts: &ExtractOptions,
    learn: bool,
    shot: Option<&str>,
) -> Result<(extract::Extracted, &'static str, u16, String), (String, &'static str)> {
    let mut g = daemon
        .ghost_mgr
        .acquire(&daemon.profile)
        .await
        .map_err(|e| (format!("browser launch failed: {e}"), "permanent"))?;
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
    let retry = if !page.cookies.is_empty() {
        daemon.fetcher.fetch(url).await.ok()
    } else {
        None
    };

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
    if let Ok(e2) = extract::extract(page.html.as_bytes(), "text/html", url, opts) {
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
        if let Ok(e3) = extract::extract(page.html.as_bytes(), "text/html", url, &lopts) {
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
        // Learning is gated on CONTENT, matching the oracle —
        // success is "we got content", not "we got HTTP 200".
        if learn {
            daemon
                .state
                .lock()
                .await
                .record_solved(host, &page.cookies, page.vendor.as_deref());
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

fn finish_result(
    ex: &extract::Extracted,
    tier: &str,
    status: u16,
    verdict: &str,
    url: &str,
) -> Value {
    json!({
        "content": [{ "type": "text", "text": ex.markdown }],
        "structuredContent": {
            "status": status,
            "tier": tier,
            "verdict": verdict,
            "thin": ex.thin,
            "content_kind": format!("{:?}", ex.content_kind),
            "title": ex.title,
            "byline": ex.byline,
            "published": ex.published,
            "site": ex.site,
            "blocks_shown": ex.blocks_shown,
            "blocks_total": ex.blocks_total,
            "total_chars": ex.total_chars,
            "next_offset": ex.next_offset,
            "tokens_est": ex.tokens_est,
            "url": url,
        },
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
    // Reload from disk first — picks up keys added/removed
    // via CLI while the daemon was running.
    daemon.byok.reload();
    if daemon.byok.is_configured() {
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

    match daemon.searcher.search(&query, max, intent).await {
        Ok(out) => {
            let md = search::render_markdown(&out, &query);
            let meta = search::render_meta(&out);
            json!({
                "content": [{ "type": "text", "text": md }],
                "structuredContent": meta,
            })
        }
        Err(e) => tool_error_kind(format!("search: {e}"), "transient"),
    }
}

fn tool_error(message: impl Into<String>) -> Value {
    tool_error_kind(message, "permanent")
}

/// Like `tool_error` but with an explicit `errorKind` for CLI
/// exit-code mapping. `kind` is one of: "permanent", "transient",
/// "walled". MCP clients ignore the extra field; the CLI uses it
/// to choose exit 1 / 2 / 3.
fn tool_error_kind(message: impl Into<String>, kind: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true,
        "errorKind": kind
    })
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
