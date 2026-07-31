//! HTTP CONNECT proxy support — the search engine's
//! egress-diversity layer. Residential proxies let each
//! engine see a different IP, each below rate limits.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::FetchError;

const PROXY_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug, Clone)]
pub struct Proxy {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
}

impl Proxy {
    /// "user:pass@host:port" compact form.
    pub fn parse(s: &str) -> Result<Self, FetchError> {
        let (auth, addr) = s
            .split_once('@')
            .ok_or_else(|| FetchError::Http(format!("proxy: bad form {s}")))?;
        let (user, pass) = auth
            .split_once(':')
            .ok_or_else(|| FetchError::Http(format!("proxy: bad auth {s}")))?;
        let (host, port) = addr
            .split_once(':')
            .ok_or_else(|| FetchError::Http(format!("proxy: bad addr {s}")))?;
        let port: u16 = port
            .parse()
            .map_err(|_| FetchError::Http(format!("proxy: bad port {s}")))?;
        Ok(Self {
            host: host.into(),
            port,
            user: user.into(),
            pass: pass.into(),
        })
    }

    /// Stable id for pool keys and health tracking.
    pub fn id(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// TCP to the proxy, then CONNECT the target through it.
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

        let auth = base64(&format!("{}:{}", self.user, self.pass));
        let req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
             Host: {target_host}:{target_port}\r\n\
             Proxy-Authorization: Basic {auth}\r\n\
             Proxy-Connection: keep-alive\r\n\r\n"
        );
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
        Ok(stream)
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
