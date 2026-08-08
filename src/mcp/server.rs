//! The stdio server: read loop, dispatch, writer task,
//! and the fetch tool handler with full escalation.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::crawl::real as crawl_real;
use crate::crawl::{CrawlMode, CrawlOptions, Crawler};
use crate::detect::walls::Verdict;
use crate::extract::{self, ExtractOptions};
use crate::fetch::client::Fetcher;
use crate::ghost::cache::{CookieRecord, GhostState, RouteDecision};
use crate::ghost::manager::GhostManager;
use crate::ghost::ops;
use crate::profile::BrowserProfile;
use crate::search::egress::EgressPool;
use crate::search::intent::Intent;
use crate::search::{self, Searcher};

use super::tools;

/// Shared daemon state, built once, lives forever.
pub struct Daemon {
    fetcher: Arc<Fetcher>,
    profile: BrowserProfile,
    ghost_mgr: Arc<GhostManager>,
    state: Mutex<GhostState>,
    searcher: Arc<Searcher>,
    crawler: Crawler,
}

impl Daemon {
    pub fn new() -> Result<Self, crate::error::FetchError> {
        let profile = BrowserProfile::host_default();
        let fetcher = Arc::new(Fetcher::new(profile.clone())?);
        let searcher = Arc::new(Searcher::new(
            Fetcher::new(profile.clone())?,
            EgressPool::from_env(),
        ));
        searcher.preflight();
        let proxies = crate::transport::proxy::load_all();
        let (crawler, _gov) = crawl_real::build(Arc::clone(&fetcher), proxies);
        Ok(Self {
            fetcher,
            profile,
            ghost_mgr: GhostManager::new(),
            state: Mutex::new(GhostState::load()),
            searcher,
            crawler,
        })
    }
}

/// Run the daemon until stdin closes. Never returns Err
/// on client garbage — only on fatal IO.
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let daemon = Arc::new(Daemon::new()?);
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

async fn call_tool(daemon: &Arc<Daemon>, params: &Value) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "fetch" => Ok(fetch_tool(daemon, &args).await),
        "search" => Ok(search_tool(daemon, &args).await),
        "crawl" => Ok(crawl_tool(daemon, &args).await),
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
        Err(e) => return tool_error(format!("crawl: {e}")),
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
fn friendly_fetch_error(e: &crate::error::FetchError) -> String {
    use crate::error::FetchError;
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
            format!("login required: {url} needs authentication — the page is behind a login wall")
        }
        Verdict::Paywall => format!("paywall: {url} requires payment to view content"),
        Verdict::SoftNotFound => format!("not found: {url} returned HTTP 404"),
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

/// Detect login-gated SPA shells: thin content + login markers.
fn is_login_shell(html: &str) -> bool {
    let lower = html.to_lowercase();
    let has_login = lower.contains("sign in")
        || lower.contains("log in")
        || lower.contains("login") && (lower.contains("password") || lower.contains("email"));
    let has_form = lower.contains("<form")
        && (lower.contains("password")
            || lower.contains("action=\"/login")
            || lower.contains("action=\"/auth")
            || lower.contains("action=\"/account"));
    has_login && has_form
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

    // === Decision: how to route this fetch? ===
    // The self-improving loop: the domain profile decides
    // cold / warm / skip-to-solve / recheck-cold.
    let route = if tier == "2" {
        RouteDecision::SkipToSolve
    } else if tier == "1" {
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
    let mut rendered_html: Option<Vec<u8>> = None;

    if !skip_tier1 {
        out = Some(match daemon.fetcher.fetch(&url).await {
            Ok(o) => o,
            Err(e) => return tool_error(friendly_fetch_error(&e)),
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

    // === SOLVE on wall ===
    // Only genuine bot walls (Challenge, Blocked) trigger
    // ghost solve. 404, AuthWall, Paywall are legitimate
    // responses — return them as errors immediately.
    let walled = out
        .as_ref()
        .map(|o| matches!(o.verdict, Verdict::Challenge(_) | Verdict::Blocked))
        .unwrap_or(true);
    if walled && tier != "1" {
        match daemon.ghost_mgr.acquire(&daemon.profile).await {
            Ok(mut g) => match ops::solve(&mut g, &url, std::time::Duration::from_secs(30)).await {
                Ok(ops::SolveOutcome::Solved(r)) => {
                    daemon.fetcher.import_cookies(&r.cookies).await;
                    daemon
                        .state
                        .lock()
                        .await
                        .record_solved(&host, &r.cookies, r.vendor.as_deref());
                    match daemon.fetcher.fetch(&url).await {
                        Ok(retry) if matches!(retry.verdict, Verdict::ContentOk) => {
                            out = Some(retry);
                            tier_used = "1+ghost-solve";
                        }
                        Ok(retry) => {
                            rendered_html = Some(r.html.into_bytes());
                            out = Some(retry);
                            tier_used = "ghost-dom";
                        }
                        Err(e) => {
                            return tool_error(friendly_fetch_error(&e));
                        }
                    }
                }
                Ok(ops::SolveOutcome::CaptchaWalled) => {
                    if let Some(p) = shot {
                        let _ = g.screenshot(p).await;
                    }
                    let v = out.as_ref().map(|o| o.verdict).unwrap_or(Verdict::Blocked);
                    return tool_error(format!(
                        "interactive captcha required at {url} — no automated solving service by design (verdict: {v:?})"
                    ));
                }
                Ok(ops::SolveOutcome::TimedOut) => {
                    let v = out.as_ref().map(|o| o.verdict).unwrap_or(Verdict::Blocked);
                    return tool_error(format!(
                        "bot wall did not clear in 30s at {url} — the challenge timed out (verdict: {v:?})"
                    ));
                }
                Err(e) => {
                    return tool_error(format!("browser automation error: {e}"));
                }
            },
            Err(e) => {
                return tool_error(format!("browser launch failed: {e}"));
            }
        }
    }

    // Body source: tier-1 bytes or ghost DOM.
    let (body, ct, final_url) = if let Some(h) = &rendered_html {
        let u = out
            .as_ref()
            .map(|o| o.url.clone())
            .unwrap_or_else(|| url.clone());
        (h.clone(), "text/html".to_string(), u)
    } else if let Some(o) = &out {
        if matches!(o.verdict, Verdict::ContentOk) {
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
            (o.body.clone(), ct, o.url.clone())
        } else {
            return tool_error(verdict_error(o.verdict, o.status, &o.url));
        }
    } else {
        return tool_error("all fetch tiers exhausted — no response received");
    };

    let mut ex = match extract::extract(&body, &ct, &final_url, &opts) {
        Ok(e) => e,
        Err(e) => return tool_error(format!("content extraction failed: {e}")),
    };

    // RENDER on JS shell: cache first, then ghost.
    // Only if the HTTP response was actually thin — not if
    // tier 2 already ran (ghost-dom has real content).
    let mut cache_hit = false;
    if ex.thin && tier == "auto" && !matches!(tier_used, "ghost-dom" | "1+ghost-solve") {
        let cached = daemon.state.lock().await.render_for(&final_url).cloned();
        if let Some(rc) = cached {
            cache_hit = true;
            if let Ok(e2) = extract::extract(rc.html.as_bytes(), "text/html", &final_url, &opts) {
                ex = e2;
                tier_used = "render-cache";
            }
        } else if let Ok(mut g) = daemon.ghost_mgr.acquire(&daemon.profile).await
            && let Ok(html) =
                ops::render(&mut g, &final_url, std::time::Duration::from_secs(30)).await
        {
            daemon.state.lock().await.record_render(&final_url, &html);
            if let Ok(e2) = extract::extract(html.as_bytes(), "text/html", &final_url, &opts) {
                // Login shell detection: if the rendered page is
                // still thin AND has login forms, it's a login wall.
                if e2.thin && is_login_shell(&html) {
                    return tool_error(format!(
                        "login required: {final_url} requires authentication — the page is behind a login wall"
                    ));
                }
                ex = e2;
                tier_used = "ghost-render";
            }
        }
    }

    let o = out.as_ref();
    let meta = json!({
        "status": o.map(|o| o.status).unwrap_or(0),
        "tier": tier_used,
        "verdict": format!("{:?}", o.map(|o| o.verdict).unwrap_or(Verdict::Blocked)),
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
        "url": final_url,
    });
    let mut result = json!({
        "content": [{ "type": "text", "text": ex.markdown }],
        "structuredContent": meta,
    });
    if cache_hit {
        result["_meta"] = json!({ "ttlMs": 300_000, "cacheScope": "session" });
    }
    result
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
    match daemon.searcher.search(&query, max, intent).await {
        Ok(out) => {
            let md = search::render_markdown(&out, &query);
            let meta = search::render_meta(&out);
            json!({
                "content": [{ "type": "text", "text": md }],
                "structuredContent": meta,
            })
        }
        Err(e) => tool_error(format!("search: {e}")),
    }
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{ "type": "text", "text": message.into() }],
        "isError": true
    })
}
