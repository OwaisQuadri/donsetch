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
    let hits = match engine {
        "brave" => parse_brave(&doc),
        "google" => parse_google(&doc),
        "bing" => parse_bing(&doc),
        // DDG primary is now lite — the html endpoint serves a
        // CAPTCHA challenge to proxy IPs.  parse_ddg (html parser)
        // is kept for the ddg_html fallback engine.
        "ddg" => parse_ddg_lite(&doc),
        "ddg_lite" => parse_ddg_lite(&doc),
        "ddg_html" => parse_ddg(&doc),
        "mojeek" => parse_mojeek(&doc),
        "yahoo" => parse_yahoo(&doc),
        _ => Vec::new(),
    };
    if hits.is_empty() && std::env::var_os("DONSEEK_DEBUG").is_some() {
        let dump = format!("/tmp/donseek_debug_{engine}.html");
        let _ = std::fs::write(&dump, html);
        eprintln!(
            "[donseek] {engine}: 0 hits, dumped {len} bytes to {dump}",
            len = html.len()
        );
    }
    hits
}

/// Google URL unwrapping: Google wraps result URLs in
/// /url?q=REAL_URL&sa=U&ved=... — extract and decode the
/// real URL. Direct http(s) links pass through unchanged.
fn decode_google(href: &str) -> String {
    if let Some((_, q)) = href.split_once("/url?") {
        let q = q.split('&').next().unwrap_or(q);
        if let Some(raw) = q.strip_prefix("q=")
            && let Ok(decoded) = urlencoding_decode(raw)
            && decoded.starts_with("http")
        {
            return decoded;
        }
    }
    href.to_string()
}

/// Google SERP parser. Google's HTML changes frequently, so
/// this uses layered selectors: primary (div.g), fallback
/// (div[data-ved]), and a shotgun mode (any a with h3 sibling).
fn parse_google(doc: &Html) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Primary: div.g blocks (the classic Google result container).
    let g_blocks = sel("div.g, div.tF2Cxc");
    let link_sel = sel("a[href]");
    let h3 = sel("h3");
    let snip_sel = sel("span.aCOpRe, div[data-sncf], span.st, div.VwiEFb, div.IsZrdc");

    for block in doc.select(&g_blocks) {
        let Some(a) = block.select(&link_sel).next() else {
            continue;
        };
        let raw_href = a.value().attr("href").unwrap_or("");
        let url = decode_google(raw_href);
        if !url.starts_with("http") || url.contains("google.com/") {
            continue;
        }
        let key = url.split('&').next().unwrap_or(&url).to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        let title = block
            .select(&h3)
            .next()
            .map(text)
            .unwrap_or_else(|| text(a));
        if title.is_empty() {
            continue;
        }
        let snippet = block.select(&snip_sel).next().map(text).unwrap_or_default();
        hits.push(Hit {
            title,
            url,
            snippet,
            rank: hits.len(),
            published: None,
        });
    }

    if hits.len() >= 3 {
        return hits;
    }

    // Fallback: any div[data-ved] with a link + h3.
    let ved_blocks = sel("div[data-ved]");
    for block in doc.select(&ved_blocks) {
        let Some(a) = block.select(&link_sel).next() else {
            continue;
        };
        let raw_href = a.value().attr("href").unwrap_or("");
        let url = decode_google(raw_href);
        if !url.starts_with("http") || url.contains("google.com/") {
            continue;
        }
        let key = url.split('&').next().unwrap_or(&url).to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        let title = block.select(&h3).next().map(text);
        if title.is_none() {
            continue;
        }
        let title = title.unwrap();
        if title.is_empty() {
            continue;
        }
        let snippet = block.select(&snip_sel).next().map(text).unwrap_or_default();
        hits.push(Hit {
            title,
            url,
            snippet,
            rank: hits.len(),
            published: None,
        });
    }

    if hits.len() >= 3 {
        return hits;
    }

    // Shotgun: any <a> with an <h3> ancestor and an http href.
    for a in doc.select(&link_sel) {
        let raw_href = a.value().attr("href").unwrap_or("");
        let url = decode_google(raw_href);
        if !url.starts_with("http") || url.contains("google.com/") {
            continue;
        }
        let key = url.split('&').next().unwrap_or(&url).to_string();
        if !seen.insert(key.clone()) {
            continue;
        }
        // Check for h3 in the ancestor chain.
        let mut title = String::new();
        for ancestor in a.ancestors() {
            if let Some(el_ref) = scraper::ElementRef::wrap(ancestor)
                && let Some(h3_el) = el_ref.select(&h3).next()
            {
                title = text(h3_el);
                break;
            }
        }
        if title.is_empty() {
            continue;
        }
        hits.push(Hit {
            title,
            url,
            snippet: String::new(),
            rank: hits.len(),
            published: None,
        });
    }

    hits
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
    // Primary: h2 a; fallback: h2 > a, a.tilk, a[data-h]
    let link = sel("h2 a, h2 > a, a.tilk, a[data-h]");
    // Primary: .b_caption p; fallback: .b_lineclamp*, [data-text]
    let cap = sel(".b_caption p, .b_lineclamp2, .b_lineclamp3, .b_lineclamp4, p[data-text]");
    let h2 = sel("h2");
    let mut hits = Vec::new();
    for (rank, li) in doc.select(&items).enumerate() {
        let Some(a) = li
            .select(&link)
            .next()
            .or_else(|| li.select(&sel("a[href]")).next())
        else {
            continue;
        };
        let url = decode_bing(a.value().attr("href").unwrap_or(""));
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        // Fallback: if the link text is empty, try h2.
        let title = if title.is_empty() {
            li.select(&h2).next().map(text).unwrap_or_default()
        } else {
            title
        };
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

/// Yahoo redirect links: r.search.yahoo.com/...RU=REAL_URL
/// or r.search.yahoo.com/..._url=REAL_URL. Decode to the
/// real URL — consensus matching depends on every engine
/// reporting the SAME url.
fn decode_yahoo(href: &str) -> String {
    if !href.contains("r.search.yahoo.com") && !href.contains("search.yahoo.com/search") {
        return href.to_string();
    }
    // Try RU= parameter (most common).
    if let Some((_, ru)) = href.split_once("RU=") {
        let raw = ru.split('&').next().unwrap_or(ru);
        // Strip trailing /RV= or similar path components.
        let raw = raw.split('/').next().unwrap_or(raw);
        if let Ok(decoded) = urlencoding_decode(raw)
            && decoded.starts_with("http")
        {
            return decoded;
        }
    }
    // Try _url= parameter.
    if let Some((_, url)) = href.split_once("_url=") {
        let raw = url.split('&').next().unwrap_or(url);
        if let Ok(decoded) = urlencoding_decode(raw)
            && decoded.starts_with("http")
        {
            return decoded;
        }
    }
    href.to_string()
}

fn parse_yahoo(doc: &Html) -> Vec<Hit> {
    // Yahoo SERP selectors with fallbacks.
    let items = sel("div.dd.algo, li div.algo, div.algo, div.compTitle");
    let link = sel("h3.title a, h3 a, a.title, a[data-mat]");
    let cap = sel(".compText, .compText a, p");
    let h3 = sel("h3");
    let a_gen = sel("a[href]");
    let mut hits = Vec::new();
    for (rank, item) in doc.select(&items).enumerate() {
        let Some(a) = item
            .select(&link)
            .next()
            .or_else(|| item.select(&a_gen).next())
        else {
            continue;
        };
        let url = decode_yahoo(a.value().attr("href").unwrap_or(""));
        if !url.starts_with("http") {
            continue;
        }
        let title = text(a);
        let title = if title.is_empty() {
            item.select(&h3).next().map(text).unwrap_or_default()
        } else {
            title
        };
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
        "google" => Some(format!(
            "https://www.google.com/search?q={q}&hl=en&gl=us&num=15&ie=utf-8&oe=utf-8"
        )),
        "brave" => Some(format!("https://search.brave.com/search?q={q}")),
        "bing" => Some(format!("https://www.bing.com/search?q={q}&count=15")),
        // DDG primary: lite endpoint (html endpoint serves CAPTCHA to proxy IPs).
        "ddg" => Some(format!("https://lite.duckduckgo.com/lite/?q={q}")),
        "ddg_lite" => Some(format!("https://lite.duckduckgo.com/lite/?q={q}")),
        "ddg_html" => Some(format!("https://html.duckduckgo.com/html/?q={q}")),
        "mojeek" => Some(format!("https://www.mojeek.com/search?q={q}")),
        "yahoo" => Some(format!("https://search.yahoo.com/search?p={q}")),
        _ => None,
    }
}
