//! The crawl orchestrator. Owns: frontier loop, worker pool,
//! budgets, stop conditions, near-dup detection, resume tokens.
//!
//! Fetch I/O goes through the `PageFetcher` trait — the real
//! crawl rides DonShadow; tests ride a mock. Never does the
//! orchestrator touch sockets directly.

pub mod frontier;
pub mod governor;
pub mod real;
pub mod score;
pub mod sitemap;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use url::Url;

use crate::detect::walls::Verdict;
use crate::extract::{self, ContentKind, ExtractOptions};

use frontier::{FrontierQueue, scope_allowed};
use governor::Governor;
use sitemap::SitemapEntry;

/// A fetched page as the orchestrator sees it.
pub struct FetchedPage {
    /// Final URL after redirects.
    pub url: String,
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub verdict: Verdict,
    pub latency: Duration,
    /// True when served from the revalidation/pool cache — these
    /// are FREE and must not count against pacing budgets.
    pub cached: bool,
    /// Human-readable failure note (network error, etc.).
    pub error_hint: Option<String>,
}

/// Pluggable fetch: real = DonShadow, tests = in-memory map.
pub type PageFetcher =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, FetchedPage> + Send + Sync>;
//            (url, lane_id) -> page

/// Crawl surface mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlMode {
    /// Both phases (default): sitemap-map first, then content.
    Full,
    /// URL map only — cheap, extractable for agent decisions.
    Map,
    /// Skip the sitemap phase, crawl links BFS-style only.
    Content,
}

/// One harvested crawl page.
pub struct CrawlPage {
    pub url: String,
    pub title: String,
    pub kind: ContentKind,
    pub markdown: String,
    pub chars: usize,
    pub quality: f32,
    /// Same-content duplicate of an already-kept page.
    pub duplicate: bool,
}

/// Why the crawl stopped. Agents MUST see this to decide
/// whether to resume or re-scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// Frontier exhausted — the whole reachable scope was read.
    FrontierEmpty,
    MaxPages,
    CharBudget,
    DepthLimit,
    Deadline,
    /// Host boxed us out (all lanes walled) — resume later.
    ThrottledOut,
}

pub struct CrawlResult {
    pub seed: String,
    pub pages: Vec<CrawlPage>,
    /// URLs discovered but not fetched (budget/depth/scope).
    pub queued: Vec<String>,
    /// URLs skipped by robots/scope rules.
    pub filtered_out: usize,
    /// URLs fetched but skipped (wall/dup/error), with reason.
    pub skipped: Vec<(String, String)>,
    pub stop: StopReason,
    pub elapsed: Duration,
    /// Sitemap map (Map phase) — capped URLs.
    pub map: Vec<String>,
    /// Resume token when stopped early, for `resume=`.
    pub resume: Option<String>,
}

#[derive(Clone)]
pub struct CrawlOptions {
    pub focus: Option<String>,
    pub mode: CrawlMode,
    /// Pages to fetch+extract beyond the seed.
    pub max_pages: usize,
    pub max_depth: u32,
    /// Sum of extracted chars across ALL pages.
    pub max_total_chars: usize,
    /// DonSift max_chars per page.
    pub per_page_max: usize,
    /// Path globs: only crawl these (empty = all).
    pub include_paths: Vec<String>,
    /// Path globs: never crawl these.
    pub exclude_paths: Vec<String>,
    /// Restrict to seed's host (default true).
    pub same_host: bool,
    /// Hard crawl deadline; partial results returned after.
    pub deadline: Duration,
    /// Worker concurrency (same host). 1 = pure polite serial.
    pub concurrency: usize,
    /// Obey robots.txt Disallow rules.
    pub respect_robots: bool,
    /// Map hard cap.
    pub map_cap: usize,
}

impl Default for CrawlOptions {
    fn default() -> Self {
        Self {
            focus: None,
            mode: CrawlMode::Full,
            max_pages: 10,
            max_depth: 2,
            max_total_chars: 60_000,
            per_page_max: 8_000,
            include_paths: Vec::new(),
            exclude_paths: Vec::new(),
            same_host: true,
            deadline: Duration::from_secs(120),
            concurrency: 1,
            respect_robots: true,
            map_cap: 120,
        }
    }
}

/// State carried in a resume token.
#[derive(serde::Serialize, serde::Deserialize)]
struct ResumeState {
    seed: String,
    queue: Vec<(String, f64, u32)>,
    /// Seen-set from run 1 — without it, run-2 pages re-link
    /// to already-fetched pages and they crawl AGAIN.
    seen: Vec<String>,
}

/// Disk-backed resume store: tokens survive process restarts,
/// so both the MCP daemon AND one-shot CLI runs can continue a
/// crawl. ~/.cache/donsetch/crawl-resumes.json, 30-min TTL.
fn resumes_path() -> std::path::PathBuf {
    dirs_cache().join("crawl-resumes.json")
}

fn dirs_cache() -> std::path::PathBuf {
    crate::paths::cache_dir()
}

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct ResumeFile {
    /// token -> (state, issued_at_unix)
    entries: std::collections::HashMap<String, (ResumeState, u64)>,
}

impl ResumeFile {
    fn load() -> Self {
        std::fs::read_to_string(resumes_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let p = resumes_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(p, s);
        }
    }

    fn sweep(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.entries.retain(|_, (_, at)| now.saturating_sub(*at) < 30 * 60);
    }
}

pub struct Crawler {
    fetch: PageFetcher,
    governor: Arc<Governor>,
    token_seq: AtomicUsize,
}

impl Crawler {
    pub fn new(fetch: PageFetcher, governor: Arc<Governor>) -> Self {
        Self { fetch, governor, token_seq: AtomicUsize::new(0) }
    }

    /// Run one crawl. Returns when a stop condition hits.
    pub async fn crawl(
        &self,
        seed: &str,
        opts: CrawlOptions,
        resume_token: Option<&str>,
    ) -> Result<CrawlResult, String> {
        let started = Instant::now();
        let seed_url = Url::parse(seed).map_err(|_| format!("bad seed url: {seed}"))?;
        let seed_host = seed_url.host_str().ok_or("seed must have a host")?.to_string();

        let host_ok = {
            let sh = seed_host.clone();
            let same = opts.same_host;
            move |u: &Url| {
                !same || u.host_str().map(|h| h == sh).unwrap_or(false)
            }
        };

        // ── Phase 1: the map ───────────────────────────────
        let mut map: Vec<String> = Vec::new();
        let mut sitemap_entries: Vec<SitemapEntry> = Vec::new();
        let mut robots = sitemap::Robots::default();
        if opts.mode != CrawlMode::Content {
            let (mut r, mut entries) =
                sitemap::discover(&self.fetch, &seed_host, opts.map_cap * 4).await;
            robots = sitemap::Robots::default();
            std::mem::swap(&mut robots, &mut r);
            // Newest first: lastmod-sorted sitemaps put fresh
            // content at the front of the crawl budget.
            entries.sort_by(|a, b| b.lastmod.cmp(&a.lastmod));
            sitemap_entries = entries;
            for e in &sitemap_entries {
                if map.len() >= opts.map_cap {
                    break;
                }
                if let Ok(u) = Url::parse(&e.loc) {
                    if !host_ok(&u) {
                        continue;
                    }
                    if !scope_allowed(u.path(), &opts.include_paths, &opts.exclude_paths) {
                        continue;
                    }
                    if let Some(q) = &opts.focus {
                        let s = score::score_candidate("", u.path(), Some(q));
                        if s <= 0.0 {
                            continue;
                        }
                    }
                    map.push(e.loc.clone());
                }
            }
        } else {
            // Content-only still reads robots for Disallow
            // rules when respect_robots is on.
            if opts.respect_robots {
                let (r, _) = sitemap::discover(&self.fetch, &seed_host, 0).await;
                robots = r;
            }
        }
        if opts.respect_robots {
            self.governor.set_crawl_delay(robots.crawl_delay);
        }
        if opts.mode == CrawlMode::Map {
            // Map-only crawl: cheap exit.
            return Ok(CrawlResult {
                seed: seed.to_string(),
                pages: Vec::new(),
                queued: Vec::new(),
                filtered_out: 0,
                skipped: Vec::new(),
                stop: StopReason::FrontierEmpty,
                elapsed: started.elapsed(),
                map,
                resume: None,
            });
        }

        // ── Frontier seeding ───────────────────────────────
        let mut queue = FrontierQueue::new();
        // Budgets are PER-CALL: a resume continues from the saved
        // position but the caller's page/char budgets apply to
        // the NEW work. (Run 2 must not instantly exhaust itself
        // against run 1's spend.)
        let fetched_pages = 0usize;
        let total_chars = 0usize;
        let seed_norm = frontier::normalize(&seed_url);
        if let Some(tok) = resume_token {
            let mut store = ResumeFile::load();
            store.sweep();
            if let Some((state, _)) = store.entries.remove(tok) {
                queue.restore_seen(state.seen);
                for (u, s, d) in state.queue {
                    queue.push_to_heap(u, s, d);
                }
                store.save();
            } else {
                return Err(format!("resume token expired or unknown: {tok}"));
            }
        } else {
            let _ = queue.push(seed_url.clone(), 10.0, 0);
            // Sitemap entries seed frontier at depth 1.
            for e in &sitemap_entries {
                if let Ok(u) = Url::parse(&e.loc) {
                    if host_ok(&u)
                        && scope_allowed(u.path(), &opts.include_paths, &opts.exclude_paths)
                        && (!opts.respect_robots || robots.allowed(u.path()))
                    {
                        let s = score::score_candidate("", u.path(), opts.focus.as_deref());
                        queue.push(u, s, 1);
                    }
                }
            }
        }

        if queue.is_empty() {
            return Ok(CrawlResult {
                seed: seed.to_string(),
                pages: Vec::new(),
                queued: Vec::new(),
                filtered_out: 0,
                skipped: vec![(seed.to_string(), "empty frontier (all filtered)".into())],
                stop: StopReason::FrontierEmpty,
                elapsed: started.elapsed(),
                map,
                resume: None,
            });
        }

        // ── Phase 2: page loop ─────────────────────────────
        let sh_queue = Arc::new(Mutex::new(queue));
        let pages: Arc<Mutex<Vec<CrawlPage>>> = Arc::new(Mutex::new(Vec::new()));
        let skipped: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let filtered_out = Arc::new(AtomicUsize::new(0));
        let dup_sigs: Arc<Mutex<HashSet<u64>>> = Arc::new(Mutex::new(HashSet::new()));
        let chars_total = Arc::new(AtomicUsize::new(total_chars));
        let pages_done = Arc::new(AtomicUsize::new(fetched_pages));
        let stop_flag: Arc<Mutex<Option<StopReason>>> = Arc::new(Mutex::new(None));
        let deadline_at = started + opts.deadline;
        let focus = Arc::new(opts.focus.clone());

        let workers = opts.concurrency.max(1);
        let mut handles = Vec::new();
        for wid in 0..workers {
            let queue = Arc::clone(&sh_queue);
            let pages = Arc::clone(&pages);
            let skipped = Arc::clone(&skipped);
            let filtered_out = Arc::clone(&filtered_out);
            let dup_sigs = Arc::clone(&dup_sigs);
            let chars_total = Arc::clone(&chars_total);
            let pages_done = Arc::clone(&pages_done);
            let stop_flag = Arc::clone(&stop_flag);
            let focus = Arc::clone(&focus);
            let fetch = self.fetch.clone();
            let governor = Arc::clone(&self.governor);
            let opts_worker = opts.clone();
            let seed_host2 = seed_host.clone();
            let seed_norm_w = seed_norm.clone();
            let robots = robots.clone();
            let max_pages = opts.max_pages;
            let max_total = opts.max_total_chars;
            let max_depth = opts.max_depth;

            handles.push(tokio::spawn(async move {
                let mut seq = wid as u64 * 1000;

                'work: loop {
                    // ── Stop conditions ──
                    if Instant::now() >= deadline_at {
                        let mut s = stop_flag.lock().unwrap();
                        if s.is_none() {
                            *s = Some(StopReason::Deadline);
                        }
                        break 'work;
                    }
                    if let Some(_) = *stop_flag.lock().unwrap() {
                        break 'work;
                    }
                    if pages_done.load(Ordering::SeqCst) >= max_pages {
                        let mut s = stop_flag.lock().unwrap();
                        if s.is_none() {
                            *s = Some(StopReason::MaxPages);
                        }
                        break 'work;
                    }
                    if chars_total.load(Ordering::SeqCst) >= max_total {
                        let mut s = stop_flag.lock().unwrap();
                        if s.is_none() {
                            *s = Some(StopReason::CharBudget);
                        }
                        break 'work;
                    }

                    // ── Pop next ──
                    let next = queue.lock().unwrap().pop();
                    let Some(item) = next else {
                        // Frontier empty — but other workers may add.
                        // Grace: spin briefly, then exit.
                        tokio::time::sleep(Duration::from_millis(150)).await;
                        if queue.lock().unwrap().is_empty() {
                            let mut s = stop_flag.lock().unwrap();
                            if s.is_none() {
                                *s = Some(StopReason::FrontierEmpty);
                            }
                            break 'work;
                        }
                        continue 'work;
                    };

                    let parsed = match Url::parse(&item.url) {
                        Ok(u) => u,
                        Err(_) => {
                            skipped
                                .lock()
                                .unwrap()
                                .push((item.url.clone(), "unparseable".into()));
                            continue 'work;
                        }
                    };
                    if item.depth > max_depth {
                        let mut s = stop_flag.lock().unwrap();
                        if s.is_none() {
                            *s = Some(StopReason::DepthLimit);
                        }
                        break 'work;
                    }
                    let host = parsed.host_str().unwrap_or("");
                    if opts_worker.same_host && host != seed_host2 {
                        filtered_out.fetch_add(1, Ordering::Relaxed);
                        continue 'work;
                    }
                    // The seed itself is explicit user intent —
                    // include/exclude globs only govern backlinks.
                    let is_seed = item.url == seed_norm_w;
                    if !is_seed
                        && !scope_allowed(parsed.path(), &opts_worker.include_paths, &opts_worker.exclude_paths)
                    {
                        filtered_out.fetch_add(1, Ordering::Relaxed);
                        continue 'work;
                    }
                    if opts_worker.respect_robots && !robots.allowed(parsed.path()) {
                        filtered_out.fetch_add(1, Ordering::Relaxed);
                        continue 'work;
                    }

                    // ── Governor-paced fetch ──
                    // Workers aren't lane-pinned: whoever's
                    // least-blocked for this host takes it.
                    let Some(lane) = governor.best_lane(host).cloned() else {
                        if governor
                            .wait_for(host, "*", seq)
                            > Duration::ZERO
                        {
                            // Whole host boxed — if the frontier
                            // holds only this host, we're done.
                            if queue.lock().unwrap().is_empty() {
                                let mut s = stop_flag.lock().unwrap();
                                if s.is_none() {
                                    *s = Some(StopReason::ThrottledOut);
                                }
                                break 'work;
                            }
                            // Requeue and yield.
                            queue.lock().unwrap().requeue(frontier::Frontier {
                                url: item.url.clone(),
                                score: item.score,
                                depth: item.depth,
                            });
                            tokio::time::sleep(Duration::from_millis(400)).await;
                            continue 'work;
                        }
                        continue 'work;
                    };
                    let lane = lane;
                    seq += 1;
                    let wait = governor.wait_for(host, &lane.id, seq);
                    // Cap wait inside remaining deadline.
                    let remain = deadline_at.saturating_duration_since(Instant::now());
                    let wait = wait.min(remain);
                    if wait > Duration::ZERO {
                        tokio::time::sleep(wait).await;
                    }

                    let page = fetch(item.url.clone(), lane.id.clone()).await;
                    if page.cached {
                        // Warm-cache hit: free — no governor signal.
                    } else {
                        match (page.status, &page.verdict) {
                            (200, Verdict::ContentOk) => {
                                governor.on_success(host, &lane.id, page.latency)
                            }
                            (429, _) | (503, _) => {
                                governor.on_throttled(host, &lane.id);
                            }
                            _ => governor.on_error(host, &lane.id),
                        }
                    }

                    // Wall/denylist verdicts → skip honestly.
                    if !matches!(page.verdict, Verdict::ContentOk) {
                        let why = page
                            .error_hint
                            .clone()
                            .unwrap_or_else(|| format!("{:?}", page.verdict));
                        skipped.lock().unwrap().push((item.url.clone(), why));
                        continue 'work;
                    }

                    // ── Extract with DonSift ──
                    let mut eo = ExtractOptions::default();
                    eo.focus = focus.as_ref().clone();
                    eo.max_chars = Some(opts_worker.per_page_max);
                    let ctype = page
                        .headers
                        .iter()
                        .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
                        .map(|(_, v)| v.as_str())
                        .unwrap_or("text/html");
                    let r = match extract::extract(&page.body, ctype, &page.url, &eo) {
                        Ok(r) => r,
                        Err(e) => {
                            skipped.lock().unwrap().push((
                                item.url.clone(),
                                format!("extract failed: {e}"),
                            ));
                            continue 'work;
                        }
                    };
                    let md = r.markdown;

                    // Near-dup signature: title + first 200 normalized
                    // chars of the CONTENT (frontmatter carries the
                    // page URL — identical docs at different URLs
                    // must still dedup).
                    let body_md = md
                        .splitn(2, "\n\n")
                        .nth(1)
                        .unwrap_or(md.as_str());
                    let sig_str = format!(
                        "{}|{}",
                        r.title.as_deref().unwrap_or("").trim().to_lowercase(),
                        body_md
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .take(200)
                            .collect::<String>()
                            .to_lowercase()
                    );
                    let sig = {
                        use std::hash::{Hash, Hasher};
                        let mut h = std::collections::hash_map::DefaultHasher::new();
                        sig_str.hash(&mut h);
                        h.finish()
                    };
                    let duplicate = !dup_sigs.lock().unwrap().insert(sig);

                    let chars = md.chars().count();
                    if !duplicate {
                        chars_total.fetch_add(chars, Ordering::SeqCst);
                        // Budget-check AFTER charge: allow partial
                        // truncation for the last page.
                    }
                    pages_done.fetch_add(1, Ordering::SeqCst);
                    pages.lock().unwrap().push(CrawlPage {
                        url: page.url.clone(),
                        title: r.title.clone().unwrap_or_default(),
                        kind: r.content_kind,
                        markdown: md,
                        chars,
                        quality: r.quality,
                        duplicate,
                    });
                    if duplicate {
                        skipped.lock().unwrap().push((
                            page.url.clone(),
                            "near-duplicate".into(),
                        ));
                        continue 'work;
                    }

                    // ── Harvest outlinks into the frontier ──
                    if item.depth < max_depth {
                        let html = String::from_utf8_lossy(&page.body).into_owned();
                        let base = Url::parse(&page.url).unwrap_or_else(|_| parsed.clone());
                        let links = self_harvest_static(&html, &base);
                        let mut q = queue.lock().unwrap();
                        for (child, anchor) in links {
                            let Some(cu) = frontier::resolve(&base, &child) else {
                                continue;
                            };
                            if opts_worker.same_host && cu.host_str() != Some(seed_host2.as_str()) {
                                filtered_out.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            if !scope_allowed(cu.path(), &opts_worker.include_paths, &opts_worker.exclude_paths) {
                                filtered_out.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            if opts_worker.respect_robots && !robots.allowed(cu.path()) {
                                filtered_out.fetch_add(1, Ordering::Relaxed);
                                continue;
                            }
                            let s = score::score_candidate(&anchor, cu.path(), focus.as_deref());
                            q.push(cu, s, item.depth + 1);
                        }
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        // ── Result + resume token ──────────────────────────
        let elapsed = started.elapsed();
        let (final_pages, stop, queued_entries) = {
            let p = std::mem::take(&mut *pages.lock().unwrap());
            let s = stop_flag.lock().unwrap().unwrap_or(StopReason::FrontierEmpty);
            let q = sh_queue.lock().unwrap().snapshot_entries();
            (p, s, q)
        };

        let skipped_v = std::mem::take(&mut *skipped.lock().unwrap());
        let filtered = filtered_out.load(Ordering::Relaxed);

        // Resume token only when stopped by budget (not frontier-empty).
        let resume = match stop {
            StopReason::MaxPages | StopReason::CharBudget | StopReason::Deadline => {
                if !queued_entries.is_empty() {
                    let id = {
                        let n = self.token_seq.fetch_add(1, Ordering::Relaxed);
                        let micros = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_micros())
                            .unwrap_or(0);
                        format!("c{micros:x}{n:x}")
                    };
                    let state = ResumeState {
                        seed: seed.to_string(),
                        queue: queued_entries.iter().map(|(u, s, d)| (u.clone(), *s, *d)).collect(),
                        seen: sh_queue.lock().unwrap().seen_snapshot(),
                    };
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let mut store = ResumeFile::load();
                    store.sweep();
                    store.entries.insert(id.clone(), (state, now));
                    // Cap the file: drop oldest beyond 50 tokens.
                    if store.entries.len() > 50 {
                        let mut keyed: Vec<(u64, String)> = store
                            .entries
                            .iter()
                            .map(|(k, (_, at))| (*at, k.clone()))
                            .collect();
                        keyed.sort();
                        for (_, k) in keyed.into_iter().take(store.entries.len() - 50) {
                            store.entries.remove(&k);
                        }
                    }
                    store.save();
                    Some(id)
                } else {
                    None
                }
            }
            _ => None,
        };

        Ok(CrawlResult {
            seed: seed.to_string(),
            pages: final_pages,
            queued: queued_entries.into_iter().map(|(u, _, _)| u).collect(),
            filtered_out: filtered,
            skipped: skipped_v,
            stop,
            elapsed,
            map,
            resume,
        })
    }
}

/// Anchor+href harvest without holding `&self` (worker closure).
fn self_harvest_static(html: &str, _base: &Url) -> Vec<(String, String)> {
    let doc = scraper::Html::parse_document(html);
    let sel = scraper::Selector::parse("a[href]").unwrap();
    let mut out = Vec::new();
    for a in doc.select(&sel) {
        let Some(href) = a.value().attr("href") else { continue };
        let anchor: String = a.text().collect::<String>().trim().to_string();
        out.push((href.to_string(), anchor));
    }
    out
}
