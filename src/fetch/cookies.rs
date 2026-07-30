//! Minimal RFC 6265 cookie jar, scoped per domain/path.

#[derive(Clone, Debug)]
pub struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    host_only: bool,
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
            let Some((name, value)) = pair.split_once('=') else { continue };
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.is_empty() {
                continue;
            }
            let mut domain = host.to_string();
            let mut host_only = true;
            let mut path = "/".to_string();
            let mut expired = false;
            for attr in parts {
                let attr = attr.trim();
                if let Some((k, val)) = attr.split_once('=') {
                    match k.trim().to_ascii_lowercase().as_str() {
                        "domain" => {
                            domain = val.trim().trim_start_matches('.').to_string();
                            host_only = false;
                        }
                        "path" => path = val.trim().to_string(),
                        "max-age" => {
                            if val.trim().parse::<i64>().unwrap_or(1) <= 0 {
                                expired = true;
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
                self.cookies.push(Cookie { name, value, domain, path, host_only });
            }
        }
    }

    /// Inject a cookie harvested out-of-band (DonGhost
    /// clearance handoff). Leading-dot domains are
    /// subdomain cookies; bare domains are host-only.
    pub fn store_raw(&mut self, name: &str, value: &str, domain: &str) {
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
        });
    }

    /// Cookie header value for a request to `host` + `path`, if any match.
    pub fn header_for(&self, host: &str, path: &str) -> Option<String> {
        let mut pairs: Vec<&Cookie> = Vec::new();
        for c in &self.cookies {
            let domain_ok = if c.host_only {
                host == c.domain
            } else {
                host == c.domain || host.ends_with(&format!(".{}", c.domain))
            };
            let path_ok = path.starts_with(&c.path);
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
}
