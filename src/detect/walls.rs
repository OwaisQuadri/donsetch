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

    // Any other status (4xx/5xx not specifically handled above)
    // is a server error, not content. Previously this fell through
    // to ContentOk, causing 400/500/502 etc. to be treated as
    // successful fetches — the agent would trust error pages as
    // real content.
    Verdict::Blocked
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

/// Smart DOM detection for ghost-rendered pages: considers
/// visible text content before challenge markers.
///
/// A real page with an embedded challenge widget (Cloudflare
/// Turnstile on a contact form, DataDome monitoring script on
/// a Forbes article) contains challenge markers but also has
/// substantial visible text. `detect_dom` alone would classify
/// these as Challenge, causing the ghost to never settle and
/// eventually return captcha=true.
///
/// This function first checks visible text: if the page has
/// ≥ 80 non-whitespace chars outside scripts/styles, it's real
/// content — return ContentOk regardless of challenge markers.
/// Only when the page is visually empty (< 80 visible chars)
/// does it fall back to `detect_dom` for challenge detection.
///
/// Challenge interstitials (CF, DataDome, PX) always have
/// < 80 visible chars — they're mostly JS/HTML structure.
/// The Amazon 51KB block page has ~50 visible chars.
/// Real pages have 80+ visible chars even when they embed
/// challenge widgets in a small section.
pub fn detect_dom_smart(body: &[u8]) -> Verdict {
    let visible = visible_text_count(body);
    if visible >= 80 {
        return Verdict::ContentOk;
    }
    detect_dom(body)
}

/// Fast visible-text estimate: strip tags + script/style bodies,
/// count non-whitespace characters. No lowercasing, no DOM —
/// byte scan. Same algorithm as ghost/ops.rs::visible_text_len.
fn visible_text_count(html: &[u8]) -> usize {
    let b = html;
    let mut n = 0usize;
    let mut i = 0usize;
    while i < b.len() {
        match b[i] {
            b'<' => {
                // Skip script/style/noscript bodies entirely.
                let close: &[u8] = if starts_ci(&b[i + 1..], b"script") {
                    b"</script"
                } else if starts_ci(&b[i + 1..], b"style") {
                    b"</style"
                } else if starts_ci(&b[i + 1..], b"noscript") {
                    b"</noscript"
                } else {
                    // Not a skipped tag — skip to end of this tag.
                    while i < b.len() && b[i] != b'>' {
                        i += 1;
                    }
                    i += 1;
                    continue;
                };
                i = find_ci(b, close, i + 8)
                    .map(|p| p + close.len() + 1)
                    .unwrap_or(b.len());
            }
            c if !c.is_ascii_whitespace() => {
                n += 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    n
}

fn starts_ci(b: &[u8], pat: &[u8]) -> bool {
    b.len() >= pat.len()
        && b[..pat.len()]
            .iter()
            .zip(pat)
            .all(|(a, p)| a.to_ascii_lowercase() == *p)
}

fn find_ci(b: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= b.len() || needle.is_empty() || b.len() < needle.len() {
        return None;
    }
    (from..=b.len() - needle.len()).find(|&p| starts_ci(&b[p..], needle))
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
                || text.contains("cf-turnstile")
                || text.contains("challenges.cloudflare.com")
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
            || text.contains("cf-turnstile")
            || text.contains("challenges.cloudflare.com")
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

    #[test]
    fn detect_dom_smart_real_page_with_turnstile_is_content() {
        // A real page with an embedded Cloudflare Turnstile widget
        // (contact form, login page) has challenge markers but also
        // substantial visible text. detect_dom_smart must return ContentOk.
        let body = b"<html><head><script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script></head><body><h1>Contact Us</h1><p>Fill out the form below and we will get back to you within 24 hours. Our team is dedicated to providing the best possible support for all your inquiries.</p><div class=\"cf-turnstile\"></div><form><input name=\"email\"><textarea name=\"message\"></textarea><button>Send</button></form></body></html>";
        let v = detect_dom_smart(body);
        assert!(
            matches!(v, Verdict::ContentOk),
            "got {v:?} — page with Turnstile widget + real content must be ContentOk"
        );
    }

    #[test]
    fn detect_dom_smart_challenge_interstitial_is_challenge() {
        // A challenge interstitial has < 80 visible chars — detect_dom_smart
        // falls back to detect_dom and correctly identifies the challenge.
        let body = b"<html><head><script src=\"https://challenges.cloudflare.com/turnstile/v0/api.js\"></script></head><body><div class=\"cf-turnstile\"></div></body></html>";
        let v = detect_dom_smart(body);
        assert!(
            matches!(v, Verdict::Challenge(_)),
            "got {v:?} — challenge interstitial must be Challenge"
        );
    }

    #[test]
    fn detect_500_is_blocked_not_content() {
        // A 500 status code should NOT be ContentOk — it's a server error.
        let body = b"<html><body>500 Internal Server Error</body></html>";
        let v = detect(500, &[], body);
        assert!(
            matches!(v, Verdict::Blocked),
            "got {v:?} — 500 must be Blocked, not ContentOk"
        );
    }

    #[test]
    fn detect_400_is_blocked_not_content() {
        // A 400 status code should NOT be ContentOk.
        let body = b"<html><body>400 Bad Request</body></html>";
        let v = detect(400, &[], body);
        assert!(
            matches!(v, Verdict::Blocked),
            "got {v:?} — 400 must be Blocked"
        );
    }

    #[test]
    fn detect_502_is_blocked_not_content() {
        // A 502 Bad Gateway should NOT be ContentOk.
        let body = b"<html><body>502 Bad Gateway</body></html>";
        let v = detect(502, &[], body);
        assert!(
            matches!(v, Verdict::Blocked),
            "got {v:?} — 502 must be Blocked"
        );
    }

    #[test]
    fn turnstile_generic_word_does_not_trigger_challenge() {
        // The word "turnstile" alone (without cf-turnstile or
        // challenges.cloudflare.com) should NOT trigger a challenge.
        let body = b"<html><body><h1>Turnstile Documentation</h1><p>This page discusses the turnstile feature in detail and how it works with various configurations.</p></body></html>";
        let v = detect(200, &[], body);
        assert!(
            matches!(v, Verdict::ContentOk),
            "got {v:?} — bare 'turnstile' word must not trigger challenge"
        );
    }
}
