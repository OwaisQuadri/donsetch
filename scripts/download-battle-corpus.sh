#!/usr/bin/env bash
# download-battle-corpus.sh — assemble the wide real-world PDF battle corpus.
# Run from repo root. Files land in bench/battle-corpus/ (gitignored).
set -uo pipefail

DEST=bench/battle-corpus
mkdir -p "$DEST"

dl() { # url file
  local url="$1" file="$2"
  [ -s "$DEST/$file" ] && { echo "skip $file"; return 0; }
  echo "GET $file"
  curl -sL --max-time 180 -o "$DEST/$file" "$url" \
    || { echo "FAIL $file"; rm -f "$DEST/$file"; }
  [ -s "$DEST/$file" ] || return 1
  head -c 5 "$DEST/$file" | grep -q "%PDF-" || {
    echo "NOTPDF $file"; rm -f "$DEST/$file"; }
}

# --- specs / RFCs (dense text, ascii-art figures, lots of prose) ---
dl https://www.rfc-editor.org/rfc/rfc7540.pdf rfc7540.pdf
dl https://www.rfc-editor.org/rfc/rfc9110.pdf rfc9110.pdf
dl https://www.rfc-editor.org/rfc/rfc9000.pdf rfc9000.pdf
dl https://www.rfc-editor.org/rfc/rfc8949.pdf rfc8949.pdf
dl https://www.rfc-editor.org/rfc/rfc7468.pdf rfc7468.pdf

# --- academic (2-col, math, dense tables, figures) ---
dl https://arxiv.org/pdf/1910.03771 t5.pdf
dl https://arxiv.org/pdf/2001.08361 gpt3.pdf
dl https://arxiv.org/pdf/1512.03385 resnet.pdf
dl https://arxiv.org/pdf/1810.04805 bert.pdf
dl https://arxiv.org/pdf/2203.02155 palm.pdf
dl https://arxiv.org/pdf/2106.09685 mlpmixer.pdf
dl https://arxiv.org/pdf/2305.16046 vetlm.pdf

# --- forms (AcroForm widgets: checkboxes, dropdowns, text fields) ---
dl https://www.irs.gov/pub/irs-pdf/fw4.pdf fw4.pdf
dl https://www.irs.gov/pub/irs-pdf/f1040.pdf f1040.pdf
dl https://www.uscis.gov/sites/default/files/document/forms/i-9.pdf i9.pdf
dl https://www.uscis.gov/sites/default/files/document/forms/i-130.pdf i130.pdf

# --- books (huge: chapters, code, TOC) ---
dl https://sourceforge.net/projects/linuxcommand/files/TLCL/19.01/TLCL-19.01.pdf/download tlcl.pdf
dl https://www.gutenberg.org/files/174/174-pdf.pdf dorian.pdf

# --- datasheets / appnotes (dense ruled tables, diagrams) ---
dl https://www.ti.com/lit/ds/symlink/ne555.pdf ne555.pdf
dl https://www.analog.com/media/en/technical-documentation/data-sheets/adxl345.pdf adxl345.pdf

# --- multilingual UDHR (CJK / RTL classes) ---
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/jpn.pdf udhr-japanese.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/arb.pdf udhr-arabic.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/zho.pdf udhr-chinese.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/hbr.pdf udhr-hebrew.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/kor.pdf udhr-korean.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/dev.pdf udhr-hindi.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/thi.pdf udhr-thai.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/rus.pdf udhr-russian.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/grk.pdf udhr-greek.pdf
dl https://www.ohchr.org/sites/default/files/UDHR/Documents/UDHR_Translations/nep.pdf udhr-nepali.pdf

echo "done. files:"; ls -la "$DEST" | tail -n +2 | wc -l
