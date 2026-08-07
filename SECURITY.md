# Security Policy

## Reporting a vulnerability

If you discover a security vulnerability in DonSeTch, please report it privately.

**Do not open a public GitHub issue.**

Email: bishesh.bhandari.contact@gmail.com

Include:
- A description of the vulnerability and its impact.
- Steps to reproduce, or a proof of concept.
- The DonSeTch version (`donsetch --version`).
- Your assessment of severity.

You will receive a response within 72 hours. If the vulnerability is confirmed, a fix will be released and you will be credited (unless you prefer otherwise).

## Scope

DonSeTch is a local MCP server that fetches web pages, searches the web, and crawls sites. Security-relevant areas:

- **TLS handling**: the custom BoringSSL stack, certificate validation, and connection pooling.
- **Browser escalation**: CDP communication, cookie handling, and the ghost process lifecycle.
- **PDF parsing**: the PDFium FFI layer and untrusted-file handling.
- **Search**: the egress pool, proxy handling, and rate-limit backoff.
- **MCP daemon**: JSON-RPC input parsing and stdio handling.

## Out of scope

- Bypassing anti-bot protections on specific sites (that's the tool's purpose, not a vulnerability).
- Rate-limiting or blocking by search engines (expected behavior, not a vulnerability).
- Captcha-solving (intentionally not implemented — not a vulnerability).

## Disclosure timeline

1. You report privately.
2. We confirm and triage within 72 hours.
3. A fix is developed and tested.
4. A patch release is published.
5. The vulnerability is disclosed publicly after the fix is available.

## Security posture

DonSeTch runs locally as a stdio process. It does not:
- Open a network port (stdio only).
- Store credentials (no API keys, no accounts).
- Phone home or telemetry.
- Execute arbitrary code from fetched pages.

DonSeTch does:
- Download model files (OCR, reranker) from HuggingFace on first use, cached locally.
- Download PDFium static libraries at build time.
- Launch a headless browser process on tier 2 escalation.
- Cache search results and cookies to `~/.cache/donsetch/`.
