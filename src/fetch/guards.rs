//! Fetch guards: SSRF prevention and binary content detection.
//!
//! These run BEFORE any network or extraction step so the caller
//! gets a clean, structured error instead of raw bytes or a
//! connection to a private address.

use std::net::IpAddr;

/// True if the URL's host is a private/loopback/link-local
/// address that must never be fetched (SSRF guard).
///
/// Handles literal IPs and well-known localhost names. For
/// DNS hostnames we can't know the IP without resolution, so
/// we only block obvious names — the transport layer's
/// Happy Eyeballs will resolve and connect; private IPs
/// from DNS are an accepted risk for a client-side tool.
pub fn is_ssrf_host(host: &str) -> bool {
    // url::Url::host_str() keeps brackets on IPv6 literals —
    // strip them so the IP parser actually sees an IP.
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    // Literal IP?
    if let Ok(ip) = unbracketed.parse::<IpAddr>() {
        return is_private_ip(&ip);
    }
    // Well-known localhost names.
    let h = host.to_lowercase();
    h == "localhost"
        || h == "localhost."
        || h.ends_with(".localhost")
        || h == "0.0.0.0"
        || h == "[::1]"
        || h == "::1"
}

/// IP-level SSRF check for post-resolution validation.
/// A hostname that resolves to a private address is just as
/// dangerous as a literal one (DNS pinning closes the
/// hostname/rebinding bypass).
pub fn is_ssrf_ip(ip: &IpAddr) -> bool {
    is_private_ip(ip)
}

fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                // Carrier-grade NAT: 100.64.0.0/10
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped (::ffff:a.b.c.d) is the v4 address —
            // check it as v4 or it slips past every v6 rule.
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private_ip(&IpAddr::V4(v4));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                // Link-local: fe80::/10
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                // Unique local: fc00::/7
                || (v6.segments()[0] & 0xfe00) == 0xfc00
        }
    }
}

/// Header values must never carry CR/LF/NUL: a value smuggled
/// from a response (e.g. a cookie) into a request line would
/// split/inject headers on the wire (request splitting).
pub fn valid_header_value(v: &str) -> bool {
    !v.contains('\r') && !v.contains('\n') && !v.contains('\0')
}

/// True if the content-type header indicates binary (non-text)
/// content that should not be passed to the extract pipeline.
pub fn is_binary_content_type(ct: &str) -> bool {
    let ct = ct.to_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    // Allow text-ish types through.
    if ct.is_empty()
        || ct.starts_with("text/")
        || ct.contains("html")
        || ct.contains("xml")
        || ct.contains("json")
        || ct.contains("javascript")
        || ct.contains("pdf")
        || ct.contains("rss")
        || ct.contains("atom")
        || ct.contains("yaml")
        || ct.contains("csv")
        || ct == "application/x-www-form-urlencoded"
    {
        return false;
    }
    // Everything else under image/video/audio/application is binary.
    ct.starts_with("image/")
        || ct.starts_with("video/")
        || ct.starts_with("audio/")
        || ct.starts_with("font/")
        || ct.starts_with("application/")
            && !ct.contains("json")
            && !ct.contains("xml")
            && !ct.contains("javascript")
            && !ct.contains("pdf")
}

/// True if the body is a PDF (magic bytes or content-type).
/// PDFs are binary but are handled by the DonSheet engine, NOT
/// rejected by the binary guard.
pub fn is_pdf(body: &[u8], content_type: &str) -> bool {
    (body.len() >= 5 && body.starts_with(b"%PDF-")) || content_type.to_lowercase().contains("pdf")
}

/// True if the body starts with known binary magic bytes.
/// Catches cases where the content-type is missing or wrong.
pub fn is_binary_body(body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    // PDFs contain null bytes (binary streams, xref tables) but
    // are NOT binary — the DonSheet engine handles them. Skip the
    // null-byte heuristic entirely for PDFs.
    if body.starts_with(b"%PDF-") {
        return false;
    }
    // Known binary magic bytes (common file signatures).
    const MAGIC: &[&[u8]] = &[
        b"\x89PNG",          // PNG
        b"\xff\xd8\xff",     // JPEG
        b"GIF8",             // GIF
        b"BM",               // BMP (2-byte)
        b"\x1f\x8b",         // gzip
        b"PK\x03\x04",       // ZIP / DOCX / XLSX
        b"\x7fELF",          // ELF binary
        b"\x00\x00\x01\x00", // ICO
        b"\x00\x00\x02\x00", // CUR
        b"RIFF",             // WAV/AVI
        b"\x00asm",          // WASM
    ];
    for m in MAGIC {
        if body.starts_with(m) {
            return true;
        }
    }
    // Null bytes in the first 1024 bytes = almost certainly binary.
    let scan = &body[..body.len().min(1024)];
    let nulls = scan.iter().filter(|&&b| b == 0).count();
    nulls > 3 || (nulls > 0 && nulls as f64 / scan.len() as f64 > 0.01)
}

/// Combine both checks: is this content binary we should reject?
/// PDFs are never binary — they're routed to the DonSheet engine.
pub fn is_binary(body: &[u8], content_type: &str) -> bool {
    if is_pdf(body, content_type) {
        return false;
    }
    is_binary_content_type(content_type) || is_binary_body(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssrf_blocks_loopback_v4() {
        assert!(is_ssrf_host("127.0.0.1"));
        assert!(is_ssrf_host("127.0.1.5"));
        assert!(is_ssrf_host("10.0.0.1"));
        assert!(is_ssrf_host("192.168.1.1"));
        assert!(is_ssrf_host("172.16.0.1"));
        assert!(is_ssrf_host("172.31.255.255"));
        assert!(is_ssrf_host("169.254.1.1"));
        assert!(is_ssrf_host("0.0.0.0"));
    }

    #[test]
    fn ssrf_blocks_loopback_v6() {
        assert!(is_ssrf_host("::1"));
        assert!(is_ssrf_host("fe80::1"));
        assert!(is_ssrf_host("fc00::1"));
        assert!(is_ssrf_host("fd12:3456::1"));
    }

    #[test]
    fn ssrf_blocks_localhost_names() {
        assert!(is_ssrf_host("localhost"));
        assert!(is_ssrf_host("localhost."));
        assert!(is_ssrf_host("myapp.localhost"));
    }

    #[test]
    fn ssrf_allows_public() {
        assert!(!is_ssrf_host("93.184.216.34"));
        assert!(!is_ssrf_host("example.com"));
        assert!(!is_ssrf_host("1.1.1.1"));
        assert!(!is_ssrf_host("8.8.8.8"));
    }

    #[test]
    fn ssrf_carrier_grade_nat() {
        assert!(is_ssrf_host("100.64.0.1"));
        assert!(!is_ssrf_host("100.128.0.1"));
    }

    #[test]
    fn ssrf_blocks_ipv4_mapped_v6() {
        // url::Url::host_str() keeps brackets; the guard must see
        // through them and treat mapped addresses as their v4 self.
        assert!(is_ssrf_host("[::ffff:127.0.0.1]"));
        assert!(is_ssrf_host("[::ffff:169.254.169.254]"));
        assert!(is_ssrf_host("[::ffff:10.0.0.1]"));
        assert!(is_ssrf_host("[fd12:3456::1]"));
        assert!(is_ssrf_host("[fe80::1]"));
    }

    #[test]
    fn ssrf_ip_level_check() {
        use std::net::IpAddr;
        let ip: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert!(is_ssrf_ip(&ip));
        let ip: IpAddr = "::ffff:8.8.8.8".parse().unwrap();
        assert!(!is_ssrf_ip(&ip));
    }

    #[test]
    fn header_value_validation() {
        assert!(valid_header_value("plain value; charset=utf-8"));
        assert!(!valid_header_value("a\r\nX-Evil: 1"));
        assert!(!valid_header_value("a\nb"));
        assert!(!valid_header_value("a\0b"));
    }

    #[test]
    fn binary_content_type_detection() {
        assert!(is_binary_content_type("image/png"));
        assert!(is_binary_content_type("image/jpeg"));
        assert!(is_binary_content_type("video/mp4"));
        assert!(is_binary_content_type("audio/mpeg"));
        assert!(is_binary_content_type("application/octet-stream"));
        assert!(is_binary_content_type("application/zip"));
        assert!(is_binary_content_type("font/woff2"));
    }

    #[test]
    fn text_content_type_allowed() {
        assert!(!is_binary_content_type("text/html"));
        assert!(!is_binary_content_type("text/plain"));
        assert!(!is_binary_content_type("text/html; charset=utf-8"));
        assert!(!is_binary_content_type("application/json"));
        assert!(!is_binary_content_type("application/xml"));
        assert!(!is_binary_content_type("application/pdf"));
        assert!(!is_binary_content_type("application/javascript"));
        assert!(!is_binary_content_type("text/csv"));
        assert!(!is_binary_content_type("application/rss+xml"));
        assert!(!is_binary_content_type(""));
    }

    #[test]
    fn binary_body_null_bytes() {
        assert!(is_binary_body(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]));
        assert!(is_binary_body(b"hello\x00world\x00\x00\x00"));
        assert!(!is_binary_body(b"hello world"));
        assert!(!is_binary_body(b""));
        assert!(!is_binary_body(b"<html>no nulls here</html>"));
    }

    #[test]
    fn is_binary_combines_both() {
        assert!(is_binary(b"\x00\x00\x00", "text/html"));
        assert!(is_binary(b"fake png", "image/png"));
        assert!(!is_binary(b"hello", "text/plain"));
        assert!(!is_binary(b"<html>", "text/html; charset=utf-8"));
    }

    #[test]
    fn pdf_not_binary_by_magic() {
        // A PDF with null bytes in first 1024 bytes (binary xref stream).
        let mut pdf = b"%PDF-1.4\n".to_vec();
        pdf.extend_from_slice(&[0x00; 200]); // null bytes — would trigger old heuristic
        pdf.extend_from_slice(b"\n%%EOF\n");
        assert!(
            !is_binary_body(&pdf),
            "PDF body must not be flagged as binary"
        );
        assert!(!is_binary(&pdf, "application/pdf"));
        assert!(is_pdf(&pdf, "application/pdf"));
    }

    #[test]
    fn pdf_not_binary_by_content_type() {
        // Even if body has null bytes, content-type=application/pdf wins.
        let body = b"\x00\x00\x00\x00\x00\x00\x00\x00";
        assert!(!is_binary(body, "application/pdf"));
    }

    #[test]
    fn pdf_detected_by_magic_bytes() {
        assert!(is_pdf(b"%PDF-1.7", ""));
        assert!(is_pdf(b"%PDF-1.4", "application/octet-stream"));
        assert!(is_pdf(b"not a pdf", "application/pdf"));
        assert!(!is_pdf(b"<html>", "text/html"));
    }
}
