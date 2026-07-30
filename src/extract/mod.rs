//! DonSift — the extraction engine of DonSeTch.
//!
//! HTML bytes in → agent-native markdown out. Block model, not
//! article text: typed blocks with heading breadcrumbs power BM25
//! focus, stable pagination, and token-war rendering policies.
//!
//! Pipeline: decode charset → parse once (no mutation) → metadata →
//! scope (selector or density-scored main) → segment blocks →
//! focus filter → render markdown → paginate.

mod blocks;
mod charset;
mod focus;
mod inline;
mod junk;
mod metadata;
mod render;
mod score;

use scraper::Html;

pub struct ExtractOptions {
    /// BM25 relevance query: keep only blocks matching, with context.
    pub focus: Option<String>,
    /// CSS selector: extract only from matching subtrees.
    pub selector: Option<String>,
    /// Max chars of markdown to return (default 16_000).
    pub max_chars: Option<usize>,
    /// Resume offset into the (post-focus) markdown.
    pub offset: usize,
    /// Keep [text](url) links; default strips to text (token saver).
    pub include_links: bool,
    /// Keep ![alt](src) media lines; default drops them.
    pub include_media: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            focus: None,
            selector: None,
            max_chars: None,
            offset: 0,
            include_links: false,
            include_media: false,
        }
    }
}

pub struct Extracted {
    pub markdown: String,
    pub title: Option<String>,
    // byline/published/site are rendered into the markdown
    // frontmatter; the MCP layer also reads them directly.
    #[allow(dead_code)]
    pub byline: Option<String>,
    #[allow(dead_code)]
    pub published: Option<String>,
    #[allow(dead_code)]
    pub site: Option<String>,
    /// Full markdown length after focus, before pagination.
    pub total_chars: usize,
    pub next_offset: Option<usize>,
    pub blocks_total: usize,
    pub blocks_shown: usize,
    /// Rough token estimate (chars / 4) of the returned markdown.
    pub tokens_est: usize,
}

#[derive(Debug)]
pub enum ExtractError {
    BadSelector(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::BadSelector(s) => write!(f, "invalid CSS selector: {s}"),
        }
    }
}

/// Extract agent-ready markdown from a fetched body.
///
/// `content_type` is the raw Content-Type header value (may be
/// empty). Non-HTML bodies pass through (truncated by max_chars).
pub fn extract(
    body: &[u8],
    content_type: &str,
    url: &str,
    opts: &ExtractOptions,
) -> Result<Extracted, ExtractError> {
    let max_chars = opts.max_chars.unwrap_or(16_000).max(200);

    // Non-HTML passthrough (json/text/xml): no extraction lies.
    let ct = content_type.to_lowercase();
    if !ct.is_empty() && !ct.contains("html") {
        let text = String::from_utf8_lossy(body);
        let (slice, next) = paginate(&text, opts.offset, max_chars);
        return Ok(Extracted {
            tokens_est: slice.len() / 4,
            total_chars: text.len(),
            markdown: slice,
            title: None,
            byline: None,
            published: None,
            site: None,
            next_offset: next,
            blocks_total: 0,
            blocks_shown: 0,
        });
    }

    let html_text = charset::decode(body, &ct);
    let doc = Html::parse_document(&html_text);
    let base = metadata::base_url(&doc).unwrap_or_else(|| url.to_string());
    let meta = metadata::metadata(&doc);

    // Scope: explicit selector or scored main-content detection.
    let roots: Vec<scraper::ElementRef<'_>> = if let Some(sel) = &opts.selector {
        let parsed = scraper::Selector::parse(sel)
            .map_err(|_| ExtractError::BadSelector(sel.clone()))?;
        doc.select(&parsed).collect()
    } else {
        score::find_main(&doc).into_iter().collect()
    };

    // Segment into typed blocks.
    let mut all_blocks = Vec::new();
    for root in &roots {
        blocks::segment(*root, &base, opts, &mut all_blocks);
    }
    let blocks_total = all_blocks.len();

    // Focus: BM25 block filter (falls back to full content on no hit).
    let kept: Vec<&blocks::Block> = match &opts.focus {
        Some(q) => focus::filter(&all_blocks, q),
        None => all_blocks.iter().collect(),
    };
    let blocks_shown = kept.len();

    // Render markdown (frontmatter + blocks) then paginate.
    let full = render::render(&meta, url, &kept, opts);
    let (slice, next) = paginate(&full, opts.offset, max_chars);
    let tokens_est = slice.len() / 4;

    Ok(Extracted {
        markdown: slice,
        title: meta.title,
        byline: meta.byline,
        published: meta.published,
        site: meta.site,
        total_chars: full.len(),
        next_offset: next,
        blocks_total,
        blocks_shown,
        tokens_est,
    })
}

/// Char-budget slice at a UTF-8 boundary, preferring a block
/// boundary ("\n\n") near the cut. Returns (slice, next_offset).
fn paginate(text: &str, offset: usize, max_chars: usize) -> (String, Option<usize>) {
    if offset >= text.len() {
        return (String::new(), None);
    }
    let start = ceil_char_boundary(text, offset);
    let mut end = (start + max_chars).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        // Prefer a block boundary within the last quarter of the window.
        let window_start = start + (end - start) * 3 / 4;
        if let Some(pos) = text[window_start..end].rfind("\n\n") {
            end = window_start + pos;
        }
    }
    let next = if end < text.len() { Some(end) } else { None };
    (text[start..end].to_string(), next)
}

fn ceil_char_boundary(text: &str, mut i: usize) -> usize {
    if i >= text.len() {
        return text.len();
    }
    while !text.is_char_boundary(i) {
        i += 1;
    }
    i
}
