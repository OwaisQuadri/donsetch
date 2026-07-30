//! Charset detection + decoding. Never trust "UTF-8 always".

/// Decode body bytes to a String using (in order): content-type
/// charset param, BOM, <meta charset> sniff in the first 2 KiB,
/// UTF-8 fallback (lossy).
pub fn decode(body: &[u8], content_type: &str) -> String {
    if let Some(enc) = from_content_type(content_type) {
        return enc.decode(body).0.into_owned();
    }
    if let Some(enc) = from_bom(body) {
        return enc.decode(body).0.into_owned();
    }
    if let Some(enc) = sniff_meta(body) {
        return enc.decode(body).0.into_owned();
    }
    String::from_utf8_lossy(body).into_owned()
}

fn from_content_type(ct: &str) -> Option<&'static encoding_rs::Encoding> {
    let idx = ct.find("charset=")?;
    let label = ct[idx + 8..].split([';', ' ', '"', '\'']).next()?;
    encoding_rs::Encoding::for_label(label.as_bytes())
}

fn from_bom(body: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    if body.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(encoding_rs::UTF_8)
    } else if body.starts_with(&[0xFF, 0xFE]) {
        Some(encoding_rs::UTF_16LE)
    } else if body.starts_with(&[0xFE, 0xFF]) {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

fn sniff_meta(body: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = &body[..body.len().min(2048)];
    let text = String::from_utf8_lossy(head).to_lowercase();
    let idx = text.find("charset")?;
    let rest = &text[idx + 7..];
    let rest = rest.trim_start_matches([' ', '=', '"', '\'']);
    let label: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if label.is_empty() {
        return None;
    }
    encoding_rs::Encoding::for_label(label.as_bytes())
}
