//! URL frontier: normalization, scoping, and the priority
//! queue that decides WHAT to fetch next.
//!
//! Normalization is where crawls live or die: `?utm_source=`
//! copies of every page kill token budgets. We strip tracking
//! params, fragments, and dedup on the canon form.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

use url::Url;

/// Query keys that never change page content. Stripped so the
/// same page reachable with 50 tracking variants dedups to 1.
const TRACKING_PARAMS: &[&str] = &[
    "utm_source", "utm_medium", "utm_campaign", "utm_term", "utm_content",
    "utm_id", "utm_reader", "utm_viz_id", "utm_pubreferrer", "utm_swu",
    "fbclid", "gclid", "gclsrc", "dclid", "gbraid", "wbraid",
    "msclkid", "twclid", "li_fat_id", "mc_cid", "mc_eid",
    "iref", "ref_src", "ref_url", "_ga", "_gl", "_hsenc", "_hsmi",
    "hsa_cam", "hsa_grp", "hsa_mt", "hsa_src", "hsa_ad", "hsa_acc",
    "hsa_net", "hsa_ver", "hsa_la", "hsa_ol", "hsa_kw",
    "igshid", "si", "spm", "scm", "bbid", "ocid", "oly_enc_id",
    "oly_anon_id", "vero_id", "wickedid", "wickedsource", "wt_mc",
    "yclid", "zanpid", "guccounter",
];

/// Lowercase schemes allowed in the crawl corpus.
fn web_scheme(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// Normalize a URL for frontier dedup.
///
/// - lowercase host, strip default ports
/// - drop fragment (never sent)
/// - strip tracking query params
/// - sort remaining query params for canon
/// - '/' for empty path
/// - trailing-slash collapse on non-root dirs is deliberately
///   NOT done (site-dependent meaning); dedup happens on hit.
pub fn normalize(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    let host = u.host_str().unwrap_or("").to_lowercase();
    let _ = u.set_host(Some(&host));
    // Explicit default ports normalize away.
    if (u.scheme() == "https" && u.port() == Some(443))
        || (u.scheme() == "http" && u.port() == Some(80))
    {
        let _ = u.set_port(None);
    }
    // Strip tracking params, sort the survivors.
    let pairs: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.contains(&k.to_lowercase().as_str()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut pairs = pairs;
    pairs.sort();
    if pairs.is_empty() {
        u.set_query(None);
    } else {
        let qs = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");
        u.set_query(Some(&qs));
    }
    if u.path().is_empty() {
        u.set_path("/");
    }
    u.to_string()
}

/// Resolve a possibly-relative link against the page URL.
/// Returns None for non-web schemes (mailto:, tel:, javascript:).
pub fn resolve(base: &Url, link: &str) -> Option<Url> {
    let joined = base.join(link).ok()?;
    if web_scheme(&joined) {
        Some(joined)
    } else {
        None
    }
}

/// One queued URL with its priority metadata.
#[derive(Clone, Debug)]
pub struct Frontier {
    pub url: String,
    pub score: f64,
    pub depth: u32,
}

impl PartialEq for Frontier {
    fn eq(&self, other: &Self) -> bool {
        self.url == other.url
    }
}
impl Eq for Frontier {}
impl PartialOrd for Frontier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Frontier {
    // Max-heap on score.
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .partial_cmp(&other.score)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.depth.cmp(&self.depth))
    }
}

/// Cross-check one URL against include/exclude path globs.
/// `None` patterns = wildcard pass. Globs: `*` = any run.
pub fn scope_allowed(path: &str, include: &[String], exclude: &[String]) -> bool {
    for pat in exclude {
        if glob_match(pat, path) {
            return false;
        }
    }
    if include.is_empty() {
        return true;
    }
    include.iter().any(|p| glob_match(p, path))
}

/// Glob match: `*` matches any run (including slashes),
/// everything else literal. `/docs/*` matches `/docs/a/b`.
pub fn glob_match(pat: &str, s: &str) -> bool {
    glob_at(pat.as_bytes(), s.as_bytes())
}

fn glob_at(pat: &[u8], s: &[u8]) -> bool {
    if pat.is_empty() {
        return s.is_empty();
    }
    match pat[0] {
        b'*' => {
            // '*' consumes zero or more.
            for skip in 0..=s.len() {
                if glob_at(&pat[1..], &s[skip..]) {
                    return true;
                }
            }
            false
        }
        c if !s.is_empty() && s[0] == c => glob_at(&pat[1..], &s[1..]),
        _ => false,
    }
}

/// The crawl frontier: queued URLs with per-URL priority.
pub struct FrontierQueue {
    heap: BinaryHeap<Frontier>,
    seen: HashSet<String>,
}

impl Default for FrontierQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl FrontierQueue {
    pub fn new() -> Self {
        Self { heap: BinaryHeap::new(), seen: HashSet::new() }
    }

    /// Push a URL if its normalized form is new.
    pub fn push(&mut self, url: Url, score: f64, depth: u32) -> bool {
        let key = normalize(&url);
        if !self.seen.insert(key.clone()) {
            return false;
        }
        self.heap.push(Frontier { url: key, score, depth });
        true
    }

    /// Requeue a popped item (e.g. host boxed, try later).
    /// NOT dedup'd — the seen-set already knows it.
    pub fn requeue(&mut self, f: Frontier) {
        self.heap.push(f);
    }

    /// Restore the seen-set from a resume state (run-1 fetches
    /// must not refetch in run 2).
    pub fn restore_seen(&mut self, urls: Vec<String>) {
        for u in urls {
            self.seen.insert(u);
        }
    }

    /// Push an entry the seen-set already recorded (resume).
    pub fn push_to_heap(&mut self, url: String, score: f64, depth: u32) {
        self.heap.push(Frontier { url, score, depth });
    }

    /// Full seen-set snapshot for resume persistence.
    pub fn seen_snapshot(&self) -> Vec<String> {
        self.seen.iter().cloned().collect()
    }

    pub fn pop(&mut self) -> Option<Frontier> {
        self.heap.pop()
    }

    /// Snapshot all queued entries (url, score, depth) for a
    /// resume token. Does not drain — the seen-set survives.
    pub fn snapshot_entries(&self) -> Vec<(String, f64, u32)> {
        self.heap.iter().map(|f| (f.url.clone(), f.score, f.depth)).collect()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_tracking() {
        let a = Url::parse("https://ex.com/page?utm_source=x&utm_medium=y&id=1").unwrap();
        let b = Url::parse("https://ex.com/page?id=1&fbclid=zzz").unwrap();
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn normalize_strips_fragment_and_case() {
        let a = Url::parse("https://Ex.Com/Path#sec").unwrap();
        let b = Url::parse("https://ex.com/Path").unwrap();
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn normalize_sorts_query() {
        let a = Url::parse("https://ex.com/p?b=2&a=1").unwrap();
        let b = Url::parse("https://ex.com/p?a=1&b=2").unwrap();
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn normalize_default_port() {
        let a = Url::parse("https://ex.com:443/x").unwrap();
        let b = Url::parse("https://ex.com/x").unwrap();
        assert_eq!(normalize(&a), normalize(&b));
    }

    #[test]
    fn resolve_web_only() {
        let base = Url::parse("https://ex.com/a").unwrap();
        assert!(resolve(&base, "mailto:a@b.c").is_none());
        assert!(resolve(&base, "javascript:void(0)").is_none());
        assert!(resolve(&base, "/b").is_some());
    }

    #[test]
    fn glob_basics() {
        assert!(glob_match("/docs/*", "/docs/a/b"));
        assert!(!glob_match("/docs/*", "/blog/a"));
        assert!(glob_match("*", ""));
        assert!(glob_match("*/x", "a/b/x"));
        assert!(!glob_match("*/x", "a/b/y"));
    }

    #[test]
    fn scope_include_exclude() {
        let inc = vec!["/docs/*".to_string()];
        let exc = vec!["*/admin*".to_string()];
        assert!(scope_allowed("/docs/guide", &inc, &exc));
        assert!(!scope_allowed("/other", &inc, &exc));
        assert!(!scope_allowed("/docs/admin/x", &inc, &exc));
        assert!(scope_allowed("/anything", &[], &[]));
    }

    #[test]
    fn queue_dedups() {
        let mut q = FrontierQueue::new();
        let u1 = Url::parse("https://ex.com/a?utm_source=x").unwrap();
        let u2 = Url::parse("https://ex.com/a?fbclid=y").unwrap();
        assert!(q.push(u1, 1.0, 0));
        assert!(!q.push(u2, 1.0, 0));
        assert_eq!(q.len(), 1);
    }

    #[test]
    fn queue_pops_max_score_first() {
        let mut q = FrontierQueue::new();
        let u1 = Url::parse("https://ex.com/a").unwrap();
        let u2 = Url::parse("https://ex.com/b").unwrap();
        q.push(u1, 1.0, 0);
        q.push(u2, 5.0, 0);
        assert!(q.pop().unwrap().url.ends_with("/b"));
    }
}
