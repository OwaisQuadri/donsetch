//! Anti-bot bypass fetch (v3.4): when the local stack hits a hard
//! wall, hand the URL to Bright Data's Web Unlocker API, which solves
//! the anti-bot challenge server-side and returns the rendered HTML.

//! This is the paid upgrade path for advanced users. It is strictly
//! opt-in: configure a key via `donsetch keys add unlocker <key>`,
//! optionally with a `::zone` suffix (default zone: `web_unlocker1`).
//! With no key, this module is inert: behavior is identical to
//! previous releases.

//! Billing: Bright Data bills only successful unlocks (standard zone
//! mode). Failures are free. Guardrails: daily cap, hard timeout,
//! and an explicit off switch — so no silent spend.

//! Env:
//!   DONSETCH_BYPASS=0                      disable bypass entirely
//!   DONSETCH_BYPASS_MAX_DAILY=<n>           max unlock calls per day (default 50)
//!   DONSETCH_BYPASS_TIMEOUT_SECS=<n>        per-request timeout (default 90)
//!   DONSETCH_BYPASS_RENDER=1               force JS render via unlocker browser
//!   DONSETCH_UNLOCKER_ZONE=<zone>           default zone when key has no ::zone
//!   DONSETCH_BYPASS_ENDPOINT=<url>          test hook: override API endpoint
//!   DONSETCH_BYPASS_CACHE_TTL_SECS=<n>      solve-cache TTL (default 21600, 0 disables)
//!   DONSETCH_BYPASS_CACHE_MAX_ENTRIES=<n>   solve-cache size cap (default 200)
//!   DONSETCH_BYPASS_CACHE=0                disable the solve-cache

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::search::byok::store::{ByokConfig, KeyState};

pub const DEFAULT_ZONE: &str = "web_unlocker1";
const PROD_ENDPOINT: &str = "https://api.brightdata.com/request";

/// Parsed runtime config. All values come from env at call time.
pub struct BypassConfig {
    pub enabled: bool,
    pub max_daily: u32,
    pub timeout: Duration,
    pub render: bool,
    pub endpoint: String,
    pub cache_ttl: Duration,
    pub cache_max: u32,
}

impl Default for BypassConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_daily: 50,
            timeout: Duration::from_secs(90),
            render: false,
            endpoint: PROD_ENDPOINT.to_string(),
            cache_ttl: Duration::from_secs(21_600),
            cache_max: 200,
        }
    }
}

fn env_bool_off(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "0" | "false" | "off" | "no" | "")
        })
        .unwrap_or(false)
}

impl BypassConfig {
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if env_bool_off("DONSETCH_BYPASS") {
            cfg.enabled = false;
        }
        if let Ok(n) = std::env::var("DONSETCH_BYPASS_MAX_DAILY")
            && let Ok(n) = n.trim().parse::<u32>()
        {
            cfg.max_daily = n.clamp(1, 10_000);
        }
        if let Ok(s) = std::env::var("DONSETCH_BYPASS_TIMEOUT_SECS")
            && let Ok(s) = s.trim().parse::<u64>()
        {
            cfg.timeout = Duration::from_secs(s.clamp(5, 600));
        }
        if std::env::var("DONSETCH_BYPASS_RENDER").is_ok_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            matches!(v.as_str(), "1" | "true" | "on" | "yes")
        }) {
            cfg.render = true;
        }
        if let Ok(e) = std::env::var("DONSETCH_BYPASS_ENDPOINT")
            && !e.trim().is_empty()
        {
            cfg.endpoint = e.trim().to_string();
        }
        if env_bool_off("DONSETCH_BYPASS_CACHE") {
            cfg.cache_ttl = Duration::ZERO;
        }
        if let Ok(n) = std::env::var("DONSETCH_BYPASS_CACHE_TTL_SECS")
            && let Ok(n) = n.trim().parse::<u64>()
        {
            cfg.cache_ttl = Duration::from_secs(n.clamp(0, 31_536_000));
        }
        if let Ok(n) = std::env::var("DONSETCH_BYPASS_CACHE_MAX_ENTRIES")
            && let Ok(n) = n.trim().parse::<u32>()
        {
            cfg.cache_max = n.clamp(1, 100_000);
        }
        cfg
    }
}

/// Split a stored key into (api_token, zone). Zone may be
/// embedded as `token::zone`; otherwise the env default, then the
/// product default, applies.
pub fn parse_key(raw: &str, default_zone: &str) -> (String, String) {
    if let Some((token, zone)) = raw.split_once("::") {
        return (token.to_string(), zone.to_string());
    }
    let zone = std::env::var("DONSETCH_UNLOCKER_ZONE")
        .ok()
        .filter(|z| !z.trim().is_empty())
        .unwrap_or_else(|| default_zone.to_string());
    (raw.to_string(), zone)
}

/// UTC YYYYMMDD, civil-from-days (no date dep).
fn date_ymd() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y2 = if m <= 2 { y + 1 } else { y };
    format!("{y2:04}{m:02}{d:02}")
}

/// Path of the daily counter file (one per UTC day).
pub fn bypass_count_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(format!("bypass-{}.count", date_ymd()))
}

/// Check the daily cap and bump the counter. Returns false when
/// the cap is already exhausted.
pub fn check_and_bump_daily(path: &Path, max: u32) -> bool {
    let count = if path.exists() {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0)
    } else {
        0
    };
    if count >= max {
        return false;
    }
    let _ = std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));
    let _ = std::fs::write(path, (count + 1).to_string());
    true
}

/// Outcome of a successful unlock request.
#[derive(Debug, Clone)]
pub struct BypassOutcome {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    /// True when served from the local solve-cache (no API spend).
    pub cached: bool,
}

/// Failure classified for key-state feedback and call-site messaging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BypassFail {
    /// The API itself rejected the call (auth, billing, rate).
    Api(u16, String),
    /// The API accepted the call but the target did not unlock (e.g.
    /// 403/404 target status, empty body, or unparseable wrapper.
    Target(String),
}

impl BypassFail {
    pub fn key_state(&self) -> Option<KeyState> {
        match self {
            Self::Api(401, _) => Some(KeyState::Invalid),
            Self::Api(403, _) => Some(KeyState::Invalid),
            Self::Api(402, _) => Some(KeyState::CreditDepleted),
            Self::Api(429, _) => Some(KeyState::RateLimited),
            _ => None,
        }
    }
}

/// Parse the unlocker wrapper (format: "json"). Accepts both the
/// `status` and `status_code` field names seen in Bright Data docs.
/// Returns (target_status, content_type, body) on success.
pub fn parse_response(api_status: u16, bytes: &[u8]) -> Result<(u16, String, Vec<u8>), BypassFail> {
    if api_status != 200 {
        let n = bytes.len().min(200);
        return Err(BypassFail::Api(
            api_status,
            String::from_utf8_lossy(&bytes[..n]).to_string(),
        ));
    }
    let v: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(_) => {
            return Err(BypassFail::Target(
                "unlocker returned unparseable JSON".to_string(),
            ));
        }
    };
    let status_num: u64 = match v
        .get("status")
        .or_else(|| v.get("status_code"))
        .and_then(|x| x.as_u64())
    {
        Some(n) => n,
        None => {
            return Err(BypassFail::Target(
                "unlocker response missing status".to_string(),
            ));
        }
    };
    let status: u16 = match u16::try_from(status_num) {
        Ok(n) => n,
        Err(_) => {
            return Err(BypassFail::Target(
                "unlocker status out of range".to_string(),
            ));
        }
    };
    let ct: String = v
        .get("headers")
        .and_then(|h| h.as_object())
        .and_then(|h| {
            h.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                .map(|(_, val)| val.as_str().unwrap_or("").to_string())
        })
        .unwrap_or_else(|| "text/html".to_string());
    let body: Vec<u8> = match v.get("body").and_then(|b| b.as_str()) {
        Some(s) => s.as_bytes().to_vec(),
        None => {
            return Err(BypassFail::Target(
                "unlocker response missing body".to_string(),
            ));
        }
    };
    if !(200..300).contains(&status) {
        return Err(BypassFail::Target(format!(
            "target returned status {status}"
        )));
    }
    if body.is_empty() {
        return Err(BypassFail::Target(
            "unlocker returned an empty body".to_string(),
        ));
    }
    Ok((status, ct, body))
}

/// Find the first active `unlocker` key from the BYOK store.
pub fn active_unlocker_key(cfg: &ByokConfig) -> Option<String> {
    cfg.providers
        .iter()
        .find(|p| p.name == "unlocker")
        .and_then(|p| {
            p.keys
                .iter()
                .find(|k| k.state == KeyState::Active)
                .map(|k| k.key.clone())
        })
}

/// Update the stored key state on API-level failures (billing, auth, rate).
pub fn apply_key_state(provider: &str, key: &str, fail: &BypassFail) {
    let Some(state) = fail.key_state() else {
        return;
    };
    let mut cfg = ByokConfig::load();
    cfg.update_key_state(provider, key, state);
    cfg.save();
}

// ── Solve-cache: never pay twice for the same page ───────────
//
// A successful unlock is stored under `bypass-cache/<sha256(url)>.json`.
// Repeated fetches of the same URL are served from cache for the TTL
// window at zero API cost. Sliding TTL: every hit rewrites the
// timestamp, so hot URLs stay alive and cold ones expire. Oldest-entry
// pruning caps disk growth. In-flight locking coalesces parallel
// requests for the same URL into one paid call.

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn bypass_cache_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("bypass-cache")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CacheEntry {
    v: u32,
    url: String,
    ts: u64,
    status: u16,
    content_type: String,
    body: String,
}

fn cache_get(cache_dir: &Path, url: &str, ttl_secs: u64) -> Option<BypassOutcome> {
    if ttl_secs == 0 {
        return None;
    }
    let path = bypass_cache_dir(cache_dir).join(format!("{}.json", cache_key(url)));
    let raw = std::fs::read_to_string(&path).ok()?;
    let entry: CacheEntry = serde_json::from_str(&raw).ok()?;
    if entry.v != 1 || entry.url != url {
        return None;
    }
    if now_ts().saturating_sub(entry.ts) > ttl_secs {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(BypassOutcome {
        status: entry.status,
        content_type: entry.content_type,
        body: entry.body.into_bytes(),
        cached: true,
    })
}

/// Refresh the timestamp on a hit (sliding TTL: hot entries survive).
fn cache_touch(cache_dir: &Path, url: &str) {
    let path = bypass_cache_dir(cache_dir).join(format!("{}.json", cache_key(url)));
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(mut entry) = serde_json::from_str::<CacheEntry>(&raw) else {
        return;
    };
    entry.ts = now_ts();
    if let Ok(json) = serde_json::to_string(&entry) {
        let _ = std::fs::write(&path, json);
    }
}

fn cache_put(cache_dir: &Path, url: &str, outcome: &BypassOutcome, max_entries: u32) {
    let dir = bypass_cache_dir(cache_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let key = cache_key(url);
    let entry = CacheEntry {
        v: 1,
        url: url.to_string(),
        ts: now_ts(),
        status: outcome.status,
        content_type: outcome.content_type.clone(),
        body: String::from_utf8_lossy(&outcome.body).to_string(),
    };
    let path = dir.join(format!("{key}.json"));
    let tmp = dir.join(format!("{key}.tmp"));
    if std::fs::write(&tmp, serde_json::to_string(&entry).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
    cache_prune(&dir, max_entries);
}

/// Oldest-first eviction until the entry count is within max.
fn cache_prune(dir: &Path, max_entries: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut metas: Vec<(u64, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let ts = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| serde_json::from_str::<CacheEntry>(&s).ok())
            .map(|c| c.ts)
            .unwrap_or(0);
        metas.push((ts, p));
    }
    if metas.len() <= max_entries as usize {
        return;
    }
    metas.sort_by_key(|(ts, _)| *ts);
    let drop = metas.len() - max_entries as usize;
    for (_, p) in metas.iter().take(drop) {
        let _ = std::fs::remove_file(p);
    }
}

/// One gate per URL: parallel fetches of the same URL share one
/// paid unlock.
fn in_flight_lock(url: &str) -> Arc<tokio::sync::Mutex<()>> {
    static MAP: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let map = MAP.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|p| p.into_inner());
    guard
        .entry(cache_key(url))
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Perform one unlock request. Cache-first: a fresh solve-cache hit
/// costs nothing and does not consume the daily cap. Parallel
/// requests for the same URL coalesce into one paid call. Pure
/// network + IO; the MCP layer composes the value (extraction,
/// envelopes) from the outcome.
pub async fn unlock(
    key: &str,
    url: &str,
    cfg: &BypassConfig,
    cache_dir: &Path,
) -> Result<BypassOutcome, BypassFail> {
    let ttl = cfg.cache_ttl.as_secs();
    if let Some(outcome) = cache_get(cache_dir, url, ttl) {
        cache_touch(cache_dir, url);
        return Ok(outcome);
    }
    let gate = in_flight_lock(url);
    let _guard = gate.lock().await;
    if let Some(outcome) = cache_get(cache_dir, url, ttl) {
        cache_touch(cache_dir, url);
        return Ok(outcome);
    }
    let count_path = bypass_count_path(cache_dir);
    if !check_and_bump_daily(&count_path, cfg.max_daily) {
        return Err(BypassFail::Target(
            "daily unlock cap reached; raise DONSETCH_BYPASS_MAX_DAILY or try tomorrow".to_string(),
        ));
    }
    let (token, zone) = parse_key(key, DEFAULT_ZONE);
    let client = reqwest::Client::builder()
        .timeout(cfg.timeout)
        .no_gzip()
        .no_deflate()
        .build()
        .map_err(|e| BypassFail::Target(format!("bypass client init failed ({e})")))?;
    let mut payload = serde_json::json!({
        "zone": zone,
        "url": url,
        "format": "json",
    });
    if cfg.render {
        payload["render"] = serde_json::json!(true);
    }
    let resp = client
        .post(&cfg.endpoint)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|e| BypassFail::Target(format!("bypass request failed ({e})")))?;
    let api_status = resp.status().as_u16();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| BypassFail::Target(format!("bypass response truncated ({e})")))?;
    let outcome =
        parse_response(api_status, &bytes).map(|(status, content_type, body)| BypassOutcome {
            status,
            content_type,
            body,
            cached: false,
        })?;
    if ttl > 0 {
        cache_put(cache_dir, url, &outcome, cfg.cache_max);
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::byok::store::{KeyEntry, ProviderConfig};

    #[test]
    fn parse_key_token_only() {
        let (token, zone) = parse_key("abc123", "web_unlocker1");
        assert_eq!(token, "abc123");
        assert_eq!(zone, "web_unlocker1");
    }

    #[test]
    fn parse_key_embedded_zone() {
        let (token, zone) = parse_key("abc123::custom_zone", "web_unlocker1");
        assert_eq!(token, "abc123");
        assert_eq!(zone, "custom_zone");
    }

    #[test]
    fn daily_cap_allows_until_exhausted() {
        let dir = std::env::temp_dir().join(format!("donsetch-bypass-test-{}", std::process::id()));
        let path = dir.join("bypass-test.count");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(check_and_bump_daily(&path, 3));
        assert!(check_and_bump_daily(&path, 3));
        assert!(check_and_bump_daily(&path, 3));
        assert!(!check_and_bump_daily(&path, 3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_response_ok_shape() {
        let resp = br#"{"status":200,"headers":{"content-type":"text/html; charset=utf-8"},"body":"<html>hi</html>"}"#;
        let (status, ct, body) = parse_response(200, resp).unwrap();
        assert_eq!(status, 200);
        assert_eq!(ct, "text/html; charset=utf-8");
        assert_eq!(body, b"<html>hi</html>");
    }

    #[test]
    fn parse_response_accepts_status_code_field() {
        let resp = br#"{"status_code":202,"headers":{},"body":"ok"}"#;
        let (status, _, body) = parse_response(200, resp).unwrap();
        assert_eq!(status, 202);
        assert_eq!(body, b"ok");
    }

    #[test]
    fn parse_response_api_error_maps_state() {
        let err = parse_response(401, b"unauthorized").unwrap_err();
        assert_eq!(err.key_state(), Some(KeyState::Invalid));
        let err = parse_response(402, b"no credit").unwrap_err();
        assert_eq!(err.key_state(), Some(KeyState::CreditDepleted));
        let err = parse_response(429, b"slow down").unwrap_err();
        assert_eq!(err.key_state(), Some(KeyState::RateLimited));
    }

    #[test]
    fn parse_response_target_error() {
        let resp = br#"{"status":403,"headers":{},"body":"forbidden"}"#;
        let err = parse_response(200, resp).unwrap_err();
        assert_eq!(
            err,
            BypassFail::Target("target returned status 403".to_string())
        );
        assert_eq!(err.key_state(), None);
    }

    #[test]
    fn active_unlocker_key_picks_active_only() {
        let cfg = ByokConfig {
            default: String::new(),
            providers: vec![ProviderConfig {
                name: "unlocker".into(),
                keys: vec![
                    KeyEntry {
                        key: "bad".into(),
                        state: KeyState::Invalid,
                        ts: 0,
                    },
                    KeyEntry {
                        key: "good".into(),
                        state: KeyState::Active,
                        ts: 0,
                    },
                ],
            }],
        };
        assert_eq!(active_unlocker_key(&cfg), Some("good".to_string()));
    }

    #[test]
    fn cache_roundtrip_hit_ttl_and_miss() {
        let dir =
            std::env::temp_dir().join(format!("donsetch-bypass-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let url = "https://example.com/x";
        let outcome = BypassOutcome {
            status: 200,
            content_type: "text/html".into(),
            body: b"<html>solved</html>".to_vec(),
            cached: false,
        };
        cache_put(&dir, url, &outcome, 10);
        let got = cache_get(&dir, url, 3600).unwrap();
        assert_eq!(got.body, outcome.body);
        assert_eq!(got.status, 200);
        assert!(got.cached);
        // ttl 0 = cache disabled, never hits
        assert!(cache_get(&dir, url, 0).is_none());
        // different URL misses
        assert!(cache_get(&dir, "https://example.com/y", 3600).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_prune_evicts_oldest() {
        let dir =
            std::env::temp_dir().join(format!("donsetch-bypass-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for i in 0..4 {
            let url = format!("https://example.com/p{i}");
            let outcome = BypassOutcome {
                status: 200,
                content_type: "text/html".into(),
                body: format!("body{i}").into_bytes(),
                cached: false,
            };
            cache_put(&dir, &url, &outcome, 2);
        }
        let dir2 = bypass_cache_dir(&dir);
        let n = std::fs::read_dir(&dir2)
            .unwrap()
            .flatten()
            .filter(|e| {
                let p = e.path();
                p.extension().and_then(|x| x.to_str()) == Some("json")
            })
            .count();
        assert_eq!(n, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn inflight_lock_shares_gate_per_url() {
        let a = in_flight_lock("https://example.com/z");
        let b = in_flight_lock("https://example.com/z");
        let c = in_flight_lock("https://example.com/w");
        assert!(Arc::ptr_eq(&a, &b));
        assert!(!Arc::ptr_eq(&a, &c));
    }
}
