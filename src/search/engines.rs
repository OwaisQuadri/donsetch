//! SERP parsers — one per engine, scraper-based, with
//! layered fallbacks. A parse that yields <3 hits counts
//! as engine failure (the health system hears about it).

use scraper::{Html, Selector};

/// One raw hit from one engine.
#[derive(Debug, Clone)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub rank: usize,
    /// ISO date when the source carries one (news vertical).
    pub published: Option<String>,
}

fn text(el: scraper::ElementRef) -> String {
    el.text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn sel(css: &str) -> Selector {
    Selector::parse(css).expect("static selector")
}

/// DDG html endpoint wraps links in /l/?uddg= redirects.
/// Decode to the real URL — consensus matching depends on
/// every engine reporting the SAME url.
fn decode_ddg(href: &str) -> String {
    if let Some((_, q)) = href.split_once("uddg=") {
        let raw = q.split('&').next().unwrap_or(q);
        if let Ok(decoded) = urlencoding_decode(raw)
            && decoded.starts_with("http")
        {
            return decoded;
        }
    }
    href.to_string()
}

fn urlencoding_decode(s: &str) -> Result<String, ()> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| ())?;
            let v = u8::from_str_radix(hex, 16).map_err(|_| ())?;
            out.push(v);
            i += 3;
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

/// Bing redirect links: /ck/a?...&u=a1aHR0cHM... (base64url
/// after the "a1" prefix).
fn decode_bing(href: &str) -> String {
    if href.contains("bing.com/ck/a")
        && let Some((_, u)) = href.split_once("&u=")
    {
        let u = u.split('&').next().unwrap_or(u);
        if let Some(b64) = u.strip_prefix("a1")
            && let Some(decoded) = base64url_decode(b64)
            && decoded.starts_with("http")
        {
            return decoded;
        }
    }
    href.to_string()
}

fn base64url_decode(s: &str) -> Option<String> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut vals = Vec::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let pos = T.iter().position(|&t| t == c)?;
        vals.push(pos as u32);
    }
    let mut out = Vec::with_capacity(vals.len() * 6 / 8);
    for chunk in vals.chunks(4) {
        let mut n = 0u32;
        for (i, &v) in chunk.iter().enumerate() {
            n |= v << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    String::from_utf8(out).ok()
}

pub fn parse(engine: &str, html: &str) -> Vec<Hit> {
    let doc = Html::parse_document(html);
    match engine {
        "brave" => parse_brave(&doc),
        "bing" => parse_bing(&doc),
        "ddg" => parse_ddg(&doc),
        "ddg_lite" => parse_ddg_lite(&doc),
        "mojeek" => parse_mojeek(&doc),
        "yahoo" => parse_yahoo(&doc),
        _ => Vec::new(),
    }
}

fn parse_brave(doc: &Html) -> Vec<Hit> {
    let blocks = sel(r#"div[data-type="web"]"#);
    let link = sel("a[href]");
    let title = sel(".title");
    let snippet = sel(".generic-snippet");
    let mut hits = Vec::new();
    for (rank, block) in doc.select(&blocks).enumerate() {
        let Some(a) = block.select(&link).next() else {
            continue;
        };
        let url = a.value().attr("href").unwrap_or("").to_string();
        if !url.starts_with("http") {
            continue;
        }
        let t = a
            .select(&title)
            .next()
            .map(text)
            .or_else(|| block.select(&title).next().map(text))
            .unwrap_or_default();
        // Snippet: card text minus the title and breadcrumb.
        let full = block.select(&snippet).next().map(text).unwrap_or_default();
        let snip = full
            .replace(&t, "")
            .trim_start_matches(|c: char| !c.is_alphanumeric())
            .to_string();
        if !t.is_empty() {
            hits.push(Hit {
                title: t,
                url,
                snippet: snip,
                rank,
                published: None,
            });
        }
    }
    hits
}

fn parse_bing(doc: &Html) -> Vec<Hit> {
    let items = sel("li.b_algo");
    let link = sel("h2 a");
    let cap = sel(".b_caption p, .b_lineclamp2, .b_lineclamp3, .b_lineclamp4");
    let mut hits = Vec::new();
    for (rank, li) in doc.select(&items).enumerate() {
        let Some(a) = li.select(&link).next() else {
            continue;
        };
        let url = decode_bing(a.value().attr("href").unwrap_or(""));
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        let snippet = li.select(&cap).next().map(text).unwrap_or_default();
        if !title.is_empty() {
            hits.push(Hit {
                title,
                url,
                snippet,
                rank,
                published: None,
            });
        }
    }
    hits
}

fn parse_ddg(doc: &Html) -> Vec<Hit> {
    let links = sel("a.result__a");
    let snippets = sel("a.result__snippet, .result__snippet");
    let snippet_vec: Vec<String> = doc.select(&snippets).map(text).collect();
    let mut hits = Vec::new();
    for (rank, a) in doc.select(&links).enumerate() {
        let url = decode_ddg(a.value().attr("href").unwrap_or(""));
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        let snippet = snippet_vec.get(rank).cloned().unwrap_or_default();
        hits.push(Hit {
            title,
            url,
            snippet,
            rank,
            published: None,
        });
    }
    hits
}

fn parse_ddg_lite(doc: &Html) -> Vec<Hit> {
    // Lite: a table — result-link anchors then snippet tds.
    let links = sel("a.result-link");
    let snippets = sel("td.result-snippet");
    let snippet_vec: Vec<String> = doc.select(&snippets).map(text).collect();
    let mut hits = Vec::new();
    for (rank, a) in doc.select(&links).enumerate() {
        let url = decode_ddg(a.value().attr("href").unwrap_or(""));
        if !url.starts_with("http") {
            continue;
        }
        hits.push(Hit {
            title: text(a),
            url,
            snippet: snippet_vec.get(rank).cloned().unwrap_or_default(),
            rank,
            published: None,
        });
    }
    hits
}

fn parse_mojeek(doc: &Html) -> Vec<Hit> {
    // Mojeek: <li><a class="ob">breadcrumb</a>
    //         <h2><a class="title" href>real title</a></h2>
    //         <p class="s">snippet</p></li>
    let items = sel("ul.results-standard li");
    let link = sel("h2 a.title, h2 a");
    let cap = sel("p.s");
    let mut hits = Vec::new();
    for (rank, li) in doc.select(&items).enumerate() {
        let Some(a) = li.select(&link).next() else {
            continue;
        };
        let url = a.value().attr("href").unwrap_or("").to_string();
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        if title.is_empty() {
            continue;
        }
        hits.push(Hit {
            title,
            url,
            snippet: li.select(&cap).next().map(text).unwrap_or_default(),
            rank,
            published: None,
        });
    }
    hits
}

fn parse_yahoo(doc: &Html) -> Vec<Hit> {
    let items = sel("div.dd.algo, li div.algo");
    let link = sel("h3.title a, h3 a");
    let cap = sel(".compText");
    let mut hits = Vec::new();
    for (rank, item) in doc.select(&items).enumerate() {
        let Some(a) = item.select(&link).next() else {
            continue;
        };
        let url = a.value().attr("href").unwrap_or("").to_string();
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        let snippet = item.select(&cap).next().map(text).unwrap_or_default();
        if !title.is_empty() {
            hits.push(Hit {
                title,
                url,
                snippet,
                rank,
                published: None,
            });
        }
    }
    hits
}

/// Engine URL builders.
pub fn serp_url(engine: &str, query: &str) -> Option<String> {
    let q = url::form_urlencoded::byte_serialize(query.as_bytes()).collect::<String>();
    match engine {
        "brave" => Some(format!("https://search.brave.com/search?q={q}")),
        "bing" => Some(format!("https://www.bing.com/search?q={q}&count=15")),
        "ddg" => Some(format!("https://html.duckduckgo.com/html/?q={q}")),
        "ddg_lite" => Some(format!("https://lite.duckduckgo.com/lite/?q={q}")),
        "mojeek" => Some(format!("https://www.mojeek.com/search?q={q}")),
        "yahoo" => Some(format!("https://search.yahoo.com/search?p={q}")),
        _ => None,
    }
}
