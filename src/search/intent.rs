//! Intent detection — routes the query to the right
//! engines + verticals, and feeds domain priors to rank.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Web,
    Code,
    Paper,
    News,
    Entity,
}

const CODE_SIGNALS: &[&str] = &[
    "error", "exception", "traceback", "how to", "install", "npm ", "cargo ",
    "pip ", "github", "stackoverflow", "compile", "undefined", "null pointer",
    "segmentation fault", "syntax", "debug", "api", "library", "crate",
    "function", "regex", "docker", "kubernetes", "rust", "python", "javascript",
    "typescript", "golang", "c++", "java ",
];
const PAPER_SIGNALS: &[&str] = &[
    "paper", "arxiv", "study", "research", "doi", "journal", "citation",
    "transformer", "benchmark", "ablation", "dataset", "preprint",
];
const NEWS_SIGNALS: &[&str] = &[
    "news", "breaking", "latest", "today", "yesterday", "announced",
    "release", "launched", "dies", "election", "war", "stock",
];
const ENTITY_SIGNALS: &[&str] = &["what is", "who is", "who was", "define", "meaning of"];

pub fn detect(query: &str) -> Intent {
    let q = query.to_lowercase();
    let score = |signals: &[&str]| signals.iter().filter(|s| q.contains(**s)).count();
    let code = score(CODE_SIGNALS);
    let paper = score(PAPER_SIGNALS);
    let news = score(NEWS_SIGNALS);
    let entity = score(ENTITY_SIGNALS);
    let max = code.max(paper).max(news).max(entity);
    if max == 0 {
        // Short proper-noun-ish query → probably an entity.
        let words = query.split_whitespace().count();
        if words <= 3
            && query
                .split_whitespace()
                .filter(|w| w.chars().next().is_some_and(char::is_uppercase))
                .count()
                >= 1
        {
            return Intent::Entity;
        }
        return Intent::Web;
    }
    if code == max {
        Intent::Code
    } else if paper == max {
        Intent::Paper
    } else if news == max {
        Intent::News
    } else {
        Intent::Entity
    }
}

/// Engines to fan out per intent. Order = trust prior.
pub fn engines_for(intent: Intent) -> &'static [&'static str] {
    match intent {
        // The full web battery.
        Intent::Web | Intent::Code | Intent::News | Intent::Entity => {
            &["brave", "bing", "ddg", "mojeek"]
        }
        Intent::Paper => &["brave", "bing", "ddg"],
    }
}

/// Verticals to fan out per intent (keyless JSON APIs).
pub fn verticals_for(intent: Intent) -> &'static [&'static str] {
    match intent {
        Intent::Code => &["github", "hn"],
        Intent::Paper => &["scholar"],
        Intent::News => &["news", "hn"],
        Intent::Entity => &["wikipedia"],
        Intent::Web => &[],
    }
}

/// Domain quality prior per intent: 0.0..1.0 bonus mass.
/// Curated seed; the consensus signal usually dominates,
/// this just breaks ties toward known-good sources.
pub fn domain_prior(intent: Intent, host: &str) -> f64 {
    let h = host.strip_prefix("www.").unwrap_or(host);
    let table: &[&str] = match intent {
        Intent::Code => &[
            "stackoverflow.com", "github.com", "docs.rs", "developer.mozilla.org",
            "learn.microsoft.com", "doc.rust-lang.org", "pkg.go.dev",
            "pypi.org", "crates.io", "npmjs.com", "readthedocs.io",
            "superuser.com", "serverfault.com", "news.ycombinator.com",
        ],
        Intent::Paper => &[
            "arxiv.org", "semanticscholar.org", "scholar.google.com",
            "nature.com", "science.org", "acm.org", "ieee.org", "openreview.net",
            "pubmed.ncbi.nlm.nih.gov", "doi.org",
        ],
        Intent::News => &[
            "reuters.com", "apnews.com", "bbc.com", "bbc.co.uk", "nytimes.com",
            "theguardian.com", "arstechnica.com", "techcrunch.com",
            "news.ycombinator.com", "bloomberg.com", "wsj.com",
        ],
        Intent::Entity => &[
            "wikipedia.org", "britannica.com", "wikidata.org", "imdb.com",
        ],
        Intent::Web => &[],
    };
    if table.iter().any(|d| h == *d || h.ends_with(&format!(".{d}"))) {
        1.0
    } else {
        0.0
    }
}

/// A normalized recall variant of the query (strip
/// question scaffolding). Goes only to top-trust engines.
pub fn variant(query: &str) -> Option<String> {
    let mut q = query.to_string();
    for pre in ["how to ", "how do i ", "how do you ", "what is ", "who is ", "why does ", "why is "] {
        if q.to_lowercase().starts_with(pre) {
            q = q[pre.len()..].to_string();
            break;
        }
    }
    let q = q.trim_end_matches(['?', '.']).trim().to_string();
    if q.len() >= 4 && q != query {
        Some(q)
    } else {
        None
    }
}
