//! Tool spec table — the single source of truth for the three
//! agent tools (fetch, search, crawl).
//!
//! Both frontends are GENERATED from this table:
//!
//! - MCP: `mcp_schema()` builds the tools/list JSON schema.
//! - CLI: `cli_command()` builds the clap subcommand, and
//!   `matches_to_json()` converts parsed argv back into the
//!   exact JSON args the MCP dispatcher receives.
//!
//! Maintenance rule: adding, removing, or changing a parameter
//! happens HERE, once. Both interfaces update together. The tool
//! functions in `mcp/server.rs` hold all logic; the adapters
//! (MCP stdio loop, CLI renderer) hold none.
//!
//! Defaults are NOT duplicated into clap: unset flags are simply
//! absent from the generated JSON, so the core's own defaults
//! remain the single default source.

use clap::{Arg, ArgAction};
use serde_json::{Value, json};

// ── Types ────────────────────────────────────────────────────

/// Parameter value kind. Drives both the JSON schema type and
/// the clap value parser.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    /// JSON string; CLI `--flag <value>`.
    Str,
    /// JSON integer; CLI `--flag <N>` (usize-parsed).
    Usize,
    /// JSON string from a fixed set; CLI validated choices.
    Enum(&'static [&'static str]),
    /// JSON array of strings; CLI repeatable + comma-splittable.
    StrList,
    /// JSON boolean; CLI flag whose presence sets `true`.
    SetTrue,
    /// JSON boolean; CLI flag whose presence sets `false`
    /// (negating flags like --any-host for same_host).
    SetFalse,
    /// JSON value passed through as-is (array of objects);
    /// CLI takes a JSON string and parses it.
    JsonStr,
}

/// How the parameter appears on the CLI.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CliKind {
    /// `--flag value` style option.
    Flag,
    /// Single positional argument (`<url>` for crawl).
    PositionalSingle,
    /// Variadic positional joined with spaces (`<query>...`).
    PositionalJoined,
    /// Variadic positional where the first value fills the JSON
    /// param and the rest are handled by the CLI adapter
    /// (`<url> [more-urls...]` bulk fetch).
    PositionalBulk,
}

pub struct ParamSpec {
    /// JSON argument name (MCP schema property).
    pub name: &'static str,
    /// CLI long flag (`--focus`). Ignored for positionals.
    pub flag: &'static str,
    pub kind: ParamKind,
    pub cli: CliKind,
    pub required: bool,
    /// Description string — used verbatim as the MCP schema
    /// description AND the clap help text. One string, both
    /// interfaces.
    pub help: &'static str,
}

pub struct ToolSpec {
    /// MCP tool name (`web_fetch`).
    pub name: &'static str,
    /// CLI subcommand (`fetch`).
    pub cli_cmd: &'static str,
    /// One-liner for `donsetch --help` listing.
    pub summary: &'static str,
    /// Full description — MCP tool description AND CLI long help.
    pub description: &'static str,
    pub params: &'static [ParamSpec],
    /// Copy-pasteable CLI examples, shown in `--help` epilog.
    pub examples: &'static [&'static str],
}

// ── web_fetch ────────────────────────────────────────────────

const FETCH_PARAMS: &[ParamSpec] = &[
    ParamSpec {
        name: "url",
        flag: "",
        kind: ParamKind::Str,
        cli: CliKind::PositionalBulk,
        required: true,
        help: "http(s) URL to fetch.",
    },
    ParamSpec {
        name: "focus",
        flag: "focus",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "Relevance query — returns ONLY blocks relevant to your question, cutting tokens 50-80% on long pages. Hybrid keyword + semantic matching catches blocks with different vocabulary than your query. If nothing matches, returns full page with a notice. ALWAYS set when you know what you're looking for — #1 token saver.",
    },
    ParamSpec {
        name: "max_chars",
        flag: "max-chars",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Max markdown chars (default 16000). Truncated pages include next_offset for resumption.",
    },
    ParamSpec {
        name: "offset",
        flag: "offset",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Resume from a previous response's next_offset to continue a truncated page.",
    },
    ParamSpec {
        name: "section",
        flag: "section",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "Heading name (substring, case-insensitive) — return only that section. Use after toc to target a specific part.",
    },
    ParamSpec {
        name: "toc",
        flag: "toc",
        kind: ParamKind::SetTrue,
        cli: CliKind::Flag,
        required: false,
        help: "true = heading outline only, no body text. Read structure first, then target with section or focus.",
    },
    ParamSpec {
        name: "selector",
        flag: "selector",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "CSS selector — extract only from matching elements. Narrows scope precisely.",
    },
    ParamSpec {
        name: "tier",
        flag: "tier",
        kind: ParamKind::Enum(&["auto", "1", "2"]),
        cli: CliKind::Flag,
        required: false,
        help: "auto (default, always use for real work): HTTP first, auto-escalates to headless browser on bot-walls/JS-shells, auto-detects and parses PDFs. \"1\" (testing): HTTP only, no browser — fails on JS sites. \"2\" (testing): browser directly — slower, skips HTTP entirely.",
    },
    ParamSpec {
        name: "links",
        flag: "links",
        kind: ParamKind::SetTrue,
        cli: CliKind::Flag,
        required: false,
        help: "Include [text](url) link URLs. Default false — saves ~30% tokens. Enable only when you need the URLs.",
    },
    ParamSpec {
        name: "media",
        flag: "media",
        kind: ParamKind::SetTrue,
        cli: CliKind::Flag,
        required: false,
        help: "Include image alt text and sources. Default false.",
    },
    ParamSpec {
        name: "actions",
        flag: "actions",
        kind: ParamKind::JsonStr,
        cli: CliKind::Flag,
        required: false,
        help: "Browser steps to run BEFORE extraction — page control inside fetch: [{\"do\":\"click\",\"selector\":\"#load-more\"},{\"do\":\"type\",\"selector\":\"input[q]\",\"text\":\"query\"},{\"do\":\"press\",\"key\":\"Enter\"},{\"do\":\"wait_text\",\"text\":\"results\"}]. Steps: wait {ms}, wait_selector {selector,timeout_ms}, wait_text {text,timeout_ms}, click {selector OR text}, hover, type {selector?,text}, press {key: Enter|Tab|Escape|Backspace|ArrowDown|...}, scroll {to: top|bottom|down | px}. Max 16 steps. Actions run in the headless browser (tier auto/2, never 1); after them the page is extracted normally — focus/section/toc still apply. First failing step aborts honestly with per-step results in structuredContent.actions; fix that step and re-run.",
    },
    ParamSpec {
        name: "shot",
        flag: "shot",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "File path — saves a PNG screenshot when blocked by interactive captcha. Only fires on captcha walls; not a general screenshot tool.",
    },
];

// ── web_search ───────────────────────────────────────────────

const SEARCH_PARAMS: &[ParamSpec] = &[
    ParamSpec {
        name: "query",
        flag: "",
        kind: ParamKind::Str,
        cli: CliKind::PositionalJoined,
        required: true,
        help: "Search query.",
    },
    ParamSpec {
        name: "max_results",
        flag: "max-results",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Max results (default 7, max 12). The most relevant results almost always live in the top 7. Increase only when results are weak.",
    },
    ParamSpec {
        name: "intent",
        flag: "intent",
        kind: ParamKind::Enum(&["auto", "web", "code", "paper", "news", "entity"]),
        cli: CliKind::Flag,
        required: false,
        help: "auto (default) detects from query. code: adds GitHub, HN, StackExchange, MDN verticals. paper: adds Scholar, arXiv. news: adds Google News, HN. entity: adds Wikipedia. web: general only.",
    },
];

// ── web_crawl ────────────────────────────────────────────────

const CRAWL_PARAMS: &[ParamSpec] = &[
    ParamSpec {
        name: "url",
        flag: "",
        kind: ParamKind::Str,
        cli: CliKind::PositionalSingle,
        required: true,
        help: "Seed http(s) URL to crawl from.",
    },
    ParamSpec {
        name: "mode",
        flag: "mode",
        kind: ParamKind::Enum(&["full", "map", "content"]),
        cli: CliKind::Flag,
        required: false,
        help: "full (default): sitemap map + content. map: URL inventory only (very cheap). content: skip sitemap, BFS from seed.",
    },
    ParamSpec {
        name: "focus",
        flag: "topic",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "Relevance query — rank pages by relevance and crawl only matching ones. Uses hybrid keyword + semantic matching. Essential for large sites; without it the crawl wastes budget on noise. Set this whenever you have a specific topic in mind.",
    },
    ParamSpec {
        name: "max_pages",
        flag: "max-pages",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Max pages to fetch+extract (default 10, cap 200).",
    },
    ParamSpec {
        name: "max_depth",
        flag: "max-depth",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Max link depth from seed (default 2). 0 = seed only.",
    },
    ParamSpec {
        name: "max_total_chars",
        flag: "max-chars",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Total extracted-char budget across all pages (default 60000, range 4000-500000).",
    },
    ParamSpec {
        name: "per_page_max",
        flag: "per-page-max",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Max markdown chars per page (default 8000, range 400-40000).",
    },
    ParamSpec {
        name: "include_paths",
        flag: "include",
        kind: ParamKind::StrList,
        cli: CliKind::Flag,
        required: false,
        help: "Path globs to include (e.g. [\"/docs/*\"]). Empty = all.",
    },
    ParamSpec {
        name: "exclude_paths",
        flag: "exclude",
        kind: ParamKind::StrList,
        cli: CliKind::Flag,
        required: false,
        help: "Path globs to exclude (e.g. [\"*/tags/*\", \"*/archive/*\"]).",
    },
    ParamSpec {
        name: "same_host",
        flag: "any-host",
        kind: ParamKind::SetFalse,
        cli: CliKind::Flag,
        required: false,
        help: "Stay on seed's host (default true). false = follow cross-domain links.",
    },
    ParamSpec {
        name: "respect_robots",
        flag: "no-robots",
        kind: ParamKind::SetFalse,
        cli: CliKind::Flag,
        required: false,
        help: "Obey robots.txt Disallow + crawl-delay (default true).",
    },
    ParamSpec {
        name: "deadline_s",
        flag: "deadline",
        kind: ParamKind::Usize,
        cli: CliKind::Flag,
        required: false,
        help: "Hard crawl deadline in seconds (default 120, range 5-600). Partial results return after.",
    },
    ParamSpec {
        name: "resume",
        flag: "resume",
        kind: ParamKind::Str,
        cli: CliKind::Flag,
        required: false,
        help: "Resume token from a previous response to continue a stopped crawl. Valid for 30 min.",
    },
];

// ── The table ────────────────────────────────────────────────

pub static TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "web_fetch",
        cli_cmd: "fetch",
        summary: "Fetch a URL as clean markdown (auto bot-wall bypass, PDF, JS render)",
        description: "Fetch one URL as clean markdown — use when you have a specific URL to read. For finding URLs, use web_search; for multi-page sites, use web_crawl.\n\nAuto-escalation: fast HTTP first; on bot-wall or JS-only page, opens a headless browser, solves the challenge, downgrades back. PDFs auto-detected (content-type or magic bytes) and parsed by DonSheet — text extraction + OCR for scanned pages, up to 100MB. Non-HTML (JSON/XML/text) passes through.\n\nPage interaction (actions): pass actions=[{...}] to click, type, press, scroll, or wait inside the real page before extraction — form submits, search boxes, load-more buttons, lazy-load scrolls. Deterministic waits (wait_selector/wait_text) beat blind sleeps. After actions, extraction runs normally (focus/section/toc apply).\n\nLong-page workflow: toc=true → heading outline, then section=\"heading\" → that section only. Or use focus to get relevant blocks.\n\nPagination: if structuredContent.next_offset is present, call again with offset=that value.\n\nResponse: content[0].text = markdown; structuredContent = {status, tier, verdict, content_ok, thin, content_kind, quality (0-1), lang, title, byline, published, site, blocks_shown, blocks_total, total_chars, next_offset, tokens_est, escalation (what was tried, per-step with ms), pdf ({pages, per_page:[{page,chars,ocr,confidence}]} for PDFs), actions (per-step results when actions used), url}. content_ok=false or thin=true = content may be a JS shell. content_kind: Article|Listing|Forum|Docs|Table|Page. isError=true on failure with structuredContent {url, status, verdict, next_action, escalation} — next_action tells you exactly what to do next.",
        params: FETCH_PARAMS,
        examples: &[
            "donsetch fetch https://example.com/article",
            "donsetch fetch https://long-docs-page --focus \"error handling\"",
            "donsetch fetch https://long-docs-page --offset 16000",
            "donsetch fetch https://a.com/x https://b.com/y   # bulk fetch",
            "donsetch fetch https://site.com/search --actions '[{\"do\":\"type\",\"selector\":\"input[q]\",\"text\":\"rust async\"},{\"do\":\"press\",\"key\":\"Enter\"},{\"do\":\"wait_text\",\"text\":\"results\"}]'",
        ],
    },
    ToolSpec {
        name: "web_search",
        cli_cmd: "search",
        summary: "Web search — 5 keyless engines merged + reranked, or your API keys",
        description: "Web search — returns URLs + titles + short snippets. Use to discover WHAT to fetch, not to read content (use web_fetch for content). Multi-engine (independent indexes + Bing family) fused by cross-engine consensus + semantic reranking (automatic, no config). Keyless verticals: GitHub, Wikipedia, HN, Scholar, news, StackExchange, MDN.\n\nBYOK: if external search providers (Tavily, Exa, Serper, TinyFish) are configured via `donsetch keys`, the local engine is bypassed and the configured provider handles search. structuredContent.provider shows which provider was used (null = local engine). Falls back to local engine if all provider keys are exhausted.\n\nResponse: content[0].text = numbered markdown list (N. **Title** — domain / snippet / URL). structuredContent = {intent, weak, cached, elapsed_ms, provider, results: [{title, url, snippet, score, consensus, engines}], engines: [{engine, status, hits, ms}]}.\n\nKey signals: weak=true = low cross-engine consensus, treat with care. consensus = how many independent engines returned this URL (higher = more authoritative). engines[].status shows per-engine health (ok|blocked:NNN|timeout|no-results). provider = which search provider was used (null = local keyless engine).\n\nAfter search, use fetch on the best URL(s) to get actual content.",
        params: SEARCH_PARAMS,
        examples: &[
            "donsetch search rust async trait objects",
            "donsetch search \"exact phrase\" --intent code",
            "donsetch search site:github.com tokio --max-results 10",
        ],
    },
    ToolSpec {
        name: "web_crawl",
        cli_cmd: "crawl",
        summary: "Crawl a site into markdown (sitemap-aware, focus-ranked, resumable)",
        description: "Crawl an entire site from a seed URL — for multi-page extraction (docs, API refs, wikis). For a single page, use web_fetch; for finding pages across the web, use web_search.\n\nTwo-phase: sitemap discovery first (cheap URL inventory), then fetch focus-ranked pages as markdown. Adaptive pacing per host prevents rate-limit triggers.\n\nModes: full (default) = sitemap map + content. map = URL inventory only (very cheap, no content — use to see what a site has before committing). content = skip sitemap, BFS from seed (use when sitemap is missing or map returns empty).

If no sitemap is found, map mode returns guidance to use mode=content.

PDF pages: auto-detected and extracted (same engine as web_fetch). Not skipped.\n\nBudget control: focus (topic) ranks pages by hybrid keyword + semantic relevance and crawls only matching ones — essential for large sites. Set it whenever you have a specific topic in mind. max_pages, max_total_chars, deadline_s cap the crawl. Resume tokens let you continue large crawls across calls.\n\nResponse: content[0].text = map (if any) + pages as markdown. structuredContent = {seed, pages: [{url, title, kind, chars, quality}], map, queued, filtered_out, skipped: [{url, reason}], stop, elapsed_s, resume}.\n\nstop = why crawl stopped: FrontierEmpty (done), MaxPages|CharBudget|DepthLimit|Deadline (budget — use resume to continue), ThrottledOut (site blocked you — wait and resume). resume = token to continue when stopped by budget/deadline. quality = 0.0-1.0 content trust per page.",
        params: CRAWL_PARAMS,
        examples: &[
            "donsetch crawl https://docs.site.com --topic \"authentication\"",
            "donsetch crawl https://docs.site.com --mode map",
            "donsetch crawl https://docs.site.com --max-pages 25 --deadline 300",
        ],
    },
];

/// Look up a tool spec by CLI subcommand name.
pub fn by_cli_cmd(cmd: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.cli_cmd == cmd)
}

// ── MCP schema generation ────────────────────────────────────

/// Build the tools/list entry for one tool. Output is identical
/// in shape to the historical hand-written schema (pinned by the
/// golden fixture test).
pub fn mcp_schema(tool: &ToolSpec) -> Value {
    let mut props = serde_json::Map::new();
    let mut required = Vec::new();
    for p in tool.params {
        let mut schema = serde_json::Map::new();
        let ty = match p.kind {
            ParamKind::Str | ParamKind::Enum(_) => "string",
            ParamKind::Usize => "integer",
            ParamKind::StrList | ParamKind::JsonStr => "array",
            ParamKind::SetTrue | ParamKind::SetFalse => "boolean",
        };
        schema.insert("type".into(), json!(ty));
        if let ParamKind::Enum(variants) = p.kind {
            schema.insert("enum".into(), json!(variants));
        }
        if p.kind == ParamKind::StrList {
            schema.insert("items".into(), json!({ "type": "string" }));
        }
        if p.kind == ParamKind::JsonStr {
            schema.insert("items".into(), json!({ "type": "object" }));
        }
        schema.insert("description".into(), json!(p.help));
        props.insert(p.name.into(), Value::Object(schema));
        if p.required {
            required.push(json!(p.name));
        }
    }
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(props),
            "required": required,
        }
    })
}

// ── CLI generation ───────────────────────────────────────────

/// Build the clap subcommand for one tool. `--json` and
/// `--quiet` are CLI-adapter flags (not MCP params), appended
/// to every tool command.
pub fn cli_command(tool: &ToolSpec) -> clap::Command {
    let mut cmd = clap::Command::new(tool.cli_cmd)
        .about(tool.summary)
        .long_about(tool.description)
        .after_help(format!(
            "EXAMPLES:\n{}",
            tool.examples
                .iter()
                .map(|e| format!("  {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    for p in tool.params {
        cmd = cmd.arg(cli_arg(p));
    }
    cmd.arg(
        Arg::new("json")
            .long("json")
            .action(ArgAction::SetTrue)
            .help("Print the full JSON envelope on stdout (content + all metadata)."),
    )
    .arg(
        Arg::new("quiet")
            .long("quiet")
            .short('q')
            .action(ArgAction::SetTrue)
            .help("Suppress the stderr stats line."),
    )
}

fn cli_arg(p: &ParamSpec) -> Arg {
    let arg = Arg::new(p.name).help(p.help);
    match p.cli {
        CliKind::PositionalSingle => arg.required(p.required),
        CliKind::PositionalJoined | CliKind::PositionalBulk => {
            arg.required(p.required).num_args(1..)
        }
        CliKind::Flag => {
            let arg = arg.long(p.flag);
            match p.kind {
                ParamKind::Str => arg.value_name("VALUE"),
                ParamKind::Usize => arg.value_name("N").value_parser(clap::value_parser!(usize)),
                ParamKind::Enum(variants) => {
                    arg.value_name("VALUE")
                        .value_parser(clap::builder::PossibleValuesParser::new(
                            variants.iter().copied(),
                        ))
                }
                ParamKind::JsonStr => arg.value_name("JSON"),
                ParamKind::StrList => arg
                    .value_name("GLOB")
                    .action(ArgAction::Append)
                    .value_delimiter(','),
                ParamKind::SetTrue => arg.action(ArgAction::SetTrue),
                ParamKind::SetFalse => arg.action(ArgAction::SetFalse),
            }
        }
    }
}

/// Convert parsed CLI matches into the exact JSON args Value
/// the MCP dispatcher receives. Unset flags are omitted — the
/// core applies its own defaults (single default source).
pub fn matches_to_json(tool: &ToolSpec, m: &clap::ArgMatches) -> Value {
    let mut map = serde_json::Map::new();
    for p in tool.params {
        match p.cli {
            CliKind::PositionalSingle | CliKind::PositionalBulk => {
                if let Some(v) = m.get_one::<String>(p.name) {
                    map.insert(p.name.into(), json!(v));
                }
            }
            CliKind::PositionalJoined => {
                let words: Vec<&str> = m
                    .get_many::<String>(p.name)
                    .map(|v| v.map(String::as_str).collect())
                    .unwrap_or_default();
                if !words.is_empty() {
                    map.insert(p.name.into(), json!(words.join(" ")));
                }
            }
            CliKind::Flag => match p.kind {
                ParamKind::Str | ParamKind::Enum(_) => {
                    if let Some(v) = m.get_one::<String>(p.name) {
                        map.insert(p.name.into(), json!(v));
                    }
                }
                ParamKind::JsonStr => {
                    if let Some(v) = m.get_one::<String>(p.name)
                        && let Ok(parsed) = serde_json::from_str::<Value>(v)
                    {
                        map.insert(p.name.into(), parsed);
                    }
                }
                ParamKind::Usize => {
                    if let Some(v) = m.get_one::<usize>(p.name) {
                        map.insert(p.name.into(), json!(v));
                    }
                }
                ParamKind::StrList => {
                    let items: Vec<&str> = m
                        .get_many::<String>(p.name)
                        .map(|v| v.map(String::as_str).collect())
                        .unwrap_or_default();
                    if !items.is_empty() {
                        map.insert(p.name.into(), json!(items));
                    }
                }
                ParamKind::SetTrue => {
                    if m.get_flag(p.name) {
                        map.insert(p.name.into(), json!(true));
                    }
                }
                ParamKind::SetFalse => {
                    if !m.get_flag(p.name) {
                        map.insert(p.name.into(), json!(false));
                    }
                }
            },
        }
    }
    Value::Object(map)
}
