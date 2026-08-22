//! DonSift — the extraction engine of DonSeTch.
//!
//! HTML bytes in → agent-native markdown out. Block model, not
//! article text: typed blocks with heading breadcrumbs power BM25
//! focus, stable pagination, and token-war rendering policies.
//!
//! Pipeline: decode charset → parse once (no mutation) → metadata →
//! scope (selector or density-scored main) → segment blocks →
//! focus filter → render markdown → paginate.

pub mod blocks;
pub mod charset;
pub mod feed;
pub mod focus;
pub mod hn;
pub mod inline;
pub mod jsdata;
pub mod junk;
pub mod language;
pub mod math;
pub mod metadata;
pub mod reddit;
pub mod render;
pub mod score;

#[cfg(test)]
mod tests;

use scraper::{Html, Node};

#[derive(Default, Clone)]
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
    /// Detected language (BCP-47 code: "en", "zh", "ja", etc.).
    #[allow(dead_code)]
    pub lang: String,
    /// Quality score 0.0..1.0 — content density, metadata
    /// completeness, structure diversity. Helps agents
    /// decide if content is trustworthy.
    #[allow(dead_code)]
    pub quality: f32,
    /// PDF only: per-page extraction stats (chars, ocr flag,
    /// confidence). Block merging intentionally flows paragraphs
    /// across page breaks for reading continuity — page
    /// boundaries are preserved HERE instead.
    pub pdf_pages: Option<Vec<crate::pdf::PageMeta>>,
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

/// Quality score 0.0..1.0 — content density, metadata
/// completeness, structure diversity, and language
/// coherence. Helps agents decide if the extracted
/// content is trustworthy enough to quote.
fn quality_score(
    meta: &metadata::Meta,
    kept: &[&blocks::Block],
    blocks_total: usize,
    raw_len: usize,
    lang_info: &language::LanguageInfo,
) -> f32 {
    let mut score = 0.0;

    // 1. Content density: extracted chars vs raw HTML bytes.
    let text_len: usize = kept.iter().map(|b| b.chars()).sum();
    if raw_len > 0 {
        let density = text_len as f64 / raw_len as f64;
        score += (density * 4.0).min(1.0) * 0.25;
    }

    // 2. Metadata completeness.
    if meta.title.is_some() {
        score += 0.1;
    }
    if meta.byline.is_some() {
        score += 0.05;
    }
    if meta.published.is_some() {
        score += 0.05;
    }
    if meta.site.is_some() {
        score += 0.05;
    }

    // 3. Structure diversity: headings + paragraphs + lists.
    let mut headings = 0;
    let mut paras = 0;
    let mut lists = 0;
    let mut code = 0;
    let mut tables = 0;
    let mut quotes = 0;
    for b in kept {
        match b {
            blocks::Block::Heading { .. } => headings += 1,
            blocks::Block::Para { .. } => paras += 1,
            blocks::Block::List { .. } => lists += 1,
            blocks::Block::Code { .. } => code += 1,
            blocks::Block::Table { .. } => tables += 1,
            blocks::Block::Quote { .. } => quotes += 1,
            _ => {}
        }
    }
    let structure_types = [
        headings > 0,
        paras > 0,
        lists > 0,
        code > 0,
        tables > 0,
        quotes > 0,
    ]
    .iter()
    .filter(|&&b| b)
    .count();
    score += (structure_types as f64 / 6.0) * 0.2;

    // 4. Block count health: enough blocks to be real content.
    if blocks_total >= 5 {
        score += 0.1;
    }
    if blocks_total >= 20 {
        score += 0.05;
    }

    // 5. Language detected (not unknown).
    if lang_info.code != "unknown" && lang_info.code != "und" {
        score += 0.05;
    }

    // 6. Text volume: actual prose exists.
    if text_len > 500 {
        score += 0.1;
    }
    if text_len > 2000 {
        score += 0.05;
    }

    score.min(1.0) as f32
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
    let is_pdf = body.len() >= 5 && body.starts_with(b"%PDF-") || ct.contains("pdf");

    // Feeds (RSS/Atom/JSON Feed): structured rendering, never a
    // raw XML blob. Checked BEFORE passthrough — feed content
    // types (text/xml, application/rss+xml…) never say "html".
    if !is_pdf
        && feed::is_feed(&ct, body)
        && let Some(ex) = feed::extract(body, url, opts)
    {
        return Ok(ex);
    }

    // A "text/plain" body that is actually HTML (misconfigured
    // servers, raw git URLs): parse it as HTML, not as literal
    // text full of angle brackets.
    let plain_is_html =
        !ct.is_empty() && !ct.contains("html") && !is_pdf && body_starts_with_html(body);

    if !ct.is_empty() && !ct.contains("html") && !is_pdf && !plain_is_html {
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
            lang: "unknown".to_string(),
            quality: 0.0,
            pdf_pages: None,
        });
    }

    // --- PDF: DonSheet parses bytes into the same block model. ---
    if is_pdf {
        match crate::pdf::parse(body) {
            Ok(parsed) => {
                return downstream(
                    &parsed.meta,
                    parsed.blocks,
                    body.len(),
                    false,
                    false,
                    parsed.notes,
                    parsed.lang_info,
                    Some(parsed.pages_meta),
                    url,
                    opts,
                    max_chars,
                );
            }
            Err(crate::pdf::PdfFailure::Encrypted) => {
                return Ok(empty_pdf(
                    url,
                    "encrypted document — a password is required; could not extract text",
                ));
            }
            Err(crate::pdf::PdfFailure::Corrupt(msg)) => {
                return Ok(empty_pdf(url, &format!("{msg}; could not extract text")));
            }
            Err(crate::pdf::PdfFailure::TooLarge(n)) => {
                return Ok(empty_pdf(
                    url,
                    &format!(
                        "document too large ({:.0} MB > limit); could not extract text",
                        n as f64 / 1_048_576.0
                    ),
                ));
            }
            Err(crate::pdf::PdfFailure::NotPdf) => {
                return Ok(empty_pdf(
                    url,
                    "bytes do not decode as a PDF; could not extract text",
                ));
            }
        }
    }

    // --- HTML upstream ---
    let html_text = charset::decode(body, &ct);
    let raw_len = body.len();

    // Reddit dedicated extractor: bypasses DonSift for
    // old.reddit.com, produces compact structured output.
    // Returns None for non-reddit or unrecognized pages.
    if let Some(extracted) = reddit::extract(&html_text, url, opts) {
        return Ok(extracted);
    }

    // Hacker News dedicated extractor: the comment tree is a
    // table layout the generic pipeline mangles. Full comment
    // text with authors and reply depth.
    if let Some(extracted) = hn::extract(&html_text, url, opts) {
        return Ok(extracted);
    }

    let doc = Html::parse_document(&html_text);
    let base = metadata::base_url(&doc).unwrap_or_else(|| url.to_string());
    let meta = metadata::metadata(&doc);
    let lang_info = language::detect(&doc);

    // A large page that yields almost nothing is a JS
    // shell (Medium, SPAs) — flag it for tier 2 routing.
    let thin_flag = raw_len > 50_000;

    // Skeleton/SPA loading detection. Only use aria-busy —
    // the word "skeleton" appears in CSS class names on
    // fully-hydrated pages (Amazon, React apps), making it
    // a false-positive. aria-busy is a reliable loading
    // signal set by the browser, not CSS.
    let lower_html = html_text.to_lowercase();
    let has_skeletons = lower_html.matches("aria-busy=\"true\"").take(3).count() >= 3;

    // Scope: explicit selector or scored main-content detection.
    let roots: Vec<scraper::ElementRef<'_>> = if let Some(sel) = &opts.selector {
        let parsed =
            scraper::Selector::parse(sel).map_err(|_| ExtractError::BadSelector(sel.clone()))?;
        doc.select(&parsed).collect()
    } else {
        score::find_main(&doc).into_iter().collect()
    };

    // Segment into typed blocks.
    let mut all_blocks = Vec::new();
    for root in &roots {
        blocks::segment(*root, &base, opts, &mut all_blocks);
    }

    let extracted = downstream(
        &meta,
        all_blocks,
        raw_len,
        thin_flag,
        has_skeletons,
        Vec::new(),
        lang_info,
        None,
        url,
        opts,
        max_chars,
    )?;

    // JSON-in-script rescue: SPAs (Next.js/React/YouTube) embed
    // their content as a JS-assigned JSON blob. DonSift sees an
    // empty shell, but the data is sitting in the HTML. When
    // DonSift came up thin, mine the embedded JSON — if it's
    // richer, it wins. This is the tier-1 unlock for the SPA
    // class of sites.
    if (extracted.thin || extracted.total_chars < 600)
        && let Some(js) = jsdata::extract(&html_text, url, opts)
        && js.total_chars > extracted.total_chars
    {
        return Ok(js);
    }

    // Raw text fallback: when block extraction fails (returns thin)
    // but the DOM has real visible text, strip tags and return text.
    // This makes "found DOM but failed to extract content" IMPOSSIBLE
    // when the DOM has real content. Less structured than block-based
    // extraction (no proper paragraphs/lists/tables), but infinitely
    // better than returning nothing. The fallback preserves heading
    // structure (h1-h6 → markdown headings) and paragraph breaks.
    let needs_fallback = extracted.thin || extracted.total_chars < 200;
    if needs_fallback && let Some(fb) = text_fallback(&html_text, &meta, url, opts, max_chars) {
        return Ok(fb);
    }
    Ok(extracted)
}

/// Does the body start (after BOM/whitespace) with an HTML
/// doctype or `<html` tag? Used to catch HTML served as
/// text/plain.
fn body_starts_with_html(body: &[u8]) -> bool {
    let mut s = body;
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        s = &s[3..];
    }
    let s = String::from_utf8_lossy(&s[..s.len().min(256)]);
    let t = s.trim_start().to_lowercase();
    t.starts_with("<!doctype html") || t.starts_with("<html")
}

/// Raw text fallback: strip tags and return visible text as
/// markdown paragraphs. Used when DonSift's block-based extraction
/// pipeline fails on complex DOMs. Preserves heading
/// structure (h1-h6 → # ## ###) and paragraph breaks. Skips
/// script/style/nav/footer/header/aside/form elements.
///
/// Returns None when there's < 200 chars of visible text — the
/// page is genuinely empty (JS shell or block page).
pub fn text_fallback(
    html_text: &str,
    meta: &metadata::Meta,
    url: &str,
    opts: &ExtractOptions,
    max_chars: usize,
) -> Option<Extracted> {
    let doc = Html::parse_document(html_text);
    let body_sel = scraper::Selector::parse("body").ok()?;
    let body = doc.select(&body_sel).next()?;

    let mut paragraphs: Vec<String> = Vec::new();
    let mut current = String::new();
    collect_fallback_text(body, &mut paragraphs, &mut current);
    if !current.trim().is_empty() {
        paragraphs.push(current.trim().to_string());
    }

    // Filter whitespace-only and single-char paragraphs
    let paragraphs: Vec<String> = paragraphs
        .into_iter()
        .filter(|p| p.len() > 1 && p.chars().any(|c| !c.is_whitespace()))
        .collect();

    let total_text: usize = paragraphs.iter().map(|p| p.len()).sum();
    if total_text < 200 {
        return None;
    }

    let mut full = String::new();
    if let Some(t) = &meta.title {
        full.push_str(&format!("# {t}\n\n"));
    }
    full.push_str(&format!("{url}\n\n"));
    full.push_str(&paragraphs.join("\n\n"));

    let (slice, next) = paginate(&full, opts.offset, max_chars);
    let blocks_total = paragraphs.len();
    let tokens_est = slice.len() / 4;

    // thin=true when < 800 chars: a JS shell with 300 chars of
    // visible text (script filenames, noscript messages, meta
    // descriptions) is NOT real content. The MCP layer must
    // escalate to ghost. Only pages with >= 800 chars of real
    // visible text are non-thin — those are genuinely complex
    // DOMs where block extraction failed but text is real.
    Some(Extracted {
        markdown: slice,
        title: meta.title.clone(),
        byline: meta.byline.clone(),
        published: meta.published.clone(),
        site: meta.site.clone(),
        total_chars: full.len(),
        next_offset: next,
        blocks_total,
        blocks_shown: blocks_total,
        tokens_est,
        thin: total_text < 800,
        content_kind: ContentKind::Page,
        lang: "unknown".to_string(),
        quality: 0.3, // lower quality than block-based extraction
        pdf_pages: None,
    })
}

const SKIP_FALLBACK_TAGS: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object", "embed", "nav",
    "aside", "footer", "header", "form", "button", "input", "select", "textarea", "option",
];

const PARAGRAPH_BREAK_TAGS: &[&str] = &[
    "p",
    "br",
    "li",
    "tr",
    "blockquote",
    "pre",
    "dt",
    "dd",
    "figcaption",
];

fn heading_level(tag: &str) -> Option<usize> {
    match tag {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

fn collect_fallback_text(
    el: scraper::ElementRef,
    paragraphs: &mut Vec<String>,
    current: &mut String,
) {
    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                let text = t.text.trim();
                if !text.is_empty() {
                    if !current.is_empty() && !current.ends_with(' ') {
                        current.push(' ');
                    }
                    current.push_str(text);
                }
            }
            Node::Element(e) => {
                let name = e.name();
                if SKIP_FALLBACK_TAGS.contains(&name) {
                    continue;
                }
                let Some(child_el) = scraper::ElementRef::wrap(child) else {
                    continue;
                };
                // Headings: flush, prefix with markdown, recurse
                if let Some(level) = heading_level(name) {
                    if !current.trim().is_empty() {
                        paragraphs.push(std::mem::take(current).trim().to_string());
                    }
                    let mut heading = String::new();
                    collect_fallback_text(child_el, paragraphs, &mut heading);
                    if !heading.trim().is_empty() {
                        paragraphs.push(format!("{} {}", "#".repeat(level), heading.trim()));
                    }
                    continue;
                }
                // Block elements: flush, recurse, flush
                if PARAGRAPH_BREAK_TAGS.contains(&name) {
                    if !current.trim().is_empty() {
                        paragraphs.push(std::mem::take(current).trim().to_string());
                    }
                    let mut inner = String::new();
                    collect_fallback_text(child_el, paragraphs, &mut inner);
                    if !inner.trim().is_empty() {
                        paragraphs.push(inner.trim().to_string());
                    }
                } else {
                    // Inline: recurse without flush
                    collect_fallback_text(child_el, paragraphs, current);
                }
            }
            _ => {}
        }
    }
}

/// Honest stub for PDFs that could not be parsed.
fn empty_pdf(url: &str, reason: &str) -> Extracted {
    let md = format!("{url}\n\n*[pdf: {reason}]*\n");
    Extracted {
        tokens_est: md.len() / 4,
        total_chars: md.len(),
        markdown: md,
        title: None,
        byline: None,
        published: None,
        site: None,
        next_offset: None,
        blocks_total: 0,
        blocks_shown: 0,
        thin: false,
        content_kind: ContentKind::Page,
        lang: "unknown".to_string(),
        quality: 0.0,
        pdf_pages: None,
    }
}

/// Shared downstream: TOC → section scope → focus BM25 → render →
/// trust signals → pagination → quality. Identical for HTML and PDF;
/// PDF upstream produces blocks + meta + notes + language and hands
/// them here.
#[allow(clippy::too_many_arguments)]
fn downstream(
    meta: &crate::extract::metadata::Meta,
    mut all_blocks: Vec<blocks::Block>,
    raw_len: usize,
    thin_flag: bool,
    has_skeletons: bool,
    notes: Vec<String>,
    lang_info: language::LanguageInfo,
    pdf_pages: Option<Vec<crate::pdf::PageMeta>>,
    url: &str,
    opts: &ExtractOptions,
    max_chars: usize,
) -> Result<Extracted, ExtractError> {
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
            title: meta.title.clone(),
            byline: meta.byline.clone(),
            published: meta.published.clone(),
            site: meta.site.clone(),
            next_offset: None,
            blocks_total: all_blocks.len(),
            blocks_shown: shown,
            thin: false,
            content_kind: ContentKind::Page,
            lang: lang_info.code.clone(),
            quality: 0.0,
            pdf_pages: None,
        });
    }

    // Section scope: keep blocks under a matching heading.
    let mut section_missed = false;
    let mut section_hit = false;
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
            section_hit = true;
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
        Some(q) => focus::filter_semantic(&all_blocks, q, &lang_info),
        None => (all_blocks.iter().collect(), false),
    };
    let blocks_shown = kept.len();

    // Render markdown (frontmatter + blocks) then paginate.
    let mut full = render::render(meta, url, &kept, opts);

    // Engine notes first (PDF scan flags etc.) — they frame any
    // other trust signal that follows.
    for note in &notes {
        full = format!("*[pdf: {note}]*\n\n{full}");
    }

    // Agent-trust signals, inline in the content:
    // - focus miss → agent must not quote wrong content
    // - empty page → silence looks like a bug
    if focus_fell_back {
        if let Some(q) = &opts.focus {
            full = format!("*[focus \"{q}\": no matches — showing full content]*\n\n{full}");
        }
    } else if section_missed {
        if let Some(s) = &opts.section {
            full = format!("*[section \"{s}\": not found — showing full content]*\n\n{full}");
        }
    } else if full.trim().is_empty() || (blocks_total == 0 && meta.title.is_none()) {
        full = format!("{url}\n\n*(no extractable content)*\n");
    }

    // JS-shell warning: agent must know the content
    // below is likely incomplete. On tier=auto the MCP
    // fetch escalates to the browser itself — this note
    // only surfaces on an explicit tier=1 request.
    //
    // Thinness: the extraction yield is the truth. Any page over
    // 5KB that yields < 800 chars is a shell (the 27KB-challenge-
    // page-with-250-chars class — three boilerplate blocks used
    // to pass as non-thin). Zero blocks or a >50KB page with
    // almost nothing are shells at any size. Skeleton markers
    // stay a secondary signal for borderline yields.
    //
    // Content density: a page > 20KB that yields < 5% of its
    // raw size as text, with < 3000 chars total, is a JS shell.
    // SPAs that server-render their layout (navigation, sidebar,
    // footer) produce enough boilerplate text to pass the < 800
    // char threshold, but the main content is client-rendered.
    // Measured: artstation 0.9% density, pixiv 2.1% (both > 20KB).
    // Real pages: 15-40%+ density. bilibili at 6% with 1476 chars
    // is NOT flagged (above the 5% density threshold, and above
    // 3000 chars is not required because density already says
    // it's real content).
    // A matched section is intentionally small — the agent asked for
    // exactly this slice. Shell detection must not fire on it (a
    // small section on a 400KB page used to escalate to ghost and
    // return the full page instead of the section).
    let density = if raw_len > 0 {
        full.len() as f64 / raw_len as f64
    } else {
        1.0
    };
    let is_shell = raw_len > 20_000 && density < 0.05 && full.len() < 3_000;
    let thin = !section_hit
        && ((full.len() < 800 && (thin_flag || raw_len > 5_000 || blocks_total == 0))
            || (thin_flag && has_skeletons && full.len() < 4000)
            || is_shell);
    if thin {
        full = format!(
            "*[note: large page rendered almost no content — likely JS-rendered (SPA). Content below may be a shell; use tier=auto to render with a real browser.]*\n\n{full}"
        );
    }
    let (slice, next) = paginate(&full, opts.offset, max_chars);
    let tokens_est = slice.len() / 4;

    let content_kind = classify(&kept);
    let quality = quality_score(meta, &kept, blocks_total, raw_len, &lang_info);
    Ok(Extracted {
        markdown: slice,
        title: meta.title.clone(),
        byline: meta.byline.clone(),
        published: meta.published.clone(),
        site: meta.site.clone(),
        total_chars: full.len(),
        next_offset: next,
        blocks_total,
        blocks_shown,
        tokens_est,
        thin,
        content_kind,
        lang: lang_info.code.clone(),
        quality,
        pdf_pages,
    })
}

/// Char-budget slice at a UTF-8 boundary, preferring a block
/// boundary ("\n\n") near the cut. Returns (slice, next_offset).
fn paginate(text: &str, offset: usize, max_chars: usize) -> (String, Option<usize>) {
    if offset >= text.len() {
        return (String::new(), None);
    }
    let mut start = ceil_char_boundary(text, offset);
    // Seek forward to the next block boundary ("\n\n") when
    // resuming. Agents who resume at offset=N should start at a
    // clean paragraph/heading boundary, not mid-sentence.
    if offset > 0 {
        // Floor the window end to a char boundary — a mid-char
        // offset into CJK text makes start+500 mid-character and
        // the slice below would panic.
        let mut search_end = start.saturating_add(500).min(text.len());
        while !text.is_char_boundary(search_end) {
            search_end -= 1;
        }
        if let Some(pos) = text[start..search_end].find("\n\n") {
            start = start + pos + 2; // skip past the "\n\n"
        }
    }
    // saturating: max_chars comes from tool args; a hostile/huge
    // value must not wrap end below start (slice panic).
    let mut end = start.saturating_add(max_chars).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        // Prefer a block boundary within the last quarter of the window.
        let window_start = ceil_char_boundary(text, start + (end - start) * 3 / 4);
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

/// Shared pagination for dedicated extractors (reddit, hn).
pub fn paginate_public(text: &str, offset: usize, max_chars: usize) -> (String, Option<usize>) {
    paginate(text, offset, max_chars)
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
