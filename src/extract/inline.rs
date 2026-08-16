//! Inline markdown: links, emphasis, code, wiki-citation
//! dropping, URL absolutizing, tracker stripping.

use scraper::{ElementRef, Node};

const MAX_DEPTH: usize = 100;

/// Render an element's inline content as markdown.
/// Returns (markdown, link_density 0..1).
pub fn markdown(el: ElementRef<'_>, base: &str, opts: &super::ExtractOptions) -> (String, f32) {
    let mut buf = String::new();
    let mut total = 0usize;
    let mut link = 0usize;
    render(el, base, opts, &mut buf, &mut total, &mut link, 0);
    let collapsed = collapse(&buf);
    let ld = if total > 0 {
        link as f32 / total as f32
    } else {
        0.0
    };
    (collapsed, ld)
}

/// Plain visible text, whitespace-collapsed.
pub fn plain(el: ElementRef<'_>) -> String {
    let mut buf = String::new();
    for t in el.text() {
        if !buf.is_empty() {
            buf.push(' ');
        }
        buf.push_str(t.trim());
    }
    collapse(&buf)
}

fn render(
    el: ElementRef<'_>,
    base: &str,
    opts: &super::ExtractOptions,
    buf: &mut String,
    total: &mut usize,
    link: &mut usize,
    depth: usize,
) {
    if depth > MAX_DEPTH {
        return;
    }
    for child in el.children() {
        match child.value() {
            Node::Text(t) => {
                let s = t.text.as_ref();
                if !s.trim().is_empty() {
                    buf.push_str(s);
                    *total += s.trim().len();
                }
            }
            Node::Element(_) => {
                let Some(c) = ElementRef::wrap(child) else {
                    continue;
                };
                let name = c.value().name();
                match name {
                    "a" => {
                        let text = plain(c);
                        if text.is_empty() {
                            continue;
                        }
                        *link += text.len();
                        *total += text.len();
                        if opts.include_links
                            && let Some(href) = c.value().attr("href")
                            && let Some(abs) = absolutize(base, href)
                        {
                            let clean = strip_trackers(&abs);
                            if text != clean {
                                buf.push_str(&format!("[{text}]({clean})"));
                                continue;
                            }
                        }
                        buf.push_str(&text);
                    }
                    "strong" | "b" => {
                        let t = plain(c);
                        if !t.is_empty() {
                            buf.push_str(&format!("**{t}**"));
                        }
                    }
                    "em" | "i" => {
                        let t = plain(c);
                        if !t.is_empty() {
                            buf.push_str(&format!("*{t}*"));
                        }
                    }
                    "code" => {
                        let t = plain(c);
                        if !t.is_empty() {
                            buf.push_str(&format!("`{}`", t.replace('`', "'")));
                        }
                    }
                    // Inline math: recover the LaTeX (`$...$`) —
                    // math elements must never be flattened away.
                    "math" => {
                        let l = super::math::latex(c);
                        if !l.is_empty() {
                            buf.push_str(&format!(" ${l}$ "));
                            *total += l.len();
                        }
                    }
                    // Superscript: keep the content (`^{...}`)
                    // unless it is a wiki-citation marker ([1],
                    // bare digits) — those are pure token waste.
                    "sup" => {
                        let t = plain(c);
                        if !t.is_empty() && !is_citation_marker(&t) {
                            buf.push_str(&format!("^{{{t}}}"));
                            *total += t.len();
                        }
                    }
                    // Subscript: keep the content (`_{...}`) —
                    // W<d sub>k</d>, x<sub>0</sub> carry meaning.
                    "sub" => {
                        let t = plain(c);
                        if !t.is_empty() {
                            buf.push_str(&format!("_{{{t}}}"));
                            *total += t.len();
                        }
                    }
                    "script" | "style" | "noscript" | "svg" | "img" | "button" | "input"
                    | "select" => {}
                    "br" => buf.push(' '),
                    // Block boundaries inside inline rendering
                    // (multi-paragraph comments, list items): at
                    // least a space — the words must never fuse.
                    "p" | "li" | "div" | "blockquote" => {
                        if !buf.is_empty() && !buf.ends_with(' ') {
                            buf.push(' ');
                        }
                        render(c, base, opts, buf, total, link, depth + 1);
                    }
                    _ => {
                        if crate::extract::junk::skip(c) {
                            continue;
                        }
                        render(c, base, opts, buf, total, link, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Wiki citation markers: "[1]", "[12]", "[a]", "[1][2]".
/// These superscripts are reference noise; real superscripts
/// (exponents, ordinal suffixes like "th") survive.
fn is_citation_marker(t: &str) -> bool {
    let stripped: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    if stripped.is_empty() {
        return true;
    }
    let inner = stripped.trim_matches(|c| c == '[' || c == ']');
    if inner.is_empty() {
        return true;
    }
    // Digits (up to 3) or a single footnote letter.
    (inner.len() <= 3 && inner.chars().all(|c| c.is_ascii_digit()))
        || inner.len() == 1 && inner.chars().all(|c| c.is_ascii_lowercase())
}

/// Collapse all whitespace runs to single spaces.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            ws = true;
        } else {
            if ws && !out.is_empty() {
                out.push(' ');
            }
            ws = false;
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Resolve possibly-relative URL against base.
pub fn absolutize(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return None;
    }
    if let Ok(b) = url::Url::parse(base)
        && let Ok(u) = b.join(href)
    {
        return Some(u.to_string());
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href.to_string());
    }
    None
}

const TRACKER_PARAMS: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "mc_cid", "mc_eid", "igshid", "ref_src", "_ga", "spm",
    "scm",
];

/// Drop tracking query params (utm_*, fbclid, …). Big token
/// saver on link-heavy pages.
pub fn strip_trackers(u: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(u) else {
        return u.to_string();
    };
    let kept: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(k, _)| !k.starts_with("utm_") && !TRACKER_PARAMS.contains(&k.as_ref()))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    if kept.len() == parsed.query_pairs().count() {
        return u.to_string();
    }
    parsed.set_query(None);
    if !kept.is_empty() {
        let qs: Vec<String> = kept.iter().map(|(k, v)| format!("{k}={v}")).collect();
        parsed.set_query(Some(&qs.join("&")));
    }
    parsed.to_string()
}
