//! Minimal RFC 6265 cookie jar, scoped per domain/path.
//! Tracks real expiry (Max-Age → expires_at) for the self-
//! improving fetch loop's cookie write-back.

use crate::ghost::cache::CookieRecord;

#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    host_only: bool,
    /// Unix-seconds expiry. None = session cookie.
    expires_at: Option<u64>,
}

#[derive(Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store all Set-Cookie headers from a response for `host`.
    pub fn store_from_headers(&mut self, host: &str, headers: &[(String, String)]) {
        for (n, v) in headers {
            if !n.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let mut parts = v.split(';');
            let Some(pair) = parts.next() else { continue };
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.is_empty() {
                continue;
            }
            // Control characters in name/value can split the
            // Cookie request header later (request splitting).
            // Reject the cookie outright.
            if name.contains(['\r', '\n', '\0']) || value.contains(['\r', '\n', '\0']) {
                continue;
            }
            let mut domain = host.to_string();
            let mut host_only = true;
            let mut path = "/".to_string();
            let mut expired = false;
            let mut expires_at: Option<u64> = None;
            for attr in parts {
                let attr = attr.trim();
                if let Some((k, val)) = attr.split_once('=') {
                    match k.trim().to_ascii_lowercase().as_str() {
                        "domain" => {
                            let d = val.trim().trim_start_matches('.').to_lowercase();
                            // RFC 6265 §5.3 step 6: reject Domain
                            // attributes that are not the request
                            // host or a parent of it — otherwise any
                            // origin can pin cookies on any victim
                            // domain (cookie tossing).
                            if d == host || host.ends_with(&format!(".{d}")) {
                                domain = d;
                                host_only = false;
                            }
                        }
                        "path" => path = val.trim().to_string(),
                        "max-age" => {
                            let secs: i64 = val.trim().parse().unwrap_or(1);
                            if secs <= 0 {
                                expired = true;
                            } else {
                                expires_at = Some(
                                    std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0)
                                        + secs as u64,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Replace any existing cookie with same (name, domain, path).
            self.cookies
                .retain(|c| !(c.name == name && c.domain == domain && c.path == path));
            if !expired {
                self.cookies.push(Cookie {
                    name,
                    value,
                    domain,
                    path,
                    host_only,
                    expires_at,
                });
            }
        }
        self.purge_expired();
    }

    /// Inject a cookie harvested out-of-band (DonGhost
    /// clearance handoff). Leading-dot domains are
    /// subdomain cookies; bare domains are host-only.
    /// `expires_at` carries the real CDP expiry.
    pub fn store_raw(&mut self, name: &str, value: &str, domain: &str, expires_at: Option<u64>) {
        // Same control-character rejection as store_from_headers:
        // CDP-harvested values must never split the Cookie header.
        if name.contains(['\r', '\n', '\0']) || value.contains(['\r', '\n', '\0']) {
            return;
        }
        let (dom, host_only) = if let Some(d) = domain.strip_prefix('.') {
            (d.to_string(), false)
        } else {
            (domain.to_string(), true)
        };
        self.cookies
            .retain(|c| !(c.name == name && c.domain == dom && c.path == "/"));
        self.cookies.push(Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: dom,
            path: "/".into(),
            host_only,
            expires_at,
        });
    }

    /// Export all cookies matching `host` as CookieRecords
    /// for write-back to the persistent domain profile.
    pub fn snapshot_for(&self, host: &str) -> Vec<CookieRecord> {
        let now = now_secs();
        self.cookies
            .iter()
            .filter(|c| c.expires_at.is_none_or(|e| e > now))
            .filter(|c| {
                if c.host_only {
                    host == c.domain
                } else {
                    host == c.domain || host.ends_with(&format!(".{}", c.domain))
                }
            })
            .map(|c| CookieRecord {
                name: c.name.clone(),
                value: c.value.clone(),
                domain: c.domain.clone(),
                expires_at: c.expires_at,
            })
            .collect()
    }

    /// Cookie header value for a request to `host` + `path`, if any match.
    pub fn header_for(&self, host: &str, path: &str) -> Option<String> {
        let now = now_secs();
        let mut pairs: Vec<&Cookie> = Vec::new();
        for c in &self.cookies {
            // Session cookies (no expiry) always match; expired
            // cookies must never be replayed.
            if c.expires_at.is_some_and(|e| e <= now) {
                continue;
            }
            let domain_ok = if c.host_only {
                host == c.domain
            } else {
                host == c.domain || host.ends_with(&format!(".{}", c.domain))
            };
            // RFC 6265 §5.1.4 path-match: exact, or prefix followed
            // by '/' (a /foo cookie must not match /foobar).
            let path_ok = path == c.path
                || (path.starts_with(&c.path)
                    && (c.path.ends_with('/') || path.as_bytes().get(c.path.len()) == Some(&b'/')));
            if domain_ok && path_ok {
                pairs.push(c);
            }
        }
        if pairs.is_empty() {
            return None;
        }
        // Longest path first, per RFC 6265 §5.4.
        pairs.sort_by_key(|c| std::cmp::Reverse(c.path.len()));
        Some(
            pairs
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// Drop cookies whose expiry has passed.
    pub fn purge_expired(&mut self) {
        let now = now_secs();
        self.cookies
            .retain(|c| c.expires_at.is_none_or(|e| e > now));
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
