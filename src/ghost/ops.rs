//! Solve + Render — the ghost's two jobs.

use std::time::{Duration, Instant};

use crate::detect::walls::{self, Verdict};
use crate::error::FetchError;

use super::Ghost;

pub struct SolveResult {
    /// Clearance + session cookies: (name, value, domain).
    pub cookies: Vec<(String, String, String)>,
    /// Last DOM snapshot — fallback content if tier 1 with
    /// harvested cookies still gets refused.
    pub html: String,
    pub took: Duration,
}

pub enum SolveOutcome {
    Solved(SolveResult),
    /// Interactive captcha — human/service territory.
    /// Honest dead end, no solving service by design.
    CaptchaWalled,
    TimedOut,
}

/// Clearance cookie names worth noting (not exhaustive —
/// a ContentOk verdict is the real success signal).
const CLEARANCE_NAMES: &[&str] = &[
    "cf_clearance",
    "datadome",
    "_px3",
    "ak_bmsc",
    "bm_sz",
    "reese84",
];

/// SOLVE mode: navigate into a wall, wait for the
/// challenge to clear, harvest everything.
///
/// The walls.rs verdict engine is the "are we through?"
/// oracle — vendor-agnostic, zero per-vendor DOM scraping.
pub async fn solve(
    ghost: &mut Ghost,
    url: &str,
    timeout: Duration,
) -> Result<SolveOutcome, FetchError> {
    let start = Instant::now();
    ghost.navigate(url).await?;
    let mut clicked = false;

    while start.elapsed() < timeout {
        tokio::time::sleep(Duration::from_millis(750)).await;
        let html = match ghost.outer_html().await {
            Ok(h) => h,
            Err(_) => continue, // mid-navigation, poll again
        };
        let verdict = walls::detect(200, &[], html.as_bytes());

        // Oracle: challenge interstitials are TINY
        // (CF ~5-15KB, DataDome ~1.5KB, PX ~10KB).
        // A large page tripping body markers is real
        // content mentioning the vendor (false hit).
        let still_challenged = html.len() < 30_000
            && matches!(
                verdict,
                Verdict::Challenge(_) | Verdict::Blocked
            );
        if std::env::var_os("DONGHOST_DEBUG").is_some() {
            eprintln!(
                "[ghost] t={:.0?} html={}B verdict={:?} challenged={}",
                start.elapsed(),
                html.len(),
                verdict,
                still_challenged,
            );
            if start.elapsed() < Duration::from_millis(1600) {
                eprintln!(
                    "[ghost] html: {}",
                    &html[..html.len().min(1200)]
                );
            }
        }

        if !still_challenged {
            ghost.touch();
            return Ok(SolveOutcome::Solved(SolveResult {
                cookies: ghost.cookies().await.unwrap_or_default(),
                html,
                took: start.elapsed(),
            }));
        }

        // Captcha walls: honest dead end.
        let lower = html.to_lowercase();
        if lower.contains("hcaptcha.com")
            || lower.contains("g-recaptcha")
            || lower.contains("www.google.com/recaptcha")
            // DataDome hard captcha (t=fe slider puzzle;
            // passive t=bv auto-clears on its own).
            || lower.contains("captcha-delivery.com/captcha")
            // PerimeterX press-and-hold captcha.
            || lower.contains("px-captcha")
        {
            return Ok(SolveOutcome::CaptchaWalled);
        }

        // Turnstile-style checkbox: one human click, once.
        if !clicked
            && (lower.contains("challenges.cloudflare.com")
                || lower.contains("turnstile")
                || lower.contains("verify you are human"))
        {
            // Checkbox sits roughly centered-left of the
            // challenge widget; aim near viewport center.
            let _ = ghost.click(480.0, 420.0).await;
            clicked = true;
        }
    }
    Ok(SolveOutcome::TimedOut)
}

/// RENDER mode: execute a JS shell, return the live DOM.
/// Success = outerHTML length stable across two polls —
/// robust for SPAs, no Network domain needed.
pub async fn render(
    ghost: &mut Ghost,
    url: &str,
    timeout: Duration,
) -> Result<String, FetchError> {
    let start = Instant::now();
    ghost.navigate(url).await?;
    let mut prev_len = 0usize;
    let mut stable = 0u8;
    let mut html = String::new();

    while start.elapsed() < timeout {
        tokio::time::sleep(Duration::from_millis(600)).await;
        html = match ghost.outer_html().await {
            Ok(h) => h,
            Err(_) => continue,
        };
        let len = html.len();
        if len > 4000 && len.abs_diff(prev_len) < len / 100 + 64 {
            stable += 1;
            if stable >= 2 {
                ghost.touch();
                return Ok(html);
            }
        } else {
            stable = 0;
        }
        prev_len = len;
    }
    ghost.touch();
    // Timeout: return whatever rendered — partial beats none.
    if html.is_empty() {
        Err(FetchError::ghost("render produced no DOM"))
    } else {
        Ok(html)
    }
}

/// Does a cookie list contain a known clearance name?
pub fn has_clearance(cookies: &[(String, String, String)]) -> bool {
    cookies
        .iter()
        .any(|(n, _, _)| CLEARANCE_NAMES.contains(&n.as_str()))
}
