<div align="center">

# 🌐 DonSeTch

**Give your AI agent the web. Built from scratch in Rust. No keys, no accounts.**

Fetch · search · crawl · bypass bot walls · read PDFs (even scanned) · semantic reranking
One MCP server · custom BoringSSL TLS · headless browser escalation · zero API keys

[![Version](https://img.shields.io/crates/v/donsetch.svg?label=crates.io)](https://crates.io/crates/donsetch)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_v3-blue.svg)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/dondai44423/donsetch/ci.yml?label=CI)](https://github.com/dondai44423/donsetch/actions/workflows/ci.yml)
[![GitHub stars](https://img.shields.io/github/stars/dondai44423/donsetch?style=social)](https://github.com/dondai44423/donsetch/stargazers)

```bash
cargo install donsetch
```

[Install](#-install) · [The 3 tools](#-the-3-tools) · [Search](#-keyless-search) · [Fetch](#-fetch--anti-bot) · [Crawl](#-crawl) · [PDF](#-pdf--ocr) · [Comparison](#-comparison) · [Gotchas](#-known-gotchas) · [Honest limits](#-honest-limits)

</div>

<br>

---

## What it is

DonSeTch is an [MCP](https://modelcontextprotocol.io) server that gives any AI agent (Claude Code, Cursor, OpenCode, Pi, anything that speaks MCP) full web research capabilities from a single local process. Three tools, zero API keys, zero accounts.

- **Built from scratch in Rust** — custom BoringSSL TLS stack (real Chrome ClientHello), own HTTP/1.1 + HTTP/2 transport, own HTML-to-markdown extraction engine, own PDF parser (PDFium FFI), own search aggregator, own crawl engine. No Python, no Playwright, no Selenium, no dependency on existing OSS web tooling.
- **$0 forever, AGPL v3** — no keys, no accounts, no per-request billing, no data routed through third-party scrapers. Search is keyless and local.
- **249 tests, zero clippy warnings** — built to survive senior review.

> DonSeTch is for the agent itself. You install it once; the agent calls it whenever it needs the web.

---

## The 3 tools

| Tool | One-liner |
|------|-----------|
| [`fetch`](#-fetch--anti-bot) | Fetch any URL as clean markdown. HTTP first, auto-escalates to a headless browser if blocked. PDFs (with OCR), `focus` for token savings, `toc`/`section` for structure, pagination. |
| [`search`](#-keyless-search) | Keyless multi-engine web search. 10+ backends in parallel, fused by cross-engine consensus + semantic reranking. Returns URLs + snippets, not content. |
| [`crawl`](#-crawl) | Best-first same-domain crawl. Sitemap discovery + frontier walk with adaptive pacing. `focus` for budget management, resume tokens for large sites. |

---

## 🔑 Keyless search

No API key, no account, no third-party service. DonSeTch's search runs **10+ keyless backends in parallel** on your machine, merges, dedups, and ranks.

- **10+ independent backends**: Brave, Bing, DuckDuckGo, Mojeek, plus keyless verticals (GitHub, Wikipedia, Hacker News, Semantic Scholar, arXiv, StackExchange, MDN, Google News). Six+ independent index families, not the same feed twice.
- **Cross-engine consensus**: a URL returned by several independent indexes gets a consensus boost. A free authority signal from merging, no extra fetches. Every result carries `score`, `consensus` count, and `engines` list.
- **Semantic reranking**: a local ONNX cross-encoder (`ms-marco-MiniLM-L-6-v2`, 23MB, Apache-2.0) reads query + title + snippet through full attention and re-scores by semantic relevance. Blends 60/40 with RRF + BM25 + consensus. Downloads once, cached, runs on CPU (~5ms/pair). Graceful no-op if the model is unavailable.
- **Intent detection**: `auto` (default) detects query intent and routes to the right verticals. `code` adds GitHub + HN + StackExchange + MDN. `paper` adds Scholar + arXiv. `news` adds Google News + HN. `entity` adds Wikipedia.
- **Adaptive egress governor**: fan-out width shrinks under stress. Engine trust is learned via EWMA. Chronic-failure engines are quarantined (3 strikes, 10-min bench). Single-flight deduplication: two identical in-flight queries spend budget once.
- **Honest reporting**: `weak=true` means low cross-engine consensus — treat with care. Per-engine status (`ok` / `blocked:429` / `timeout` / `no-results`) is always visible. Never a fake "no results" that's actually a rate limit.

<details>
<summary><b>Search engine resilience</b></summary>

Scraping public engines from your IP can be rate-limited. DonSeTch is honest about that, then makes the no-proxy case as reliable as possible:

| # | Mechanism | What it does |
|---|-----------|--------------|
| 1 | **Per-engine trust EWMA** | Each engine has a learned trust score (0.2..2.0). High-trust engines get fan-out priority; low-trust engines are the first cut under stress. |
| 2 | **Adaptive fan-out width** | Healthy pool = all engines. Stressed (30%+) = max 3. Heavy (50%+) = max 2. Starved (65%+) = top engine + verticals. Consensus survives at width 2 by construction. |
| 3 | **Chronic-failure quarantine** | 3 consecutive failures = benched for 10 min. A walled engine stops wasting a fan-out slot every query. |
| 4 | **Single-flight** | Two identical in-flight queries share one leader's result. Agent parallel tool calls that hit the same query spend egress budget once. |
| 5 | **Retry wave** | Failed engines get one retry through a fresh egress — but only when the first wave left the merge thin. A healthy merge never pays retry latency. |
| 6 | **Persistent disk cache** | Queries cached to disk with intent + recency-aware TTL. Every cached query is one that never touches an egress. Survives process restarts. |
| 7 | **`DONSEEK_PROXIES`** | Route engine requests through your own proxies (HTTP/SOCKS5). Proxy preflight at startup benches dead lines before the first query. |

</details>

---

## 🌐 Fetch & anti-bot

`fetch` tries plain HTTP first (~1s) using DonSeTch's own BoringSSL TLS stack — real Chrome ClientHello, not a RustTLS fingerprint that gets you blocked. If the site serves a bot wall or a JS shell, it auto-escalates to a headless browser, solves the challenge, and downgrades back.

- **Custom BoringSSL TLS**: the same BoringSSL that Chrome uses, with `mlkem` post-quantum key exchange. Your fetch has Chrome's TLS fingerprint, not a library's.
- **Own HTTP/1.1 + HTTP/2**: built from scratch, including HPACK header compression, flow control, and connection pooling. No `reqwest`, no `hyper`, no `isahc`.
- **Two-tier escalation**: tier 1 = fast stealth HTTP. Tier 2 = headless browser (CDP, no Runtime/Console/Debugger domains — pure DOM reads). Auto-escalation on wall detection, auto-downgrade after solve.
- **Self-improving fetch loop**: the domain profile remembers which sites need tier 2. After one solve, clearance cookies ride tier 1 from the next fetch (warm start). Cookie lifetimes are learned adaptively. If warm cookies go stale, the loop rechecks cold first.
- **Bot wall detection**: Cloudflare, DataDome, PerimeterX, Akamai, generic interstitials. Detected by page size + marker heuristics. Interactive captchas (hCaptcha, reCAPTCHA, Turnstile checkbox) are an honest dead end — no solving service by design.
- **DonSift extraction engine**: HTML bytes in, agent-native markdown out. Block model with heading breadcrumbs. Token-war policies: link-farm drops, bare-link line drops, table caps, cross-block duplicate suppression. Content classification: `Article` / `Listing` / `Forum` / `Docs` / `Table` / `Page`.
- **Query-focused extraction**: `focus="query"` returns only BM25-relevant blocks. Cuts context 80%+ on long pages. If nothing matches, returns full page with a notice.
- **`toc` + `section` workflow**: `toc=true` returns the heading outline only. Then `section="heading name"` targets one section. Two cheap calls instead of one expensive one.
- **Pagination**: long pages truncate with a `next_offset` marker. Call again with `offset=that value` to continue.
- **Token savers**: links and images stripped by default (~30% token savings). Enable with `links=true`, `media=true`.

<details>
<summary><b>Anti-bot benchmark</b></summary>

| Site | Protection | Status | Notes |
|---|---|---|---|
| Cloudflare-protected sites | Cloudflare interstitial | **200 OK** | Challenge solved, content extracted |
| DataDome sites | DataDome | **200 OK** | Clearance cookies harvested |
| Stack Overflow | Cloudflare | **200 OK** | Full page content |
| Medium | Cloudflare | **200 OK** | Article content |
| NowSecure | Cloudflare challenge | **200 OK** | Title + content |
| Hacker News | None (baseline) | **200 OK** | Full page |
| Interactive captcha sites | hCaptcha / reCAPTCHA | **Honest block** | No solving service by design |

**Stealth signals:**
- `navigator.webdriver` = not detected (CDP only, no Runtime domain)
- TLS fingerprint = real Chrome BoringSSL (not RustTLS)
- No HeadlessChrome in any response
- Cookie jar persists clearance across fetches

</details>

---

## 🕷️ Crawl

`crawl` walks same-domain links in best-first order. Two phases: sitemap discovery (cheap URL inventory), then Governor-paced frontier walk with DonSift extraction per page.

- **Three modes**: `full` (default) = sitemap map + content. `map` = URL inventory only (very cheap, no content — see what a site has before committing). `content` = skip sitemap, BFS from seed.
- **Focus-ranked frontier**: `focus="query"` ranks pages by BM25 relevance and crawls only matching ones. Essential for large sites; without it the crawl wastes budget on noise.
- **Adaptive pacing**: the Governor paces per (host, lane) with adaptive backoff. Success → steady pace. 429/503 → exponential backoff. Error → cooldown. Crawl big sites without triggering rate limits.
- **Resume tokens**: large crawls stopped by budget/deadline return a resume token. Call again with `resume=token` to continue. Valid for 30 min, survives process restarts.
- **Near-dup detection**: title + first 200 normalized chars → hash signature. Duplicates are skipped, not re-extracted.
- **Honest stop reasons**: `FrontierEmpty` (done), `MaxPages` / `CharBudget` / `DepthLimit` / `Deadline` (budget — use resume), `ThrottledOut` (site blocked you — wait and resume).
- **Budget control**: `max_pages` (default 10, cap 200), `max_depth` (default 2), `max_total_chars` (default 60K), `per_page_max` (default 8K), `deadline_s` (default 120).
- **Path scoping**: `include_paths` / `exclude_paths` globs. `same_host` (default true). `respect_robots` (default true).

---

## 📄 PDF + OCR

DonSeTch detects PDFs (by Content-Type or `%PDF` magic bytes) and parses them to structured markdown using a custom PDFium FFI binding — no external PDF library, no Python subprocess.

- **Three-engine fusion**: PDFium text extraction → pixel-truth OCR (PP-OCR via ONNX Runtime) → form field extraction. Results fused by a three-evidence arbitration system.
- **Scanned PDFs**: image-only pages are auto-detected and OCR'd. The OCR arbitration cascade tries English → Chinese → Devanagari based on detected script.
- **Tables as markdown**: real table extraction, not flattened text. Multi-column reading order preserved.
- **Honest flags**: `encrypted` (password required), `scanned` (OCR was used), `vertical` (reading order may be wrong), `corrupt` (parse failed). Quality claims explicitly separated from verified evidence.
- **Forms as data**: AcroForm field names + values extracted as a structured table, not flattened text.
- **Orientation canonicalization**: rotated pages auto-detected and corrected. BiDi text handled via Unicode bidirectional algorithm.

<details>
<summary><b>PDF battle test results</b></summary>

40-document battle corpus, zero garbage output, 6-14x faster than Python-based alternatives. 120/120 fuzz clean.

| Document type | Result | Notes |
|---|---|---|
| Academic papers (Attention Is All You Need, etc.) | **Clean text** | Math symbols recovered, not CID garbage |
| Scanned documents | **OCR'd** | PP-OCR cascade, confidence-scored |
| Tax forms (W-9, etc.) | **Forms as data** | Field names + values as table |
| Multi-column layouts | **Reading order preserved** | Column detection + merge |
| Encrypted PDFs | **Honest flag** | `encrypted: password required` |
| Corrupt PDFs | **Honest flag** | `corrupt: parse failed at offset N` |

</details>

---

## Install

### Prerequisites

| Dependency | Why | Linux | macOS | Windows |
|---|---|---|---|---|
| **Rust 1.75+** | Build toolchain | `rustup` | `rustup` | `rustup` |
| **Go 1.22+** | BoringSSL build system | `pacman -S go` / `apt install golang` | `brew install go` | `winget install GoLang.Go` |
| **NASM** | BoringSSL x86_64 assembly | `pacman -S nasm` / `apt install nasm` | `brew install nasm` | `choco install nasm` |
| **LLVM/Clang** | bindgen (BoringSSL headers) | usually pre-installed | usually pre-installed | `choco install llvm` |
| **CMake** | BoringSSL build | `pacman -S cmake` / `apt install cmake` | `brew install cmake` | `winget install cmake` |
| **Chromium** (optional) | Tier 2 browser escalation | `pacman -S chromium` | `brew install chromium` | Edge works |

### Build

```bash
git clone https://github.com/dondai44423/donsetch.git
cd donsetch
cargo build --release
```

The binary lands at `target/release/donsetch`.

<details>
<summary><b>Build notes</b></summary>

- **BoringSSL** is vendored and built from source via `boring-sys`. First build takes ~5 min (compiling BoringSSL). Subsequent builds are cached.
- **PDFium** is downloaded as a static library by `build.rs` (Linux/macOS/Windows). No manual setup.
- **ONNX Runtime** is downloaded by `oar-ocr` (OCR) and `ort` (reranker) at build time.
- **Models** (OCR + reranker) download on first use to `~/.cache/donsetch/`, not bundled in the binary.
- **Feature flags**: `default = ["ocr", "rerank"]`. Build with `--no-default-features` for HTTP-only (no OCR, no reranker, smaller binary).
- **Cross-compilation**: targets installed via `rustup target add`. Platform code is `#[cfg]`-gated, not forked.

</details>

### Configure your MCP client

DonSeTch speaks stdio MCP. Point any MCP client at the binary:

**Claude Code** (`~/.claude/mcp.json`):
```json
{
  "mcpServers": {
    "donsetch": {
      "command": "/path/to/donsetch"
    }
  }
}
```

**Cursor** (Settings → MCP):
```json
{
  "mcpServers": {
    "donsetch": {
      "command": "/path/to/donsetch"
    }
  }
}
```

**Pi** (`~/.pi/agent/extensions/donsetch.json`):
```json
{
  "type": "stdio",
  "command": "/path/to/donsetch"
}
```

No arguments, no API keys, no environment variables needed.

<details>
<summary><b>Optional environment variables</b></summary>

| Variable | Purpose |
|---|---|
| `DONSEEK_PROXIES` | Comma-separated proxies for search engines (`http://host:port`, `socks5://...`, `user:pass@host:port`). Proxy preflight at startup benches dead lines. |
| `DONGHOST_DEBUG` | Print ghost solve/render debug to stderr. |
| `DONSHEET_DEBUG` | Print PDF/OCR debug to stderr. |
| `DONSHEET_DEBUG_CHARS` | Print PDF layout character stream. |

</details>

---

## Comparison

| | **DonSeTch** | Hound | Crawl4AI | Jina Reader | Firecrawl (OSS) |
|---|---|---|---|---|---|
| **Language** | Rust | Python | Python | Python (API) | TypeScript |
| **Price** | $0 forever | $0 forever | $0 (self-host) | free, rate-limited | $0 self-host / 1K free |
| **Runs locally** | yes | yes | yes | no (their API) | self-host: yes |
| **TLS fingerprint** | Real Chrome (BoringSSL) | primp (curl-impersonate) | requests | their servers | their servers |
| **Web search** | yes (keyless, 10+ backends) | yes (keyless, 10 backends) | **no** | yes | **no** |
| **Semantic reranking** | yes (local ONNX cross-encoder) | yes (local ONNX) | no | no | no |
| **Deep crawl** | yes (best-first, sitemap, resume) | yes (best-first, sitemap) | yes | no | yes (cloud) |
| **Anti-bot** | built-in (BoringSSL + CDP browser) | built-in (Patchright) | limited | none | not by default |
| **PDF → markdown** | yes (PDFium + OCR, tables, forms) | yes (pdfplumber + OCR) | partial | yes (native) | yes (cloud + OCR) |
| **Scanned-PDF OCR** | yes (PP-OCR, ONNX) | yes (rapidocr) | **no** | no | yes (cloud paid) |
| **Query-focused extraction** | yes (`focus`, BM25) | yes (`focus`, BM25) | yes (BM25) | no | no |
| **MCP server** | yes (official) | yes (official) | community | yes (official) | build it |
| **Token cost (tools/list)** | ~1.8K (3 tools) | ~2.7K (6 tools) | varies | n/a | varies |
| **License** | AGPL v3 | MIT | Apache 2.0 | proprietary | MIT |

**The short version:** DonSeTch is the only tool that builds the entire stack from scratch in Rust — TLS, HTTP/2, extraction, search, crawl, PDF — giving you Chrome's real TLS fingerprint without Python, without Playwright, and without a single API key.

---

## ⚠️ Known gotchas

| Gotcha | What to know |
|---|---|
| **First build takes ~5 min** | BoringSSL is compiled from source. Subsequent builds are cached via `Swatinem/rust-cache`. |
| **Go is a build dependency** | BoringSSL's build system is Go-based. You need Go installed even though DonSeTch is Rust. |
| **Interactive captchas are not solved** | hCaptcha, reCAPTCHA, and Turnstile checkboxes are an honest dead end. No solving service by design. The response says exactly what blocked you. |
| **robots.txt is ON by default for crawl** | `respect_robots=true` is the default for crawl. `fetch` doesn't check robots (many sites disallow all non-Googlebot agents). |
| **Search rate-limits without a proxy** | Keyless search scrapes public engines from your IP. Sustained heavy use can get rate-limited. Set `DONSEEK_PROXIES` for heavy use. |
| **ML-DSA sigalgs not yet supported** | BoringSSL 5.1.0 lacks ML-DSA post-quantum signatures. Will be added when BoringSSL gains it. |
| **Not built for mass scraping** | DonSeTch is designed for agentic research: AI agents fetching pages, searching, crawling docs. Not a bulk scraping tool. |

---

## ⚠️ Honest limits

| Limit | What happens instead |
|---|---|
| **Interactive captchas** | Not bypassed. The response tells the agent to switch sources. |
| **Sites requiring login** | Out of scope (DonSeTch does page rendering, not authenticated sessions). |
| **outerWidth/Height in headless** | Protocol-level override only — some sites may detect the mismatch. |
| **Windows/macOS PDF subsystem** | Compiled but CI verification pending — Linux is the primary target. |
| **Search without any healthy engine** | If all engines are blocked, search returns an error with per-engine status. Honest, not a fake "no results". |

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DonSeTch is AGPL v3 — all contributions must be under the same license.

---

## License

Copyright (c) 2026 Bishesh Bhandari. Licensed under [AGPL v3](LICENSE).

---

<div align="center">

### If DonSeTch saves you time, ⭐ the repo

[![GitHub stars](https://img.shields.io/github/stars/dondai44423/donsetch?style=social)](https://github.com/dondai44423/donsetch/stargazers)

**AGPL v3** · [Changelog](CHANGELOG.md) · [Issues](https://github.com/dondai44423/donsetch/issues)

</div>
