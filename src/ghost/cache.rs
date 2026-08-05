//! Disk-backed ghost caches. Two kinds:
//!
//! - **Solved domains**: after a SOLVE, record that a
//!   domain needed tier 2 + the harvested cookies. Next
//!   process start warms tier 1's jar directly — the
//!   ghost never launches until clearance expires.
//! - **Rendered pages**: SPA renders cached with a TTL —
//!   repeat visits skip the browser entirely.
//!
//! Files: ~/.cache/donsetch/ghost-state.json

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub struct GhostState {
    /// host -> solved record.
    #[serde(default)]
    pub solved: HashMap<String, SolvedDomain>,
    /// url -> rendered page cache.
    #[serde(default)]
    pub renders: HashMap<String, RenderCache>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SolvedDomain {
    /// (name, value, domain) triples.
    pub cookies: Vec<(String, String, String)>,
    /// unix seconds when solved.
    pub at: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RenderCache {
    pub html: String,
    pub at: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> PathBuf {
    crate::paths::cache_dir().join("ghost-state.json")
}

/// Clearance typically expires in ~30-60 min. Treat a
/// solve as fresh for 30 min.
const SOLVED_TTL: u64 = 30 * 60;
/// SPA renders stale after 5 min.
const RENDER_TTL: u64 = 5 * 60;

impl GhostState {
    pub fn load() -> Self {
        let p = path();
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(self) {
            let _ = std::fs::write(p, s);
        }
    }

    /// Record a solved domain + its harvested cookies.
    pub fn record_solved(&mut self, host: &str, cookies: &[(String, String, String)]) {
        self.solved.insert(
            host.to_string(),
            SolvedDomain { cookies: cookies.to_vec(), at: now() },
        );
        self.save();
    }

    /// Fresh solve for this host, if any.
    pub fn solved_for(&self, host: &str) -> Option<&SolvedDomain> {
        self.solved.get(host).filter(|s| now() - s.at < SOLVED_TTL)
    }

    /// Cache a rendered page.
    pub fn record_render(&mut self, url: &str, html: &str) {
        self.renders.insert(
            url.to_string(),
            RenderCache { html: html.to_string(), at: now() },
        );
        self.save();
    }

    /// Fresh render for this URL, if any.
    pub fn render_for(&self, url: &str) -> Option<&RenderCache> {
        self.renders.get(url).filter(|r| now() - r.at < RENDER_TTL)
    }
}
