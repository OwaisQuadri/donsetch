<div align="center">

# 🌐 DonSeTch

**Give your AI agent the web. Built from scratch in Rust. No keys, no accounts.**

Fetch · search · crawl · bypass bot walls · read PDFs (even scanned) · semantic reranking
One MCP server · custom BoringSSL TLS · headless browser · zero API keys · zero Python

[![Release](https://img.shields.io/github/v/release/dondai44423/donsetch?color=00d4aa&label=release&style=flat-square)](https://github.com/dondai44423/donsetch/releases)
[![Rust](https://img.shields.io/badge/Rust-1.75+-ce422b?style=flat-square)](https://www.rust-lang.org)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-00d4aa&style=flat-square)](LICENSE)
[![CI](https://img.shields.io/github/actions/workflow/status/dondai44423/donsetch/ci.yml?label=CI&style=flat-square)](https://github.com/dondai44423/donsetch/actions/workflows/ci.yml)
[![Stars](https://img.shields.io/github/stars/dondai44423/donsetch?color=ff9f43&style=flat-square)](https://github.com/dondai44423/donsetch)

</div>

<br>

```bash
cargo install donsetch
```

[Install](#-install) · [The 3 tools](#-the-3-tools) · [Search](#-keyless-search) · [Fetch](#-fetch--anti-bot) · [Crawl](#-crawl) · [PDF](#-pdf--ocr) · [Architecture](#-built-from-scratch) · [Comparison](#-comparison) · [Gotchas](#-gotchas) · [Limits](#-honest-limits)

---

DonSeTch is an [MCP](https://modelcontextprotocol.io) server that gives any AI agent (Claude Code, Cursor, OpenCode, anything that speaks MCP) full web research from a single local process. Three tools, zero API keys, zero accounts.

Built in **Rust**. One binary. No Python, no Playwright, no Selenium, no `reqwest`, no `hyper`. Every layer — TLS, HTTP, extraction, search, crawl, PDF — built from scratch. That's the point.

Speaks MCP **2024-11-05 through 2026-07-28**. Works with every MCP client, old and new.

### What makes it different

| Innovation | What it means | Status |
|---|---|---|
| **Real Chrome TLS** | Custom BoringSSL stack — same TLS library Chrome uses. Your fetch has Chrome's ClientHello, not a RustTLS fingerprint that gets you blocked. | Live |
| **Built from scratch** | Own HTTP/1.1 + HTTP/2, own HTML-to-markdown engine, own PDF parser (PDFium FFI), own search aggregator, own crawl engine. Zero dependency on existing OSS web tooling. | 249 tests |
| **Self-improving fetch** | Remembers which sites need tier 2. After one solve, clearance cookies ride tier 1 next time. Cookie lifetimes learned adaptively. | Live |
| **Semantic reranking** | Local ONNX cross-encoder reads query + title + snippet through full attention. Pushes out generic Wikipedia articles that keyword-match but aren't about the topic. | A/B verified |
| **Keyless search** | 10+ backends in parallel, fused by cross-engine consensus. No API keys, no accounts, no billing. $0 forever. | Live |
| **Adaptive egress governor** | Fan-out width shrinks under stress. Engine trust learned via EWMA. Chronic failures quarantined. Single-flight dedup for parallel agent calls. | Live |
| **PDF from scratch** | PDFium FFI + PP-OCR via ONNX. Three-engine fusion, honest flags. Tables as markdown, forms as data, scanned PDFs auto-OCR'd. | 40-doc battle |

---

## 🚀 Install

### Prerequisites

| Dependency | Why | Linux | macOS | Windows |
|---|---|---|---|---|
| **Rust 1.75+** | Build toolchain | `rustup` | `rustup` | `rustup` |
| **Go 1.22+** | BoringSSL build | `pacman -S go` | `brew install go` | `winget install GoLang.Go` |
| **NASM** | BoringSSL assembly | `pacman -S nasm` | `brew install nasm` | `choco install nasm` |
| **LLVM/Clang** | bindgen headers | pre-installed | pre-installed | `choco install llvm` |
| **CMake** | BoringSSL build | `pacman -S cmake` | `brew install cmake` | `winget install cmake` |
| **Chromium** *(optional)* | Tier 2 browser | `pacman -S chromium` | `brew install chromium` | Edge works |

### Build

```bash
git clone https://github.com/dondai44423/donsetch.git
cd donsetch
cargo build --release
```

Binary lands at `target/release/donsetch`. First build takes ~5 min (compiling BoringSSL). Subsequent builds are cached.

<details>
<summary><b>Build notes</b></summary>

- **BoringSSL** is vendored and built from source via `boring-sys`. First build compiles it (~5 min), then it's cached.
- **PDFium** is downloaded as a static library by `build.rs` — no manual setup.
- **ONNX Runtime** is downloaded by `oar-ocr` (OCR) and `ort` (reranker) at build time.
- **Models** (OCR + reranker) download on first use to `~/.cache/donsetch/`, not bundled in the binary.
- **Feature flags**: `default = ["ocr", "rerank"]`. Build with `--no-default-features` for HTTP-only (no OCR, no reranker, smaller binary).
- **Cross-compilation**: `rustup target add <target>`. Platform code is `#[cfg]`-gated, not forked.

</details>

### Connect your AI agent

MCP over stdio — point your agent at the binary:

```json
{ "mcpServers": { "donsetch": { "command": "/path/to/donsetch" } } }
```

No arguments, no API keys, no environment variables. Done.

<details>
<summary><b>Optional environment variables</b></summary>

| Env Var | What it does |
|---|---|
| `DONSEEK_PROXIES` | Comma-separated proxies for search engines (`http://host:port`, `socks5://...`, `user:pass@host:port`). Preflight at startup benches dead lines. |
| `DONGHOST_DEBUG` | Print ghost solve/render debug to stderr. |
| `DONSHEET_DEBUG` | Print PDF/OCR debug to stderr. |
| `DONSHEET_DEBUG_CHARS` | Print PDF layout character stream. |

</details>

---

## 🎯 The 3 tools

| Tool | One-liner |
|------|-----------|
| [`fetch`](#-fetch--anti-bot) | Fetch any URL as clean markdown. HTTP first, escalates to headless browser if blocked. PDFs with OCR, `focus` for token savings, `toc`/`section`, pagination. |
| [`search`](#-keyless-search) | Keyless multi-engine web search. 10+ backends in parallel, consensus + semantic reranking. Returns URLs + snippets, not content. |
| [`crawl`](#-crawl) | Best-first same-domain crawl. Sitemap + frontier, adaptive pacing, resume tokens. `focus` for budget management. |

---

## 🔑 Keyless search

No API key, no account, no third-party service. 10+ keyless backends in parallel on your machine, merged, deduped, ranked.

- **10+ independent backends**: Brave, Bing, DuckDuckGo, Mojeek + keyless verticals (GitHub, Wikipedia, HN, Semantic Scholar, arXiv, StackExchange, MDN, Google News). Six+ independent index families, not the same feed twice.
- **Cross-engine consensus**: a URL returned by several independent indexes gets a consensus boost. Free authority signal from merging. Every result carries `score`, `consensus` count, and `engines` list.
- **Semantic reranking**: local ONNX cross-encoder (`ms-marco-MiniLM-L-6-v2`, 23MB, Apache-2.0) reads query + title + snippet through full attention. Blends 60/40 with RRF + BM25 + consensus. ~5ms/pair on CPU. Graceful no-op if model unavailable.
- **Intent detection**: `auto` (default) routes to the right verticals. `code` adds GitHub + HN + StackExchange + MDN. `paper` adds Scholar + arXiv. `news` adds Google News + HN. `entity` adds Wikipedia.
- **Honest reporting**: `weak=true` means low consensus — treat with care. Per-engine status (`ok` / `blocked:429` / `timeout` / `no-results`) always visible. Never a fake "no results" that's actually a rate limit.

<details>
<summary><b>Search engine resilience</b></summary>

| # | Mechanism | What it does |
|---|-----------|--------------|
| 1 | **Per-engine trust EWMA** | Each engine has a learned trust score (0.2..2.0). High-trust engines get fan-out priority; low-trust cut first under stress. |
| 2 | **Adaptive fan-out width** | Healthy pool = all engines. Stressed (30%+) = max 3. Heavy (50%+) = max 2. Starved (65%+) = top engine + verticals. Consensus survives at width 2. |
| 3 | **Chronic-failure quarantine** | 3 consecutive failures = benched for 10 min. A walled engine stops wasting a slot. |
| 4 | **Single-flight** | Two identical in-flight queries share one leader's result. Agent parallel calls spend budget once. |
| 5 | **Retry wave** | Failed engines get one retry through a fresh egress — but only when the merge is thin. A healthy merge never pays retry latency. |
| 6 | **Persistent disk cache** | Queries cached with intent + recency-aware TTL. Every cached query never touches an egress. Survives restarts. |
| 7 | **`DONSEEK_PROXIES`** | Route engine requests through your proxies. Preflight benches dead lines before the first query. |

</details>

---

## 🌐 Fetch & anti-bot

`fetch` tries plain HTTP first (~1s) using DonSeTch's own BoringSSL TLS stack — real Chrome ClientHello. If the site serves a bot wall or a JS shell, it auto-escalates to a headless browser, solves the challenge, and downgrades back.

- **Custom BoringSSL TLS**: same BoringSSL Chrome uses, with `mlkem` post-quantum key exchange. Your fetch has Chrome's TLS fingerprint, not a library's.
- **Own HTTP/1.1 + HTTP/2**: built from scratch — HPACK header compression, flow control, connection pooling. No `reqwest`, no `hyper`, no `isahc`.
- **Two-tier escalation**: tier 1 = fast stealth HTTP. Tier 2 = headless browser (CDP, no Runtime/Console/Debugger — pure DOM reads). Auto-escalation on wall, auto-downgrade after solve.
- **Self-improving fetch loop**: domain profile remembers which sites need tier 2. After one solve, clearance cookies ride tier 1 from the next fetch (warm start). Cookie lifetimes learned adaptively. If warm cookies go stale, rechecks cold first.
- **Bot wall detection**: Cloudflare, DataDome, PerimeterX, Akamai, generic interstitials. Interactive captchas (hCaptcha, reCAPTCHA, Turnstile checkbox) are an honest dead end — no solving service by design.
- **DonSift extraction**: HTML bytes in, agent-native markdown out. Block model with heading breadcrumbs. Token-war policies: link-farm drops, bare-link line drops, table caps, cross-block duplicate suppression. Content classified: `Article` / `Listing` / `Forum` / `Docs` / `Table` / `Page`.
- **`focus`** — BM25-relevant blocks only. Cuts context 80%+ on long pages. No match = full page with a notice.
- **`toc` + `section`** — heading outline first, then target one section. Two cheap calls instead of one expensive one.
- **Pagination** — `next_offset` in the response. Call again with `offset=that value`.
- **Token savers** — links and images stripped by default (~30% savings). Enable with `links=true`, `media=true`.

<details>
<summary><b>Anti-bot benchmark</b></summary>

| Site | Protection | Status |
|---|---|---|
| Cloudflare-protected sites | Cloudflare interstitial | **200 OK** |
| DataDome sites | DataDome | **200 OK** |
| Stack Overflow | Cloudflare | **200 OK** |
| Medium | Cloudflare | **200 OK** |
| NowSecure | Cloudflare challenge | **200 OK** |
| Hacker News | None (baseline) | **200 OK** |
| Interactive captcha sites | hCaptcha / reCAPTCHA | **Honest block** |

Stealth signals: `navigator.webdriver` not detected (CDP only, no Runtime domain), TLS = real Chrome BoringSSL, no HeadlessChrome in any response, cookie jar persists clearance across fetches.

</details>

---

## 🕷️ Crawl

`crawl` walks same-domain links in best-first order. Two phases: sitemap discovery (cheap URL inventory), then Governor-paced frontier walk with extraction per page.

- **Three modes**: `full` (default) = sitemap map + content. `map` = URL inventory only (very cheap — see what a site has before committing). `content` = skip sitemap, BFS from seed.
- **Focus-ranked frontier**: `focus="query"` ranks pages by BM25 relevance and crawls only matching ones. Essential for large sites; without it the crawl wastes budget on noise.
- **Adaptive pacing**: the Governor paces per (host, lane) with adaptive backoff. Success → steady. 429/503 → exponential. Error → cooldown. Crawl big sites without triggering rate limits.
- **Resume tokens**: large crawls stopped by budget/deadline return a resume token. Call again with `resume=token` to continue. Valid 30 min, survives restarts.
- **Near-dup detection**: title + first 200 normalized chars → hash. Duplicates skipped, not re-extracted.
- **Honest stop reasons**: `FrontierEmpty` (done), `MaxPages` / `CharBudget` / `DepthLimit` / `Deadline` (budget — use resume), `ThrottledOut` (site blocked you — wait and resume).

---

## 📄 PDF + OCR

DonSeTch detects PDFs (by Content-Type or `%PDF` magic bytes) and parses them to structured markdown using a custom PDFium FFI — no external PDF library, no Python subprocess.

- **Three-engine fusion**: PDFium text extraction → pixel-truth OCR (PP-OCR via ONNX Runtime) → form field extraction. Results fused by three-evidence arbitration.
- **Scanned PDFs**: image-only pages auto-detected and OCR'd. Cascade tries English → Chinese → Devanagari.
- **Tables as markdown**: real table extraction, not flattened text. Multi-column reading order preserved.
- **Forms as data**: AcroForm field names + values extracted as a structured table.
- **Orientation canonicalization**: rotated pages auto-detected and corrected. BiDi text via Unicode bidirectional algorithm.
- **Honest flags**: `encrypted` (password required), `scanned` (OCR used), `vertical` (reading order may be wrong), `corrupt` (parse failed). Quality claims separated from verified evidence.

<details>
<summary><b>PDF battle test results</b></summary>

40-document battle corpus, zero garbage output, 6-14x faster than Python alternatives. 120/120 fuzz clean.

| Document type | Result |
|---|---|
| Academic papers | **Clean text** — math symbols recovered, not CID garbage |
| Scanned documents | **OCR'd** — PP-OCR cascade, confidence-scored |
| Tax forms (W-9) | **Forms as data** — field names + values as table |
| Multi-column layouts | **Reading order preserved** — column detection + merge |
| Encrypted PDFs | **Honest flag** — `encrypted: password required` |
| Corrupt PDFs | **Honest flag** — `corrupt: parse failed at offset N` |

</details>

---

## 🏗️ Built from scratch

Every layer built in Rust. No dependency on existing OSS web tooling.

| Component | What it does | Key files |
|---|---|---|
| DonShadow | Tier 1 stealth HTTP (BoringSSL TLS, own HTTP/1.1 + HTTP/2) | `src/fetch/`, `src/transport/` |
| DonGhost | Tier 2 headless browser (CDP, no Runtime/Console/Debugger) | `src/ghost/` |
| DonSift | HTML-to-markdown extraction (block model, BM25 focus) | `src/extract/` |
| DonSeek | Keyless multi-engine search (RRF + BM25 + semantic reranking) | `src/search/` |
| DonTread | Crawl engine (sitemap, frontier, Governor pacing) | `src/crawl/` |
| DonSheet | PDF extraction (PDFium FFI, OCR arbitration, fusion) | `src/pdf/` |
| MCP daemon | stdio server (JSON-RPC 2.0, MCP 2024-11-05+) | `src/mcp/` |

**249 tests. Zero clippy warnings.** `cargo clippy --release -- -Dwarnings` is the law.

---

## 📊 Comparison

| | **DonSeTch** | Hound | Crawl4AI | Jina Reader | Firecrawl |
|---|---|---|---|---|---|
| **Language** | Rust | Python | Python | Python (API) | TypeScript |
| **TLS fingerprint** | Real Chrome (BoringSSL) | curl-impersonate | requests | their servers | their servers |
| **Web search** | yes (keyless, 10+) | yes (keyless, 10) | **no** | yes | **no** |
| **Semantic reranking** | yes (local ONNX) | yes (local ONNX) | no | no | no |
| **Deep crawl** | yes (resume tokens) | yes | yes | no | yes (cloud) |
| **Anti-bot** | BoringSSL + CDP browser | Patchright | limited | none | not by default |
| **PDF → markdown** | yes (PDFium + OCR) | yes (pdfplumber + OCR) | partial | yes (native) | yes (cloud) |
| **Scanned-PDF OCR** | yes (PP-OCR) | yes (rapidocr) | **no** | no | yes (paid) |
| **Query focus** | yes (BM25) | yes (BM25) | yes (BM25) | no | no |
| **Runs locally** | yes | yes | yes | no | self-host |
| **MCP server** | yes | yes | community | yes | build it |
| **Token cost** | ~1.8K (3 tools) | ~2.7K (6 tools) | varies | n/a | varies |
| **License** | AGPL v3 | MIT | Apache 2.0 | proprietary | MIT |

**The short version:** DonSeTch is the only tool that builds the entire stack from scratch in Rust — TLS, HTTP/2, extraction, search, crawl, PDF — giving you Chrome's real TLS fingerprint without Python, without Playwright, and without a single API key.

---

## ⚠️ Gotchas

| Surprise | Why |
|---|---|
| **First build takes ~5 min** | BoringSSL is compiled from source. Cached after that. |
| **Go is a build dependency** | BoringSSL's build system is Go-based. You need Go even though DonSeTch is Rust. |
| **Interactive captchas not solved** | hCaptcha, reCAPTCHA, Turnstile checkbox = honest dead end. No solving service by design. Response says exactly what blocked you. |
| **robots.txt ON by default for crawl** | `respect_robots=true` for crawl. `fetch` doesn't check robots (many sites disallow all non-Googlebot). |
| **Search rate-limits without a proxy** | Keyless search scrapes public engines from your IP. Sustained heavy use can get rate-limited. Set `DONSEEK_PROXIES`. |
| **Not built for mass scraping** | DonSeTch is for agentic research, not bulk extraction. |

---

## 🧱 Honest limits

| What it can NOT do | Why |
|---|---|
| **Solve CAPTCHAs** | Deliberate. You get a clear error, not a hang. Hand off to a solver. |
| **Sites requiring login** | Out of scope (page rendering, not authenticated sessions). |
| **ML-DSA post-quantum signatures** | BoringSSL 5.1.0 lacks them. Will be added when BoringSSL gains it. |
| **Windows/macOS PDF CI** | Compiled but CI verification pending. Linux is the primary target. |
| **Search with all engines down** | Returns an error with per-engine status. Honest, not a fake "no results". |

---

## 🤝 Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Run `cargo clippy --release -- -Dwarnings` and `cargo test --release` before submitting. AGPL v3 — all contributions under the same license.

## 📄 License

Copyright (c) 2026 Bishesh Bhandari. AGPL-3.0 — see [LICENSE](LICENSE).
