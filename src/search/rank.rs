//! rank.rs — weighted RRF + consensus + relevance +
//! priors + diversity. This is where naive-merge
//! metasearch (SearXNG) loses.

use std::collections::HashMap;

use super::engines::Hit;
use super::intent::{self, Intent};

const RRF_K: f64 = 60.0;
const CONSENSUS_MULT: f64 = 0.5;
const BM25_WEIGHT: f64 = 0.3;
const PRIOR_WEIGHT: f64 = 0.15;
const MAX_PER_DOMAIN: usize = 2;
/// Vertical-only hits rank below web-engine consensus:
/// a GitHub/HN/wiki hit with no engine corroboration is
/// a weaker signal than a URL three engines agree on.
const VERTICAL_WEIGHT: f64 = 0.6;

fn is_vertical(engine: &str) -> bool {
    matches!(engine, "github" | "hn" | "wikipedia" | "scholar" | "news" | "arxiv")
}

/// A merged, scored result.
#[derive(Debug, Clone)]
pub struct Merged {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// (engine, rank) pairs that produced this result.
    pub sources: Vec<(String, usize)>,
    pub score: f64,
    /// News vertical fills this; freshness ranking reads it.
    #[allow(dead_code)]
    pub published: Option<String>,
}

/// Normalize a URL for consensus matching: scheme-less,
/// www-less, trailing-slash-less, lowercase host.
pub fn norm_key(raw: &str) -> String {
    let Ok(u) = url::Url::parse(raw) else {
        return raw.to_lowercase();
    };
    let host = u
        .host_str()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_lowercase();
    let path = u
        .path()
        .trim_end_matches('/')
        .trim_end_matches("/index.html")
        .trim_end_matches("/index.htm")
        .to_lowercase();
    let query = u.query().map(|q| format!("?{q}")).unwrap_or_default();
    format!("{host}{path}{query}")
}

pub fn host_of(raw: &str) -> String {
    url::Url::parse(raw)
        .ok()
        .and_then(|u| u.host_str().map(String::from))
        .unwrap_or_default()
        .trim_start_matches("www.")
        .to_string()
}

/// BM25-lite relevance of (title, snippet) against query.
/// IDF is estimated from the result set itself.
fn relevance(query: &str, docs: &[(String, String)]) -> Vec<f64> {
    let terms: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() > 2)
        .map(String::from)
        .collect();
    if terms.is_empty() {
        return vec![0.0; docs.len()];
    }
    let n = docs.len() as f64;
    // document frequency per term
    let mut df: HashMap<&String, usize> = HashMap::new();
    let tokenized: Vec<Vec<String>> = docs
        .iter()
        .map(|(t, s)| {
            format!("{t} {s}")
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .map(String::from)
                .collect()
        })
        .collect();
    for toks in &tokenized {
        for term in &terms {
            if toks.iter().any(|t| t == term) {
                *df.entry(term).or_insert(0) += 1;
            }
        }
    }
    let avg_len = tokenized.iter().map(|t| t.len()).sum::<usize>() as f64 / n.max(1.0);
    const K1: f64 = 1.2;
    const B: f64 = 0.75;
    tokenized
        .iter()
        .map(|toks| {
            let mut score = 0.0;
            for term in &terms {
                let tf = toks.iter().filter(|t| *t == term).count() as f64;
                if tf == 0.0 {
                    continue;
                }
                let dfv = *df.get(term).unwrap_or(&0) as f64;
                let idf = ((n - dfv + 0.5) / (dfv + 0.5) + 1.0).ln();
                let len_norm = 1.0 - B + B * (toks.len() as f64 / avg_len.max(1.0));
                score += idf * (tf * (K1 + 1.0)) / (tf + K1 * len_norm);
            }
            score
        })
        .collect()
}

/// Merge all engine hits into one ranked list.
/// `trust` maps engine -> 0.0..=2.0 (learned EWMA).
pub fn merge(
    per_engine: &[(String, Vec<Hit>)],
    query: &str,
    intent: Intent,
    trust: &HashMap<String, f64>,
    max_results: usize,
) -> Vec<Merged> {
    // Group by normalized URL.
    let mut groups: HashMap<String, Merged> = HashMap::new();
    for (engine, hits) in per_engine {
        let base = trust.get(engine).copied().unwrap_or(1.0);
        let tw = if is_vertical(engine) {
            base * VERTICAL_WEIGHT
        } else {
            base
        };
        for hit in hits {
            let key = norm_key(&hit.url);
            let entry = groups.entry(key).or_insert_with(|| Merged {
                title: hit.title.clone(),
                url: hit.url.clone(),
                snippet: hit.snippet.clone(),
                sources: Vec::new(),
                score: 0.0,
                published: None,
            });
            // Keep the longest snippet (most informative),
            // skipping redirect stubs.
            if hit.snippet.len() > entry.snippet.len()
                && !hit.snippet.starts_with("Redirecting")
            {
                entry.snippet = hit.snippet.clone();
            }
            // Best title: breadcrumbs ("a › b › c") and
            // URL-echoes are longer than real titles — keep
            // the shortest CLEAN candidate.
            let bad = |t: &str| {
                t.contains(" › ")
                    || t.starts_with("http")
                    || t.len() < 3
            };
            if !bad(&hit.title)
                && (bad(&entry.title) || hit.title.len() < entry.title.len())
            {
                entry.title = hit.title.clone();
            }
            if entry.published.is_none() && hit.published.is_some() {
                entry.published = hit.published.clone();
            }
            entry.score += tw / (RRF_K + hit.rank as f64 + 1.0);
            entry.sources.push((engine.clone(), hit.rank));
        }
    }

    let mut results: Vec<Merged> = groups.into_values().collect();

    // Consensus multiplier: engines are independent-ish
    // (Brave/Mojeek truly independent; Bing/DDG share an
    // index — count shared-index sources at half weight).
    for r in &mut results {
        let engines: Vec<&str> = r.sources.iter().map(|(e, _)| e.as_str()).collect();
        let mut independent: Vec<&str> = Vec::new();
        for e in &engines {
            let fam = engine_family(e);
            if !independent.iter().any(|x| engine_family(x) == fam) {
                independent.push(e);
            }
        }
        let consensus = independent.len() as f64;
        r.score *= 1.0 + CONSENSUS_MULT * (consensus - 1.0).max(0.0);
    }

    // BM25 relevance bonus.
    let docs: Vec<(String, String)> = results
        .iter()
        .map(|r| (r.title.clone(), r.snippet.clone()))
        .collect();
    let rel = relevance(query, &docs);
    let max_rel = rel.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    for (r, rv) in results.iter_mut().zip(rel) {
        r.score += BM25_WEIGHT * (rv / max_rel);
    }

    // Domain prior bonus.
    for r in &mut results {
        let prior = intent::domain_prior(intent, &host_of(&r.url));
        r.score += PRIOR_WEIGHT * prior;
    }

    results.sort_by(|a, b| b.score.total_cmp(&a.score));

    // Diversity cap: max MAX_PER_DOMAIN per domain.
    let mut domain_count: HashMap<String, usize> = HashMap::new();
    let mut diverse = Vec::with_capacity(max_results);
    let mut overflow = Vec::new();
    for r in results {
        let host = host_of(&r.url);
        let c = domain_count.entry(host).or_insert(0);
        if *c < MAX_PER_DOMAIN {
            *c += 1;
            diverse.push(r);
        } else {
            overflow.push(r);
        }
        if diverse.len() >= max_results {
            break;
        }
    }
    if diverse.len() < max_results {
        diverse.extend(overflow.into_iter().take(max_results - diverse.len()));
    }
    diverse.truncate(max_results);
    diverse
}

/// Index family: engines sharing an index count once for
/// consensus (a Bing hit + DDG hit = one opinion, not two).
fn engine_family(engine: &str) -> &str {
    match engine {
        "bing" | "ddg" | "ddg_lite" | "yahoo" => "bing",
        "brave" => "brave",
        "mojeek" => "mojeek",
        other => other, // verticals are their own family
    }
}

/// Weak-results honesty: no cross-family consensus and a
/// low top score means the merge is not trustworthy.
pub fn is_weak(results: &[Merged]) -> bool {
    if results.is_empty() {
        return true;
    }
    let top = &results[0];
    let families: std::collections::HashSet<&str> =
        top.sources.iter().map(|(e, _)| engine_family(e)).collect();
    families.len() < 2 && results.len() < 5
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::engines::Hit;
    use crate::search::intent::Intent;

    fn hit(url: &str, rank: usize) -> Hit {
        Hit {
            title: format!("title for {url}"),
            url: url.into(),
            snippet: "rust async runtime comparison".into(),
            rank,
            published: None,
        }
    }

    #[test]
    fn consensus_beats_vertical_only() {
        // URL A: brave #5 + bing #5 (two families).
        // URL B: github vertical #0 only.
        let per = vec![
            ("brave".to_string(), vec![hit("https://a.com/x", 5)]),
            ("bing".to_string(), vec![hit("https://a.com/x", 5)]),
            ("github".to_string(), vec![hit("https://b.com/y", 0)]),
        ];
        let trust = std::collections::HashMap::new();
        let out = merge(&per, "rust async runtime", Intent::Code, &trust, 10);
        assert_eq!(out[0].url, "https://a.com/x", "consensus must win");
    }

    #[test]
    fn diversity_caps_domains() {
        // other.com ranks higher than any same.com, so a
        // naive merge would still flood the top with
        // same.com #1..5 below it. Cap: max 2 per domain
        // before other domains get their slots; overflow
        // only backfills when the list runs short.
        let mut hits: Vec<Hit> = vec![hit("https://other.com/a", 0)];
        hits.extend((0..5).map(|i| hit(&format!("https://same.com/p{i}"), i + 1)));
        hits.push(hit("https://third.com/z", 9));
        let per = vec![("brave".to_string(), hits)];
        let trust = std::collections::HashMap::new();
        let out = merge(&per, "rust async runtime", Intent::Web, &trust, 10);
        // third.com must appear before the 3rd same.com hit.
        let pos_third = out.iter().position(|r| r.url.contains("third.com")).unwrap();
        let third_same = out
            .iter()
            .enumerate()
            .filter(|(_, r)| r.url.contains("same.com"))
            .nth(2)
            .map(|(i, _)| i);
        if let Some(p3) = third_same {
            assert!(pos_third < p3, "diversity violated: third@{pos_third} same#3@{p3}");
        }
    }

    #[test]
    fn norm_key_unifies_variants() {
        let a = norm_key("https://www.docs.rs/ratatui/index.html");
        let b = norm_key("http://docs.rs/ratatui/");
        assert_eq!(a, b);
    }

    #[test]
    fn empty_merge_is_safe() {
        let trust = std::collections::HashMap::new();
        let out = merge(&[], "anything", Intent::Web, &trust, 10);
        assert!(out.is_empty());
        assert!(is_weak(&out));
    }

    #[test]
    fn title_prefers_clean_over_breadcrumb() {
        let mut dirty = hit("https://en.wikipedia.org/wiki/Rust", 0);
        dirty.title = "en.wikipedia.org › wiki › Rust".into();
        let mut clean = hit("https://en.wikipedia.org/wiki/Rust", 3);
        clean.title = "Rust - Wikipedia".into();
        let per = vec![
            ("brave".to_string(), vec![dirty]),
            ("ddg".to_string(), vec![clean]),
        ];
        let trust = std::collections::HashMap::new();
        let out = merge(&per, "rust", Intent::Web, &trust, 10);
        assert_eq!(out[0].title, "Rust - Wikipedia");
    }
}
