<div align="center">

<img src="assets/herobanner.png" width="640" style="border-radius: 12px" alt="DonSeTch — The web, for AI agents">

**$0. No keys, no accounts. Built from scratch in Rust.**

Fetch · search · crawl · bypass bot walls · read PDFs (even scanned) · semantic reranking

One binary · Chrome's own TLS · headless browser escalation · zero Python

Works with **Claude Code**, **Cursor**, **OpenCode**, **Pi**, **Windsurf**, and any MCP client.

</div>

<br>

<div align="center">

[![npm](https://img.shields.io/npm/v/donsetch?color=cb3837&logo=npm)](https://www.npmjs.com/package/donsetch)
[![npm downloads](https://img.shields.io/npm/dm/donsetch?color=cb3837&logo=npm&label=downloads)](https://www.npmjs.com/package/donsetch)
[![GitHub stars](https://img.shields.io/github/stars/dondai44423/donsetch?style=flat&logo=github&color=e3b341)](https://github.com/dondai44423/donsetch/stargazers)
[![CI](https://img.shields.io/github/actions/workflow/status/dondai44423/donsetch/ci.yml?branch=master&logo=github&label=CI)](https://github.com/dondai44423/donsetch/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/Rust-edition%202024-ce422b?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![MCP](https://img.shields.io/badge/MCP-server-7c3aed?logo=modelcontextprotocol&logoColor=white)](https://modelcontextprotocol.io)
[![License](https://img.shields.io/badge/license-AGPL%203.0-2563eb)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-442%20passed-00d4aa)](#)

</div>

<br>

```bash
npm install -g donsetch
```

<div align="center">

[Install](#-install) · [Two ways to use it](#-two-ways-to-use-it) · [The 3 tools](#-the-3-tools) · [Chrome TLS](#-chrome-tls-not-chrome-like) · [Solve & Bounce](#-solve-and-bounce) · [Search](#-keyless-search) · [PDF](#-pdf--ocr) · [Comparison](#-comparison) · [Gotchas](#-gotchas) · [Limits](#-honest-limits)

</div>

---

DonSeTch gives any AI agent full web research from a single local process. Three tools, zero API keys, zero accounts. Built in Rust — one binary, no Python, no Playwright, no Selenium, no `reqwest`, no `hyper`. Every layer built from scratch.

Works with every MCP client: Claude Code, Cursor, OpenCode, Pi, anything that speaks MCP. Also works as a standalone CLI.

## ✨ What makes it different

| | What it does |
|---|---|
| 🛡️ **Real Chrome TLS** | Drives Chrome's own BoringSSL natively. Your ClientHello IS Chrome's ClientHello — fingerprint is emergent from the real engine, not a faked table that rots. |
| ⏱️ **Temporal stealth** | TLS session resumption, conditional revalidation (304), persistent cookies, connection pooling. The loudest remaining bot tell — and nobody else fakes it. |
| 👻 **Solve-and-bounce** | Browser solves the challenge, hands cookies to tier 1, goes to sleep. Tier 1 fetches at full speed. The browser almost never fetches content. |
| 🧠 **Self-improving fetch** | Learns from every fetch. Cookie lifetimes learned adaptively. Warm starts skip the browser entirely. Converges to optimal routing per domain. |
| 🔎 **Semantic reranking** | Local ONNX cross-encoder reads query + result through full attention. Pushes out generic articles that keyword-match but aren't about the topic. |
| 🔑 **Keyless search** | 10+ backends in parallel, fused by cross-engine consensus. No API keys, no accounts, no billing. $0 forever. BYOK optional. |
| 📄 **Pixel-fusion PDF** | Glyphs + rendered pixels from the same stream, fused deterministically. No hallucination. Per-region trust audit. Scanned PDFs auto-OCR'd. |
| 🧬 **Built from scratch** | Own HTTP/2 (HPACK, flow control), own extraction engine, own PDF parser, own search aggregator, own crawl engine. Zero dependency on existing OSS web tooling. |

---

## 📦 Install

### Option 1 — npm (recommended)

```bash
npm install -g donsetch
```

Downloads the prebuilt binary for your platform from GitHub Releases with SHA256 verification. No build tools needed.

| Platform | Binary |
|---|---|
| Linux x86_64 | `donsetch-linux-x64.tar.gz` |
| macOS arm64 | `donsetch-darwin-arm64.tar.gz` |
| Windows x86_64 | `donsetch-win32-x64.tar.gz` |

### Option 2 — Build from source

| Dependency | Why | Linux | macOS | Windows |
|---|---|---|---|---|
| **Rust** | Build toolchain | `rustup` | `rustup` | `rustup` |
| **Go** | BoringSSL build | `pacman -S go` | `brew install go` | `winget install GoLang.Go` |
| **NASM** | BoringSSL assembly | `pacman -S nasm` | `brew install nasm` | `choco install nasm` |
| **CMake** | BoringSSL build | `pacman -S cmake` | `brew install cmake` | `winget install cmake` |

```bash
git clone https://github.com/dondai44423/donsetch.git
cd donsetch
cargo build --release
```

Binary lands at `target/release/donsetch`. First build takes ~2 min (compiling BoringSSL). Subsequent builds are cached.

<details>
<summary><b>Build notes</b></summary>

- **BoringSSL** is vendored and built from source via `boring-sys`. First build compiles it (~2 min), then cached.
- **PDFium** is downloaded as a static library by `build.rs` — no manual setup.
- **ONNX Runtime** is downloaded at build time by `oar-ocr` (OCR) and `ort` (reranker).
- **Models** (OCR + reranker) download on first use to `~/.cache/donsetch/`, not bundled in the binary.
- **Feature flags**: `default = ["ocr", "rerank"]`. Build with `--no-default-features` for HTTP-only (no OCR, no reranker, smaller binary).
- **Chromium** (optional): needed for tier 2 browser escalation on bot-walled sites. Linux: `pacman -S chromium`. macOS: `brew install chromium`. Windows: Edge works.
- **Linux Xvfb**: for headful Chrome on Linux, `xorg-server-xvfb` is needed. DonSeTch starts Xvfb automatically.

</details>

---

## 🎯 Two ways to use it

DonSeTch is **one binary, two interfaces**. Same engine, same output, same reliability.

### MCP Server (for AI agents)

Point your AI agent at the binary. No arguments, no API keys, no environment variables.

```json
{
  "mcpServers": {
    "donsetch": { "command": "donsetch", "args": ["mcp"] }
  }
}
```

Or use `npx` without global install:

```json
{
  "mcpServers": {
    "donsetch": { "command": "npx", "args": ["donsetch", "mcp"] }
  }
}
```

Works with Claude Code, Cursor, OpenCode, Pi, Windsurf, and any client that speaks MCP. Three tools: `web_fetch`, `web_search`, `web_crawl`.

### CLI (for humans and scripts)

```bash
donsetch fetch https://example.com
donsetch search "rust async patterns"
donsetch crawl https://docs.example.com --topic "api reference"
```

Same engine, same output — the CLI is a thin adapter over the same `call_tool` function the MCP server uses. Every feature available to the agent is available on the command line.

```bash
donsetch fetch https://example.com --focus "pricing" --max-chars 2000
donsetch search "GLM 5.2" --intent paper --max-results 5 --json
donsetch crawl https://docs.python.org --mode map --topic "asyncio"
donsetch keys add tinyfish sk-tinyfish-...
donsetch doctor
donsetch update
```

<details>
<summary><b>All CLI commands</b></summary>

| Command | What it does |
|---|---|
| `fetch <url>` | Fetch a URL as clean markdown. Auto bot-wall bypass, PDF, JS render. |
| `search <query>` | Web search — 10+ keyless engines merged + reranked, or your API keys. |
| `crawl <url>` | Crawl a site into markdown. Sitemap-aware, focus-ranked, resumable. |
| `mcp` | Start MCP server (JSON-RPC on stdio). |
| `keys` | Manage BYOK search provider keys (`add`, `remove`, `list`, `default`, `reset`, `export`, `import`, `clear`). |
| `proxy` | Manage proxy configuration (`add`, `remove`, `list`, `check`, `clear`, `test`, `import`, `export`). |
| `status` | Quick status overview — version, keys, proxies, cache, health. |
| `doctor` | Health check with auto-fix. |
| `update` | Self-update from GitHub Releases. |
| `rollback` | Revert to previous version. |
| `version` | Show version and build info. |
| `tools` | Print tool schemas as JSON (same as MCP `tools/list`). |

</details>

---

## 🎯 The 3 tools

| Tool | One-liner |
|------|-----------|
| 🌐 **`web_fetch`** | Fetch any URL as clean markdown. HTTP first, escalates to headless browser if blocked. PDFs with OCR + per-page confidence, `focus` for token savings, `toc`/`section`, pagination, `actions` for in-page browser control (click/type/press/scroll/wait). |
| 🔎 **`web_search`** | Keyless multi-engine web search. 10+ backends in parallel, consensus + semantic reranking, query-aware official-source placement. Returns URLs + snippets, not content. |
| 🕷️ **`web_crawl`** | Best-first same-domain crawl. Sitemap + frontier, elastic pacing, resume tokens. `focus` for budget management. |

---

## 🖱️ Browser actions — page control inside fetch (v2)

`web_fetch` accepts an `actions` array executed in the real headless browser **before** extraction:

```json
{
  "url": "https://duckduckgo.com",
  "actions": [
    { "do": "type", "selector": "input[name=q]", "text": "rust async tokio" },
    { "do": "press", "key": "Enter" },
    { "do": "wait_text", "text": "tokio" }
  ],
  "focus": "tokio"
}
```

Steps: `wait`, `wait_selector`, `wait_text` (deterministic waits — no blind sleeps), `click` (by CSS selector or visible text), `hover`, `type` (human-cadence keystrokes), `press`, `scroll`. Up to 16 steps. After the script runs, the normal extraction pipeline works on the final DOM — `focus`, `section`, `toc` all apply to the interacted-with page. Per-step results come back in `structuredContent.actions`; the first failing step aborts honestly with everything that succeeded, so you fix one step and re-run. Form submits, search flows, load-more buttons, lazy-load scrolls — one call, no separate browser tool.

---

## 🛡️ Chrome TLS, not Chrome-like

Everyone in the impersonation game (curl-impersonate, rquest, wreq) patches a foreign TLS stack to *resemble* Chrome and ships hardcoded fingerprint tables that rot as browsers evolve.

DonSeTch does something different in kind:

> **We drive Chrome's own TLS library (BoringSSL) with its native Chrome behaviors switched on** — GREASE, extension permutation, ECH-GREASE, ALPS, SCT, OCSP, brotli cert-compression. The ClientHello is *generated by the same machinery* that generates Chrome's.

When Chrome's TLS posture shifts, we update a data table, not patch a C library. **The fingerprint isn't a table we fake. It's emergent from the real engine.**

<details>
<summary><b>Verified against live Chromium 150 at tls.peet.ws</b></summary>

| Signal | Match |
|---|---|
| **JA4** | cipher hash identical to Chrome 150 |
| **Akamai h2 fingerprint** | `1:65536;2:0;4:6291456;6:262144\|15663105\|0\|m,a,s,p` — exact match |
| **h2 header order** | sec-ch-ua → sec-ch-ua-mobile → sec-ch-ua-platform → ... — exact match |
| **Extension set** | identical (contents differ only in random GREASE/key material, like real Chrome) |

</details>

### Own HTTP/2 stack (because off-the-shelf leaks)

Off-the-shelf h2 (hyper's `h2` crate) doesn't expose pseudo-header order, exact SETTINGS set, WINDOW_UPDATE values, or HPACK indexing strategy — all fingerprintable (Akamai h2 fingerprint). So we wrote our own:

- Own HPACK (RFC 7541 — all 257 Huffman symbols + 61 static entries verified)
- Own frame engine (SETTINGS, HEADERS, DATA, WINDOW_UPDATE, PING, GOAWAY, RST_STREAM, CONTINUATION)
- Own flow control with WINDOW_UPDATE replenishment
- Own connection pool with TLS 1.3 session resumption

**No `reqwest`. No `hyper`. No `isahc`.** Every byte on the wire is ours.

### Temporal stealth (the tell nobody else fakes)

Everyone fakes the *handshake*. But a bot wall's second look is **temporal**: what does the client do *over time*?

| Mechanism | What it does | Why it matters |
|---|---|---|
| TLS session resumption | Per-origin session-ticket cache. Resumed handshakes are abbreviated. | Scrapers never resume. Chrome always does. |
| h2 connection pool | Connections kept alive, reused across fetches. | Fresh connection every time = bot signal. |
| Conditional revalidation | Sends `If-None-Match` / `If-Modified-Since` on refetch. `304` → serve cached body. | Scrapers never send conditional headers. Browsers always do. |
| Happy Eyeballs | Races IPv6 vs IPv4 with 250ms stagger. | Chrome does exactly this. Fixes dead-IP 10s timeouts. |
| Persistent cookie jar | Cookies survive across calls like a real browser profile. | A client with no cookie memory is a bot. |

**More stealth AND faster.** The rare quadrant.

---

## 👻 Solve-and-bounce

The entire industry does tier 2 wrong: **use a 600MB browser for everything.** Every fetch pays browser startup, browser RAM, browser CPU. And their stealth is *subtractive*: launch with automation flags, then inject JS patches to hide the damage. Every patch is a detectable lie.

DonSeTch inverts both.

> **The browser almost never fetches content.** It exists to do exactly two things HTTP can't: pass JS challenges and execute JS-rendered pages. Its output is *cookies* (handed to tier 1, which fetches at full speed) or *rendered HTML* (handed to the extraction engine).

| Step | What happens | Speed |
|---|---|---|
| 1. Tier 1 fetch | Fast stealth HTTP (BoringSSL TLS) | ~100-300ms |
| 2. Wall detected | Cloudflare / DataDome / PerimeterX / Akamai | — |
| 3. Ghost solves | Headless browser navigates, waits for challenge to clear, harvests clearance cookies | ~2-6s |
| 4. **Bounce** | Cookies handed to tier 1. Tier 1 re-fetches at full speed. Browser goes to sleep. | ~100-300ms |
| 5. Subsequent fetches | Tier 1 with warm cookies. Browser stays asleep. | ~100-300ms |

### Nothing is patched because nothing is broken

Raw CDP launch without `--enable-automation` means `navigator.webdriver` is *natively* false. No JS injection ever. No Runtime, Console, or Debugger domains. Environmental truthfulness instead of spoofing: real window, real GPU, real locale, consistent story.

<details>
<summary><b>Process lifecycle — the RAM-smart part</b></summary>

The ghost process is **SIGSTOP'd** (frozen, not killed) after 20s of idleness:

| State | RAM | CPU | Wake time |
|---|---|---|---|
| Active | full | real | — |
| Frozen (SIGSTOP) | mapped but cold | 0 | ~50ms (SIGCONT) |
| Reaped (>10 min frozen) | freed | 0 | ~1-2s (relaunch, profile keeps warmth) |

Crash-transparent: thaw finds a dead browser → silent relaunch. The agent never sees the lifecycle.

Persistent profile dir `~/.cache/donsetch/ghost-profile`: aged cookie/history state makes challenges EASIER (real users have history) and `cf_clearance` survives daemon restarts.

</details>

---

## 🧠 Self-improving fetch (experimental)

> **Experimental.** Works reliably for most domains, but edge cases exist (stale profiles, cookie race conditions). Disk persistence can be disabled with `DONSEEK_NO_DISK_STATE=1`.

> Every fetch is both an action AND an observation. The system learns from each outcome and routes the next fetch more efficiently. **The more you use DonSeTch, the less it escalates to tier 2.**

No ML, no prediction models. Pure deterministic observation + state → better routing. The loop converges, it doesn't guess.

| Visit | Route decision | What happens |
|---|---|---|
| Visit 1 (unknown) | `Cold` | Tier 1 → walled → ghost solves → cookies stored |
| Visit 2 (cookies fresh) | `Warm` | Tier 1 with injected cookies → success. Ghost stays asleep. |
| Visit N (cookies expired) | `SkipToSolve` | Skip the doomed tier-1 round-trip, go straight to ghost |
| Visit M (24h since cold check) | `RecheckCold` | Try tier 1 cold — the wall may have been removed |

When warm cookies go stale, the system learns the real lifetime: `observed_lifetime = min(previous, now - last_solved)`. Over multiple cycles, it converges to the real cookie lifetime for each domain.

**Only clearance cookies are persisted** (cf_clearance, datadome, _abck, etc.) — tracking cookies are filtered out to keep the state file compact.

**Disable disk persistence** with `DONSEEK_NO_DISK_STATE=1` — keeps in-memory state for the session but skips writing to `~/.cache/donsetch/ghost-state.json`. On by default.

---

<div align="center">

<img src="assets/owlsearch.png" style="border-radius: 12px" alt="DonSeTch Search">

</div>

## 🔎 Keyless search

No API key, no account, no third-party service. 10+ keyless backends in parallel on your machine, merged, deduped, ranked.

- **10+ independent backends**: Brave, Bing, DuckDuckGo, Mojeek, Yandex, Startpage + keyless verticals (GitHub, Wikipedia, HN, Semantic Scholar, arXiv, StackExchange, MDN, Google News).
- **Semantic reranking**: local ONNX cross-encoder (`ms-marco-MiniLM-L-6-v2`, 23MB, Apache-2.0) reads query + title + snippet through full transformer attention. 60/40 blend with RRF + BM25 + consensus. ~5ms/pair on CPU.
- **Cross-engine consensus**: a URL returned by several independent indexes gets a consensus boost. Every result carries `score`, `consensus` count, and `engines` list.
- **Entity coverage penalty**: anchor entities (hyphenated compounds like "B-tree") and specifiers (version numbers, years) checked against results. Wrong entity → 0.3× score penalty. Fixes BM25 splitting "B-tree" → "b" + "tree" where "binary tree" matches.
- **Honest reporting**: `weak=true` means low consensus. Per-engine status always visible. Never a fake "no results" that's actually a rate limit.

### BYOK (Bring Your Own Keys)

Want even better search? Add a paid search provider key and the local engine is bypassed entirely — the provider handles search, IP, and rate limits. Falls back to local if the provider is exhausted.

```bash
donsetch keys add tinyfish sk-tinyfish-...
donsetch keys add tavily tvly-...
donsetch keys add serper ...
donsetch keys add exa ...
```

Prefer the local engine but want keys as fallback? Set local as default:

```bash
donsetch keys default local       # local first, BYOK fallback
donsetch keys default exa          # BYOK first, local fallback
```

Back up, transfer, or wipe your keys:

```bash
donsetch keys export ~/donsetch-keys.json
donsetch keys import ~/donsetch-keys.json
donsetch keys clear
```

Providers: **TinyFish** (free tier), **Tavily**, **Serper.dev**, **Exa**. Key rotation, rate-limit cooldown, credit-depletion detection, and local fallback all automatic.

---

<div align="center">

<img src="assets/fetch.png" style="border-radius: 12px" alt="DonSeTch Fetch">

</div>

## 🌐 Fetch

`fetch` tries plain HTTP first (~100-300ms). If the site serves a bot wall or a JS shell, it auto-escalates to the ghost browser, solves the challenge, bounces cookies back to tier 1, and re-fetches at full speed.

- **DonSift extraction engine**: HTML bytes in, agent-native markdown out. Block model: typed blocks (Heading/Para/List/Table/Code/Quote/Media) with heading breadcrumbs.
- **`focus`** — BM25-relevant blocks only. Cuts context 80%+ on long pages. 12-language BM25: CJK character unigrams + bigrams, stopword lists, light stemming, accent folding.
- **`toc` + `section`** — heading outline first, then target one section. Two cheap calls instead of one expensive one.
- **Pagination** — `next_offset` in the response. Call again with `offset=that value`.
- **Token-war policies** — links stripped by default (~30% savings), link-farm lists dropped, bare-link lines dropped, wiki `[edit]` junk dropped, cross-block duplicate suppression.
- **Content classification**: `Article` / `Listing` / `Forum` / `Docs` / `Table` / `Page`.
- **Quality score** (0.0-1.0): content density, metadata, structure, language, text volume.
- **Agent-trust signals inline** — focus-miss notice, section-miss notice, JS-shell warning, empty-content note — all in the content, not metadata.

<details>
<summary><b>Anti-bot benchmark</b></summary>

| Site | Protection | Status |
|---|---|---|
| Cloudflare-protected sites | Cloudflare interstitial | ✅ 200 OK |
| DataDome sites | DataDome | ✅ 200 OK |
| Stack Overflow | Cloudflare | ✅ 200 OK |
| Medium | Cloudflare | ✅ 200 OK |
| Reddit | bot detection | ✅ 200 OK |
| Hacker News | None (baseline) | ✅ 200 OK |
| Interactive captcha sites | hCaptcha / reCAPTCHA / Turnstile | ⛔ Honest block |

</details>

---

<div align="center">

<img src="assets/crawl.png" style="border-radius: 12px" alt="DonSeTch Crawl">

</div>

## 🕷️ Crawl

`crawl` walks same-domain links in best-first order. Two phases: sitemap discovery (cheap URL inventory in one fetch), then Governor-paced frontier walk with extraction per page.

- **Three modes**: `full` (default) = sitemap map + content. `map` = URL inventory only (very cheap). `content` = skip sitemap, BFS from seed.
- **Focus-ranked frontier**: `focus="query"` ranks pages by BM25 relevance and crawls only matching ones.
- **Adaptive pacing**: the Governor paces per (host, lane). Success → steady. 429/503 → exponential. Error → cooldown. Dwell-time variance proportional to page size breaks metronome fingerprinting.
- **Resume tokens**: stopped crawls return a resume token. Call again with `resume=token` to continue. Valid 30 min, survives restarts.
- **Near-dup detection**: title + first 200 normalized chars → hash. Duplicates skipped.
- **Honest stop reasons**: `FrontierEmpty` (done), `MaxPages`/`CharBudget`/`DepthLimit`/`Deadline` (use resume), `ThrottledOut` (wait and resume).
- **Seed always in scope**: `--include`/`--exclude` apply to discovered links only, not the seed entry point.

---

## 📄 PDF + OCR

DonSeTch detects PDFs (by Content-Type or `%PDF` magic bytes) and parses them to structured markdown using a custom PDFium FFI. No external PDF library, no Python subprocess.

> **DonSeTch fuses both modalities, deterministically.** PDFium gives us both a document's glyph stream (exact text + positions) AND rendered pixels (exact visual ground truth). Both come from the same content stream, so they're already aligned.

**Pixels tell the truth about structure. Glyphs tell the truth about text. The fusion is exact because both render from one stream. No guessing, no hallucination.**

| Innovation | What it does |
|---|---|
| Pixel-fusion rule extraction | Tables/borders/separators detected on the rendered bitmap via morphological opening. A rule line is a fact, not a hypothesis. |
| Span detection by ink continuity | A cell spans a separator iff the separator has NO ink under the cell's row band. Deterministic colspan/rowspan. |
| Trust audit + per-region arbitration | Glyph stream authoritative UNLESS: zero glyphs + pixels (scan), or region ≥30% PUA/garbage. Corrupt regions get OCR'd from pixels even when neighbors read fine. |
| Orientation canonicalization | Vertical/rotated text = same document rotated. Rotate coordinate frames, run ONE pipeline. |
| Confidence honesty | Verbatim glyphs or OCR with per-line confidence. `[uncertain: ...]` markers below threshold. Neural extractors cannot offer this. |
| Forms as data | AcroForm widgets → name/type/value triples. |

| Tier | What | When |
|---|---|---|
| Tier A (always) | Glyph stream + pixel-fusion layout engine | Every page |
| Tier B (lazy) | OCR for pages/regions with no trustworthy text | Scans, broken ToUnicode |

<details>
<summary><b>PDF battle test results</b></summary>

40-document battle corpus, zero garbage output, 6-14x faster than Python alternatives. 120/120 fuzz clean.

| Document type | Result |
|---|---|
| Academic papers | ✅ Clean text — math symbols recovered, not CID garbage |
| Scanned documents | ✅ OCR'd — PP-OCR cascade (En → Zh → Deva), confidence-scored |
| Tax forms (W-9) | ✅ Forms as data — field names + values as table |
| Multi-column layouts | ✅ Reading order preserved — column detection + merge |
| Encrypted PDFs | ⛔ Honest flag — `encrypted: password required` |
| Corrupt PDFs | ⛔ Honest flag — `corrupt: parse failed at offset N` |
| Nepali UDHR (broken ToUnicode) | ✅ 10,542 usable Nepali chars at 86% confidence (pymupdf: 28) |

</details>

---

## 🏗️ Built from scratch

Every layer built in Rust. No dependency on existing OSS web tooling.

| Component | What it does | Key files |
|---|---|---|
| 🛡️ **DonShadow** | Tier 1 stealth HTTP — BoringSSL TLS, own HTTP/1.1 + HTTP/2, temporal stealth, cookie jar | `src/fetch/`, `src/transport/` |
| 👻 **DonGhost** | Tier 2 ghost browser — CDP (no Runtime/Console/Debugger), solve-and-bounce, SIGSTOP lifecycle | `src/ghost/` |
| 📝 **DonSift** | HTML-to-markdown — block model, 12-language BM25 focus, token-war policies | `src/extract/` |
| 🔎 **DonSeek** | Keyless multi-engine search — weighted RRF + BM25 + consensus + semantic reranking | `src/search/` |
| 🕷️ **DonTread** | Crawl engine — sitemap, focus-ranked frontier, Governor pacing, resume tokens | `src/crawl/` |
| 📄 **DonSheet** | PDF extraction — PDFium FFI, pixel-truth fusion, OCR arbitration cascade, forms | `src/pdf/` |
| 🔌 **MCP daemon** | stdio server — JSON-RPC 2.0, 3 tools | `src/mcp/` |

**442 tests. Zero clippy warnings.** `cargo clippy --release -- -Dwarnings` is the law.

---

## 📊 Comparison

| | **DonSeTch** | Hound | Crawl4AI | Jina Reader | Firecrawl |
|---|---|---|---|---|---|
| **Language** | Rust | Python | Python | Python (API) | TypeScript |
| **TLS fingerprint** | Real Chrome (BoringSSL) | curl-impersonate | requests | their servers | their servers |
| **HTTP/2 stack** | Own (HPACK, flow control) | primp | requests | their servers | their servers |
| **Temporal stealth** | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Tier 2 strategy** | Solve-and-bounce | Browser fetches all | n/a | n/a | n/a |
| **Self-improving** | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Web search** | ✅ (keyless, 10+) | ✅ (keyless, 10) | ❌ | ✅ | ❌ |
| **Semantic reranking** | ✅ (local ONNX) | ✅ | ❌ | ❌ | ❌ |
| **Deep crawl** | ✅ (resume tokens) | ✅ | ✅ | ❌ | ✅ (cloud) |
| **PDF → markdown** | ✅ (pixel-fusion) | ✅ (pdfplumber) | partial | ✅ | ✅ (cloud) |
| **Scanned-PDF OCR** | ✅ (PP-OCR) | ✅ (rapidocr) | ❌ | ❌ | ✅ (paid) |
| **Query focus** | ✅ (12-language BM25) | ✅ | ✅ | ❌ | ❌ |
| **CLI** | ✅ | ❌ | ✅ | ❌ | ❌ |
| **Runs locally** | ✅ | ✅ | ✅ | ❌ | self-host |
| **MCP server** | ✅ | ✅ | community | ✅ | build it |
| **Token cost** | ~1.8K (3 tools) | ~2.7K (6 tools) | varies | n/a | varies |
| **License** | AGPL v3 | MIT | Apache 2.0 | proprietary | MIT |

---

## ⚠️ Gotchas

| Surprise | Why |
|---|---|
| First build takes ~2 min | BoringSSL is compiled from source. Cached after that. |
| Go is a build dependency | BoringSSL's build system is Go-based. You need Go even though DonSeTch is Rust. |
| Interactive captchas not solved | hCaptcha, reCAPTCHA, Turnstile checkbox = honest dead end. No solving service by design. |
| robots.txt ON by default for crawl | `respect_robots=true` for crawl. `fetch` doesn't check robots. |
| Search rate-limits without a proxy | Keyless search scrapes public engines from your IP. Set `DONSEEK_PROXIES` for heavy use. |
| Not built for mass scraping | DonSeTch is for agentic research, not bulk extraction. |
| Disable disk persistence | `DONSEEK_NO_DISK_STATE=1` skips writing self-improvement data to disk. In-memory state still works for the session. |

---

## 🧱 Honest limits

| What it can NOT do | Why |
|---|---|
| Solve CAPTCHAs | Deliberate. You get a clear error, not a hang. |
| Sites requiring login | Out of scope (page rendering, not authenticated sessions). |
| ML-DSA post-quantum signatures | BoringSSL 5.1.0 lacks them. Will be added when BoringSSL gains it. |
| Search with all engines down | Returns an error with per-engine status. Honest, not fake. |

---

## 🤝 Contributing

PRs welcome. See [CONTRIBUTING.md](CONTRIBUTING.md). Run `cargo clippy --release -- -Dwarnings` and `cargo test --release` before submitting. AGPL v3 — all contributions under the same license.

## 📄 License

Copyright (c) 2026 Bishesh Bhandari. AGPL-3.0 — see [LICENSE](LICENSE).

---

<div align="center">

### If DonSeTch saves you time, ⭐ the repo

[![Stars](https://img.shields.io/github/stars/dondai44423/donsetch?color=ff9f43&style=flat-square)](https://github.com/dondai44423/donsetch)

**AGPL v3** · [Changelog](CHANGELOG.md) · [Issues](https://github.com/dondai44423/donsetch/issues) · [Releases](https://github.com/dondai44423/donsetch/releases)

</div>
