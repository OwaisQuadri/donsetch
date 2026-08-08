//! Tool schemas for the MCP tools/list response.
//!
//! Descriptions are LLM-optimized: dense, self-contained,
//! and actionable. An agent reading only the description
//! (never our source) should know exactly when to call,
//! which params to set, and how to interpret the response.

use serde_json::{Value, json};

/// Protocol versions we speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &[
    "2026-07-28",
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];

pub const SERVER_NAME: &str = "donsetch";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// tools/list payload.
pub fn list() -> Value {
    json!({
        "tools": [{
            "name": "fetch",
            "description": "Fetch one URL as clean markdown — use when you have a specific URL to read. For finding URLs, use search; for multi-page sites, use crawl.\n\nAuto-escalation: fast HTTP first; on bot-wall or JS-only page, opens a headless browser, solves the challenge, downgrades back. PDFs auto-detected and parsed (text + OCR for scanned). Non-HTML (JSON/XML/text) passes through.\n\nToken savers: focus returns only BM25-relevant blocks (if nothing matches, returns full page with a notice); links and images stripped by default (enable with links=true, media=true).\n\nLong-page workflow: toc=true → heading outline, then section=\"heading\" → that section only. Or use focus to get relevant blocks.\n\nPagination: if structuredContent.next_offset is present, call again with offset=that value.\n\nResponse: content[0].text = markdown; structuredContent = {status, tier, verdict, thin, content_kind, title, byline, published, site, blocks_shown, blocks_total, total_chars, next_offset, tokens_est, url}. thin=true = JS shell (content may be incomplete). content_kind: Article|Listing|Forum|Docs|Table|Page. isError=true on failure (blocked, captcha, network).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "http(s) URL to fetch."
                    },
                    "focus": {
                        "type": "string",
                        "description": "BM25 query — return only blocks relevant to it. #1 token saver on long pages. If nothing matches, returns full page with a notice."
                    },
                    "max_chars": {
                        "type": "integer",
                        "description": "Max markdown chars (default 16000). Truncated pages include next_offset for resumption."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Resume from a previous response's next_offset to continue a truncated page."
                    },
                    "section": {
                        "type": "string",
                        "description": "Heading name (substring, case-insensitive) — return only that section. Use after toc to target a specific part."
                    },
                    "toc": {
                        "type": "boolean",
                        "description": "true = heading outline only, no body text. Read structure first, then target with section or focus."
                    },
                    "selector": {
                        "type": "string",
                        "description": "CSS selector — extract only from matching elements. Narrows scope precisely."
                    },
                    "tier": {
                        "type": "string",
                        "enum": ["auto", "1", "2"],
                        "description": "auto (default): HTTP first, browser escalation on wall/JS-shell. \"1\": HTTP only (fastest, fails on JS sites). \"2\": browser directly (slower, for known JS-heavy sites)."
                    },
                    "links": {
                        "type": "boolean",
                        "description": "Include [text](url) link URLs. Default false — saves ~30% tokens. Enable only when you need the URLs."
                    },
                    "media": {
                        "type": "boolean",
                        "description": "Include image alt text and sources. Default false."
                    },
                    "shot": {
                        "type": "string",
                        "description": "File path — saves a PNG screenshot when blocked by interactive captcha. Only fires on captcha walls; not a general screenshot tool."
                    }
                },
                "required": ["url"]
            }
        }, {
            "name": "search",
            "description": "Web search — returns URLs + titles + short snippets. Use to discover WHAT to fetch, not to read content (use fetch for content). Multi-engine (independent indexes + Bing family) fused by cross-engine consensus + semantic reranking (automatic, no config). Keyless verticals: GitHub, Wikipedia, HN, Scholar, news, StackExchange, MDN.\n\nResponse: content[0].text = numbered markdown list (N. **Title** — domain / snippet / URL). structuredContent = {intent, weak, cached, elapsed_ms, results: [{title, url, snippet, score, consensus, engines}], engines: [{engine, status, hits, ms}]}.\n\nKey signals: weak=true = low cross-engine consensus, treat with care. consensus = how many independent engines returned this URL (higher = more authoritative). engines[].status shows per-engine health (ok|blocked:NNN|timeout|no-results).\n\nAfter search, use fetch on the best URL(s) to get actual content. Default 7 results is usually enough — don't increase unless the first 7 are all weak.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query."
                    },
                    "max_results": {
                        "type": "integer",
                        "description": "Max results (default 7, max 12). The most relevant results almost always live in the top 7. Increase only when results are weak."
                    },
                    "intent": {
                        "type": "string",
                        "enum": ["auto", "web", "code", "paper", "news", "entity"],
                        "description": "auto (default) detects from query. code: adds GitHub, HN, StackExchange, MDN verticals. paper: adds Scholar, arXiv. news: adds Google News, HN. entity: adds Wikipedia. web: general only."
                    }
                },
                "required": ["query"]
            }
        }, {
            "name": "crawl",
            "description": "Crawl an entire site from a seed URL — for multi-page extraction (docs, API refs, wikis). For a single page, use fetch; for finding pages across the web, use search.\n\nTwo-phase: sitemap discovery first (cheap URL inventory), then fetch focus-ranked pages as markdown. Adaptive pacing per host prevents rate-limit triggers.\n\nModes: full (default) = sitemap map + content. map = URL inventory only (very cheap, no content — use to see what a site has before committing). content = skip sitemap, BFS from seed (use when sitemap is missing).\n\nBudget control: focus ranks pages by relevance and crawls only matching ones — essential for large sites. max_pages, max_total_chars, deadline_s cap the crawl. Resume tokens let you continue large crawls across calls.\n\nResponse: content[0].text = map (if any) + pages as markdown. structuredContent = {seed, pages: [{url, title, kind, chars, quality}], map, queued, filtered_out, skipped: [{url, reason}], stop, elapsed_s, resume}.\n\nstop = why crawl stopped: FrontierEmpty (done), MaxPages|CharBudget|DepthLimit|Deadline (budget — use resume to continue), ThrottledOut (site blocked you — wait and resume). resume = token to continue when stopped by budget/deadline. quality = 0.0-1.0 content trust per page.",
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
                        "description": "full (default): sitemap map + content. map: URL inventory only (very cheap). content: skip sitemap, BFS from seed."
                    },
                    "focus": {
                        "type": "string",
                        "description": "BM25 query — rank pages by relevance, crawl only matching ones. Essential for large sites; without it the crawl wastes budget on noise."
                    },
                    "max_pages": {
                        "type": "integer",
                        "description": "Max pages to fetch+extract (default 10, cap 200)."
                    },
                    "max_depth": {
                        "type": "integer",
                        "description": "Max link depth from seed (default 2). 0 = seed only."
                    },
                    "max_total_chars": {
                        "type": "integer",
                        "description": "Total extracted-char budget across all pages (default 60000, range 4000-500000)."
                    },
                    "per_page_max": {
                        "type": "integer",
                        "description": "Max markdown chars per page (default 8000, range 400-40000)."
                    },
                    "include_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path globs to include (e.g. [\"/docs/*\"]). Empty = all."
                    },
                    "exclude_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path globs to exclude (e.g. [\"*/tags/*\", \"*/archive/*\"])."
                    },
                    "same_host": {
                        "type": "boolean",
                        "description": "Stay on seed's host (default true). false = follow cross-domain links."
                    },
                    "respect_robots": {
                        "type": "boolean",
                        "description": "Obey robots.txt Disallow + crawl-delay (default true)."
                    },
                    "deadline_s": {
                        "type": "integer",
                        "description": "Hard crawl deadline in seconds (default 120, range 5-600). Partial results return after."
                    },
                    "resume": {
                        "type": "string",
                        "description": "Resume token from a previous response to continue a stopped crawl. Valid for 30 min."
                    }
                },
                "required": ["url"]
            }
        }]
    })
}
