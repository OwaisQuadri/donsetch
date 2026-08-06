//! Disk-backed self-improving fetch intelligence.
//!
//! Every fetch is both an action AND an observation. This store
//! learns from each outcome and routes the next fetch more
//! efficiently. The more you use DonSeTch, the less it escalates
//! to tier 2 — domains it has already solved get warm tier-1
//! with their clearance cookies injected, and cookies are kept
//! alive by write-back from successful warm fetches.
//!
//! Two persistent stores:
//!
//! - **Domain profiles**: per-host routing intelligence + cookie
//!   vault. `route_for(host)` decides cold / warm / skip-to-solve
//!   / recheck-cold. `record_*` methods observe outcomes.
//! - **Rendered pages**: SPA renders cached with a TTL.
//!
//! File: ~/.cache/donsetch/ghost-state.json (atomic writes).

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ────────────────────────── types ──────────────────────────

/// A cookie with its server-set expiry. `None` = session cookie
/// (no server-declared expiry; the adaptive-TTL layer fills in).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CookieRecord {
    pub name: String,
    pub value: String,
    pub domain: String,
    #[serde(default)]
    pub expires_at: Option<u64>, // unix seconds; None = session
}

/// Per-domain intelligence. Evolves with every fetch outcome.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DomainProfile {
    // === Cookie vault ===
    #[serde(default)]
    pub cookies: Vec<CookieRecord>,
    /// When tier 2 last solved (unix seconds).
    #[serde(default)]
    pub last_solved: u64,
    /// When tier 1 last refreshed cookies via Set-Cookie write-back.
    #[serde(default)]
    pub last_refreshed: u64,

    // === Routing intelligence ===
    #[serde(default)]
    pub fetch_count: u32,
    #[serde(default)]
    pub walled_count: u32,
    #[serde(default)]
    pub warm_ok_count: u32,
    #[serde(default)]
    pub warm_fail_count: u32,
    #[serde(default)]
    pub solve_count: u32,

    // === Wall signature ===
    #[serde(default)]
    pub wall_vendor: Option<String>,
    /// Known to need tier 2 (seen a challenge here).
    #[serde(default)]
    pub needs_tier2: bool,
    /// Last time we tried tier 1 cold here (for wall-removal recheck).
    #[serde(default)]
    pub last_cold_check: u64,

    // === Adaptive TTL ===
    /// Shortest observed cookie lifetime (learned from warm-stale).
    /// When cookies die before their stated expiry, the system
    /// learns the real lifetime and re-solves proactively.
    #[serde(default)]
    pub observed_lifetime: Option<u64>,
}

/// How to route a fetch to this host.
#[derive(Debug)]
pub enum RouteDecision {
    /// No profile (first visit) or easy domain. Tier 1 cold.
    Cold,
    /// Known to need tier 2, cookies still fresh — inject them.
    Warm(Vec<CookieRecord>),
    /// Known to need tier 2, cookies stale, cold-check recent.
    /// Skip the doomed tier-1 round-trip — go straight to solve.
    SkipToSolve,
    /// Known to need tier 2, but hasn't been cold-checked in a
    /// while — try tier 1 cold. The wall may have been removed.
    RecheckCold,
}

#[derive(Default, Serialize, Deserialize)]
pub struct GhostState {
    #[serde(default)]
    pub profiles: HashMap<String, DomainProfile>,
    #[serde(default)]
    pub renders: HashMap<String, RenderCache>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct RenderCache {
    pub html: String,
    pub at: u64,
}

// ────────────────────────── constants ──────────────────────────

/// Safety cap: never trust a solve older than this, even if the
/// cookie's stated expiry is further out. Covers server-side
/// invalidation (IP change, session revoke).
const TTL_CAP: u64 = 2 * 60 * 60; // 2 hours

/// If a domain is known to need tier 2, periodically try tier 1
/// cold anyway — the wall may have been removed.
const RECHECK_INTERVAL: u64 = 24 * 60 * 60; // 24 hours

/// SPA renders stale after 5 min.
const RENDER_TTL: u64 = 5 * 60;

// ────────────────────────── helpers ──────────────────────────

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path() -> PathBuf {
    crate::paths::cache_dir().join("ghost-state.json")
}

/// Are the stored cookies still fresh at time `now`?
/// Testable: takes `now` as a parameter.
pub fn cookies_fresh_at(profile: &DomainProfile, now: u64) -> bool {
    if profile.cookies.is_empty() {
        return false;
    }

    // Server-set expiry: the earliest-expiring cookie is the
    // weakest link — if it's past, the batch is stale.
    if let Some(exp) = profile
        .cookies
        .iter()
        .filter_map(|c| c.expires_at)
        .min()
    {
        if now >= exp {
            return false;
        }
    }

    // Observed lifetime: if cookies died before their stated
    // expiry in the past, trust the observation over the server.
    if let Some(observed) = profile.observed_lifetime {
        if now - profile.last_solved >= observed {
            return false;
        }
    }

    // Safety cap.
    if now - profile.last_solved >= TTL_CAP {
        return false;
    }

    true
}

// ────────────────────────── GhostState impl ──────────────────────────

impl GhostState {
    pub fn load() -> Self {
        let p = path();
        if let Ok(s) = std::fs::read_to_string(&p) {
            // Try new format.
            if let Ok(state) = serde_json::from_str::<Self>(&s) {
                return state;
            }
            // Try legacy format and migrate.
            if let Ok(old) = serde_json::from_str::<LegacyState>(&s) {
                let mut state = Self::default();
                for (host, solved) in old.solved {
                    state.profiles.insert(
                        host,
                        DomainProfile {
                            cookies: solved
                                .cookies
                                .into_iter()
                                .map(|(n, v, d)| CookieRecord {
                                    name: n,
                                    value: v,
                                    domain: d,
                                    expires_at: None,
                                })
                                .collect(),
                            last_solved: solved.at,
                            needs_tier2: true,
                            ..Default::default()
                        },
                    );
                }
                state.renders = old.renders;
                state.save();
                return state;
            }
        }
        Self::default()
    }

    /// Atomic save: write to temp, rename. Survives crashes.
    /// No-op in test builds — tests exercise the pure decision
    /// and freshness logic without disk side effects.
    pub fn save(&self) {
        #[cfg(not(test))]
        {
            let p = path();
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(s) = serde_json::to_string(self) {
                let tmp = p.with_extension("json.tmp");
                if std::fs::write(&tmp, &s).is_ok() {
                    let _ = std::fs::rename(&tmp, &p);
                }
            }
        }
    }

    // ── Decision ──

    /// Route decision for the next fetch to this host.
    pub fn route_for(&self, host: &str) -> RouteDecision {
        let Some(profile) = self.profiles.get(host) else {
            return RouteDecision::Cold;
        };
        let n = now();
        if profile.needs_tier2 {
            if cookies_fresh_at(profile, n) {
                return RouteDecision::Warm(profile.cookies.clone());
            }
            // Cookies stale. Should we recheck cold?
            if n - profile.last_cold_check > RECHECK_INTERVAL {
                return RouteDecision::RecheckCold;
            }
            // Skip the doomed tier-1 attempt — go straight to solve.
            return RouteDecision::SkipToSolve;
        }
        // Easy domain — tier 1 cold.
        RouteDecision::Cold
    }

    // ── Observation ──

    /// Tier 1 cold succeeded. If the domain was previously known
    /// to need tier 2, the wall is gone — clear the flag.
    pub fn record_cold_ok(&mut self, host: &str) {
        let n = now();
        let p = self.profiles.entry(host.to_string()).or_default();
        p.fetch_count += 1;
        p.last_cold_check = n;
        if p.needs_tier2 {
            p.needs_tier2 = false;
            p.wall_vendor = None;
            // Cookies from a previous solve are stale context — clear.
            p.cookies.clear();
            p.observed_lifetime = None;
        }
        self.save();
    }

    /// Tier 1 cold was walled — domain needs tier 2.
    pub fn record_cold_walled(&mut self, host: &str, vendor: Option<&str>) {
        let n = now();
        let p = self.profiles.entry(host.to_string()).or_default();
        p.fetch_count += 1;
        p.walled_count += 1;
        p.needs_tier2 = true;
        p.last_cold_check = n;
        if let Some(v) = vendor {
            p.wall_vendor = Some(v.to_string());
        }
        self.save();
    }

    /// Tier 1 warm succeeded — cookies are still valid. Refresh
    /// the cookie vault from the response's Set-Cookie headers
    /// so the on-disk cookies stay as fresh as the server's latest
    /// response. This is the write-back that keeps sessions alive
    /// across restarts, just like a real browser tab.
    pub fn record_warm_ok(&mut self, host: &str, refreshed: &[CookieRecord]) {
        let n = now();
        let p = self.profiles.entry(host.to_string()).or_default();
        p.fetch_count += 1;
        p.warm_ok_count += 1;
        p.last_refreshed = n;
        // Merge: replace by (name, domain), add new ones.
        for new in refreshed {
            if let Some(existing) = p
                .cookies
                .iter_mut()
                .find(|c| c.name == new.name && c.domain == new.domain)
            {
                *existing = new.clone();
            } else {
                p.cookies.push(new.clone());
            }
        }
        self.save();
    }

    /// Tier 1 warm was walled — cookies went stale. Learn the
    /// real lifetime: it's at most `now - last_solved`. Next
    /// time, trust the observation over the server's claim and
    /// re-solve before the cookies expire.
    pub fn record_warm_stale(&mut self, host: &str) {
        let n = now();
        let p = self.profiles.entry(host.to_string()).or_default();
        p.fetch_count += 1;
        p.warm_fail_count += 1;
        let elapsed = n.saturating_sub(p.last_solved);
        p.observed_lifetime = Some(match p.observed_lifetime {
            Some(prev) => prev.min(elapsed),
            None => elapsed,
        });
        // Cookies are dead — clear so route_for doesn't serve them.
        p.cookies.clear();
        p.last_refreshed = 0;
        self.save();
    }

    /// Tier 2 solved the wall — store fresh cookies with real
    /// expiry captured from CDP.
    pub fn record_solved(
        &mut self,
        host: &str,
        cookies: &[CookieRecord],
        vendor: Option<&str>,
    ) {
        let n = now();
        let p = self.profiles.entry(host.to_string()).or_default();
        p.cookies = cookies.to_vec();
        p.last_solved = n;
        p.last_refreshed = n;
        p.solve_count += 1;
        p.needs_tier2 = true;
        if let Some(v) = vendor {
            p.wall_vendor = Some(v.to_string());
        }
        self.save();
    }

    // ── Render cache (unchanged from v1) ──

    pub fn record_render(&mut self, url: &str, html: &str) {
        self.renders.insert(
            url.to_string(),
            RenderCache {
                html: html.to_string(),
                at: now(),
            },
        );
        self.save();
    }

    pub fn render_for(&self, url: &str) -> Option<&RenderCache> {
        self.renders
            .get(url)
            .filter(|r| now() - r.at < RENDER_TTL)
    }
}

// ────────────────────────── legacy migration ──────────────────────────

#[derive(Deserialize)]
struct LegacyState {
    #[serde(default)]
    solved: HashMap<String, LegacySolved>,
    #[serde(default)]
    renders: HashMap<String, RenderCache>,
}

#[derive(Deserialize)]
struct LegacySolved {
    cookies: Vec<(String, String, String)>,
    at: u64,
}

// ────────────────────────── tests ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cr(name: &str, value: &str, domain: &str, exp: Option<u64>) -> CookieRecord {
        CookieRecord {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            expires_at: exp,
        }
    }

    // ── cookies_fresh_at ──

    #[test]
    fn fresh_no_cookies() {
        let p = DomainProfile::default();
        assert!(!cookies_fresh_at(&p, 1000));
    }

    #[test]
    fn fresh_server_expiry_ok() {
        let p = DomainProfile {
            cookies: vec![cr("cf", "x", ".a.com", Some(2000))],
            last_solved: 1000,
            ..Default::default()
        };
        assert!(cookies_fresh_at(&p, 1500));
    }

    #[test]
    fn fresh_server_expiry_past() {
        let p = DomainProfile {
            cookies: vec![cr("cf", "x", ".a.com", Some(1500))],
            last_solved: 1000,
            ..Default::default()
        };
        assert!(!cookies_fresh_at(&p, 1500));
    }

    #[test]
    fn fresh_earliest_expires_wins() {
        let p = DomainProfile {
            cookies: vec![
                cr("a", "x", ".c.com", Some(5000)),
                cr("b", "y", ".c.com", Some(1200)),
            ],
            last_solved: 1000,
            ..Default::default()
        };
        assert!(!cookies_fresh_at(&p, 1200));
        assert!(cookies_fresh_at(&p, 1199));
    }

    #[test]
    fn fresh_observed_lifetime_shorter() {
        // Server says 1h, but we learned cookies die at 300s.
        let p = DomainProfile {
            cookies: vec![cr("cf", "x", ".a.com", Some(100000))],
            last_solved: 1000,
            observed_lifetime: Some(300),
            ..Default::default()
        };
        assert!(cookies_fresh_at(&p, 1299));
        assert!(!cookies_fresh_at(&p, 1300));
    }

    #[test]
    fn fresh_ttl_cap() {
        // No server expiry, no observed lifetime — cap at 2h.
        let p = DomainProfile {
            cookies: vec![cr("s", "x", ".a.com", None)],
            last_solved: 1000,
            ..Default::default()
        };
        assert!(cookies_fresh_at(&p, 1000 + 2 * 3600 - 1));
        assert!(!cookies_fresh_at(&p, 1000 + 2 * 3600));
    }

    #[test]
    fn fresh_session_cookie_no_expiry() {
        // Session cookie (None): relies on observed_lifetime or TTL_CAP.
        let p = DomainProfile {
            cookies: vec![cr("s", "x", ".a.com", None)],
            last_solved: 1000,
            ..Default::default()
        };
        assert!(cookies_fresh_at(&p, 1000));
    }

    // ── route_for ──

    #[test]
    fn route_unknown_domain_is_cold() {
        let s = GhostState::default();
        assert!(matches!(s.route_for("new.com"), RouteDecision::Cold));
    }

    #[test]
    fn route_easy_domain_is_cold() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "easy.com".into(),
            DomainProfile {
                needs_tier2: false,
                ..Default::default()
            },
        );
        assert!(matches!(s.route_for("easy.com"), RouteDecision::Cold));
    }

    #[test]
    fn route_hard_fresh_is_warm() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "hard.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("cf", "x", ".hard.com", Some(now() + 3600))],
                last_solved: now(),
                ..Default::default()
            },
        );
        assert!(matches!(s.route_for("hard.com"), RouteDecision::Warm(_)));
    }

    #[test]
    fn route_hard_stale_recent_is_skip() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "hard.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("cf", "x", ".hard.com", Some(100))],
                last_solved: 100,
                last_cold_check: now() - 100, // recent
                ..Default::default()
            },
        );
        assert!(matches!(s.route_for("hard.com"), RouteDecision::SkipToSolve));
    }

    #[test]
    fn route_hard_stale_old_cold_check_is_recheck() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "hard.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("cf", "x", ".hard.com", Some(100))],
                last_solved: 100,
                last_cold_check: 1, // very old — > RECHECK_INTERVAL
                ..Default::default()
            },
        );
        assert!(matches!(s.route_for("hard.com"), RouteDecision::RecheckCold));
    }

    // ── observation: convergence ──

    #[test]
    fn cold_ok_clears_needs_tier2() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "walled.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("cf", "x", ".walled.com", Some(9999))],
                last_solved: 100,
                ..Default::default()
            },
        );
        s.record_cold_ok("walled.com");
        let p = &s.profiles["walled.com"];
        assert!(!p.needs_tier2);
        assert!(p.cookies.is_empty()); // stale context cleared
        assert!(p.observed_lifetime.is_none());
    }

    #[test]
    fn warm_stale_learns_lifetime() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "hard.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("cf", "x", ".hard.com", Some(9999))],
                last_solved: 1000,
                ..Default::default()
            },
        );
        // Simulate: warm fetch at t=1300 was walled.
        // We can't call record_warm_stale with a custom now,
        // but we can verify the logic by setting last_solved.
        // The method uses now() internally — test structurally:
        // after record_warm_stale, cookies should be cleared
        // and observed_lifetime should be Some.
        s.record_warm_stale("hard.com");
        let p = &s.profiles["hard.com"];
        assert!(p.cookies.is_empty());
        assert!(p.observed_lifetime.is_some());
        assert_eq!(p.warm_fail_count, 1);
    }

    #[test]
    fn warm_ok_refreshes_cookies() {
        let mut s = GhostState::default();
        s.profiles.insert(
            "hard.com".into(),
            DomainProfile {
                needs_tier2: true,
                cookies: vec![cr("old", "old_val", ".hard.com", Some(9999))],
                last_solved: 1000,
                ..Default::default()
            },
        );
        // Simulate: server sent a refreshed cookie.
        let refreshed = vec![
            cr("old", "new_val", ".hard.com", Some(9999)),
            cr("new", "new_cookie", ".hard.com", Some(9999)),
        ];
        s.record_warm_ok("hard.com", &refreshed);
        let p = &s.profiles["hard.com"];
        assert_eq!(p.cookies.len(), 2);
        assert_eq!(p.warm_ok_count, 1);
        // Old cookie value was replaced.
        assert_eq!(p.cookies[0].value, "new_val");
        // New cookie was added.
        assert!(p.cookies.iter().any(|c| c.name == "new"));
    }

    #[test]
    fn solved_stores_cookies_and_vendor() {
        let mut s = GhostState::default();
        let cookies = vec![
            cr("cf_clearance", "tok", ".hard.com", Some(now() + 3600)),
        ];
        s.record_solved("hard.com", &cookies, Some("cloudflare"));
        let p = &s.profiles["hard.com"];
        assert!(p.needs_tier2);
        assert_eq!(p.solve_count, 1);
        assert_eq!(p.wall_vendor.as_deref(), Some("cloudflare"));
        assert_eq!(p.cookies.len(), 1);
        assert!(!p.cookies.is_empty());
    }

    // ── convergence simulation ──

    #[test]
    fn convergence_lifecycle() {
        // Simulate the full lifecycle of a domain through the loop.
        let mut s = GhostState::default();
        let host = "cf-protected.com";
        let now = now();

        // Visit 1: unknown → cold → walled → solve
        assert!(matches!(s.route_for(host), RouteDecision::Cold));
        s.record_cold_walled(host, Some("cloudflare"));
        let cookies = vec![cr("cf_clearance", "tok1", ".cf-protected.com", Some(now + 3600))];
        s.record_solved(host, &cookies, Some("cloudflare"));

        // Visit 2: hard + fresh → warm
        match s.route_for(host) {
            RouteDecision::Warm(c) => assert_eq!(c.len(), 1),
            other => panic!("expected Warm, got {other:?}"),
        }

        // Visit 2 outcome: warm ok → cookies refreshed
        let refreshed = vec![cr("cf_clearance", "tok2", ".cf-protected.com", Some(now + 7200))];
        s.record_warm_ok(host, &refreshed);

        // Visit 3: still warm (cookies refreshed, still fresh)
        match s.route_for(host) {
            RouteDecision::Warm(c) => {
                assert_eq!(c[0].value, "tok2"); // write-back worked
            }
            other => panic!("expected Warm after refresh, got {other:?}"),
        }

        // Verify the domain profile converged
        let p = &s.profiles[host];
        assert_eq!(p.warm_ok_count, 1);
        assert_eq!(p.solve_count, 1);
        assert_eq!(p.walled_count, 1);
        assert!(p.needs_tier2);
    }

    // ── legacy migration ──

    #[test]
    fn legacy_migration() {
        // Verify LegacyState deserialization — the production
        // load() uses this to migrate old state files.
        let legacy_json = serde_json::json!({
            "solved": {
                "old.com": {
                    "cookies": [["cf", "val", ".old.com"]],
                    "at": 1000u64
                }
            },
            "renders": {}
        });
        let old: LegacyState =
            serde_json::from_str(&legacy_json.to_string()).unwrap();
        assert_eq!(old.solved.len(), 1);
        assert_eq!(old.solved["old.com"].cookies[0].0, "cf");
    }

    // ── save is a no-op in test builds (see save() impl) ──
}
