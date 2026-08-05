# DonSheet corpus

Real-world PDFs the battery asserts on. NOT committed — fetch with
`scripts/download-corpus.sh` from the repo root. Tests skip silently
when files are missing.

| File | Source | Class under test |
|---|---|---|
| `attention.pdf` | arXiv 1706.03762 | 2-column academic, math, result tables, running footers |
| `swin.pdf` | arXiv 2103.14030 (Swin Transformer) | 2-column academic, figure-heavy, mono code |
| `w9.pdf` | IRS fw9.pdf | Report-generator Type3 fonts (GetFontSize lies), Wingdings checkboxes, justified letterspacing |
| `pdf-spec.pdf` | Adobe PDF 32000:2008 | 22 MB / 31+ pp enterprise spec, speed + memory stress |
| `progit.pdf` | progit2 releases | 501-page book: chapters, code listings, TOC dot leaders |
| `cjk.pdf` | generated (Chromium print-to-pdf) | Japanese + Chinese + embedded English, script-boundary spacing (see script for the exact HTML) |
| `vertical.pdf` | generated | `writing-mode: vertical-rl` honest-flag lane |
| `scanned.pdf` | generated | image-only pages → scanned flag |
