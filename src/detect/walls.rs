//! Vendor-aware wall detection → honest verdicts.
//!
//! A 200 is never trusted on its own: challenge interstitials are
//! frequently served as 200 with a tiny JS shell. Detection runs on
//! status + headers + (decompressed) body markers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Cloudflare,
    DataDome,
    Akamai,
    PerimeterX,
    Imperva,
    Sucuri,
    Wordfence,
    Generic,
}

#[allow(dead_code)] // full verdict surface used by MCP layer
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Real content, safe to use.
    ContentOk,
    /// Bot-wall challenge page (maybe JS-less cookie challenge, maybe
    /// full JS challenge). Vendor identified when possible.
    Challenge(Vendor),
    /// Hard block page (no path forward at this tier).
    Blocked,
    /// Login required.
    AuthWall,
    /// Paywalled.
    Paywall,
    /// 404 or content-less page dressed as success.
    SoftNotFound,
}

pub fn detect(status: u16, headers: &[(String, String)], body: &[u8]) -> Verdict {
    let server = header(headers, "server").unwrap_or_default().to_lowercase();
    let cf_ray = header(headers, "cf-ray").is_some();
    let is_cf = server.contains("cloudflare") || cf_ray;
    // Challenge markers live in the title/head — scanning
    // the whole body false-positives on articles that merely
    // MENTION a vendor (a Wikipedia page about Akamai).
    let scan = &body[..body.len().min(64 * 1024)];
    let text = String::from_utf8_lossy(scan).to_lowercase();

    match status {
        401 | 402 => return Verdict::AuthWall,
        404 => return Verdict::SoftNotFound,
        403 | 429 | 503 => {
            return classify_wall(&text, headers, is_cf, status, true);
        }
        _ => {}
    }

    if (200..300).contains(&status) {
        // Interstitials dressed as 200. Body markers only
        // count on SMALL pages: interstitials are tiny,
        // while real pages (a Bing SERP, an article about
        // Cloudflare) mention vendors in passing — the
        // lesson the ghost oracle learned first.
        let allow_body_markers = scan.len() < 32 * 1024;
        let v = classify_wall(&text, headers, is_cf, status, allow_body_markers);
        if v != Verdict::ContentOk {
            return v;
        }
        return Verdict::ContentOk;
    }

    Verdict::ContentOk
}

fn classify_wall(
    text: &str,
    headers: &[(String, String)],
    is_cf: bool,
    status: u16,
    allow_body_markers: bool,
) -> Verdict {
    // Header-based detection is always active: a
    // cf-mitigated / x-datadome header never lies,
    // regardless of page size.
    if is_cf && (status == 403 || status == 503) {
        // CF 403/503: could be a challenge page OR a
        // WAF block / origin error. Check body for
        // challenge markers before classifying.
        if allow_body_markers
            && (text.contains("just a moment")
                || text.contains("cf-chl")
                || text.contains("challenge-platform")
                || text.contains("turnstile")
                || text.contains("attention required"))
        {
            return Verdict::Challenge(Vendor::Cloudflare);
        }
        // No challenge markers — WAF block (403) or
        // origin error (503). Ghost solve won't help.
        return Verdict::Blocked;
    }
    if header(headers, "x-datadome").is_some() {
        return Verdict::Challenge(Vendor::DataDome);
    }

    // Body markers below. On 2xx these only run for SMALL
    // pages: interstitials are tiny; large real pages
    // (Bing SERPs embed inactive turnstile scripts,
    // articles mention vendors) false-positive otherwise —
    // the lesson the ghost oracle learned first.
    if !allow_body_markers {
        return Verdict::ContentOk;
    }

    // Google: sorry/consent interstitials. "unusual traffic"
    // + recaptcha is the sorry page; /sorry/ + recaptcha is
    // its form target. Both are challenge pages, not content —
    // without this, a CAPTCHA page passes as ContentOk.
    if (text.contains("unusual traffic") && text.contains("recaptcha"))
        || (text.contains("/sorry/") && text.contains("recaptcha"))
    {
        return Verdict::Challenge(Vendor::Generic);
    }

    // Cloudflare
    if is_cf || text.contains("cf-chl") || text.contains("cloudflare") {
        if text.contains("attention required") {
            return Verdict::Blocked; // CF hard block page
        }
        if text.contains("just a moment")
            || text.contains("challenge-platform")
            || text.contains("cf-chl")
            || text.contains("turnstile")
            || status == 403
            || status == 503
        {
            return Verdict::Challenge(Vendor::Cloudflare);
        }
    }
    // DataDome
    if text.contains("datadome") || text.contains("captcha-delivery.com") {
        return Verdict::Challenge(Vendor::DataDome);
    }
    // Akamai: block pages carry "Reference #…" +
    // edgesuite. A bare "akamai" match false-positives on
    // articles about Akamai Technologies.
    if text.contains("reference #") && text.contains("errors.edgesuite.net")
        || text.contains("_abck")
        || header(headers, "x-akamai-transformed").is_some() && (status == 403 || status == 503)
    {
        return Verdict::Challenge(Vendor::Akamai);
    }
    // PerimeterX / HUMAN
    if text.contains("perimeterx")
        || text.contains("px-captcha")
        || text.contains("human-challenge")
    {
        return Verdict::Challenge(Vendor::PerimeterX);
    }
    // Imperva / Incapsula
    if text.contains("incapsula")
        || text.contains("_incapsula_resource")
        || text.contains("imperva")
    {
        return Verdict::Challenge(Vendor::Imperva);
    }
    // Sucuri
    if text.contains("sucuri") || text.contains("cloudproxy") {
        return Verdict::Challenge(Vendor::Sucuri);
    }
    // Wordfence
    if text.contains("wordfence") || text.contains("generated by wordfence") {
        return Verdict::Challenge(Vendor::Wordfence);
    }
    // Generic challenge signals on error statuses.
    if status == 403 || status == 503 || status == 429 {
        if header(headers, "set-cookie").is_some() {
            return Verdict::Challenge(Vendor::Generic); // cookie-warm retry candidate
        }
        if text.contains("captcha") || text.contains("are you a robot") || text.contains("bot") {
            return Verdict::Challenge(Vendor::Generic);
        }
        return Verdict::Blocked;
    }
    // Reddit-style interstitials (often served as 200).
    if text.contains("prove your humanity")
        || text.contains("not for bots")
        || text.contains("please wait for verification")
    {
        return Verdict::Challenge(Vendor::Generic);
    }
    // Small 200-page captchas (Mojeek et al.): a real page
    // is never this tiny with a challenge form on it.
    if text.len() < 16_384
        && text.contains("captcha")
        && (text.contains("verification") || text.contains("challenge") || text.contains("robot"))
    {
        return Verdict::Challenge(Vendor::Generic);
    }
    Verdict::ContentOk
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_serp_with_vendor_mentions_is_content() {
        let body = include_bytes!("../../tests/fixtures/bing-serp.html").to_vec();
        let scan = &body[..body.len().min(64 * 1024)];
        let text = String::from_utf8_lossy(scan).to_lowercase();
        let v = detect(200, &[], &body);
        assert!(matches!(v, Verdict::ContentOk), "got {v:?}");
    }

    #[test]
    fn small_captcha_page_is_challenge() {
        let body = include_bytes!("../../tests/fixtures/mojeek-captcha.html").to_vec();
        let v = detect(200, &[], &body);
        assert!(matches!(v, Verdict::Challenge(_)), "got {v:?}");
    }
}
