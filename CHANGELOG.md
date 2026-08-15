# Changelog

All notable changes to DonSeTch are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-08-15

First stable release. Feature-complete MCP server + CLI for web fetch, search, and crawl.

### Added

- **CLI**: full command-line interface — `fetch`, `search`, `crawl` with same engine as MCP.
  - `--json` for machine-readable output, `-q` for quiet mode, `--tier` for manual escalation control.
  - `keys` subcommand: manage BYOK search provider keys (`add`, `remove`, `list`, `default`, `reset`).
  - `doctor`: 9-check health diagnostics with auto-fix.
  - `update`: self-update from GitHub Releases (no API rate limits).
  - `rollback`: revert to previous version.
  - `version`: version + build info.
  - `tools`: print tool schemas as JSON (same as MCP `tools/list`).

- **BYOK search providers**: external search providers (TinyFish, Tavily, Serper, Exa) bypass the local engine entirely. Key stacking, rotation, rate-limit cooldown (60s auto-recovery), credit-depletion detection, local fallback. Config: `~/.cache/donsetch/byok-keys.json`.

- **Query-entity coverage penalty**: anchor entities (hyphenated compounds like "B-tree") and specifiers (version numbers, years) checked against results. Wrong entity = 0.3× score penalty. Fixes BM25 splitting "B-tree" → "b" + "tree" where "binary tree" matches. Universal — no-op for queries without entities.

- **Crawl v2**: transient retry (max 2), canonical URL resolution, pagination (`<link rel="next">`), RSS/Atom feed discovery, `<base href>` resolution, binary content-type guard, referer + sec-fetch-site chaining, parent metadata, score-sorted output, sitemap `<priority>` + `<lastmod>`, ghost escalation (capped 3/crawl). Seed URL always in scope.

- **Xvfb socket-file polling**: replaced `xdpyinfo` dependency with `/tmp/.X11-unix/X99` socket polling for Xvfb readiness. Fixes ghost browser launch failure on systems without `xorg-xdpyinfo`.

- **npm package**: `npm install -g donsetch` downloads platform-correct binary from GitHub Releases at install time (SHA256-verified).

- **Release workflow**: tag-triggered, 3-platform build (Linux x86_64, macOS arm64, Windows x86_64), binary verification, packaging (tar.gz + SHA256), GitHub release.

### Changed

- README rewritten for v1.0.0: removed BETA warnings, added two-usage-modes section (MCP + CLI), updated test counts, cleaned stale info.
- Rust edition 2024 (let-chains support).
- Test count: 388 (was 249 at 0.5.0).

### Fixed

- TinyFish BYOK adapter: GET (not POST), root path `/` (not `/search`), query params (not JSON body). Old endpoint returned 404 (Next.js catch-all), misclassified as rate-limited.
- Crawl seed scope: `--include`/`--exclude` apply to discovered links only, not the seed entry point.
- Flaky PDF test under parallel execution: non-PDF body + PDF content-type instead of fake `%PDF-1.4` body (avoids PDFium race).
- Xvfb readiness check: `xdpyinfo` dependency removed, socket-file polling added.

## [0.5.0] - 2026-08-07

Initial public beta. Feature-complete MCP server for web fetch, search, and crawl.

### Added

- **Fetch** (`fetch`): two-tier stealth HTTP fetch with auto-escalation to headless browser.
  - Custom BoringSSL TLS stack (real Chrome ClientHello, `mlkem` post-quantum key exchange).
  - Own HTTP/1.1 + HTTP/2 transport (HPACK, flow control, connection pooling). No `reqwest`, no `hyper`.
  - Self-improving fetch loop: persistent domain intelligence, adaptive cookie lifetimes, warm-start after solve.
  - Bot wall detection: Cloudflare, DataDome, PerimeterX, Akamai, generic interstitials.
  - DonSift extraction engine: block model, BM25 focus, heading breadcrumbs, token-war policies.
  - `toc` / `section` / `focus` / `selector` / `offset` / `links` / `media` params.
  - PDF detection and parsing (PDFium FFI, OCR, tables, forms).
  - Non-HTML passthrough (JSON, XML, text).
  - Content classification: Article / Listing / Forum / Docs / Table / Page.

- **Search** (`search`): keyless multi-engine web search.
  - 10+ backends in parallel: Brave, Bing, DuckDuckGo, Mojeek + keyless verticals (GitHub, Wikipedia, HN, Scholar, arXiv, StackExchange, MDN, Google News).
  - Cross-engine consensus ranking (weighted RRF + BM25 + domain priors + diversity cap).
  - Semantic reranking: local ONNX cross-encoder (`ms-marco-MiniLM-L-6-v2`, 23MB, Apache-2.0). 60/40 blend with RRF+BM25+consensus. Graceful no-op if model unavailable.
  - Intent detection: auto / web / code / paper / news / entity. Routes to appropriate verticals.
  - Adaptive egress governor: fan-out width shrinks under stress, engine trust EWMA, chronic-failure quarantine (3 strikes, 10-min bench), single-flight deduplication.
  - Persistent disk cache with intent + recency-aware TTL.
  - Honest reporting: `weak` flag, per-engine status, never a fake "no results".

- **Crawl** (`crawl`): best-first same-domain crawl.
  - Three modes: `full` (sitemap map + content), `map` (URL inventory only), `content` (BFS from seed).
  - Focus-ranked frontier: BM25 relevance scoring, crawl only matching pages.
  - Adaptive pacing: Governor with per-(host, lane) backoff. Success → steady, 429/503 → exponential, error → cooldown.
  - Resume tokens: continue stopped crawls across calls. Disk-backed, 30-min TTL.
  - Near-dup detection: title + content hash signature.
  - Path scoping: `include_paths` / `exclude_paths`, `same_host`, `respect_robots`.
  - Honest stop reasons: FrontierEmpty, MaxPages, CharBudget, DepthLimit, Deadline, ThrottledOut.

- **PDF engine** (DonSheet): custom PDFium FFI, three-engine fusion.
  - PDFium text extraction + pixel-truth OCR (PP-OCR via ONNX Runtime) + form field extraction.
  - OCR arbitration cascade: English → Chinese → Devanagari.
  - Tables as markdown, multi-column reading order, orientation canonicalization, BiDi text.
  - Forms as data: AcroForm field names + values as structured table.
  - Honest flags: encrypted, scanned, vertical, corrupt.
  - 40-doc battle corpus tested, 120/120 fuzz clean.

- **MCP daemon**: stdio server, JSON-RPC 2.0, MCP protocol 2024-11-05+.
  - 3 tools, ~1.8K tokens at `tools/list`.
  - Dense, LLM-optimized tool definitions with full response format documentation.

- **CI**: 3-platform matrix (Linux, macOS, Windows), clippy (`-Dwarnings`), fmt check.
- **License**: AGPL v3.

### Known limitations

- Interactive captchas (hCaptcha, reCAPTCHA, Turnstile checkbox) are not solved — no solving service by design.
- ML-DSA post-quantum signatures not yet supported (BoringSSL 5.1.0 lacks them).
- `outerWidth/Height` in headless: protocol-level override only.
- Windows/macOS PDF subsystem compiled but CI verification pending.
