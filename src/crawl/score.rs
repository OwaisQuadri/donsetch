//! Frontier relevance scoring — BM25-lite over anchor text +
//! URL path tokens. The crawl spends its budget on pages that
//! MATTER to the focus query, not on the sitemap's order.
//!
//! Reuses the DonSift focus tokenizer: CJK bigrams, 12-language
//! stopwords, light stemming, accent folding all apply to crawl
//! scoring for free.

use crate::extract::focus;
use crate::extract::language;

/// Score one candidate URL against the focus query.
/// `anchor` = the link text where we found it ("" from sitemaps).
/// `path` = URL path. `focus` = None means no focus — score = 0
/// and the queue falls back to sitemap/depth order.
pub fn score_candidate(anchor: &str, path: &str, focus: Option<&str>) -> f64 {
    let Some(q) = focus else {
        return depth_prior(path);
    };
    let qlang = language::detect_from_text(q);
    let qtoks = focus::tokenize(q, &qlang);
    if qtoks.is_empty() {
        return depth_prior(path);
    }

    // Candidate text: anchor words are highest signal.
    let anchor_toks = focus::tokenize(anchor, &qlang);
    // Path tokens: split on /-_.
    let path_text = path.replace(['/', '-', '_', '.'], " ");
    let path_toks = focus::tokenize(&path_text, &qlang);

    let mut score = 0.0f64;
    for qt in &qtoks {
        // Anchor hit: strongest evidence.
        if anchor_toks.iter().any(|t| t == qt) {
            score += 3.0;
        }
        // Path hit: still meaningful.
        if path_toks.iter().any(|t| t == qt) {
            score += 1.5;
        }
    }
    // Normalize by query size so 1-term and 5-term queries are
    // comparable. Saturation: each token caps at its first hit.
    score / qtoks.len().max(1) as f64 + depth_prior(path)
}

/// Path-depth prior: prefer shallower pages when relevance is
/// neutral. /docs/guide > /a/b/c/d/e.
fn depth_prior(path: &str) -> f64 {
    let segs = path.split('/').filter(|s| !s.is_empty()).count();
    -(segs as f64) * 0.15
}

/// Check if any focus query token appears in the anchor text or
/// URL path. Used as a hard gate for crawl outlinks: when a
/// focus query is set, links with zero token matches are NOT
/// enqueued. This prevents the crawler from following
/// navigation, footer, and sidebar links to off-topic sections.
/// Returns true for empty focus (no filter).
///
/// Compound token handling: query terms containing `_` or `-`
/// (e.g. `spawn_blocking`, `async-await`) are treated as compound
/// identifiers. A compound term matches if:
///   (a) the full compound form appears as a substring in the
///       path or anchor (e.g. `spawn_blocking` in the path), OR
///   (b) ALL its fragments appear in the path/anchor tokens.
/// This prevents the stemmed fragment `block` (from splitting
/// `spawn_blocking` → `spawn` + `block`) from matching unrelated
/// paths like `/ant-libp2p-allow-block-list/` where only `block`
/// appears without `spawn`.
pub fn focus_match(anchor: &str, path: &str, focus: &str) -> bool {
    let qlang = language::detect_from_text(focus);
    let qtoks = focus::tokenize(focus, &qlang);
    if qtoks.is_empty() {
        return true;
    }
    let anchor_toks = focus::tokenize(anchor, &qlang);
    let path_text = path.replace(['/', '-', '_', '.'], " ");
    let path_toks = focus::tokenize(&path_text, &qlang);
    let all_toks: Vec<&String> = anchor_toks.iter().chain(path_toks.iter()).collect();

    let lower_path = path.to_lowercase();
    let lower_anchor = anchor.to_lowercase();

    // Split query into whitespace-separated terms.
    for term in focus.split_whitespace() {
        let lower_term = term.to_lowercase();

        // Compound term (contains _ or -): check full form as
        // substring, or ALL fragments as token matches.
        if lower_term.contains('_') || lower_term.contains('-') {
            // (a) Full compound form as substring.
            if lower_path.contains(&lower_term) || lower_anchor.contains(&lower_term) {
                return true;
            }
            // (b) ALL fragments must match as tokens.
            let fragments = focus::tokenize(&lower_term, &qlang);
            if !fragments.is_empty() && fragments.iter().all(|ft| all_toks.contains(&ft)) {
                return true;
            }
        } else {
            // Simple term: check if any token matches.
            let term_toks = focus::tokenize(&lower_term, &qlang);
            if term_toks.iter().any(|tt| all_toks.contains(&tt)) {
                return true;
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_anchor_beats_path() {
        let a = score_candidate("the migration guide", "/blog/x", Some("migration"));
        let b = score_candidate("click here", "/docs/migration", Some("migration"));
        assert!(a > b);
        assert!(b > 0.0);
    }

    #[test]
    fn no_focus_depth_prior_only() {
        let shallow = score_candidate("", "/a", None);
        let deep = score_candidate("", "/a/b/c/d", None);
        assert!(shallow > deep);
    }

    #[test]
    fn empty_query_falls_back() {
        assert_eq!(
            score_candidate("x", "/a", Some("")),
            0.0 + depth_prior("/a")
        );
    }

    #[test]
    fn cjk_focus_scores() {
        let s = score_candidate("什么是机器学习", "/some/article", Some("机器学习"));
        assert!(s > 0.0);
    }

    #[test]
    fn focus_match_basic() {
        assert!(focus_match(
            "spawn blocking tutorial",
            "/docs/async",
            "spawn_blocking"
        ));
        assert!(focus_match(
            "click here",
            "/tokio/task/spawn_blocking",
            "spawn_blocking"
        ));
        assert!(!focus_match("login", "/login", "spawn_blocking vs spawn"));
        assert!(!focus_match("pricing", "/pricing", "spawn_blocking"));
        assert!(focus_match("", "/tokio/spawn", "spawn_blocking vs spawn"));
        assert!(!focus_match("", "/tokio/bytes", "spawn_blocking vs spawn"));
    }

    #[test]
    fn focus_match_empty_is_passthrough() {
        assert!(focus_match("anything", "/any/path", ""));
    }

    #[test]
    fn focus_match_compound_no_false_positive() {
        // `block` from `spawn_blocking` must NOT match paths that
        // contain `block` but not `spawn` (e.g. unrelated crates
        // on docs.rs that happen to have "block" in the name).
        assert!(!focus_match(
            "",
            "/ant-libp2p-allow-block-list",
            "spawn_blocking vs spawn"
        ));
        assert!(!focus_match(
            "",
            "/async-blocking-bridger",
            "spawn_blocking vs spawn"
        ));
        assert!(!focus_match("", "/asm_block", "spawn_blocking vs spawn"));
    }

    #[test]
    fn focus_match_compound_in_path() {
        // Full compound form in path → match.
        assert!(focus_match(
            "",
            "/tokio/task/spawn_blocking",
            "spawn_blocking vs spawn"
        ));
    }

    #[test]
    fn focus_match_all_fragments() {
        // Both fragments `spawn` and `block` in path → match.
        assert!(focus_match(
            "spawn block tutorial",
            "/docs/spawn/block",
            "spawn_blocking vs spawn"
        ));
    }
}
