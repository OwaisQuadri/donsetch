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
        }]
    })
}
