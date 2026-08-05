//! Tool schemas + the fetch handler.

use serde_json::{Value, json};

/// Protocol versions we speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] =
    &["2026-07-28", "2025-06-18", "2025-03-26", "2024-11-05"];

pub const SERVER_NAME: &str = "donsetch";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// tools/list payload.
pub fn list() -> Value {
    json!({
        "tools": [{
            "name": "fetch",
            "description": concat!(
                "Fetch a web page as clean markdown. Two-tier: ",
                "fast stealth HTTP first; on a bot-wall or JS-only ",
                "page it auto-escalates to a real ghost browser, ",
                "solves the challenge, and downgrades back to fast ",
                "fetch. Returns markdown in content and machine ",
                "metadata (status, tier, verdict, next_offset, ",
                "tokens_est) in structuredContent. ",
                "Use focus for query-relevant blocks only; toc for ",
                "the heading outline then section to read one part; ",
                "paginate long pages with offset."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "http(s) URL to fetch."
                    },
                    "focus": {
                        "type": "string",
                        "description": "BM25 query — return only blocks relevant to it. On no match the full page is returned and the content says so."
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Max characters of markdown (default 16000). Long pages truncate with a next_offset marker."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Resume from a previous response's next_offset."
                    },
                    "section": {
                        "type": "string",
                        "description": "Heading name (substring, case-insensitive) — return only that section."
                    },
                    "toc": {
                        "type": "boolean",
                        "description": "true = heading outline only. Read structure first, then target with section."
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector to scope extraction."
                    },
                    "tier": {
                        "type": "string",
                        "enum": ["auto", "1", "2"],
                        "description": "auto (default, recommended): tier 1 first, ghost escalation on wall/JS-shell. 1 = fast HTTP only. 2 = ghost browser directly."
                    },
                    "links": {
                        "type": "boolean",
                        "description": "Include link URLs (default stripped, saves ~30% tokens)."
                    },
                    "media": {
                        "type": "boolean",
                        "description": "Include image alt text and sources."
                    },
                    "shot": {
                        "type": "string",
                        "description": "Absolute path — save a PNG of what the ghost saw (only when blocked)."
                    }
                },
                "required": ["url"]
            }
        }, {
            "name": "search",
            "description": concat!(
                "Web search: returns urls + titles + short ",
                "snippets — just enough to decide WHAT to fetch, ",
                "not the content itself (use the fetch tool for ",
                "that). Multi-engine (independent indexes + Bing ",
                "family) fused by cross-engine consensus ranking, ",
                "plus keyless verticals (GitHub, Wikipedia, HN, ",
                "Scholar, news). structuredContent carries urls, ",
                "scores, consensus counts, engine health. ",
                "weak=true means low consensus, treat carefully."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max results (default 7; the most relevant results almost always live within the top 7)."
                    },
                    "intent": {
                        "type": "string",
                        "enum": ["auto", "web", "code", "paper", "news", "entity"],
                        "description": "auto (default) detects intent. code adds GitHub+HN; paper adds Semantic Scholar; news adds Google News; entity adds Wikipedia."
                    }
                },
                "required": ["query"]
            }
        }, {
            "name": "crawl",
            "description": concat!(
                "Crawl a site from a seed URL: sitemap-map first ",
                "(cheap URL inventory), then fetch focus-ranked ",
                "pages as clean markdown through DonSift. The ",
                "Governor paces per (host, lane) with adaptive ",
                "backoff — crawl big sites without triggering ",
                "rate limits. Set focus to spend budget only on ",
                "relevant pages; mode=map for the URL inventory ",
                "alone; resume to continue a stopped crawl."
            ),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "Seed http(s) URL to crawl from."
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["full", "map", "content"],
                        "description": "full (default): map + content. map: URL inventory only (very cheap). content: skip sitemap, BFS from seed."
                    },
                    "focus": {
                        "type": "string",
                        "description": "BM25 query — rank frontier by relevance; crawl only what matters."
                    },
                    "max_pages": {
                        "type": "integer",
                        "description": "Max pages to fetch and extract (default 10, cap 200)."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Max link depth from the seed (default 2)."
                    },
                    "max_total_chars": {
                        "type": "integer",
                        "description": "Total extracted-characters budget across all pages (default 60000)."
                    },
                    "per_page_max": {
                        "type": "integer",
                        "description": "Max markdown characters per page (default 8000)."
                    },
                    "include_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path globs to include (e.g. [\"/docs/*\"]). Empty = all."
                    },
                    "exclude_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path globs to exclude (e.g. [\"*/tags/*\"])."
                    },
                    "same_host": {
                        "type": "boolean",
                        "description": "Stay on the seed's host (default true)."
                    },
                    "respect_robots": {
                        "type": "boolean",
                        "description": "Obey robots.txt Disallow + crawl-delay (default true)."
                    },
                    "deadline_s": {
                        "type": "integer",
                        "description": "Hard crawl deadline in seconds; partial results return after it (default 120)."
                    },
                    "resume": {
                        "type": "string",
                        "description": "Resume token from a previous response to continue the crawl."
                    }
                },
                "required": ["url"]
            }
        }]
    })
}
