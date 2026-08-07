# DonSeTch

> Web fetch, search and crawl for AI agents. Zero API keys.

[![CI](https://github.com/dondai44423/donsetch/actions/workflows/ci.yml/badge.svg)](https://github.com/dondai44423/donsetch/actions/workflows/ci.yml)

DonSeTch is an MCP server that gives AI agents web research capabilities:
fetch any URL, search across 10+ engines, and crawl multi-page docs —
all with Chrome-true TLS fingerprinting and zero API keys.

## Install

```bash
npm install -g donsetch
```

The postinstall script downloads the prebuilt binary for your platform
from [GitHub Releases](https://github.com/dondai44423/donsetch/releases).

| Platform | Binary |
|---|---|
| Linux x86_64 | `donsetch-linux-x64.tar.gz` |
| macOS arm64 | `donsetch-darwin-arm64.tar.gz` |
| Windows x86_64 | `donsetch-win32-x64.tar.gz` |

## Usage

```bash
# Fetch a URL (Chrome-true TLS, anti-bot bypass)
donsetch fetch https://example.com

# Search (10+ keyless backends, semantic reranking)
donsetch search "rust async patterns"

# Crawl (sitemap-aware, focus-filtered)
donsetch crawl https://docs.example.com --focus "api reference"

# Start MCP server (for Claude Code, Cursor, Pi, etc.)
donsetch mcp
```

## MCP Configuration

Add to your MCP client config:

```json
{
  "mcpServers": {
    "donsetch": {
      "command": "donsetch",
      "args": ["mcp"]
    }
  }
}
```

Or use `npx` without global install:

```json
{
  "mcpServers": {
    "donsetch": {
      "command": "npx",
      "args": ["donsetch", "mcp"]
    }
  }
}
```

Three tools: `web_fetch`, `web_search`, `web_crawl`.

## Build from source

Requirements: Rust 1.75+, Go 1.22+, NASM, LLVM/Clang (for BoringSSL bindgen).

```bash
git clone https://github.com/dondai44423/donsetch
cd donsetch
cargo build --release
```

## License

AGPL-3.0 — Copyright (c) 2026 Bishesh Bhandari
