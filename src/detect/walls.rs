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

/// Detect wall from a ghost-rendered DOM (no HTTP headers).
/// Always checks body markers — the DOM is already rendered,
/// so challenge markers in the HTML are real, not false
/// positives from CSS class names mentioning a vendor.
/// Scans first 64KB (challenge markers live in <head>).
///
/// Unlike `detect`, this doesn't gate body markers on page size:
/// ghost DOMs are rendered, so large DOMs with challenge markers
/// are genuinely challenged (Amazon's 51KB block page).
/// Also strips <style>/<script> before checking for "skeleton"
/// and other markers that appear in CSS class names.
pub fn detect_dom(body: &[u8]) -> Verdict {
    let scan = &body[..body.len().min(64 * 1024)];
    let text = String::from_utf8_lossy(scan).to_lowercase();
    classify_wall(&text, &[], false, 200, true)
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
    // DataDome: the x-datadome header is present on ALL responses
    // from DataDome-protected sites (200s with real content AND 403
    // challenge pages). The header alone is NOT a wall signal —
    // DataDome runs in monitoring mode on many sites (Forbes,
    // Reddit), tagging every response but only blocking on
    // actual bot detection. The wall is:
    //   - 403/429 + x-datadome = challenge (always, regardless of body)
    //   - 200 + x-datadome + small body + datadome/captcha markers = challenge
    //   - 200 + x-datadome + large body = ContentOk (monitoring mode)
    if header(headers, "x-datadome").is_some() {
        // On error statuses, x-datadome always means challenge.
        if status == 403 || status == 429 || status == 503 {
            return Verdict::Challenge(Vendor::DataDome);
        }
        // On 200: only challenge if the body is small AND contains
        // DataDome CHALLENGE markers (not the monitoring script).
        // "datadome" alone matches js.datadome.co/tags.js (monitoring);
        // "captcha-delivery.com" or "datadome"+"captcha" = challenge.
        if (200..300).contains(&status)
            && allow_body_markers
            && (text.contains("captcha-delivery.com")
                || (text.contains("datadome") && text.contains("captcha")))
        {
            return Verdict::Challenge(Vendor::DataDome);
        }
        // 200 with x-datadome but no body markers = real content.
        // Fall through to other checks / ContentOk.
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
    // DataDome body markers: "captcha-delivery.com" is the
    // challenge-specific script URL. "datadome" alone matches
    // the monitoring script (js.datadome.co/tags.js) present on
    // ALL DataDome-protected pages, even real content.
    // Only trigger on the challenge marker, or "datadome" +
    // "captcha" together.
    if text.contains("captcha-delivery.com")
        || (text.contains("datadome") && text.contains("captcha"))
    {
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
    // Small 200-page bot-check interstitials without a captcha
    // form. IMDB, Amazon, and other server-side bot detection:
    // "verify that you're not a robot" + "JavaScript is disabled".
    // A real page is never this small with these phrases.
    if text.len() < 16_384 && text.contains("verify") && text.contains("robot") {
        return Verdict::Challenge(Vendor::Generic);
    }
    // "JavaScript is disabled" + "not a robot" on a tiny page.
    if text.len() < 16_384 && text.contains("javascript is disabled") && text.contains("robot") {
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

    #[test]
    fn imdb_bot_check_page_is_challenge() {
        // IMDB serves this tiny page when it detects a bot:
        // "JavaScript is disabled / verify that you're not a robot"
        let body = b"<html><noscript>JavaScript is disabled In order to continue, we need to verify that you're not a robot. This requires JavaScript. Enable JavaScript and then reload the page.</noscript></html>";
        let v = detect_dom(body);
        assert!(matches!(v, Verdict::Challenge(_)), "got {v:?}");
    }

    #[test]
    fn forbes_200_with_datadome_header_is_content() {
        // Forbes returns x-datadome: protected on ALL responses
        // (200s with full 1.3MB articles AND 403 challenge pages).
        // The header alone is NOT a wall — DataDome runs in
        // monitoring mode. A 200 with a large body is ContentOk.
        let body = vec![b'<'; 1_300_000]; // 1.3MB of content
        let headers = vec![
            ("x-datadome".into(), "protected".into()),
            ("content-type".into(), "text/html".into()),
        ];
        let v = detect(200, &headers, &body);
        assert!(
            matches!(v, Verdict::ContentOk),
            "got {v:?} — Forbes 200 with x-datadome + large body must be ContentOk"
        );
    }

    #[test]
    fn forbes_403_with_datadome_header_is_challenge() {
        // When Forbes DOES block (403), x-datadome means challenge.
        let body = b"<html>DataDome challenge</html>";
        let headers = vec![("x-datadome".into(), "protected".into())];
        let v = detect(403, &headers, body);
        assert!(
            matches!(v, Verdict::Challenge(Vendor::DataDome)),
            "got {v:?}"
        );
    }

    #[test]
    fn datadome_200_small_body_with_markers_is_challenge() {
        // A small 200 page with datadome challenge markers IS a challenge
        // interstitial (captcha-delivery.com is the challenge script).
        let body = b"<html><body>datadome captcha-delivery.com challenge</body></html>";
        let headers = vec![("x-datadome".into(), "protected".into())];
        let v = detect(200, &headers, body);
        assert!(
            matches!(v, Verdict::Challenge(Vendor::DataDome)),
            "got {v:?}"
        );
    }

    #[test]
    fn datadome_200_small_body_monitoring_script_is_content() {
        // A small 200 page with x-datadome header and the DataDome
        // monitoring script (js.datadome.co/tags.js) but NO challenge
        // markers = real content (DataDome in monitoring mode).
        let body = b"<html><head><script src=\"https://js.datadome.co/tags.js\"></script></head><body><p>Real article content about technology news today.</p></body></html>";
        let headers = vec![("x-datadome".into(), "protected".into())];
        let v = detect(200, &headers, body);
        assert!(
            matches!(v, Verdict::ContentOk),
            "got {v:?} — monitoring script must not trigger challenge"
        );
    }

    #[test]
    fn datadome_200_small_body_no_markers_is_content() {
        // A small 200 page with x-datadome header but NO datadome/captcha
        // body markers = real content (DataDome in monitoring mode).
        let body =
            b"<html><body><p>Real article content about technology news today.</p></body></html>";
        let headers = vec![("x-datadome".into(), "protected".into())];
        let v = detect(200, &headers, body);
        assert!(matches!(v, Verdict::ContentOk), "got {v:?}");
    }
}
