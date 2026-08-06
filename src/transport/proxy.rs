//! Proxy support — the search engine's egress-diversity
//! layer. Residential proxies let each engine see a
//! different IP, each below rate limits.
//!
//! Two protocols: HTTP CONNECT (RFC 7231 §4.3.6) and
//! SOCKS5 (RFC 1928 + RFC 1929 auth). SOCKS5 matters
//! because many residential-proxy providers offer
//! SOCKS5-only lines — and SOCKS5 sends the target host
//! as a domain name so the PROXY resolves DNS, not us
//! (no local DNS leak = stealth-preserving).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::FetchError;

const PROXY_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Socks5,
}

#[derive(Debug, Clone)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub scheme: ProxyScheme,
}

impl Proxy {
    /// Accepts:
    ///   "socks5://user:pass@host:port"
    ///   "http://user:pass@host:port"
    ///   "user:pass@host:port"  (bare = HTTP CONNECT, backward compat)
    ///   "host:port"            (no auth, HTTP CONNECT)
    pub fn parse(s: &str) -> Result<Self, FetchError> {
        let (scheme, rest) = if let Some(r) = s.strip_prefix("socks5://") {
            (ProxyScheme::Socks5, r)
        } else if let Some(r) = s.strip_prefix("socks5h://") {
            (ProxyScheme::Socks5, r) // socks5h = remote DNS (same as our domain ATYP)
        } else if let Some(r) = s.strip_prefix("http://") {
            (ProxyScheme::Http, r)
        } else {
            (ProxyScheme::Http, s)
        };

        // Split auth@addr — auth is optional.
        let (user, pass, addr) = match rest.split_once('@') {
            Some((auth, addr)) => {
                let (u, p) = auth
                    .split_once(':')
                    .ok_or_else(|| FetchError::Http(format!("proxy: bad auth in {s}")))?;
                (u.to_string(), p.to_string(), addr)
            }
            None => (String::new(), String::new(), rest),
        };

        let (host, port) = addr
            .rsplit_once(':')
            .ok_or_else(|| FetchError::Http(format!("proxy: bad addr in {s}")))?;
        // rsplit_once handles IPv6 brackets too: [::1]:1080 → ("[::1]", "1080")
        let port: u16 = port
            .parse()
            .map_err(|_| FetchError::Http(format!("proxy: bad port in {s}")))?;
        // Strip IPv6 brackets if present.
        let host = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(host)
            .to_string();

        Ok(Self {
            host,
            port,
            user,
            pass,
            scheme,
        })
    }

    /// Stable id for pool keys and health tracking.
    pub fn id(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// TCP to the proxy, then tunnel the target through it
    /// via HTTP CONNECT or SOCKS5 depending on scheme.
    pub async fn connect(
        &self,
        target_host: &str,
        target_port: u16,
    ) -> Result<TcpStream, FetchError> {
        let mut stream = tokio::time::timeout(
            PROXY_TIMEOUT,
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        .map_err(|_| FetchError::Timeout)??;
        stream.set_nodelay(true).ok();

        match self.scheme {
            ProxyScheme::Http => {
                self.http_connect(&mut stream, target_host, target_port)
                    .await?
            }
            ProxyScheme::Socks5 => {
                self.socks5_handshake(&mut stream, target_host, target_port)
                    .await?
            }
        }
        Ok(stream)
    }

    // ── HTTP CONNECT (RFC 7231 §4.3.6) ──

    async fn http_connect(
        &self,
        stream: &mut TcpStream,
        target_host: &str,
        target_port: u16,
    ) -> Result<(), FetchError> {
        let req = if self.user.is_empty() {
            format!(
                "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
                 Host: {target_host}:{target_port}\r\n\
                 Proxy-Connection: keep-alive\r\n\r\n"
            )
        } else {
            let auth = base64(&format!("{}:{}", self.user, self.pass));
            format!(
                "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
                 Host: {target_host}:{target_port}\r\n\
                 Proxy-Authorization: Basic {auth}\r\n\
                 Proxy-Connection: keep-alive\r\n\r\n"
            )
        };
        tokio::time::timeout(PROXY_TIMEOUT, stream.write_all(req.as_bytes()))
            .await
            .map_err(|_| FetchError::Timeout)??;

        // Read the response head (until \r\n\r\n).
        let mut buf = Vec::with_capacity(512);
        let mut byte = [0u8; 1];
        let read_head = async {
            while !buf.ends_with(b"\r\n\r\n") {
                if stream.read(&mut byte).await? == 0 {
                    return Err(FetchError::Http("proxy: closed during CONNECT".into()));
                }
                buf.push(byte[0]);
                if buf.len() > 4096 {
                    return Err(FetchError::Http("proxy: huge CONNECT response".into()));
                }
            }
            Ok::<(), FetchError>(())
        };
        tokio::time::timeout(PROXY_TIMEOUT, read_head)
            .await
            .map_err(|_| FetchError::Timeout)??;

        let head = String::from_utf8_lossy(&buf);
        let status: u32 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if status != 200 {
            return Err(FetchError::Http(format!(
                "proxy {} CONNECT -> {status}",
                self.id()
            )));
        }
        Ok(())
    }

    // ── SOCKS5 (RFC 1928 + RFC 1929 auth) ──

    async fn socks5_handshake(
        &self,
        stream: &mut TcpStream,
        target_host: &str,
        target_port: u16,
    ) -> Result<(), FetchError> {
        // Step 1: greeting — offer no-auth (0x00) and if we
        // have credentials, username/password (0x02).
        let has_auth = !self.user.is_empty();
        let methods: &[u8] = if has_auth { &[0x00, 0x02] } else { &[0x00] };
        let greeting = {
            let mut g = vec![0x05, methods.len() as u8];
            g.extend_from_slice(methods);
            g
        };
        tokio::time::timeout(PROXY_TIMEOUT, stream.write_all(&greeting))
            .await
            .map_err(|_| FetchError::Timeout)??;

        // Step 2: server selects a method.
        let mut sel = [0u8; 2];
        tokio::time::timeout(PROXY_TIMEOUT, stream.read_exact(&mut sel))
            .await
            .map_err(|_| FetchError::Timeout)??;
        if sel[0] != 0x05 {
            return Err(FetchError::Http(format!(
                "proxy {} SOCKS5: bad version {}",
                self.id(),
                sel[0]
            )));
        }
        match sel[1] {
            0x00 => {} // no auth needed
            0x02 if has_auth => {
                // RFC 1929: username/password sub-negotiation.
                let user = self.user.as_bytes();
                let pass = self.pass.as_bytes();
                if user.len() > 255 || pass.len() > 255 {
                    return Err(FetchError::Http(format!(
                        "proxy {} SOCKS5: credentials too long",
                        self.id()
                    )));
                }
                let mut auth_req = vec![0x01, user.len() as u8];
                auth_req.extend_from_slice(user);
                auth_req.push(pass.len() as u8);
                auth_req.extend_from_slice(pass);
                tokio::time::timeout(PROXY_TIMEOUT, stream.write_all(&auth_req))
                    .await
                    .map_err(|_| FetchError::Timeout)??;

                let mut auth_resp = [0u8; 2];
                tokio::time::timeout(PROXY_TIMEOUT, stream.read_exact(&mut auth_resp))
                    .await
                    .map_err(|_| FetchError::Timeout)??;
                if auth_resp[1] != 0x00 {
                    return Err(FetchError::Http(format!(
                        "proxy {} SOCKS5: auth failed (status {})",
                        self.id(),
                        auth_resp[1]
                    )));
                }
            }
            0xFF => {
                return Err(FetchError::Http(format!(
                    "proxy {} SOCKS5: no acceptable methods",
                    self.id()
                )));
            }
            other => {
                return Err(FetchError::Http(format!(
                    "proxy {} SOCKS5: unsupported method {:#04x}",
                    self.id(),
                    other
                )));
            }
        }

        // Step 3: CONNECT request. We send the target as a
        // DOMAIN NAME (ATYP 0x03) so the proxy resolves DNS
        // — no local DNS leak, stealth-preserving.
        let host_bytes = target_host.as_bytes();
        if host_bytes.len() > 255 {
            return Err(FetchError::Http(format!(
                "proxy {} SOCKS5: hostname too long",
                self.id()
            )));
        }
        let mut req = vec![
            0x05, // VER
            0x01, // CMD = CONNECT
            0x00, // RSV
            0x03, // ATYP = domain name
        ];
        req.push(host_bytes.len() as u8);
        req.extend_from_slice(host_bytes);
        req.extend_from_slice(&target_port.to_be_bytes());
        tokio::time::timeout(PROXY_TIMEOUT, stream.write_all(&req))
            .await
            .map_err(|_| FetchError::Timeout)??;

        // Step 4: server reply.
        // VER(1) | REP(1) | RSV(1) | ATYP(1) | BND.ADDR(variable) | BND.PORT(2)
        let mut header = [0u8; 4];
        tokio::time::timeout(PROXY_TIMEOUT, stream.read_exact(&mut header))
            .await
            .map_err(|_| FetchError::Timeout)??;
        if header[0] != 0x05 {
            return Err(FetchError::Http(format!(
                "proxy {} SOCKS5: bad reply version {}",
                self.id(),
                header[0]
            )));
        }
        if header[1] != 0x00 {
            // Map RFC 1928 reply codes to readable errors.
            let reason = match header[1] {
                0x01 => "general failure",
                0x02 => "connection not allowed",
                0x03 => "network unreachable",
                0x04 => "host unreachable",
                0x05 => "connection refused",
                0x06 => "TTL expired",
                0x07 => "command not supported",
                0x08 => "address type not supported",
                code => {
                    return Err(FetchError::Http(format!(
                        "proxy {} SOCKS5: reply error {:#04x}",
                        self.id(),
                        code
                    )));
                }
            };
            return Err(FetchError::Http(format!(
                "proxy {} SOCKS5: {reason}",
                self.id()
            )));
        }

        // Skip BND.ADDR + BND.PORT — we don't need the
        // bound address, just consume it so the stream is
        // clean for the caller's TLS handshake.
        let addr_len = match header[3] {
            0x01 => 4, // IPv4
            0x03 => {
                // domain: read 1 length byte, then that many
                let mut len = [0u8; 1];
                tokio::time::timeout(PROXY_TIMEOUT, stream.read_exact(&mut len))
                    .await
                    .map_err(|_| FetchError::Timeout)??;
                len[0] as usize
            }
            0x04 => 16, // IPv6
            other => {
                return Err(FetchError::Http(format!(
                    "proxy {} SOCKS5: bad ATYP {:#04x} in reply",
                    self.id(),
                    other
                )));
            }
        };
        // For domain ATYP we already consumed the length byte
        // above; for IPv4/IPv6 addr_len is the full address.
        let mut discard = vec![0u8; addr_len + 2]; // +2 for BND.PORT
        tokio::time::timeout(PROXY_TIMEOUT, stream.read_exact(&mut discard))
            .await
            .map_err(|_| FetchError::Timeout)??;

        Ok(())
    }
}

fn base64(input: &str) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = input.as_bytes();
    let mut out = String::with_capacity(b.len() * 4 / 3 + 4);
    for chunk in b.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, &c)| acc | ((c as u32) << (16 - 8 * i)));
        for i in 0..4 {
            let shift = 18 - 6 * i;
            let pad = chunk.len() * 8 < shift + 6;
            out.push(if pad {
                '='
            } else {
                T[((n >> shift) & 63) as usize] as char
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bare_http() {
        let p = Proxy::parse("user:pass@1.2.3.4:8080").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Http);
        assert_eq!(p.host, "1.2.3.4");
        assert_eq!(p.port, 8080);
        assert_eq!(p.user, "user");
        assert_eq!(p.pass, "pass");
    }

    #[test]
    fn parse_explicit_http() {
        let p = Proxy::parse("http://u:p@host:3128").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Http);
        assert_eq!(p.host, "host");
        assert_eq!(p.port, 3128);
    }

    #[test]
    fn parse_socks5() {
        let p = Proxy::parse("socks5://u:p@5.6.7.8:1080").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Socks5);
        assert_eq!(p.host, "5.6.7.8");
        assert_eq!(p.port, 1080);
        assert_eq!(p.user, "u");
        assert_eq!(p.pass, "p");
    }

    #[test]
    fn parse_socks5h_alias() {
        let p = Proxy::parse("socks5h://u:p@host:1080").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Socks5);
    }

    #[test]
    fn parse_socks5_no_auth() {
        let p = Proxy::parse("socks5://host:1080").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Socks5);
        assert_eq!(p.user, "");
        assert_eq!(p.pass, "");
    }

    #[test]
    fn parse_http_no_auth() {
        let p = Proxy::parse("host:8080").unwrap();
        assert_eq!(p.scheme, ProxyScheme::Http);
        assert_eq!(p.host, "host");
        assert_eq!(p.user, "");
    }

    #[test]
    fn parse_ipv6_brackets() {
        let p = Proxy::parse("socks5://u:p@[::1]:1080").unwrap();
        assert_eq!(p.host, "::1");
        assert_eq!(p.port, 1080);
    }

    #[test]
    fn id_stable() {
        let p = Proxy::parse("socks5://u:p@host:1080").unwrap();
        assert_eq!(p.id(), "host:1080");
    }

    #[test]
    fn parse_bad() {
        assert!(Proxy::parse("garbage").is_err());
        assert!(Proxy::parse("u:p@bad").is_err());
        assert!(Proxy::parse("u:p@host:99999").is_err());
    }
}
