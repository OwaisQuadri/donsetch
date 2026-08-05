//! Fetch orchestrator with temporal stealth: origin pool, TLS session
//! resumption, persistent cookie jar, conditional revalidation cache,
//! Happy Eyeballs, single idempotent retry.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::detect::walls::{self, Verdict};
use crate::error::FetchError;
use crate::memory::DomainMap;
use crate::profile::BrowserProfile;
use crate::transport::pool::Pool;
use crate::transport::{h1, h2::conn::H2Conn, proxy, tcp, tls};

use super::cookies::CookieJar;
use super::decompress;
use super::revalidate::{CacheCheck, RevalidationCache};

const MAX_REDIRECTS: u8 = 10;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheState {
    None,
    /// Served from a fresh cache window, no request was made.
    Fresh,
    /// Server said 304; body merged from cache.
    Revalidated,
}

pub struct FetchOutcome {
    /// Final URL after redirects.
    pub url: String,
    pub status: u16,
    pub alpn: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub redirects: u8,
    pub cache: CacheState,
    /// True when the request rode a pooled (pre-existing) connection.
    pub used_pool: bool,
    pub verdict: Verdict,
    pub elapsed: Duration,
}

pub struct Fetcher {
    profile: BrowserProfile,
    connector: boring::ssl::SslConnector,
    sessions: tls::SessionStore,
    pool: Mutex<Pool>,
    jar: Mutex<CookieJar>,
    cache: Mutex<RevalidationCache>,
    memory: Mutex<DomainMap>,
}

impl Fetcher {
    pub fn new(profile: BrowserProfile) -> Result<Self, FetchError> {
        let sessions = tls::new_session_store();
        let connector = tls::build_connector(&profile, sessions.clone())?;
        Ok(Self {
            profile,
            connector,
            sessions,
            pool: Mutex::new(Pool::new()),
            jar: Mutex::new(CookieJar::new()),
            cache: Mutex::new(RevalidationCache::new()),
            memory: Mutex::new(DomainMap::new()),
        })
    }

    #[allow(dead_code)] // MCP surface will need this.
    pub fn profile(&self) -> &BrowserProfile {
        &self.profile
    }

    /// Import cookies harvested by DonGhost (tier-2
    /// solve) into the persistent jar so the tier-1
    /// re-fetch carries the clearance.
    pub async fn import_cookies(&self, cookies: &[(String, String, String)]) {
        let mut jar = self.jar.lock().unwrap();
        for (name, value, domain) in cookies {
            jar.store_raw(name, value, domain);
        }
    }

    /// Fetch with browser-correct redirects, cookies, cache revalidation.
    pub async fn fetch(&self, url_str: &str) -> Result<FetchOutcome, FetchError> {
        self.fetch_via(url_str, None).await
    }

    /// Fetch through a specific egress lane (proxy). Redirects,
    /// cookies, revalidation all follow the lane — pool keys are
    /// proxy-scoped so egress IPs never share conns.
    pub async fn fetch_via(
        &self,
        url_str: &str,
        proxy: Option<&proxy::Proxy>,
    ) -> Result<FetchOutcome, FetchError> {
        self.fetch_via_jar(url_str, proxy, true).await
    }

    /// Full lane control: `use_jar=false` keeps the shared cookie
    /// jar OUT of the request. Proxy lanes stay unlinked — the
    /// direct lane's session cookies must never transit a third
    /// egress IP.
    pub async fn fetch_via_jar(
        &self,
        url_str: &str,
        proxy: Option<&proxy::Proxy>,
        use_jar: bool,
    ) -> Result<FetchOutcome, FetchError> {
        let started = Instant::now();

        // Fresh-window cache hit: no request at all (browser-true).
        let check = {
            let cache = self.cache.lock().unwrap();
            cache.check(url_str)
        };
        let conditional = match check {
            CacheCheck::Fresh(body, status, headers) => {
                return Ok(FetchOutcome {
                    url: url_str.into(),
                    status,
                    alpn: "cache".into(),
                    headers,
                    body,
                    redirects: 0,
                    cache: CacheState::Fresh,
                    used_pool: false,
                    verdict: Verdict::ContentOk,
                    elapsed: started.elapsed(),
                });
            }
            CacheCheck::Revalidate(cond) => cond,
            CacheCheck::None => Vec::new(),
        };

        let mut current = url_str.to_string();
        let mut redirects = 0u8;

        loop {
            let host = host_of(&current)?;
            let mut out = self
                .fetch_once_via(&current, &conditional, proxy, use_jar)
                .await?;
            {
                let mut jar = self.jar.lock().unwrap();
                jar.store_from_headers(&host, &out.headers);
            }

            // 304: merge body from cache.
            if out.status == 304 {
                if let Some((body, status, headers)) =
                    self.cache.lock().unwrap().stored(&current)
                {
                    out.status = status;
                    out.headers = headers;
                    out.body = body;
                    out.cache = CacheState::Revalidated;
                    out.elapsed = started.elapsed();
                    out.redirects = redirects;
                    return Ok(out);
                }
            }

            match out.status {
                301 | 302 | 303 | 307 | 308 => {
                    redirects += 1;
                    if redirects > MAX_REDIRECTS {
                        return Err(FetchError::TooManyRedirects);
                    }
                    let Some(loc) = header_value(&out.headers, "location") else {
                        out.elapsed = started.elapsed();
                        out.redirects = redirects;
                        return Ok(out);
                    };
                    let base = url::Url::parse(&current)
                        .map_err(|_| FetchError::InvalidUrl(current.clone()))?;
                    let next = base
                        .join(&loc)
                        .map_err(|_| FetchError::Http(format!("bad redirect target: {loc}")))?;
                    if !matches!(next.scheme(), "http" | "https") {
                        // Non-web scheme (file://, ftp://, …):
                        // returned honestly, not followed.
                        out.elapsed = started.elapsed();
                        out.redirects = redirects;
                        return Ok(out);
                    }
                    current = next.to_string();
                }
                _ => {
                    {
                        let mut cache = self.cache.lock().unwrap();
                        cache.store(&current, out.status, &out.headers, &out.body);
                    }
                    out.verdict = walls::detect(out.status, &out.headers, &out.body);

                    // Wall pushed back. If it left a cookie, do ONE
                    // cookie-warm retry (JS-less cookie walls pass on the
                    // second, cookie-carrying request).
                    if let Verdict::Challenge(_) = out.verdict {
                        self.memory
                            .lock()
                            .unwrap()
                            .update(&host, |m| m.challenged = true);
                        if header_value(&out.headers, "set-cookie").is_some() {
                            if let Ok(mut retry) =
                                self.fetch_once_via(&current, &[], proxy, use_jar).await
                            {
                                {
                                    let mut jar = self.jar.lock().unwrap();
                                    jar.store_from_headers(&host, &retry.headers);
                                }
                                retry.verdict =
                                    walls::detect(retry.status, &retry.headers, &retry.body);
                                {
                                    let mut cache = self.cache.lock().unwrap();
                                    cache.store(&current, retry.status, &retry.headers, &retry.body);
                                }
                                if retry.verdict == Verdict::ContentOk {
                                    self.memory.lock().unwrap().update(&host, |m| {
                                        m.warm_retry_worked = true;
                                    });
                                }
                                out = retry;
                            }
                        }
                        if matches!(out.verdict, Verdict::Challenge(_)) {
                            self.memory
                                .lock()
                                .unwrap()
                                .update(&host, |m| m.needs_tier2 = true);
                        }
                    }

                    out.elapsed = started.elapsed();
                    out.redirects = redirects;
                    return Ok(out);
                }
            }
        }
    }

    /// Same, optionally through a CONNECT proxy. Pool keys
    /// are proxy-scoped so egress IPs never share conns.
    /// `use_jar=false` keeps cookies out entirely — search
    /// engines get cookie-less requests so egress lanes
    /// stay unlinked and the fetch-tool jar stays clean.
    pub async fn fetch_once_via(
        &self,
        url_str: &str,
        conditional: &[(String, String)],
        proxy: Option<&proxy::Proxy>,
        use_jar: bool,
    ) -> Result<FetchOutcome, FetchError> {
        let url = url::Url::parse(url_str).map_err(|_| FetchError::InvalidUrl(url_str.into()))?;
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return Err(FetchError::InvalidUrl(url_str.into()));
        }
        let is_https = scheme == "https";
        let host = url.host_str().ok_or_else(|| FetchError::InvalidUrl(url_str.into()))?;
        let default_port = if is_https { 443 } else { 80 };
        let port = url.port().unwrap_or(default_port);
        let mut path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        if path.is_empty() {
            path = "/".into();
        }
        let authority = if port == default_port { host.to_string() } else { format!("{host}:{port}") };
        let origin = match proxy {
            Some(p) => format!("{}|{}", p.id(), authority),
            None => authority.clone(),
        };

        // Header set from profile (Chrome order, coherence) + cookie + conditionals.
        let mut req_headers = self.profile.h1_headers(&authority);
        if use_jar {
            let jar = self.jar.lock().unwrap();
            if let Some(cookie) = jar.header_for(host, &path) {
                let pos = req_headers
                    .iter()
                    .position(|(n, _)| n == "priority")
                    .unwrap_or(req_headers.len());
                req_headers.insert(pos, ("cookie".into(), cookie));
            }
        }
        req_headers.extend(conditional.iter().cloned());

        // 1) Try a pooled h2 connection for this origin.
        let pooled = self.pool.lock().unwrap().take_h2(&origin);
        if let Some(mut conn) = pooled {
            match self.h2_request(&mut conn, &authority, &path, &req_headers, true).await {
                Ok(mut out) => {
                    out.verdict = walls::detect(out.status, &out.headers, &out.body);
                    self.pool.lock().unwrap().put_h2(&origin, conn);
                    return Ok(out);
                }
                Err(_) => { /* conn died; drop it and go fresh */ }
            }
        }

        // 2) Fresh connection, one retry on network failure (Chrome-true).
        let mut last_err = FetchError::Http("unreachable".into());
        for attempt in 0..2 {
            match self
                .fresh_request(is_https, &origin, host, port, &authority, &path, &req_headers, proxy)
                .await
            {
                Ok(mut out) => {
                    out.verdict = walls::detect(out.status, &out.headers, &out.body);
                    return Ok(out);
                }
                Err(e) => {
                    last_err = e;
                    if attempt == 1 {
                        break;
                    }
                }
            }
        }
        Err(last_err)
    }

    async fn fresh_request(
        &self,
        is_https: bool,
        origin: &str,
        host: &str,
        port: u16,
        authority: &str,
        path: &str,
        req_headers: &[(String, String)],
        proxy: Option<&proxy::Proxy>,
    ) -> Result<FetchOutcome, FetchError> {
        let tcp = match proxy {
            Some(p) => p.connect(host, port).await?,
            None => tcp::happy_connect(host, port).await?,
        };

        // ── Plaintext http://: raw TCP straight into h1. ──
        // No h2 over plaintext (no browser does h2c); no TLS,
        // no session resumption, no ALPN.
        if !is_https {
            let mut stream = tcp;
            let resp = tokio::time::timeout(
                RESPONSE_TIMEOUT,
                h1::get(&mut stream, path, req_headers),
            )
            .await
            .map_err(|_| FetchError::Timeout)??;
            return finish(
                url_of("http", authority, path),
                "h1",
                resp.status,
                resp.headers,
                resp.body,
                false,
            );
        }

        let session_key = match proxy {
            Some(p) => format!("{}|{}", p.id(), host),
            None => host.to_string(),
        };
        let mut tls_stream = tokio::time::timeout(
            Duration::from_secs(15),
            tls::connect(&self.profile, &self.connector, host, tcp, &self.sessions, &session_key),
        )
        .await
        .map_err(|_| FetchError::Timeout)??;
        let alpn = tls_stream
            .ssl()
            .selected_alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .unwrap_or_else(|| "none".into());

        if alpn == "h2" {
            let mut conn = H2Conn::start(tls_stream, &self.profile).await?;
            let out = self.h2_request(&mut conn, authority, path, req_headers, false).await?;
            self.pool.lock().unwrap().put_h2(origin, conn);
            Ok(out)
        } else {
            let resp = tokio::time::timeout(
                RESPONSE_TIMEOUT,
                h1::get(&mut tls_stream, path, req_headers),
            )
            .await
            .map_err(|_| FetchError::Timeout)??;
            finish(url_of("https", authority, path), "h1", resp.status, resp.headers, resp.body, false)
        }
    }

    async fn h2_request(
        &self,
        conn: &mut H2Conn,
        authority: &str,
        path: &str,
        req_headers: &[(String, String)],
        used_pool: bool,
    ) -> Result<FetchOutcome, FetchError> {
        let h2_headers: Vec<(String, String)> = req_headers
            .iter()
            .filter(|(n, _)| n != "host" && n != "connection")
            .cloned()
            .collect();
        let resp = tokio::time::timeout(RESPONSE_TIMEOUT, conn.get(authority, path, &h2_headers))
            .await
            .map_err(|_| FetchError::Timeout)??;
        finish(url_of("https", authority, path), "h2", resp.status, resp.headers, resp.body, used_pool)
    }
}

fn url_of(scheme: &str, authority: &str, path: &str) -> String {
    format!("{scheme}://{authority}{path}")
}

fn finish(
    url: String,
    alpn: &str,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    used_pool: bool,
) -> Result<FetchOutcome, FetchError> {
    let encoding = headers
        .iter()
        .find(|(n, _)| n == "content-encoding")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let body = decompress::decompress(&encoding, &body)?;
    Ok(FetchOutcome {
        url,
        status,
        alpn: alpn.into(),
        headers,
        body,
        redirects: 0,
        cache: CacheState::None,
        used_pool,
        verdict: Verdict::ContentOk,
        elapsed: Duration::ZERO,
    })
}

fn host_of(url_str: &str) -> Result<String, FetchError> {
    let url = url::Url::parse(url_str).map_err(|_| FetchError::InvalidUrl(url_str.into()))?;
    url.host_str()
        .map(|h| h.to_string())
        .ok_or_else(|| FetchError::InvalidUrl(url_str.into()))
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}
