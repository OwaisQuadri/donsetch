# Contributing to DonSeTch

Thanks for your interest in contributing. DonSeTch is AGPL v3 — all contributions must be under the same license.

## Build from source

```bash
git clone https://github.com/dondai44423/donsetch.git
cd donsetch
cargo build --release                     # core build (fetch, search, crawl, PDF)
cargo build --release --features ocr,rerank  # full build (adds OCR + semantic reranking)
```

**Prerequisites**: Rust 1.75+, Go 1.22+, NASM, LLVM/Clang, CMake. See the [README](README.md#install) for platform-specific install commands.

## Development workflow

```bash
cargo test --features ocr,rerank          # 485 tests
cargo clippy --features ocr,rerank -- -Dwarnings   # zero warnings enforced
cargo fmt --all -- --check    # formatting check
```

All three must pass before a PR can merge. CI runs the same checks on Linux, macOS, and Windows.

## Commit conventions

Conventional Commits:

```
feat: add proxy rotation
fix: handle 429 retry-after
docs: update README
chore: bump deps
feat!: breaking change (the ! marks breaking)
```

Scope optional: `feat(search): add scholar vertical`.

## What we're looking for

- **Bug fixes** with a test that would have caught the bug.
- **New search engines** that are keyless and don't require API keys.
- **Anti-bot improvements** — new wall vendors, better detection heuristics.
- **PDF extraction** edge cases — corrupt files, weird encodings, exotic tables.
- **Performance** — measured improvements, not micro-optimizations.

## What we're not looking for

- Features that require API keys or paid services.
- Adding Python or Node.js dependencies. DonSeTch is Rust, built from scratch. That's the point.
- Solving interactive captchas (hCaptcha, reCAPTCHA). That's a hard line, not a TODO.
- Mass-scraping features. DonSeTch is for agentic research, not bulk extraction.

## Architecture overview

DonSeTch is built from scratch — no dependency on existing OSS web tooling:

| Component | What it does | Key files |
|---|---|---|
| DonShadow | Tier 1 stealth HTTP fetch (BoringSSL TLS, own HTTP/1.1 + HTTP/2) | `src/fetch/`, `src/transport/` |
| DonGhost | Tier 2 headless browser (CDP, no Runtime/Console/Debugger) | `src/ghost/` |
| DonSift | HTML-to-markdown extraction engine (block model, BM25 focus) | `src/extract/` |
| DonSeek | Keyless multi-engine search (RRF + BM25 + semantic reranking) | `src/search/` |
| DonTread | Crawl engine (sitemap, frontier, Governor pacing) | `src/crawl/` |
| DonSheet | PDF extraction (PDFium FFI, OCR arbitration, fusion) | `src/pdf/` |
| MCP daemon | stdio server (JSON-RPC 2.0, MCP 2024-11-05+) | `src/mcp/` |

## Pull requests

1. Fork the repo, create a branch (`feat/...`, `fix/...`, `docs/...`).
2. Write tests for your change.
3. Ensure `cargo test --features ocr,rerank`, `cargo clippy --features ocr,rerank -- -Dwarnings`, and `cargo fmt --check` all pass.
4. Open a PR with a conventional commit title.
5. CI must be green on all 3 platforms before merge.

## Reporting issues

- **Bugs**: include the URL you tried to fetch/search/crawl, the DonSeTch version (`donsetch --version`), and the structuredContent from the response.
- **Anti-bot failures**: include the site URL and the `verdict` field from the response.
- **Search issues**: include the query, the `engines` report from structuredContent, and whether `weak=true`.

## License

By contributing, you agree that your contributions will be licensed under the AGPL v3.
