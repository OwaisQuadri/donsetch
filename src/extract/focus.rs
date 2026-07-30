//! BM25 block filter for focus=. Hand-rolled: tokenize, df/idf
//! across blocks, k1=1.2 b=0.75. Blocks that score keep their
//! heading breadcrumbs for context. No hits → full content
//! (never punish the agent for a bad query).

use std::collections::HashMap;

use super::blocks::Block;

const K1: f64 = 1.2;
const B: f64 = 0.75;

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "was", "were",
    "for", "on", "with", "as", "at", "by", "from", "it", "its", "this", "that",
    "be", "been", "has", "have", "had", "not", "but", "they", "their", "we",
    "you", "he", "she", "his", "her", "what", "which", "who", "how", "when",
    "do", "does", "did", "can", "could", "will", "would", "about",
];

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(|t| t.to_lowercase())
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
        .collect()
}

/// BM25 block filter. Returns (kept blocks, fell_back).
/// fell_back = true when the query matched nothing and we
/// returned the full page — the CALLER must signal this,
/// or the agent mistakes full content for focus matches.
pub fn filter<'a>(blocks: &'a [Block], query: &str) -> (Vec<&'a Block>, bool) {
    let qterms = tokenize(query);
    if qterms.is_empty() || blocks.is_empty() {
        return (blocks.iter().collect(), false);
    }

    // Document stats.
    let docs: Vec<Vec<String>> = blocks.iter().map(|b| tokenize(&b.text())).collect();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for doc in &docs {
        let mut seen = std::collections::HashSet::new();
        for t in doc {
            if seen.insert(t.as_str()) {
                *df.entry(t.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = blocks.len() as f64;
    let avgdl = docs.iter().map(|d| d.len()).sum::<usize>() as f64 / n.max(1.0);

    // Score.
    let mut scored: Vec<(usize, f64)> = Vec::new();
    for (i, doc) in docs.iter().enumerate() {
        let mut tf: HashMap<&str, usize> = HashMap::new();
        for t in doc {
            *tf.entry(t.as_str()).or_insert(0) += 1;
        }
        let dl = doc.len() as f64;
        let mut score = 0.0;
        for q in &qterms {
            let Some(&term_df) = df.get(q.as_str()) else { continue };
            let idf = (1.0 + (n - term_df as f64 + 0.5) / (term_df as f64 + 0.5)).ln();
            let f = tf.get(q.as_str()).copied().unwrap_or(0) as f64;
            if f > 0.0 {
                score += idf * (f * (K1 + 1.0)) / (f + K1 * (1.0 - B + B * dl / avgdl.max(1.0)));
            }
        }
        if score > 0.0 {
            scored.push((i, score));
        }
    }

    if scored.is_empty() {
        return (blocks.iter().collect(), true); // no hits → full, SIGNAL it
    }

    // Keep blocks above a fraction of the max score, in doc order.
    let max_score = scored.iter().map(|(_, s)| *s).fold(0.0, f64::max);
    let threshold = max_score * 0.15;
    let kept = scored
        .into_iter()
        .filter(|(_, s)| *s >= threshold)
        .map(|(i, _)| &blocks[i])
        .collect();
    (kept, false)
}
