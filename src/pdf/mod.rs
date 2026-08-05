//! DonSheet — the PDF extraction engine.
//!
//! Bytes in, DonSift blocks out. See `design/pdf.md` for the full
//! architecture. This module owns: the PDFium FFI boundary (`sys` +
//! `engine`), the geometry pipeline (line assembly, reading order,
//! furniture), semantic block classification, and the honest-flag
//! detection (encrypted / scanned / corrupt / vertical).
//!
//! Sibling naming: DonShadow fetches the bytes, DonSheet reads them.

pub mod blocks;
pub mod engine;
pub mod layout;
pub mod reading;
pub mod sys;
pub mod tables;

use crate::extract::blocks::Block;
use crate::extract::metadata::Meta;

pub use engine::{LoadError, OutlineItem};

/// Hard input ceiling (server-side fetched PDFs can be huge).
const DEFAULT_MAX_BYTES: usize = 100 * 1024 * 1024;

#[derive(Debug)]
pub enum PdfFailure {
    Encrypted,
    Corrupt(String),
    TooLarge(usize),
    NotPdf,
}

impl std::fmt::Display for PdfFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfFailure::Encrypted => write!(f, "pdf: encrypted document (password required)"),
            PdfFailure::Corrupt(msg) => write!(f, "pdf: {msg}"),
            PdfFailure::TooLarge(n) => {
                write!(f, "pdf: document exceeds size limit ({} MB)", n / 1024 / 1024)
            }
            PdfFailure::NotPdf => write!(f, "pdf: bytes do not look like a PDF"),
        }
    }
}

impl std::error::Error for PdfFailure {}

/// What a fully-parsed PDF yields: blocks + trust metadata.
#[allow(dead_code)] // outline/page_count/lang/images/fonts wire into MCP meta
pub struct ParsedPdf {
    pub blocks: Vec<Block>,
    pub meta: Meta,
    pub outline: Vec<OutlineItem>,
    pub page_count: usize,
    /// Agent-visible notes (scanned pages, unsupported lanes...).
    pub notes: Vec<String>,
    /// Language code ("en", "ja", ...), best-effort.
    pub lang: String,
    /// Full language fingerprints for focus/tokenization reuse.
    pub lang_info: crate::extract::language::LanguageInfo,
    pub images: usize,
    pub fonts: Vec<String>,
}

/// Normalize a PDF date ("D:20260525080808+00'00'") to YYYY-MM-DD.
fn pdf_date(raw: &Option<String>) -> Option<String> {
    let r = raw.as_ref()?;
    let d = r.strip_prefix("D:").unwrap_or(r);
    if d.len() >= 8 && d[..8].chars().all(|c| c.is_ascii_digit()) {
        Some(format!("{}-{}-{}", &d[..4], &d[4..6], &d[6..8]))
    } else if r.trim().is_empty() {
        None
    } else {
        Some(r.clone())
    }
}

/// Parse `bytes` into DonSift blocks with full honesty flags.
pub fn parse(bytes: &[u8]) -> Result<ParsedPdf, PdfFailure> {
    let limit = std::env::var("DONSETCH_PDF_MAX_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|mb| mb * 1024 * 1024)
        .unwrap_or(DEFAULT_MAX_BYTES);
    if bytes.len() > limit {
        return Err(PdfFailure::TooLarge(bytes.len()));
    }

    let mut pages: Vec<layout::PageLines> = Vec::new();
    let mut raw_chars_total = 0usize;
    let (raw, ()) = match engine::load_document(bytes, |pc| {
        raw_chars_total += pc.chars.len();
        pages.push(layout::assemble(pc));
    }) {
        Ok((r, _)) => (r, ()),
        Err(LoadError::Encrypted) => return Err(PdfFailure::Encrypted),
        Err(LoadError::NotPdf) => return Err(PdfFailure::NotPdf),
        Err(LoadError::Corrupt(code)) => {
            return Err(PdfFailure::Corrupt(format!("corrupt document (pdfium error {code})")))
        }
    };

    let mut notes: Vec<String> = Vec::new();

    // Scanned / image-only page detection.
    let images: usize = pages.iter().map(|p| p.images).sum();
    let mut scanned_pages = 0usize;
    for p in &pages {
        if p.lines.is_empty() && p.images > 0 {
            scanned_pages += 1;
        }
    }
    if raw_chars_total == 0 && images > 0 {
        notes.push(format!(
            "scanned/image-only PDF ({} pages): no text layer exists; OCR is not available; the pages could not be extracted",
            raw.page_count
        ));
    } else if scanned_pages > 0 {
        notes.push(format!(
            "{scanned_pages} of {} pages are scanned with no text layer (skipped; content may be incomplete)",
            raw.page_count
        ));
    }
    if pages.iter().all(|p| p.lines.is_empty()) && images == 0 && raw.page_count > 0 {
        notes.push("no extractable text found in this PDF".to_string());
    }

    // Running heads / footers.
    reading::suppress_furniture(&mut pages);

    // Reading order per page. Vertical/rotated pages are an honest
    // flagged lane (best-effort; no column reconstruction in v1).
    let dbg = std::env::var("DONSHEET_DEBUG").is_ok();
    if dbg {
        eprintln!("[parse] reading order start: {} pages", pages.len());
    }
    let mut vertical_pages = 0usize;
    let mut ordered_by_page: Vec<Vec<layout::Line>> = Vec::with_capacity(pages.len());
    for p in &pages {
        if reading::is_vertical_suspect(p) {
            vertical_pages += 1;
        }
        if dbg {
            eprintln!("[parse] page {} ordering ({} lines)", p.index, p.lines.len());
        }
        ordered_by_page.push(reading::page_order(p.lines.clone()));
    }
    if vertical_pages > 0 {
        notes.push(format!(
            "{vertical_pages} of {} page(s) contain vertical or rotated text — vertical text extraction is best-effort and may be out of order",
            raw.page_count
        ));
    }

    // Font context across the document.
    let all_lines: Vec<&layout::Line> =
        ordered_by_page.iter().flat_map(|v| v.iter()).collect();
    let ctx = blocks::font_ctx(&all_lines);

    // Semantic blocks.
    if dbg {
        eprintln!("[parse] classify start");
    }
    let doc_blocks = blocks::classify(&pages, &ordered_by_page, &ctx);
    if dbg {
        eprintln!("[parse] classify end: {} blocks", doc_blocks.len());
    }

    // Language sniff on the produced text.
    let mut sample = String::new();
    for l in all_lines.iter().take(400) {
        sample.push_str(&l.text);
        sample.push(' ');
        if sample.len() > 24_000 {
            break;
        }
    }
    let lang_info = crate::extract::language::detect_from_text(&sample);

    let meta = Meta {
        title: raw.meta.title.clone(),
        byline: raw.meta.author.clone(),
        published: pdf_date(&raw.meta.created).or(pdf_date(&raw.meta.modified)),
        site: None,
        description: raw.meta.subject.clone(),
        canonical: None,
    };

    Ok(ParsedPdf {
        blocks: doc_blocks,
        meta,
        outline: raw.outline,
        page_count: raw.page_count,
        notes,
        lang: lang_info.code.clone(),
        lang_info,
        images,
        fonts: raw.fonts,
    })
}

#[cfg(test)]
mod tests;
