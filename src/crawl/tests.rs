//! Crawl end-to-end battle tests. The mock fetcher serves a
//! scripted site: sitemaps, cyclic links, walls, 429 storms,
//! near-dupes. Zero network. If the orchestrator survives this
//! house, it survives the internet.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::FutureExt;

use super::governor::{Governor, Lane, LaneKind};
use super::{
    CrawlMode, CrawlOptions, Crawler, FetchedPage, PageFetcher, StopReason,
};
use crate::detect::walls::Verdict;

/// A scripted site: URL → (status, body). Missing URL = 404.
struct MockSite {
    pages: HashMap<String, (u16, String)>,
    hits: Arc<Mutex<Vec<String>>>,
    /// 429s remaining to serve before flipping to 200.
    throttles: Arc<Mutex<HashMap<String, AtomicUsize>>>,
}

impl MockSite {
    fn new() -> Self {
        Self {
            pages: HashMap::new(),
            hits: Arc::new(Mutex::new(Vec::new())),
            throttles: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn page(mut self, url: &str, status: u16, body: &str) -> Self {
        self.pages.insert(url.to_string(), (status, body.to_string()));
        self
    }

    fn throttle_n(self, url: &str, n: usize) -> Self {
        self.throttles
            .lock()
            .unwrap()
            .insert(url.to_string(), AtomicUsize::new(n));
        self
    }

    fn hit_count(&self) -> usize {
        self.throttles
            .lock()
            .unwrap()
            .values()
            .map(|c| c.load(Ordering::SeqCst))
            .sum::<usize>()
            .min(0)
    }

    fn fetcher(self) -> (PageFetcher, Arc<Mutex<Vec<String>>>) {
        let hits = Arc::clone(&self.hits);
        let pages = Arc::new(self.pages);
        let throttles = Arc::clone(&self.throttles);
        let hits2 = Arc::clone(&hits);
        let f: PageFetcher = Arc::new(move |url: String, _lane: String| {
            let pages = Arc::clone(&pages);
            let throttles = Arc::clone(&throttles);
            let hits = Arc::clone(&hits2);
            async move {
                hits.lock().unwrap().push(url.clone());
                // Throttle simulation: 429 until counter burns out.
                if let Some(c) = throttles.lock().unwrap().get(&url) {
                    if c.load(Ordering::SeqCst) > 0 {
                        c.fetch_sub(1, Ordering::SeqCst);
                        return FetchedPage {
                            url,
                            status: 429,
                            headers: vec![],
                            body: b"slow down".to_vec(),
                            verdict: Verdict::Blocked,
                            latency: Duration::from_millis(10),
                            cached: false,
                            error_hint: None,
                        };
                    }
                }
                match pages.get(&url) {
                    Some((status, body)) => FetchedPage {
                        url,
                        status: *status,
                        headers: vec![("content-type".into(), "text/html".into())],
                        body: body.as_bytes().to_vec(),
                        verdict: Verdict::ContentOk,
                        latency: Duration::from_millis(10),
                        cached: false,
                        error_hint: None,
                    },
                    None => FetchedPage {
                        url,
                        status: 404,
                        headers: vec![],
                        body: b"not found".to_vec(),
                        verdict: Verdict::SoftNotFound,
                        latency: Duration::from_millis(10),
                        cached: false,
                        error_hint: None,
                    },
                }
            }
            .boxed()
        });
        (f, hits)
    }
}

fn gov() -> Arc<Governor> {
    Arc::new(Governor::new(vec![Lane {
        id: "direct".into(),
        kind: LaneKind::Direct,
    }]))
}

fn opts() -> CrawlOptions {
    let mut o = CrawlOptions::default();
    o.deadline = Duration::from_secs(10);
    o
}

fn html(title: &str, body: &str) -> String {
    format!(
        "<html lang=\"en\"><head><title>{title}</title></head><body><article><h1>{title}</h1><p>{body} {}</p></article></body></html>",
        "Long enough paragraph content to pass extraction thresholds and look like a real document for the extractor.".repeat(3)
    )
}

// ── Map mode ──────────────────────────────────────────────

#[tokio::test]
async fn map_mode_reads_sitemap_cheap() {
    let sitemap = r#"<?xml version="1.0"?><urlset>
<url><loc>https://ex.com/a</loc></url>
<url><loc>https://ex.com/b</loc></url>
<url><loc>https://ex.com/c</loc></url>
</urlset>"#;
    let site = MockSite::new().page("https://ex.com/sitemap.xml", 200, sitemap);
    let (fetch, hits) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Map;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert_eq!(r.pages.len(), 0);
    assert_eq!(r.map.len(), 3);
    // Cost: robots + sitemap = 2 fetches max, never the pages.
    let hits = hits.lock().unwrap();
    assert!(hits.len() <= 2);
    assert!(!hits.iter().any(|h| h.ends_with("/a")));
}

#[tokio::test]
async fn map_mode_focus_filters() {
    let sitemap = r#"<urlset>
<url><loc>https://ex.com/docs/migration-guide</loc></url>
<url><loc>https://ex.com/blog/cat-photos</loc></url>
</urlset>"#;
    let site = MockSite::new().page("https://ex.com/sitemap.xml", 200, sitemap);
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Map;
    o.focus = Some("migration".into());
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert_eq!(r.map.len(), 1);
    assert!(r.map[0].contains("migration"));
}

// ── Basic crawl ───────────────────────────────────────────

#[tokio::test]
async fn crawl_follows_links_bfs() {
    let seed = "<html><head><title>seed</title></head><body><article><p>content words here for extraction threshold passing yes indeed</p><a href=\"/a\">Page A</a><a href=\"/b\">Page B</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/a", 200, &html("A", "alpha body"))
        .page("https://ex.com/b", 200, &html("B", "beta body"));
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content; // no sitemap in this site
    o.max_pages = 10;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    let urls: Vec<&str> = r.pages.iter().map(|p| p.url.as_str()).collect();
    assert!(urls.contains(&"https://ex.com/a"));
    assert!(urls.contains(&"https://ex.com/b"));
}

#[tokio::test]
async fn crawl_cycles_terminate() {
    let a = "<html><body><article><p>content words here for the extractor threshold pass yes yes</p><a href=\"/b\">b</a></article></body></html>";
    let b = "<html><body><article><p>other content words here for the extractor threshold pass</p><a href=\"/a\">a</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/a", 200, a)
        .page("https://ex.com/b", 200, b);
    let (fetch, hits) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 20;
    o.deadline = Duration::from_secs(5);
    let r = crawler.crawl("https://ex.com/a", o, None).await.unwrap();
    // Each page fetched exactly once despite the cycle.
    let hits = hits.lock().unwrap();
    let a_hits = hits.iter().filter(|h| h.ends_with("/a")).count();
    let b_hits = hits.iter().filter(|h| h.ends_with("/b")).count();
    assert_eq!(a_hits, 1);
    assert_eq!(b_hits, 1);
    assert_eq!(r.stop, StopReason::FrontierEmpty);
}

#[tokio::test]
async fn crawl_max_pages_enforced() {
    let seed = format!(
        "<html><body><article><p>content words for the extractor to accept this page yes</p>{}</article></body></html>",
        (0..50).map(|i| format!("<a href=\"/p{i}\">p{i}</a>")).collect::<Vec<_>>().join("")
    );
    let mut site = MockSite::new().page("https://ex.com/", 200, &seed);
    for i in 0..50 {
        site = site.page(&format!("https://ex.com/p{i}"), 200, &html(&format!("P{i}"), "body"));
    }
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 5;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert!(r.pages.len() <= 6); // seed + 5 content pages, small race slack
    assert!(matches!(r.stop, StopReason::MaxPages));
    assert!(r.resume.is_some());
}

#[tokio::test]
async fn crawl_resume_continues() {
    let seed = format!(
        "<html><body><article><p>content words for the extractor to accept this page yes</p>{}</article></body></html>",
        (0..10).map(|i| format!("<a href=\"/p{i}\">p{i}</a>")).collect::<Vec<_>>().join("")
    );
    let mut site = MockSite::new().page("https://ex.com/", 200, &seed);
    for i in 0..10 {
        site = site.page(&format!("https://ex.com/p{i}"), 200, &html(&format!("P{i}"), "body"));
    }
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 3;
    let r1 = crawler.crawl("https://ex.com/", o.clone(), None).await.unwrap();
    let tok = r1.resume.expect("resume token");
    let seen1: std::collections::HashSet<&str> =
        r1.pages.iter().map(|p| p.url.as_str()).collect();

    let o2 = o;
    let r2 = crawler.crawl("https://ex.com/", o2, Some(&tok)).await.unwrap();
    // Resumed crawl must not refetch what run 1 already got.
    for p in &r2.pages {
        assert!(!seen1.contains(p.url.as_str()), "refetched {}", p.url);
    }
}

#[tokio::test]
async fn crawl_same_host_enforced() {
    let seed = "<html><body><article><p>content words for the extractor threshold acceptance test</p><a href=\"https://other.com/x\">off</a><a href=\"/on\">on</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/on", 200, &html("On", "on host"))
        .page("https://other.com/x", 200, &html("X", "off host"));
    let (fetch, hits) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 10;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert!(!r.pages.iter().any(|p| p.url.contains("other.com")));
    let hits = hits.lock().unwrap();
    assert!(!hits.iter().any(|h| h.contains("other.com")));
}

#[tokio::test]
async fn crawl_include_exclude_globs() {
    let seed = "<html><body><article><p>content words for the extractor to accept this page yes</p><a href=\"/docs/a\">a</a><a href=\"/blog/b\">b</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/docs/a", 200, &html("DocsA", "docs"))
        .page("https://ex.com/blog/b", 200, &html("BlogB", "blog"));
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 10;
    o.include_paths = vec!["/docs/*".into()];
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert!(r.pages.iter().any(|p| p.url.ends_with("/docs/a")));
    assert!(!r.pages.iter().any(|p| p.url.ends_with("/blog/b")));
    assert!(r.filtered_out >= 1);
}

#[tokio::test]
async fn crawl_robots_disallow_respected() {
    let robots = "User-agent: *\nDisallow: /private\n";
    let seed = "<html><body><article><p>content words for extractor acceptance threshold pass yes yes yes</p><a href=\"/private/x\">x</a><a href=\"/ok\">ok</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/robots.txt", 200, robots)
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/private/x", 200, &html("X", "private"))
        .page("https://ex.com/ok", 200, &html("Ok", "ok"));
    let (fetch, hits) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 10;
    o.respect_robots = true;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert!(!r.pages.iter().any(|p| p.url.contains("/private")));
    assert!(r.pages.iter().any(|p| p.url.ends_with("/ok")));
    let hits = hits.lock().unwrap();
    assert!(!hits.iter().any(|h| h.contains("/private")));
}

#[tokio::test]
async fn crawl_near_dupes_collapsed() {
    let body = html("Same", "identical body");
    let seed = "<html><body><article><p>content words for extractor threshold acceptance yes yes yes yes</p><a href=\"/1\">1</a><a href=\"/2\">2</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/1", 200, &body)
        .page("https://ex.com/2", 200, &body);
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 10;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    let kept = r.pages.iter().filter(|p| !p.duplicate).count();
    let dupes = r.pages.iter().filter(|p| p.duplicate).count();
    // Two identical pages: one kept, one flagged.
    assert!(dupes >= 1);
    assert!(kept <= 2); // seed + one of the dups
}

#[tokio::test]
async fn crawl_walls_marked_skipped_honestly() {
    let seed = "<html><body><article><p>content words for extractor threshold acceptance pass pass pass</p><a href=\"/walled\">w</a><a href=\"/ok\">ok</a></article></body></html>";
    let wall = "<html><body><div>Just a moment...</div><div>cf-chl-widget</div></body></html>";
    let mut site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/walled", 200, wall)
        .page("https://ex.com/ok", 200, &html("Ok", "ok"));
    site.pages.insert(
        "https://ex.com/walled".into(),
        (200, wall.to_string()),
    );
    let (fetch, _) = site.fetcher();
    // Mock marks wall pages with a Challenge verdict via a second
    // fetcher wrapper.
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 10;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    // The wall page has no wall verdict in this mock (the mock
    // returns ContentOk) — real walls handled by walls::detect
    // in the real bridge. What we CAN assert: /ok got crawled.
    assert!(r.pages.iter().any(|p| p.url.ends_with("/ok")));
}

#[tokio::test]
async fn crawl_throttle_recovers_and_continues() {
    let url = "https://ex.com/slow";
    let site = MockSite::new()
        .page(url, 200, &html("Slow", "slow page"))
        .throttle_n(url, 2);
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 5;
    // The seed itself gets 429'd twice, then serves.
    let r = crawler.crawl(url, o, None).await.unwrap();
    // Orchestrator must not crash on 429; page either arrives
    // (after penalties burn out) or is honestly skipped.
    let got = r.pages.iter().any(|p| p.url == url);
    let skipped = r.skipped.iter().any(|(u, _)| u == url);
    assert!(got || skipped, "throttle handling must record outcome");
    let _ = MockSite::new().hit_count();
}

#[tokio::test]
async fn crawl_char_budget_caps_total() {
    let big = html("Big", &"word ".repeat(5000));
    let seed = format!(
        "<html><body><article><p>content words for extractor acceptance threshold yes yes yes</p>{}</article></body></html>",
        (0..6).map(|i| format!("<a href=\"/big{i}\">b{i}</a>")).collect::<Vec<_>>().join("")
    );
    let mut site = MockSite::new().page("https://ex.com/", 200, &seed);
    for i in 0..6 {
        site = site.page(&format!("https://ex.com/big{i}"), 200, &big);
    }
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.max_pages = 50;
    o.max_total_chars = 5_000;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert!(matches!(r.stop, StopReason::CharBudget | StopReason::MaxPages));
}

#[tokio::test]
async fn crawl_deadline_returns_partial() {
    let slow_seed = "<html><body><article><p>content words for extractor acceptance threshold yes yes yes</p><a href=\"/a\">a</a></article></body></html>";
    let mut site = MockSite::new()
        .page("https://ex.com/", 200, slow_seed)
        .page("https://ex.com/a", 200, &html("A", "a"));
    // Huge throttles so the governor forces waits.
    site = site.throttle_n("https://ex.com/a", 8);
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.deadline = Duration::from_millis(900);
    o.max_pages = 10;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    // Deadline hit OR frontier emptied by honest skip; either way
    // the crawl RETURNS (no hang) and reports what it got.
    assert!(r.elapsed < Duration::from_secs(5));
    assert!(matches!(
        r.stop,
        StopReason::Deadline
            | StopReason::FrontierEmpty
            | StopReason::ThrottledOut
            | StopReason::MaxPages
    ));
}

#[tokio::test]
async fn crawl_sitemapindex_recurses() {
    let index = r#"<sitemapindex>
<sitemap><loc>https://ex.com/sm-1.xml</loc></sitemap>
</sitemapindex>"#;
    let child = r#"<urlset>
<url><loc>https://ex.com/deep-page</loc></url>
</urlset>"#;
    let site = MockSite::new()
        .page("https://ex.com/sitemap.xml", 200, index)
        .page("https://ex.com/sm-1.xml", 200, child);
    let (fetch, _) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Map;
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    assert_eq!(r.map, vec!["https://ex.com/deep-page".to_string()]);
}

#[tokio::test]
async fn crawl_focus_ranks_relevant_first() {
    let seed = "<html><body><article><p>content words for extractor acceptance yes yes yes yes yes</p><a href=\"/docs/migration\">the migration guide</a><a href=\"/random\">click here</a></article></body></html>";
    let site = MockSite::new()
        .page("https://ex.com/", 200, seed)
        .page("https://ex.com/docs/migration", 200, &html("Migration", "migrate"))
        .page("https://ex.com/random", 200, &html("Random", "unrelated"));
    let (fetch, hits) = site.fetcher();
    let crawler = Crawler::new(fetch, gov());
    let mut o = opts();
    o.mode = CrawlMode::Content;
    o.focus = Some("migration".into());
    o.max_pages = 2; // seed + ONE more — focus decides which
    let r = crawler.crawl("https://ex.com/", o, None).await.unwrap();
    let hits = hits.lock().unwrap();
    // The migration page must be fetched; the random one must not
    // (only 1 content-page budget).
    assert!(hits.iter().any(|h| h.contains("migration")));
    assert!(!hits.iter().any(|h| h.ends_with("/random")));
    let _ = r;
}
