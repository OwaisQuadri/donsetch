#!/usr/bin/env bash
# DonSheet corpus acquisition. Real-world PDFs the battery asserts on.
set -euo pipefail
cd "$(dirname "$0")/../tests/pdf-corpus"
UA="Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0 Safari/537.36"
dl() { # name url
  if [ ! -f "$1" ]; then curl -fSL -A "$UA" -o "$1" "$2"; fi
  sha256sum "corpus.md" >/dev/null 2>&1 || true
}
dl attention.pdf "https://arxiv.org/pdf/1706.03762"
dl swin.pdf "https://arxiv.org/pdf/2103.14030"
dl w9.pdf "https://www.irs.gov/pub/irs-pdf/fw9.pdf"
dl pdf-spec.pdf "https://opensource.adobe.com/dc-acrobat-sdk-docs/pdfstandards/PDF32000_2008.pdf"
dl progit.pdf "https://github.com/progit/progit2/releases/latest/download/progit.pdf"
# CJK / scanned / vertical are generated deterministically with Chromium:
if ! command -v chromium >/dev/null; then echo "chromium needed for CJK/scanned/vertical generator" >&2; exit 1; fi
GEN=$(mktemp -d)
if [ ! -f cjk.pdf ]; then
cat > "$GEN/cjk.html" <<'HTML'
<!doctype html><html><head><meta charset="utf-8"><style>
body{font-family:'Noto Sans CJK JP','Noto Sans CJK SC',sans-serif;font-size:12pt;margin:1in}
h1{font-size:20pt} h2{font-size:15pt}
</style></head><body>
<h1>日本語のテスト文書</h1>
<p>これはPDF抽出エンジンのテストです。東京は日本の首都であり、人口は約1400万人です。
この文書には英語と日本語が混在しています。The quick brown fox jumps over the lazy dog.</p>
<h2>経済の概要</h2>
<p>日本のGDPは世界第4位です。2024年の名目GDPは約4兆ドルでした。
製造業とサービス業が経済の中心です。</p>
<h2>中文测试</h2>
<p>这是一个中文段落。北京是中国的首都，人口约2180万。
PDF提取必须正确处理CJK字符的排版。混合English text here too。</p>
</body></html>
HTML
chromium --headless --print-to-pdf="$PWD/cjk.pdf" --no-pdf-header-footer "file://$GEN/cjk.html"
fi
if [ ! -f vertical.pdf ]; then
cat > "$GEN/vertical.html" <<'HTML'
<!doctype html><html><head><meta charset="utf-8"><style>
body{writing-mode:vertical-rl;font-family:'Noto Sans CJK JP',sans-serif;font-size:14pt;margin:1in;height:8in}
</style></head><body><p>これは縦書きのテストです。日本語の縦書き文書を正しく抽出できるかを確認します。</p><p>第二段落。横混じり123です。</p></body></html>
HTML
chromium --headless --print-to-pdf="$PWD/vertical.pdf" --no-pdf-header-footer "file://$GEN/vertical.html"
fi
if [ ! -f scanned.pdf ]; then
cat > "$GEN/scanned.html" <<'HTML'
<!doctype html><html><head><style>body{margin:0}img{width:100%;height:1100px;background:#eee}</style></head>
<body><div><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="></div></body></html>
HTML
chromium --headless --print-to-pdf="$PWD/scanned.pdf" --no-pdf-header-footer "file://$GEN/scanned.html"
fi
rm -rf "$GEN"
echo "corpus ready:"; ls -la
