//! The stdio server: read loop, dispatch, writer task,
//! and the fetch tool handler with full escalation.

use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, mpsc};

use crate::detect::walls::Verdict;
use crate::extract::{self, ExtractOptions};
use crate::fetch::client::Fetcher;
use crate::ghost::cache::GhostState;
use crate::ghost::manager::GhostManager;
use crate::ghost::ops;
use crate::profile::BrowserProfile;
use crate::search::{self, Searcher};
use crate::search::egress::EgressPool;
use crate::search::intent::Intent;

use super::tools;

/// Shared daemon state, built once, lives forever.
pub struct Daemon {
    fetcher: Fetcher,
    profile: BrowserProfile,
    ghost_mgr: Arc<GhostManager>,
    state: Mutex<GhostState>,
    searcher: Searcher,
}

impl Daemon {
    pub fn new() -> Result<Self, crate::error::FetchError> {
        let profile = BrowserProfile::host_default();
        let fetcher = Fetcher::new(profile.clone())?;
        let searcher = Searcher::new(
            Fetcher::new(profile.clone())?,
            EgressPool::from_env(),
        );
        Ok(Self {
            fetcher,
            profile,
            ghost_mgr: GhostManager::new(),
            state: Mutex::new(GhostState::load()),
            searcher,
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
    if id.is_none() {
        return None;
    }
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

async fn call_tool(
    daemon: &Arc<Daemon>,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "fetch" => Ok(fetch_tool(daemon, &args).await),
        "search" => Ok(search_tool(daemon, &args).await),
        _ => Err((-32602, format!("unknown tool: {name}"))),
    }
}

/// The fetch tool: tier 1 → verdict → ghost solve/render
/// → DonSift. Ports the CLI escalation into the daemon,
/// with warm-start and render cache.
async fn fetch_tool(daemon: &Arc<Daemon>, args: &Value) -> Value {
    let url = match args.get("url").and_then(Value::as_str) {
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => {
            u.to_string()
        }
        _ => return tool_error("fetch: url must be http(s)"),
    };
    let mut opts = ExtractOptions::default();
    opts.focus = args.get("focus").and_then(Value::as_str).map(String::from);
    opts.max_chars =
        args.get("max_chars").and_then(Value::as_u64).map(|n| n as usize);
    opts.offset =
        args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    opts.section =
        args.get("section").and_then(Value::as_str).map(String::from);
    opts.selector =
        args.get("selector").and_then(Value::as_str).map(String::from);
    opts.toc = args.get("toc").and_then(Value::as_bool).unwrap_or(false);
    opts.include_links =
        args.get("links").and_then(Value::as_bool).unwrap_or(false);
    opts.include_media =
        args.get("media").and_then(Value::as_bool).unwrap_or(false);
    let tier = args.get("tier").and_then(Value::as_str).unwrap_or("auto");
    let shot = args.get("shot").and_then(Value::as_str);

    let host = url::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default();

    // Warm start: previous solve feeds tier 1's jar.
    let mut tier_used = "1";
    if tier != "2" {
        let state = daemon.state.lock().await;
        if let Some(rec) = state.solved_for(&host) {
            let cookies = rec.cookies.clone();
            drop(state);
            daemon.fetcher.import_cookies(&cookies).await;
            tier_used = "1(warm)";
        }
    }

    let mut out = match daemon.fetcher.fetch(&url).await {
        Ok(o) => o,
        Err(e) => return tool_error(format!("fetch: {e}")),
    };
    let mut rendered_html: Option<Vec<u8>> = None;

    // SOLVE on wall.
    let walled = !matches!(out.verdict, Verdict::ContentOk);
    if walled && tier != "1" {
        match daemon.ghost_mgr.acquire(&daemon.profile).await {
            Ok(mut g) => {
                match ops::solve(
                    &mut g,
                    &url,
                    std::time::Duration::from_secs(30),
                )
                .await
                {
                    Ok(ops::SolveOutcome::Solved(r)) => {
                        daemon.fetcher.import_cookies(&r.cookies).await;
                        daemon
                            .state
                            .lock()
                            .await
                            .record_solved(&host, &r.cookies);
                        match daemon.fetcher.fetch(&url).await {
                            Ok(retry)
                                if matches!(
                                    retry.verdict,
                                    Verdict::ContentOk
                                ) =>
                            {
                                out = retry;
                                tier_used = "1+ghost-solve";
                            }
                            Ok(retry) => {
                                rendered_html =
                                    Some(r.html.into_bytes());
                                out = retry;
                                tier_used = "ghost-dom";
                            }
                            Err(e) => {
                                return tool_error(format!(
                                    "fetch after solve: {e}"
                                ));
                            }
                        }
                    }
                    Ok(ops::SolveOutcome::CaptchaWalled) => {
                        if let Some(p) = shot {
                            let _ = g.screenshot(p).await;
                        }
                        return tool_error(format!(
                            "blocked: interactive captcha at {url} — no solving service by design (verdict: {:?})",
                            out.verdict
                        ));
                    }
                    Ok(ops::SolveOutcome::TimedOut) => {
                        return tool_error(format!(
                            "blocked: challenge did not clear in 30s at {url} (verdict: {:?})",
                            out.verdict
                        ));
                    }
                    Err(e) => {
                        return tool_error(format!("ghost solve: {e}"));
                    }
                }
            }
            Err(e) => {
                return tool_error(format!("ghost launch: {e}"));
            }
        }
    }

    // Body source: tier-1 bytes or ghost DOM.
    let (body, ct, final_url) = if let Some(h) = &rendered_html {
        (h.clone(), "text/html".to_string(), out.url.clone())
    } else if matches!(out.verdict, Verdict::ContentOk) {
        let ct = out
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        (out.body.clone(), ct, out.url.clone())
    } else {
        return tool_error(format!(
            "blocked: {:?} at {} (status {})",
            out.verdict, out.url, out.status
        ));
    };

    let mut ex = match extract::extract(&body, &ct, &final_url, &opts) {
        Ok(e) => e,
        Err(e) => return tool_error(format!("extract: {e}")),
    };

    // RENDER on JS shell: cache first, then ghost.
    let mut cache_hit = false;
    if ex.thin && tier == "auto" {
        let cached = daemon.state.lock().await.render_for(&final_url).cloned();
        if let Some(rc) = cached {
            cache_hit = true;
            if let Ok(e2) = extract::extract(
                rc.html.as_bytes(),
                "text/html",
                &final_url,
                &opts,
            ) {
                ex = e2;
                tier_used = "render-cache";
            }
        } else if let Ok(mut g) =
            daemon.ghost_mgr.acquire(&daemon.profile).await
        {
            if let Ok(html) = ops::render(
                &mut g,
                &final_url,
                std::time::Duration::from_secs(30),
            )
            .await
            {
                daemon.state.lock().await.record_render(&final_url, &html);
                if let Ok(e2) = extract::extract(
                    html.as_bytes(),
                    "text/html",
                    &final_url,
                    &opts,
                ) {
                    ex = e2;
                    tier_used = "ghost-render";
                }
            }
        }
    }

    let meta = json!({
        "status": out.status,
        "tier": tier_used,
        "verdict": format!("{:?}", out.verdict),
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
        result["_meta"] =
            json!({ "ttlMs": 300_000, "cacheScope": "session" });
    }
    result
}

async fn search_tool(daemon: &Arc<Daemon>, args: &Value) -> Value {
    let query = match args.get("query").and_then(Value::as_str) {
        Some(q) if !q.trim().is_empty() => q.to_string(),
        _ => return tool_error("search: query required"),
    };
    let max = args
        .get("max_results")
        .and_then(Value::as_u64)
        .unwrap_or(10) as usize;
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
