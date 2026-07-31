//! DonSeek — keyless multi-engine search.
//!
//! Intent → fan-out (engines across egresses + verticals
//! direct) → weighted RRF merge → ranked results with
//! honest engine reporting.

pub mod egress;
pub mod engines;
pub mod intent;
pub mod rank;
pub mod verticals;

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::detect::walls::Verdict;
use crate::error::FetchError;
use crate::fetch::client::Fetcher;

use egress::EgressPool;
use intent::Intent;
use rank::Merged;

const ENGINE_TIMEOUT: Duration = Duration::from_secs(8);
const CACHE_TTL: Duration = Duration::from_secs(600);

pub struct Searcher {
    fetcher: Fetcher,
    pool: EgressPool,
    /// engine -> trust EWMA (1.0 seed; 0.2..2.0 clamp).
    trust: Mutex<HashMap<String, f64>>,
    /// exact-query cache: zero egress cost on repeats.
    cache: Mutex<HashMap<String, (Instant, Vec<Merged>)>>,
}

/// Per-engine outcome for honest reporting.
#[derive(Debug, Clone)]
pub struct EngineReport {
    pub engine: String,
    pub status: String,
    pub hits: usize,
    pub ms: u64,
}

pub struct SearchOutcome {
    pub results: Vec<Merged>,
    pub weak: bool,
    pub intent: Intent,
    pub report: Vec<EngineReport>,
    pub cached: bool,
    pub elapsed: Duration,
}

impl Searcher {
    pub fn new(fetcher: Fetcher, pool: EgressPool) -> Self {
        Self {
            fetcher,
            pool,
            trust: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        forced_intent: Option<Intent>,
    ) -> Result<SearchOutcome, FetchError> {
        let started = Instant::now();
        let intent = forced_intent.unwrap_or_else(|| intent::detect(query));
        let cache_key = format!("{query}|{max_results}|{intent:?}");

        if let Some((at, cached)) = self.cache.lock().unwrap().get(&cache_key) {
            if at.elapsed() < CACHE_TTL {
                return Ok(SearchOutcome {
                    results: cached.clone(),
                    weak: rank::is_weak(cached),
                    intent,
                    report: Vec::new(),
                    cached: true,
                    elapsed: started.elapsed(),
                });
            }
        }

        let engines = intent::engines_for(intent);
        let verticals = intent::verticals_for(intent);

        // Fan out: engines each get their own egress
        // (spreading is the anti-rate-limit move).
        let mut futures: Vec<TaskFut> = Vec::new();
        let mut used_egresses: Vec<String> = Vec::new();
        let mut queries: Vec<String> = vec![query.to_string()];
        if let Some(v) = intent::variant(query) {
            queries.push(v);
        }
        // Engines get the original query; the recall variant
        // goes only to the first two engines (top trust).
        let mut assignments: Vec<(String, String)> =
            engines.iter().map(|e| (e.to_string(), query.to_string())).collect();
        if queries.len() > 1 {
            for e in engines.iter().take(2) {
                assignments.push((e.to_string(), queries[1].clone()));
            }
        }

        for (engine, q) in assignments {
            let Some(eg) = self.pool.pick(&engine, &used_egresses) else {
                break;
            };
            used_egresses.push(eg.id.clone());
            self.pool.pace(&engine, &eg.id).await;
            futures.push(Box::pin(engine_task(engine, q, eg.id, eg.proxy, &self.fetcher)));
        }
        // Verticals: direct, friendly APIs.
        for v in verticals {
            futures.push(Box::pin(vertical_task(v.to_string(), query.to_string(), &self.fetcher)));
        }

        let outcomes = futures_util::future::join_all(futures).await;

        let mut per_engine: Vec<(String, Vec<engines::Hit>)> = Vec::new();
        let mut report = Vec::new();
        for (engine, outcome) in outcomes {
            match outcome {
                Ok((hits, ms, egress_id, was_engine)) => {
                    if was_engine {
                        self.pool.report_ok(&engine, &egress_id);
                        self.bump_trust(&engine, true);
                    }
                    report.push(EngineReport {
                        engine: engine.clone(),
                        status: "ok".into(),
                        hits: hits.len(),
                        ms,
                    });
                    per_engine.push((engine, hits));
                }
                Err((status, egress_id, was_engine)) => {
                    if was_engine {
                        if status.starts_with("dead") {
                            self.pool.report_dead(&egress_id);
                        } else {
                            self.pool.report_blocked(&engine, &egress_id);
                        }
                        self.bump_trust(&engine, false);
                    }
                    report.push(EngineReport {
                        engine,
                        status,
                        hits: 0,
                        ms: 0,
                    });
                }
            }
        }

        if per_engine.is_empty() {
            return Err(FetchError::Http(format!(
                "search: all engines failed — {}",
                report
                    .iter()
                    .map(|r| format!("{}:{}", r.engine, r.status))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }

        let trust = self.trust.lock().unwrap().clone();
        let results = rank::merge(&per_engine, query, intent, &trust, max_results);
        let weak = rank::is_weak(&results);
        self.cache
            .lock()
            .unwrap()
            .insert(cache_key, (Instant::now(), results.clone()));

        Ok(SearchOutcome {
            results,
            weak,
            intent,
            report,
            cached: false,
            elapsed: started.elapsed(),
        })
    }

    fn bump_trust(&self, engine: &str, ok: bool) {
        let mut trust = self.trust.lock().unwrap();
        // Base engine name (variant queries share it).
        let base = engine.split('@').next().unwrap_or(engine).to_string();
        let t = trust.entry(base).or_insert(1.0);
        let target = if ok { 1.2 } else { 0.3 };
        *t = (*t * 0.7 + target * 0.3).clamp(0.2, 2.0);
    }
}

type EngineResult = Result<
    (Vec<engines::Hit>, u64, String, bool),
    (String, String, bool),
>;

type TaskFut<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = (String, EngineResult)> + Send + 'a>,
>;

async fn engine_task(
    engine: String,
    query: String,
    egress_id: String,
    proxy: Option<crate::transport::proxy::Proxy>,
    fetcher: &Fetcher,
) -> (String, EngineResult) {
    let label = engine.clone();
    let started = Instant::now();
    let Some(url) = engines::serp_url(&engine, &query) else {
        return (label, Err(("no-url".into(), egress_id, true)));
    };
    let out =
        match tokio::time::timeout(ENGINE_TIMEOUT, fetcher.fetch_once_via(&url, &[], proxy.as_ref()))
            .await
        {
            Err(_) => return (label, Err(("timeout".into(), egress_id, true))),
            Ok(Err(e)) => {
                let status = match &e {
                    FetchError::Timeout => "timeout",
                    FetchError::Http(m) if m.contains("CONNECT") => "dead-proxy",
                    _ => "net",
                };
                return (label, Err((status.into(), egress_id, true)));
            }
            Ok(Ok(o)) => o,
        };
    let ms = started.elapsed().as_millis() as u64;
    if out.status == 429 || !matches!(out.verdict, Verdict::ContentOk) {
        return (label, Err((format!("blocked:{}", out.status), egress_id, true)));
    }
    let html = String::from_utf8_lossy(&out.body).to_string();
    let hits = engines::parse(&engine, &html);
    if hits.len() < 3 {
        return (label, Err(("empty-parse".into(), egress_id, true)));
    }
    (label, Ok((hits, ms, egress_id, true)))
}

async fn vertical_task(
    vertical: String,
    query: String,
    fetcher: &Fetcher,
) -> (String, EngineResult) {
    let started = Instant::now();
    match tokio::time::timeout(
        ENGINE_TIMEOUT,
        verticals::run(&fetcher, &vertical, &query),
    )
    .await
    {
        Err(_) => (vertical, Err(("timeout".into(), "direct".into(), false))),
        Ok(Err(e)) => (vertical, Err((format!("{e}"), "direct".into(), false))),
        Ok(Ok(hits)) => {
            let ms = started.elapsed().as_millis() as u64;
            (vertical, Ok((hits, ms, "direct".into(), false)))
        }
    }
}

/// Markdown rendering for the MCP/CLI surface.
pub fn render_markdown(out: &SearchOutcome, query: &str) -> String {
    let mut md = format!("# Search: {query}\n\n");
    for (i, r) in out.results.iter().enumerate() {
        let host = rank::host_of(&r.url);
        md.push_str(&format!("{}. **{}** — {}\n", i + 1, r.title, host));
        if !r.snippet.is_empty() {
            let snip: String = r.snippet.chars().take(220).collect();
            md.push_str(&format!("   {snip}\n"));
        }
        md.push_str(&format!("   {}\n", r.url));
    }
    if out.weak {
        md.push_str("\n*weak results: low cross-engine consensus — treat with care*\n");
    }
    md
}

/// structuredContent metadata.
pub fn render_meta(out: &SearchOutcome) -> Value {
    json!({
        "intent": format!("{:?}", out.intent),
        "weak": out.weak,
        "cached": out.cached,
        "elapsed_ms": out.elapsed.as_millis() as u64,
        "results": out.results.iter().map(|r| json!({
            "title": r.title,
            "url": r.url,
            "snippet": r.snippet.chars().take(300).collect::<String>(),
            "score": (r.score * 1000.0).round() / 1000.0,
            "consensus": r.sources.len(),
            "engines": r.sources.iter().map(|(e, _)| e.clone()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "engines": out.report.iter().map(|r| json!({
            "engine": r.engine, "status": r.status, "hits": r.hits, "ms": r.ms,
        })).collect::<Vec<_>>(),
    })
}
