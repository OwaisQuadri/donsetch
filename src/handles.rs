//! Reference handles (v3): short session-scoped IDs that stand in
//! for URLs. The URL-noise tax is real — a link-heavy page bleeds
//! hundreds of tokens on raw URLs the daemon can remember instead.
//!
//! Two namespaces:
//!
//! - **`L{n}`** — link handles, interned from fetched-page markdown
//!   (`[text](L12)` instead of `[text](https://very-long-url…)`).
//!   Stable: the same URL always maps to the same handle while the
//!   entry lives. Monotonic counter, LRU-evicted, persisted.
//! - **`S{n}`** — search handles, position-bound: `S3` is result 3
//!   of the most recent search. Rebound on every search — intuitive
//!   for the agent ("fetch S3") and always current.
//!
//! `web_fetch` accepts a handle anywhere it accepts a URL. TTL 24h,
//! cap 2048 L-entries. File: ~/.cache/donsetch/handles.json
//! (atomic tmp+rename writes, same discipline as ghost-state).

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Handle lifetime. A handle that outlives a research session by a
/// day is a handle nobody remembers producing.
const TTL_SECS: u64 = 24 * 60 * 60;
/// L-entry cap — bounded memory, oldest-number eviction.
const MAX_L_ENTRIES: usize = 2048;

#[derive(Clone, Serialize, Deserialize)]
struct LEntry {
    url: String,
    at: u64,
}

#[derive(Serialize, Deserialize)]
struct Persisted {
    next_l: u64,
    l: BTreeMap<String, LEntry>,
    s: Vec<String>,
}

/// The handle table. Shared via the Daemon; mutations flush to disk.
#[derive(Default)]
pub struct HandleTable {
    next_l: u64,
    /// L-number → entry, ordered by number (LRU = lowest number).
    l: BTreeMap<u64, LEntry>,
    /// URL → "L{n}" reverse index for stable re-interning.
    rev: HashMap<String, u64>,
    /// Search result URLs by position (S1 = index 0).
    s: Vec<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> PathBuf {
    crate::paths::cache_dir().join("handles.json")
}

impl HandleTable {
    pub fn load() -> Self {
        let mut t = Self::default();
        let Some(bytes) = std::fs::read(path()).ok() else {
            return t;
        };
        let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) else {
            // Corrupt table: treat as empty. Handles are a cache,
            // never a source of truth — losing them costs nothing.
            return t;
        };
        let cutoff = now().saturating_sub(TTL_SECS);
        t.next_l = p.next_l;
        for (k, e) in p.l {
            if e.at < cutoff {
                continue;
            }
            let Ok(n) = k.parse::<u64>() else { continue };
            t.rev.insert(e.url.clone(), n);
            t.l.insert(n, e);
        }
        t.s = p.s;
        t
    }

    /// Atomic flush (tmp + rename, same pattern as ghost-state).
    pub fn flush(&self) {
        let p = Persisted {
            next_l: self.next_l,
            l: self
                .l
                .iter()
                .map(|(n, e)| (n.to_string(), e.clone()))
                .collect(),
            s: self.s.clone(),
        };
        let dir = crate::paths::cache_dir();
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(".handles.json.tmp");
        if serde_json::to_vec(&p)
            .map_err(|e| e.to_string())
            .and_then(|b| std::fs::write(&tmp, b).map_err(|e| e.to_string()))
            .is_ok()
        {
            let _ = std::fs::rename(&tmp, path());
        }
    }

    /// Intern a link URL → stable "L{n}".
    pub fn intern_link(&mut self, url: &str) -> String {
        if let Some(&n) = self.rev.get(url) {
            if let Some(e) = self.l.get_mut(&n) {
                e.at = now();
            }
            return format!("L{n}");
        }
        self.next_l += 1;
        let n = self.next_l;
        self.rev.insert(url.to_string(), n);
        self.l.insert(
            n,
            LEntry {
                url: url.to_string(),
                at: now(),
            },
        );
        // Evict oldest-numbered entries past the cap.
        while self.l.len() > MAX_L_ENTRIES {
            if let Some((&oldest, _)) = self.l.iter().next() {
                if let Some(e) = self.l.remove(&oldest) {
                    self.rev.remove(&e.url);
                }
            } else {
                break;
            }
        }
        format!("L{n}")
    }

    /// Bind search handles: S1..Sn = the given URLs in order.
    /// Returns the handle for each URL (same order).
    pub fn set_search_results(&mut self, urls: &[String]) -> Vec<String> {
        self.s = urls.to_vec();
        (0..urls.len()).map(|i| format!("S{}", i + 1)).collect()
    }

    /// Resolve a handle ("l12"/"S3", case-insensitive) to its URL.
    pub fn resolve(&self, h: &str) -> Option<String> {
        let h = h.trim();
        let lower = h.to_ascii_lowercase();
        if let Some(num) = lower.strip_prefix('s') {
            let idx: usize = num.parse().ok()?;
            return self.s.get(idx.checked_sub(1)?).cloned();
        }
        if let Some(num) = lower.strip_prefix('l') {
            let n: u64 = num.parse().ok()?;
            return self.l.get(&n).map(|e| e.url.clone());
        }
        None
    }

    /// Rewrite `[text](https://…)` markdown links to
    /// `[text](L{n})`. Returns the new markdown and the number of
    /// handles created/reused.
    pub fn replace_link_urls(&mut self, md: &str) -> (String, usize) {
        let mut out = String::with_capacity(md.len());
        let mut pos = 0usize;
        let mut count = 0usize;
        while let Some(rel) = md[pos..].find("](http") {
            let url_start = pos + rel + 2;
            let Some(close_rel) = md[url_start..].find(')') else {
                break;
            };
            let url = &md[url_start..url_start + close_rel];
            // Sanity: spaces or control chars mean this isn't a
            // clean generated link — leave it untouched.
            if url.chars().any(|c| c.is_whitespace()) {
                let skip = pos + rel + 6;
                out.push_str(&md[pos..skip]);
                pos = skip;
                continue;
            }
            let handle = self.intern_link(url);
            out.push_str(&md[pos..url_start]);
            out.push_str(&handle);
            out.push(')');
            pos = url_start + close_rel + 1;
            count += 1;
        }
        out.push_str(&md[pos..]);
        (out, count)
    }
}

/// Is this string a handle (not a URL)? `L12`, `s3`, …
pub fn is_handle(s: &str) -> bool {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix('l').or_else(|| lower.strip_prefix('s')) else {
        return false;
    };
    !rest.is_empty() && rest.len() <= 6 && rest.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> HandleTable {
        HandleTable {
            next_l: 0,
            l: BTreeMap::new(),
            rev: HashMap::new(),
            s: Vec::new(),
        }
    }

    #[test]
    fn intern_is_stable_and_monotonic() {
        let mut t = table();
        let a = t.intern_link("https://a.example.com/x");
        let b = t.intern_link("https://b.example.com/y");
        let a2 = t.intern_link("https://a.example.com/x");
        assert_eq!(a, "L1");
        assert_eq!(b, "L2");
        assert_eq!(a2, "L1", "same URL must keep its handle");
        assert_eq!(t.resolve("L1").unwrap(), "https://a.example.com/x");
        assert_eq!(t.resolve("l2").unwrap(), "https://b.example.com/y");
    }

    #[test]
    fn search_handles_are_positional() {
        let mut t = table();
        let hs =
            t.set_search_results(&["https://x.example/1".into(), "https://x.example/2".into()]);
        assert_eq!(hs, vec!["S1", "S2"]);
        assert_eq!(t.resolve("S2").unwrap(), "https://x.example/2");
        assert_eq!(t.resolve("s1").unwrap(), "https://x.example/1");
        // Rebind: a new search replaces the bindings.
        t.set_search_results(&["https://y.example/only".into()]);
        assert_eq!(t.resolve("S1").unwrap(), "https://y.example/only");
        assert_eq!(t.resolve("S2"), None);
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let t = table();
        assert_eq!(t.resolve("L99"), None);
        assert_eq!(t.resolve("S99"), None);
        assert_eq!(t.resolve("not-a-handle"), None);
    }

    #[test]
    fn is_handle_recognizes_and_rejects() {
        assert!(is_handle("L12"));
        assert!(is_handle("s3"));
        assert!(!is_handle("https://example.com"));
        assert!(!is_handle("L"));
        assert!(!is_handle("L12x"));
        assert!(!is_handle("LOL"));
        assert!(!is_handle(""));
    }

    #[test]
    fn replace_rewrites_markdown_links() {
        let mut t = table();
        let md = "See [docs](https://example.com/docs/a) and [more](https://example.com/b?x=1) plus text.";
        let (out, n) = t.replace_link_urls(md);
        assert_eq!(n, 2);
        assert_eq!(out, "See [docs](L1) and [more](L2) plus text.");
        // Re-run: stable handles, no double-interning.
        let (out2, n2) = t.replace_link_urls(md);
        assert_eq!(n2, 2);
        assert_eq!(out2, out);
    }

    #[test]
    fn replace_leaves_non_urls_and_bare_text_alone() {
        let mut t = table();
        let md = "plain text with no links\n[anchor](#section) and [x](mailto:a@b.c)\n";
        let (out, n) = t.replace_link_urls(md);
        assert_eq!(n, 0);
        assert_eq!(out, md);
    }

    #[test]
    fn replace_handles_cjk_and_multibyte_around_links() {
        let mut t = table();
        let md = "検索結果: [公式](https://example.com/日本語) ここまで。";
        let (out, n) = t.replace_link_urls(md);
        assert_eq!(n, 1);
        assert!(out.contains("[公式](L1)"));
        assert!(out.contains("ここまで。"));
    }

    #[test]
    fn eviction_caps_entries_keeps_freshest() {
        let mut t = table();
        for i in 0..MAX_L_ENTRIES + 10 {
            t.intern_link(&format!("https://e.example.com/{i}"));
        }
        assert_eq!(t.l.len(), MAX_L_ENTRIES);
        // The first 10 URLs were evicted; the latest is present.
        assert_eq!(t.resolve("L1"), None);
        let last = format!("L{}", MAX_L_ENTRIES + 10);
        assert!(t.resolve(&last).is_some());
    }
}
