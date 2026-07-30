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
    /// Outline only: heading tree, no body text. Lets an
    /// agent read structure first, then target a section.
    pub toc: bool,
    /// Scope to one heading section (substring, case-
    /// insensitive). Pairs with toc.
    pub section: Option<String>,
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
            toc: false,
            section: None,
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
    /// True when the page was large but almost no content
    /// extracted — a JS shell. Tier 2's job.
    pub thin: bool,
    /// Best-guess content kind from block composition.
    /// Conservative: only non-Page when confident.
    pub content_kind: ContentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Article,
    Listing,
    Forum,
    Docs,
    Table,
    Page, // unsure
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

/// Classify content from block composition.
/// Conservative — Page when nothing is dominant.
fn classify(blocks: &[&blocks::Block]) -> ContentKind {
    let mut code = 0usize;
    let mut tables = 0usize;
    let mut lists = 0usize;
    let mut list_items = 0usize;
    let mut list_chars = 0usize;
    let mut quotes = 0usize;
    let mut para_chars = 0usize;
    let mut paras = 0usize;
    let mut headings = 0usize;
    for b in blocks {
        match b {
            blocks::Block::Code { .. } => code += 1,
            blocks::Block::Table { .. } => tables += 1,
            blocks::Block::List { items, .. } => {
                lists += 1;
                list_items += items.len();
                list_chars += items.iter().map(|i| i.len()).sum::<usize>();
            }
            blocks::Block::Quote { .. } => quotes += 1,
            blocks::Block::Para { md, .. } => {
                paras += 1;
                para_chars += md.len();
            }
            blocks::Block::Heading { .. } => headings += 1,
            _ => {}
        }
    }
    if code >= 3 {
        return ContentKind::Docs;
    }
    // Article = heading-STRUCTURED prose: several
    // headings, substantial paragraphs between them.
    // Char mass lies (reference lists outweigh prose).
    if headings >= 3 && paras >= 5 && para_chars / paras.max(1) > 150 {
        return ContentKind::Article;
    }
    if tables >= 2 && tables >= paras {
        return ContentKind::Table;
    }
    if quotes >= 5 {
        return ContentKind::Forum;
    }
    if lists >= 3 && list_items >= 12 && list_chars > paras * 200 {
        return ContentKind::Listing;
    }
    if paras >= 3 && para_chars / paras.max(1) > 200 {
        return ContentKind::Article;
    }
    ContentKind::Page
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
            thin: false,
            content_kind: ContentKind::Page,
        });
    }

    let html_text = charset::decode(body, &ct);
    let raw_len = body.len();
    let doc = Html::parse_document(&html_text);
    let base = metadata::base_url(&doc).unwrap_or_else(|| url.to_string());
    let meta = metadata::metadata(&doc);

    // A large page that yields almost nothing is a JS
    // shell (Medium, SPAs) — flag it for tier 2 routing.
    let thin = raw_len > 50_000;

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

    // TOC mode: heading tree only.
    if opts.toc {
        let mut md = String::new();
        if let Some(t) = &meta.title {
            md.push_str(&format!("# {t}\n\n"));
        }
        let mut shown = 0usize;
        for b in &all_blocks {
            if let blocks::Block::Heading { level, text, .. } = b {
                let indent = "  ".repeat((*level as usize).saturating_sub(1));
                md.push_str(&format!("{indent}- {text}\n"));
                shown += 1;
            }
        }
        if shown == 0 {
            md.push_str("*(no headings — flat page)*\n");
        }
        return Ok(Extracted {
            tokens_est: md.len() / 4,
            total_chars: md.len(),
            markdown: md,
            title: meta.title,
            byline: meta.byline,
            published: meta.published,
            site: meta.site,
            next_offset: None,
            blocks_total: all_blocks.len(),
            blocks_shown: shown,
            thin: false,
            content_kind: ContentKind::Page,
        });
    }

    // Section scope: keep blocks under a matching heading.
    let mut section_missed = false;
    if let Some(sec) = &opts.section {
        let needle = sec.to_lowercase();
        let mut in_section = false;
        let mut section_level = 0u8;
        let mut kept_idx: Vec<usize> = Vec::new();
        for (i, b) in all_blocks.iter().enumerate() {
            if let blocks::Block::Heading { level, text, .. } = b {
                if in_section && *level <= section_level {
                    // Section ends at the next heading
                    // of same-or-higher level.
                    in_section = false;
                }
                if !in_section && text.to_lowercase().contains(&needle) {
                    in_section = true;
                    section_level = *level;
                }
            }
            if in_section {
                kept_idx.push(i);
            }
        }
        if !kept_idx.is_empty() {
            all_blocks = kept_idx
                .into_iter()
                .map(|i| all_blocks[i].clone())
                .collect();
        } else {
            // No match → full page, but SIGNAL it.
            section_missed = true;
        }
    }

    let blocks_total = all_blocks.len();

    // Focus: BM25 block filter. fell_back = query matched
    // nothing → full content returned, MUST be signaled.
    let (kept, focus_fell_back): (Vec<&blocks::Block>, bool) = match &opts.focus {
        Some(q) => focus::filter(&all_blocks, q),
        None => (all_blocks.iter().collect(), false),
    };
    let blocks_shown = kept.len();

    // Render markdown (frontmatter + blocks) then paginate.
    let mut full = render::render(&meta, url, &kept, opts);

    // Agent-trust signals, inline in the content:
    // - focus miss → agent must not quote wrong content
    // - empty page → silence looks like a bug
    if focus_fell_back {
        if let Some(q) = &opts.focus {
            full = format!(
                "*[focus \"{q}\": no matches — showing full content]*\n\n{full}"
            );
        }
    } else if section_missed {
        if let Some(s) = &opts.section {
            full = format!(
                "*[section \"{s}\": not found — showing full content]*\n\n{full}"
            );
        }
    } else if full.trim().is_empty() || (blocks_total == 0 && meta.title.is_none()) {
        full = format!("{url}\n\n*(no extractable content)*\n");
    }

    // JS-shell warning: agent must know the content
    // below is likely incomplete.
    let thin = thin && full.len() < 800;
    if thin {
        full = format!(
            "*[note: large page rendered almost no content — likely JS-rendered (SPA). Content below may be a shell; tier 2 renders JS.]*\n\n{full}"
        );
    }
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
        thin,
        content_kind: classify(&kept),
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
    let mut slice = text[start..end].to_string();
    // In-content truncation marker: agents read content,
    // not metadata — the resume instruction must be IN
    // the markdown.
    if let Some(n) = next {
        slice.push_str(&format!("\n\n*[truncated — continue with offset={n}]*"));
    }
    (slice, next)
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
