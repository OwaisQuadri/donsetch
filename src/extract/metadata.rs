//! Page metadata: title, byline, published, site, base URL.

use scraper::Html;

pub struct Meta {
    pub title: Option<String>,
    pub byline: Option<String>,
    pub published: Option<String>,
    pub site: Option<String>,
}

pub fn base_url(doc: &Html) -> Option<String> {
    let sel = scraper::Selector::parse("base[href]").ok()?;
    doc.select(&sel)
        .next()
        .and_then(|b| b.value().attr("href"))
        .map(|s| s.to_string())
}

pub fn metadata(doc: &Html) -> Meta {
    let title = meta_attr(doc, "meta[property='og:title']", "content")
        .or_else(|| meta_attr(doc, "meta[name='twitter:title']", "content"))
        .or_else(|| {
            let sel = scraper::Selector::parse("title").ok()?;
            doc.select(&sel).next().map(|t| t.text().collect::<String>().trim().to_string())
        })
        .filter(|t| !t.is_empty());

    let byline = meta_attr(doc, "meta[name='author']", "content")
        .or_else(|| meta_attr(doc, "meta[property='article:author']", "content"))
        .or_else(|| json_ld_find(doc, "\"author\"", "\"name\""));

    let published = meta_attr(doc, "meta[property='article:published_time']", "content")
        .or_else(|| meta_attr(doc, "meta[name='date']", "content"))
        .or_else(|| meta_attr(doc, "meta[itemprop='datePublished']", "content"))
        .or_else(|| {
            let sel = scraper::Selector::parse("time[datetime]").ok()?;
            doc.select(&sel)
                .next()
                .and_then(|t| t.value().attr("datetime"))
                .map(|s| s.to_string())
        })
        .or_else(|| json_ld_find(doc, "\"datePublished\"", ""))
        .map(|d| d.chars().take(10).collect()); // date part only

    let site = meta_attr(doc, "meta[property='og:site_name']", "content")
        .or_else(|| meta_attr(doc, "meta[name='application-name']", "content"));

    Meta { title, byline, published, site }
}

fn meta_attr(doc: &Html, selector: &str, attr: &str) -> Option<String> {
    let sel = scraper::Selector::parse(selector).ok()?;
    doc.select(&sel)
        .next()
        .and_then(|m| m.value().attr(attr))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Minimal hand-rolled JSON-LD value finder. JSON-LD in the
/// wild is malformed often enough that a real parser fails
/// more than a tolerant string search.
fn json_ld_find(doc: &Html, key: &str, subkey: &str) -> Option<String> {
    let sel = scraper::Selector::parse("script[type='application/ld+json']").ok()?;
    for script in doc.select(&sel) {
        let text: String = script.text().collect();
        let Some(ki) = text.find(key) else { continue };
        let region = &text[ki + key.len()..text.len().min(ki + key.len() + 300)];
        let hay = if subkey.is_empty() {
            region
        } else {
            let Some(si) = region.find(subkey) else { continue };
            &region[si + subkey.len()..]
        };
        // First "..." string after the key.
        let Some(q1) = hay.find('"') else { continue };
        let Some(q2) = hay[q1 + 1..].find('"') else { continue };
        let val = &hay[q1 + 1..q1 + 1 + q2];
        if !val.is_empty() && val.len() < 200 {
            return Some(val.to_string());
        }
    }
    None
}
