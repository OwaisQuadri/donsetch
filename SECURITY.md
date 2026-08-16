# Security Policy

## Reporting a vulnerability

If you discover a security vulnerability in DonSeTch, **use GitHub's private vulnerability reporting** — do not open a public issue.

**[Report a vulnerability →](https://github.com/dondai44423/donsetch/security/advisories/new)**

This creates a private security advisory that only the maintainer can see. You can include details, reproduction steps, and severity assessment directly in the form. GitHub notifies the maintainer immediately.

Include in your report:

- A description of the vulnerability and its impact.
- Steps to reproduce, or a proof of concept.
- The DonSeTch version (`donsetch --version`).
- Your assessment of severity (low / medium / high / critical).

## Response timeline

| Stage | Target |
|---|---|
| Acknowledgment | Within 72 hours of report |
| Triage and severity assessment | Within 7 days |
| Fix development | Within 30 days (severity-dependent) |
| Patch release | Coordinated with reporter |
| Public disclosure | After patch is available |

If the vulnerability is confirmed, a fix will be released and you will be credited in the advisory (unless you prefer to remain anonymous).

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
