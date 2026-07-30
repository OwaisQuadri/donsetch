//! Fetch orchestrator: url → TCP → Chrome-true TLS → ALPN → h2|h1 → response,
//! with browser-correct redirect following and a cookie jar.

use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use super::cookies::CookieJar;
use super::decompress;
use crate::error::FetchError;
use crate::profile::BrowserProfile;
use crate::transport::{h1, h2::conn::H2Conn, tls};

const MAX_REDIRECTS: u8 = 10;

pub struct FetchOutcome {
    /// Final URL after redirects.
    pub url: String,
    pub status: u16,
    pub alpn: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub redirects: u8,
    pub elapsed: Duration,
}

pub struct Fetcher {
    profile: BrowserProfile,
    connector: boring::ssl::SslConnector,
}

impl Fetcher {
    pub fn new(profile: BrowserProfile) -> Result<Self, FetchError> {
        let connector = tls::build_connector(&profile)?;
        Ok(Self { profile, connector })
    }

    #[allow(dead_code)] // MCP surface will need this.
    pub fn profile(&self) -> &BrowserProfile {
        &self.profile
    }

    #[allow(dead_code)]
    fn _api_surface_note(&self) {}

    /// Fetch with browser-correct redirect following (301/302/303/307/308).
    pub async fn fetch(&self, url_str: &str) -> Result<FetchOutcome, FetchError> {
        let started = Instant::now();
        let mut jar = CookieJar::new();
        let mut current = url_str.to_string();
        let mut redirects = 0u8;

        loop {
            let host = host_of(&current)?;
            let mut out = self.fetch_once(&current, &jar).await?;
            jar.store_from_headers(&host, &out.headers);

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
                    if next.scheme() != "https" {
                        // Plain-http downgrade: not yet supported. Return the
                        // redirect response honestly instead of following blind.
                        out.elapsed = started.elapsed();
                        out.redirects = redirects;
                        return Ok(out);
                    }
                    current = next.to_string();
                }
                _ => {
                    out.elapsed = started.elapsed();
                    out.redirects = redirects;
                    return Ok(out);
                }
            }
        }
    }

    async fn fetch_once(
        &self,
        url_str: &str,
        jar: &CookieJar,
    ) -> Result<FetchOutcome, FetchError> {
        let url = url::Url::parse(url_str).map_err(|_| FetchError::InvalidUrl(url_str.into()))?;
        let host = url.host_str().ok_or_else(|| FetchError::InvalidUrl(url_str.into()))?;
        let port = url.port().unwrap_or(443);
        let mut path = match url.query() {
            Some(q) => format!("{}?{q}", url.path()),
            None => url.path().to_string(),
        };
        if path.is_empty() {
            path = "/".into();
        }
        let authority = if port == 443 { host.to_string() } else { format!("{host}:{port}") };

        // DNS + TCP with timeout.
        let addr = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::lookup_host((host, port)),
        )
        .await
        .map_err(|_| FetchError::Timeout)??
        .next()
        .ok_or_else(|| FetchError::Http(format!("dns: no address for {host}")))?;
        let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
            .await
            .map_err(|_| FetchError::Timeout)??;
        tcp.set_nodelay(true).ok();

        // Chrome-true TLS.
        let mut tls_stream = tokio::time::timeout(
            Duration::from_secs(15),
            tls::connect(&self.profile, &self.connector, host, tcp),
        )
        .await
        .map_err(|_| FetchError::Timeout)??;
        let alpn = tls_stream
            .ssl()
            .selected_alpn_protocol()
            .map(|p| String::from_utf8_lossy(p).into_owned())
            .unwrap_or_else(|| "none".into());

        // Header set from profile (Chrome order, coherence), + cookie.
        let mut req_headers = self.profile.h1_headers(&authority);
        if let Some(cookie) = jar.header_for(host, &path) {
            // Insert before "priority" (Chrome puts cookie late in the block).
            let pos = req_headers
                .iter()
                .position(|(n, _)| n == "priority")
                .unwrap_or(req_headers.len());
            req_headers.insert(pos, ("cookie".into(), cookie));
        }

        let (status, headers, body) = if alpn == "h2" {
            let h2_headers: Vec<(String, String)> = req_headers
                .into_iter()
                .filter(|(n, _)| n != "host" && n != "connection")
                .collect();
            let mut conn = H2Conn::start(tls_stream, &self.profile).await?;
            let resp = tokio::time::timeout(
                Duration::from_secs(30),
                conn.get(&authority, &path, &h2_headers),
            )
            .await
            .map_err(|_| FetchError::Timeout)??;
            (resp.status, resp.headers, resp.body)
        } else {
            let resp = tokio::time::timeout(
                Duration::from_secs(30),
                h1::get(&mut tls_stream, &path, &req_headers),
            )
            .await
            .map_err(|_| FetchError::Timeout)??;
            (resp.status, resp.headers, resp.body)
        };

        let encoding = headers
            .iter()
            .find(|(n, _)| n == "content-encoding")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let body = decompress::decompress(&encoding, &body)?;

        Ok(FetchOutcome {
            url: url_str.into(),
            status,
            alpn,
            headers,
            body,
            redirects: 0,
            elapsed: Duration::ZERO,
        })
    }
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
