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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::detect::walls::Verdict;
use crate::error::FetchError;
use crate::fetch::client::Fetcher;

use egress::EgressPool;
use intent::Intent;
use rank::Merged;

const ENGINE_TIMEOUT: Duration = Duration::from_secs(8);

/// Intent + recency-aware cache TTL. Every cached query
/// is a query that never touches an egress — the #1 rate
/// reducer. But a cached answer presented as fresh is
/// WORSE than honest latency when the world moved:
/// time-sensitive queries (even outside news intent —
/// "X release date", "inflation 2026") get news-grade
/// TTLs regardless of detected intent.
fn cache_ttl(intent: Intent, query: &str) -> Duration {
    const RECENCY: &[&str] = &[
        "latest", "today", "breaking", "recent", "this week", "this month",
        "price", "stock", "weather", "deadline", "release date", "news",
        "2024", "2025", "2026", "2027",
    ];
    let q = query.to_lowercase();
    if RECENCY.iter().any(|s| q.contains(s)) {
        return Duration::from_secs(300);
    }
    match intent {
        Intent::News => Duration::from_secs(300),
        Intent::Code => Duration::from_secs(900),
        _ => Duration::from_secs(1800),
    }
}

/// Normalize a query for cache keys: casing, punctuation
/// and stopwords don't change intent, so they don't get
/// to spend egress budget twice.
fn norm_query(q: &str) -> String {
    const STOP: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "of", "in", "on", "at", "to",
        "for", "and", "or", "what", "which", "how", "do", "does", "i", "you", "it",
    ];
    q.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty() && !STOP.contains(w))
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct Searcher {
    fetcher: Fetcher,
    pool: EgressPool,
    /// engine -> trust EWMA (1.0 seed; 0.2..2.0 clamp).
    trust: Mutex<HashMap<String, f64>>,
    /// normalized-query cache: zero egress cost on repeats.
    /// Stores up to 12 results; reads truncate to the
    /// requested max so max_results variants share entries.
    cache: Mutex<HashMap<String, (Instant, Vec<Merged>, usize)>>,
    /// Chronic-failure quarantine: engine -> (consecutive
    /// failures, last failure). 3 strikes across any
    /// egresses = benched for QUARANTINE_TTL so a walled
    /// engine stops wasting a fan-out slot every query.
    failures: Mutex<HashMap<String, (u32, Instant)>>,
    /// Single-flight: two identical in-flight queries spend
    /// egress budget ONCE — the follower awaits the
    /// leader's result. Stampedes are an agent reality
    /// (parallel tool calls love the same query).
    inflight: Mutex<std::collections::HashSet<String>>,
}

/// Per-engine outcome for honest reporting.
#[derive(Debug, Clone)]
pub struct EngineReport {
    pub engine: String,
    pub status: String,
    pub hits: usize,
    pub ms: u64,
    /// Which lane carried it (observability for the
    /// governor's routing decisions).
    pub egress: String,
}

#[derive(Debug, Clone)]
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
            cache: Mutex::new(load_cache_disk()),
            failures: Mutex::new(HashMap::new()),
            inflight: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Proxy preflight: probe every proxy at startup so
    /// dead lines are benched BEFORE a query ever gets
    /// assigned to them. Runs in the background; the first
    /// queries just use healthy lanes.
    pub fn preflight(self: &Arc<Self>) {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let proxies = this.pool.proxies();
            let total = proxies.len();
            let mut dead = 0usize;
            for proxy in proxies {
                let id = proxy.id();
                let probe = this.fetcher.fetch_once_via(
                    "https://api.ipify.org/",
                    &[],
                    Some(&proxy),
                    false,
                );
                match tokio::time::timeout(Duration::from_secs(6), probe).await {
                    Ok(Ok(o)) if o.status == 200 => {}
                    Ok(Err(e)) if format!("{e}").contains("CONNECT -> 407") => {
                        this.pool.report_auth_fail(&id);
                    }
                    _ => {
                        dead += 1;
                        this.pool.report_dead(&id);
                    }
                }
            }
            // ALL proxies failing means the PROBE endpoint
            // died, not the pool — clear the marks rather
            // than bench every lane over our own bug.
            if total > 0 && dead == total {
                this.pool.revive_all();
            }
        });
    }

    /// True when an engine is benched for chronic failure.
    fn quarantined(&self, engine: &str) -> bool {
        const QUARANTINE_TTL: Duration = Duration::from_secs(600);
        let f = self.failures.lock().unwrap();
        matches!(f.get(engine), Some(&(n, at)) if n >= 3 && at.elapsed() < QUARANTINE_TTL)
    }

    fn record_outcome(&self, engine: &str, ok: bool) {
        let mut f = self.failures.lock().unwrap();
        if ok {
            f.remove(engine);
        } else {
            let e = f.entry(engine.to_string()).or_insert((0, Instant::now()));
            e.0 += 1;
            e.1 = Instant::now();
        }
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        forced_intent: Option<Intent>,
    ) -> Result<SearchOutcome, FetchError> {
        let started = Instant::now();
        let intent_probe = forced_intent.unwrap_or_else(|| intent::detect(query));
        let sf_key = format!("{}|{intent_probe:?}|{max_results}", norm_query(query));
        let leader = {
            let mut m = self.inflight.lock().unwrap();
            m.insert(sf_key.clone())
        };
        if !leader {
            // Follower: poll for the leader's cache write.
            // The leader publishes into the query cache on
            // completion, so followers read it from there.
            for _ in 0..120 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let hit = self
                    .cache
                    .lock()
                    .unwrap()
                    .get(&format!("{}|{intent_probe:?}", norm_query(query)))
                    .cloned();
                if let Some((at, cached, total)) = hit {
                    if at.elapsed() < cache_ttl(intent_probe, query) {
                        let weak = rank::is_weak(&cached, total);
                        return Ok(SearchOutcome {
                            results: cached.iter().take(max_results).cloned().collect(),
                            weak,
                            intent: intent_probe,
                            report: Vec::new(),
                            cached: true,
                            elapsed: started.elapsed(),
                        });
                    }
                }
            }
            // Leader died or timed out — compute ourselves.
        }
        let _inflight_guard = InflightGuard {
            map: &self.inflight,
            key: sf_key,
        };
        self.search_inner(query, max_results, forced_intent, started).await
    }

    async fn search_inner(
        &self,
        query: &str,
        max_results: usize,
        forced_intent: Option<Intent>,
        started: Instant,
    ) -> Result<SearchOutcome, FetchError> {
        // Cache stores top-12; asking for more just
        // re-lists the same tail.
        let max_results = max_results.clamp(1, 12);
        let intent = forced_intent.unwrap_or_else(|| intent::detect(query));
        let cache_key = format!("{}|{intent:?}", norm_query(query));

        if let Some((at, cached, total)) = self.cache.lock().unwrap().get(&cache_key) {
            if at.elapsed() < cache_ttl(intent, query) {
                let weak = rank::is_weak(cached, *total);
                return Ok(SearchOutcome {
                    results: cached.iter().take(max_results).cloned().collect(),
                    weak,
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
        let mut live: Vec<&str> = engines
            .iter()
            .filter(|e| !self.quarantined(e))
            .copied()
            .collect();
        // Rank engines by learned trust so width cuts drop
        // the weakest first.
        {
            let trust = self.trust.lock().unwrap();
            live.sort_by(|a, b| {
                trust
                    .get(*b)
                    .copied()
                    .unwrap_or(1.0)
                    .total_cmp(&trust.get(*a).copied().unwrap_or(1.0))
            });
        }
        // ── Adaptive fan-out width: the governor. Under
        // stress the system shrinks its appetite instead of
        // burning lanes — consensus survives at width 2 by
        // construction (two independent index families).
        let width = width_for_stress(self.pool.stress(), live.len());
        live.truncate(width);
        let mut assignments: Vec<(String, String)> =
            live.iter().map(|e| (e.to_string(), query.to_string())).collect();
        // Recall variants spend lanes — only when the
        // governor did NOT cut the roster (healthy pool).
        if queries.len() > 1 && self.pool.stress() < 0.15 {
            for e in live.iter().take(2) {
                assignments.push((e.to_string(), queries[1].clone()));
            }
        }

        // The premium-lane cap only exists when proxies
        // exist to be workhorses. A zero-config install has
        // ONE lane — direct serves every engine, paced
        // strictly (the governor's jitter does that).
        let cap_direct = self.pool.has_proxies();
        let mut direct_used = false;
        for (engine, q) in assignments {
            let Some(eg) = self
                .pool
                .pick(&engine, &used_egresses, !direct_used || !cap_direct)
            else {
                break;
            };
            if eg.proxy.is_none() {
                direct_used = true;
            }
            // In no-proxy mode every engine shares the one
            // lane — exclusion by egress id would bench
            // engines 2..N, so only exclude when proxies
            // give lanes their own identity.
            if cap_direct {
                used_egresses.push(eg.id.clone());
            }
            futures.push(Box::pin(engine_task(
                engine, q, eg.id, eg.proxy, &self.fetcher, &self.pool,
            )));
        }
        // Verticals: direct, friendly APIs.
        let verticals: Vec<&&str> = verticals
            .iter()
            .filter(|v| !self.quarantined(v))
            .collect();
        for v in verticals {
            futures.push(Box::pin(vertical_task(
                v.to_string(), query.to_string(), &self.fetcher, None,
            )));
        }

        let outcomes = futures_util::future::join_all(futures).await;

        // ── Retry wave: failed engines get one more shot
        // through a fresh egress — but ONLY when the first
        // wave left the merge thin. A healthy merge never
        // pays retry latency; a degraded one recovers.
        let ok_engines = outcomes.iter().filter(|(_, r)| r.is_ok()).count();
        let ok_hits: usize = outcomes
            .iter()
            .filter_map(|(_, r)| r.as_ref().ok())
            .map(|(h, _, _, _)| h.len())
            .sum();
        let merge_thin = ok_engines < 3 || ok_hits < 15;
        let failed: Vec<String> = if merge_thin {
            outcomes
                .iter()
                .filter(|(_, r)| {
                    matches!(r, Err((s, _, _)) if s != "no-results")
                })
                .map(|(e, _)| e.split('@').next().unwrap_or(e).to_string())
                .collect()
        } else {
            Vec::new()
        };
        let mut retry_futures: Vec<TaskFut> = Vec::new();
        for engine in &failed {
            let is_vertical = matches!(
                engine.as_str(),
                "github" | "hn" | "wikipedia" | "scholar" | "news" | "arxiv"
                    | "stackexchange" | "mdn"
            );
            if is_vertical {
                // Vertical retry rides a proxy egress (their
                // direct IP is what got rate-limited).
                let Some(eg) = self.pool.pick("github", &[], false) else { continue };
                retry_futures.push(Box::pin(vertical_task(
                    engine.clone(),
                    query.to_string(),
                    &self.fetcher,
                    eg.proxy,
                )));
                continue;
            }
            // ddg's lite endpoint often lives when html dies.
            let retry_engine = if engine == "ddg" { "ddg_lite" } else { engine };
            let Some(eg) = self
                .pool
                .pick(engine, &used_egresses, !direct_used || !cap_direct)
            else {
                continue;
            };
            if eg.proxy.is_none() {
                direct_used = true;
            }
            retry_futures.push(Box::pin(engine_task(
                retry_engine.to_string(),
                query.to_string(),
                eg.id,
                eg.proxy,
                &self.fetcher,
                &self.pool,
            )));
        }
        let retry_outcomes = if retry_futures.is_empty() {
            Vec::new()
        } else {
            match tokio::time::timeout(
                Duration::from_secs(3),
                futures_util::future::join_all(retry_futures),
            )
            .await
            {
                Ok(o) => o,
                Err(_) => Vec::new(),
            }
        };

        let mut per_engine: Vec<(String, Vec<engines::Hit>)> = Vec::new();
        let mut report = Vec::new();
        let all: Vec<(String, EngineResult)> =
            outcomes.into_iter().chain(retry_outcomes).collect();
        for (engine, outcome) in all {
            match outcome {
                Ok((hits, ms, egress_id, was_engine)) => {
                    let base = engine.split('_').next().unwrap_or(&engine);
                    self.record_outcome(base, true);
                    if was_engine {
                        self.pool.report_ok(base, &egress_id);
                        self.bump_trust(base, true);
                    }
                    report.push(EngineReport {
                        engine: engine.clone(),
                        status: "ok".into(),
                        hits: hits.len(),
                        ms,
                        egress: egress_id.clone(),
                    });
                    per_engine.push((engine, hits));
                }
                Err((status, egress_id, was_engine)) => {
                    let base = engine.split('_').next().unwrap_or(&engine);
                    // Dead proxies are egress failures, not
                    // engine failures — don't quarantine.
                    if !status.starts_with("dead")
                        && status != "auth-fail"
                        && status != "no-results"
                    {
                        self.record_outcome(base, false);
                    }
                    if was_engine {
                        if status.starts_with("dead") {
                            self.pool.report_dead(&egress_id);
                        } else if status == "auth-fail" {
                            self.pool.report_auth_fail(&egress_id);
                        } else if status != "no-results" {
                            self.pool.report_blocked(base, &egress_id);
                            self.bump_trust(base, false);
                        }
                    }
                    report.push(EngineReport {
                        engine,
                        status,
                        hits: 0,
                        ms: 0,
                        egress: egress_id,
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
        let total = rank::merged_total(&per_engine);
        let results = rank::merge(&per_engine, query, intent, &trust, max_results);
        let weak = rank::is_weak(&results, total);
        // Poisoning guard: a merge built while engines
        // were down must NOT persist for 30 minutes —
        // degraded-period results expire with the moment.
        let cacheable = ok_engines >= 2 && total >= 8;
        if cacheable {
            let mut cache = self.cache.lock().unwrap();
            // LRU-ish cap: drop oldest when full.
            if cache.len() >= 500 {
                if let Some(oldest) = cache
                    .iter()
                    .max_by_key(|(_, (at, _, _))| at.elapsed())
                    .map(|(k, _)| k.clone())
                {
                    cache.remove(&oldest);
                }
            }
            cache.insert(
                cache_key,
                (Instant::now(), results.iter().take(12).cloned().collect(), total),
            );
            save_cache_disk(&cache);
        }

        Ok(SearchOutcome {
            results,
            weak,
            intent,
            report,
            cached: false,
            elapsed: started.elapsed(),
        })
    }

    fn bump_trust(&self, base_engine: &str, ok: bool) {
        let mut trust = self.trust.lock().unwrap();
        let t = trust.entry(base_engine.to_string()).or_insert(1.0);
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
    pool: &EgressPool,
) -> (String, EngineResult) {
    let label = engine.clone();
    pool.pace(&engine, &egress_id).await;
    let started = Instant::now();
    let Some(url) = engines::serp_url(&engine, &query) else {
        return (label, Err(("no-url".into(), egress_id, true)));
    };
    let out =
        match tokio::time::timeout(ENGINE_TIMEOUT, fetcher.fetch_once_via(&url, &[], proxy.as_ref(), false))
            .await
        {
            Err(_) => return (label, Err(("timeout".into(), egress_id, true))),
            Ok(Err(e)) => {
                let status = match &e {
                    FetchError::Timeout => "timeout",
                    FetchError::Http(m) if m.contains("CONNECT -> 407") => "auth-fail",
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
        // Honest "no results" is NOT an engine failure —
        // don't burn trust/lanes for a dry query.
        let lower = html.to_lowercase();
        let dry = lower.contains("no results")
            || lower.contains("did not match any")
            || lower.contains("no good results")
            || lower.contains("nothing found");
        let status = if dry { "no-results" } else { "empty-parse" };
        return (label, Err((status.into(), egress_id, true)));
    }
    (label, Ok((hits, ms, egress_id, true)))
}

async fn vertical_task(
    vertical: String,
    query: String,
    fetcher: &Fetcher,
    proxy: Option<crate::transport::proxy::Proxy>,
) -> (String, EngineResult) {
    let started = Instant::now();
    match tokio::time::timeout(
        ENGINE_TIMEOUT,
        verticals::run(&fetcher, &vertical, &query, proxy.as_ref()),
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

/// Disk cache path (ghost-state pattern).
fn cache_path() -> Option<std::path::PathBuf> {
    let dir = dirs_cache()?;
    Some(dir.join("search-cache.json"))
}

fn dirs_cache() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = std::path::PathBuf::from(home).join(".cache/donsetch");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// On disk: (key, age_secs, results, total) — age lets us
/// re-base Instant across process restarts.
fn save_cache_disk(cache: &HashMap<String, (Instant, Vec<Merged>, usize)>) {
    let Some(path) = cache_path() else { return };
    let now = Instant::now();
    let entries: Vec<(String, u64, Vec<Merged>, usize)> = cache
        .iter()
        .map(|(k, (at, r, t))| {
            (k.clone(), now.saturating_duration_since(*at).as_secs(), r.clone(), *t)
        })
        .collect();
    if let Ok(json) = serde_json::to_string(&entries) {
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_shrinks_width_under_stress() {
        assert_eq!(width_for_stress(0.05, 4), 4);
        assert_eq!(width_for_stress(0.30, 4), 3);
        assert_eq!(width_for_stress(0.50, 4), 2);
        assert_eq!(width_for_stress(0.90, 4), 1);
        assert_eq!(width_for_stress(0.90, 0), 0);
    }

    #[test]
    fn norm_query_collapses_variants() {
        assert_eq!(
            norm_query("Rust Async: the Runtime, comparison!"),
            norm_query("rust async runtime comparison")
        );
        assert_ne!(norm_query("kafka vs nats"), norm_query("kafka"));
    }

    #[test]
    fn cache_ttl_is_intent_and_recency_aware() {
        assert!(cache_ttl(Intent::News, "anything") < cache_ttl(Intent::Web, "anything"));
        assert!(cache_ttl(Intent::Code, "anything") < cache_ttl(Intent::Web, "anything"));
        // recency signal forces news-grade TTL even for web intent
        assert_eq!(
            cache_ttl(Intent::Web, "nepal inflation 2026 rate"),
            cache_ttl(Intent::News, "x")
        );
        assert_eq!(
            cache_ttl(Intent::Web, "rust ownership explained"),
            Duration::from_secs(1800)
        );
    }
}

fn load_cache_disk() -> HashMap<String, (Instant, Vec<Merged>, usize)> {
    let mut map = HashMap::new();
    let Some(path) = cache_path() else { return map };
    let Ok(raw) = std::fs::read_to_string(path) else { return map };
    let Ok(entries) =
        serde_json::from_str::<Vec<(String, u64, Vec<Merged>, usize)>>(&raw)
    else {
        return map;
    };
    for (key, age, results, total) in entries {
        // TTL is intent + recency keyed (the query text
        // is the key's first segment).
        let (qpart, ipart) = key.rsplit_once('|').unwrap_or((key.as_str(), ""));
        let intent = match ipart {
            "News" => Intent::News,
            "Code" => Intent::Code,
            "Paper" => Intent::Paper,
            "Entity" => Intent::Entity,
            _ => Intent::Web,
        };
        let ttl = cache_ttl(intent, qpart);
        if Duration::from_secs(age) < ttl {
            map.insert(
                key,
                (
                    Instant::now() - Duration::from_secs(age),
                    results,
                    total,
                ),
            );
        }
    }
    map
}

/// Governor: fan-out width under stress. Healthy pool →
/// all engines; stressed → shrink appetite (you can't be
/// rate-limited if you never exceed the rate); starved →
/// top engine + verticals. Consensus survives at width 2
/// by construction (two independent index families).
fn width_for_stress(stress: f64, available: usize) -> usize {
    if stress < 0.15 {
        available
    } else if stress < 0.40 {
        available.min(3)
    } else if stress < 0.65 {
        available.min(2)
    } else {
        available.min(1)
    }
}

/// Markdown rendering for the MCP/CLI surface.
pub fn render_markdown(out: &SearchOutcome, query: &str) -> String {
    // Search answers ONE question: "what should I fetch?"
    // Snippets carry just enough to decide — content is
    // the fetch tool's job.
    let mut md = format!("# Search: {query}\n\n");
    for (i, r) in out.results.iter().enumerate() {
        let host = rank::host_of(&r.url);
        md.push_str(&format!("{}. **{}** — {}\n", i + 1, r.title, host));
        if !r.snippet.is_empty() {
            let snip: String = r.snippet.chars().take(120).collect();
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
            "egress": if r.egress == "direct" { "direct".to_string() } else { "proxy".to_string() },
        })).collect::<Vec<_>>(),
    })
}

/// Removes the inflight key when the leader finishes
/// (success or failure) so the set never grows unbounded.
struct InflightGuard<'a> {
    map: &'a Mutex<std::collections::HashSet<String>>,
    key: String,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.key);
    }
}
