//! Fuzz the full extraction pipeline: arbitrary bytes + a
//! content-type string → DonSift. This is the broadest net: it
//! covers the block segmenter, junk filters, focus BM25, rendering,
//! pagination, and every dedicated extractor (reddit, HN, feed,
//! jsdata) that claims the input. Every v2.5 daemon-abort panic
//! class (charset multi-byte, js_unescape, paginate overflow,
//! debug slice) lives under this target.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Derive a content-type from the first bytes so the fuzzer
    // explores both the HTML path and the passthrough path.
    let ct = match data.first() {
        Some(b'h') => "text/html; charset=utf-8",
        Some(b'p') => "application/pdf",
        Some(b'x') => "text/xml",
        Some(b'j') => "application/json",
        _ => "",
    };
    let body = &data[data.len().min(1)..];
    let url = "https://fuzz.example.com/page";

    // Exercise a spread of option combinations deterministically
    // derived from the input tail, so each corpus entry explores a
    // different feature path (focus, section, toc, selector,
    // offsets, hostile max_chars).
    let tail = data.get(data.len().saturating_sub(8)..).unwrap_or(&[]);
    let mut opts = donsetch::extract::ExtractOptions::default();
    if tail.first() == Some(&b'f') {
        opts.focus = Some("target topic".to_string());
    }
    if tail.get(1) == Some(&b's') {
        opts.section = Some("intro".to_string());
    }
    if tail.get(2) == Some(&b't') {
        opts.toc = true;
    }
    if tail.get(3) == Some(&b'l') {
        opts.include_links = true;
    }
    if let Some(n) = tail.get(4) {
        opts.offset = (*n as usize) << 20; // hostile offsets
        opts.max_chars = Some((*n as usize) << 24); // hostile max_chars
    }

    let _ = donsetch::extract::extract(body, ct, url, &opts);

    // Wall detection over the same bytes — DOM smart detection is
    // on every fetch's hot path.
    let _ = donsetch::detect::walls::detect_dom_smart(body);
    let _ = donsetch::detect::walls::visible_text_count(body);
});
