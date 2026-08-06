#!/usr/bin/env bash
# generate-platform-corpus.sh — deterministic platform-class PDFs via headless Chromium.
# These are REAL Chromium print jobs: font embedding, shaping, and encoding
# behavior match what platforms actually ship.
set -uo pipefail
DEST=bench/battle-corpus
mkdir -p "$DEST" /tmp/donsetch-gen

gen() { # name html
  local out="$DEST/$1"
  [ -s "$out" ] && { echo "skip $1"; return 0; }
  echo "GEN $1"
  cat > "/tmp/donsetch-gen/$1.html"
  chromium --headless --disable-gpu --no-sandbox \
    --print-to-pdf="$out" --no-pdf-header-footer \
    "file:///tmp/donsetch-gen/$1.html" >/dev/null 2>&1
  [ -s "$out" ] && head -c 5 "$out" | grep -q "%PDF-" || { echo "FAIL $1"; rm -f "$out"; }
}

gen menu.pdf <<'HTML'
<html><body style="font-family:Georgia,serif;width:600px;margin:auto">
<h1 style="text-align:center">Café Musica</h1>
<table style="width:100%">
<tr><td>Espresso</td><td></td><td align="right">$3.50</td></tr>
<tr><td>Latte</td><td></td><td align="right">$4.75</td></tr>
<tr><td>Cold Brew</td><td></td><td align="right">$5.00</td></tr>
<tr><td>Sourdough Toast</td><td style="color:#888">(avocado, dukkah)</td><td align="right">$8.50</td></tr>
<tr><td>Basque Cheesecake</td><td></td><td align="right">$6.25</td></tr>
</table>
<p>Open daily 7:00 – 17:00 · 12 Bakhundole Marga, Lalitpur · VAT included</p>
</body></html>
HTML

gen invoice.pdf <<'HTML'
<html><body style="font-family:Helvetica,Arial,sans-serif;width:640px;margin:auto">
<h2>INVOICE #2026-0847</h2><p>Date: 2026-08-05 · Due: 2026-09-04<br>Billed to: Mero Kirana Pasal, Pokhara</p>
<table border="1" cellspacing="0" cellpadding="6" style="width:100%;border-collapse:collapse">
<tr><th>Item</th><th>Qty</th><th>Unit (NPR)</th><th>Amount (NPR)</th></tr>
<tr><td>Hosting, annual</td><td>1</td><td>14,000</td><td>14,000</td></tr>
<tr><td>SSL wildcard</td><td>1</td><td>6,500</td><td>6,500</td></tr>
<tr><td>Support hrs</td><td>7.5</td><td>2,200</td><td>16,500</td></tr>
<tr><td colspan="3" align="right"><b>Total</b></td><td><b>37,000</b></td></tr>
</table><p>Bank: Nabil Bank · Acct 0123456789012 · Ref INV-2026-0847</p>
</body></html>
HTML

gen resume.pdf <<'HTML'
<html><body style="font-family:Calibri,Arial,sans-serif;width:700px;margin:auto">
<h1>Anish Maharjan</h1><p>Backend Engineer · Kathmandu · anish@example.com</p>
<h3>Experience</h3>
<p><b>Senior Backend Engineer</b>, Leapfrog Technology (2024–now)<br>- Led payments platform serving 2.1M users<br>- Cut p99 latency 340ms → 120ms</p>
<h3>Skills</h3>
<p>Rust, Go, PostgreSQL, Kafka, AWS, k8s, Terraform</p>
<h3>Education</h3><p>B.E. Computer, Pulchowk Campus, 2019</p>
</body></html>
HTML

gen trifold.pdf <<'HTML'
<html><body style="font-family:Georgia,serif">
<style>.row{display:flex;gap:18px}.col{flex:1;border-left:1px solid #ccc;padding:0 10px}</style>
<div class="row">
<div class="col"><h3>Panel 1</h3><p>A three-panel brochure with independent columns. The rain in Spain stays mainly in the plain.</p></div>
<div class="col"><h3>Panel 2</h3><p>Second column content must stay in second-column order and never interleave with the third.</p></div>
<div class="col"><h3>Panel 3</h3><p>Third column closing thoughts. Contact us at hello@example.com or +977-1-5551234.</p></div>
</div></body></html>
HTML

gen slides.pdf <<'HTML'
<html><head><style>
@page { size: 960px 540px; margin: 0; }
.slide { width: 960px; height: 540px; page-break-after: always; padding: 60px; font-family: Verdana,sans-serif; }
</style></head><body>
<div class="slide" style="background:#123;color:#fff"><h1>Quarterly Review</h1><p>Engine · Q3 2026</p></div>
<div class="slide"><h2>Numbers</h2><ul><li>Uptime 99.97%</li><li>1.2M req/day</li><li>Cost −14%</li></ul></div>
</body></html>
HTML

gen cjk-japanese.pdf <<'HTML'
<html><body style="font-family:'Noto Sans CJK JP',sans-serif;width:640px;margin:auto">
<h2>電子帳簿保存法の概要</h2>
<p>電子帳簿保存法は、国税関係帳簿書類を電磁的記録により保存することを認める法律です。2024年1月の改正で、宥恕措置が明確化されました。請求書の発行側と受領側の双方に適用されます。</p>
<p>English mixed line: invoice 請求書 total ¥128,000 合計 date 2026年8月5日.</p>
<p>対象は国税関係帳簿（仕訳帳、総勘定元帳など）および国税関係書類（決算関係書類など）であり、スキャナ保存の要件は解像度200dpi相当以上です。</p>
</body></html>
HTML

gen cjk-chinese.pdf <<'HTML'
<html><body style="font-family:'Noto Sans CJK SC',sans-serif;width:640px;margin:auto">
<h2>电子发票管理办法</h2>
<p>根据国家税务总局公告，全面数字化电子发票自2024年12月1日起在全国推广应用。纳税人可以通过税务机关指定的电子发票服务平台开具和交付发票。</p>
<p>Mixed: 发票金额 RMB 1,234.00, tax 税款 13%, buyer 购买方, seller 销售方.</p>
<p>数电票与纸质发票具有同等法律效力。开票信息传输时必须确保真实、完整、不可篡改。</p>
</body></html>
HTML

gen arabic.pdf <<'HTML'
<html><body dir="rtl" style="font-family:'Noto Sans Arabic',sans-serif;width:640px;margin:auto">
<h2>طلب التحاق بالجامعة</h2>
<p>تعلن جامعة الملك عبدالعزيز عن فتح باب القبول للعام الجامعي 1448هـ للبرامج الجامعية والدراسات العليا، وذلك عبر بوابة القبول الإلكترونية.</p>
<p>الشروط: أن يكون المتقدم حاصلاً على الثانوية العامة بمعدل لا يقل عن 80%، وألا يكون قد مضى على تخرجه أكثر من خمس سنوات.</p>
<p>Mixed line: deadline موعد 2026-09-15 email قبول@example.edu.sa</p>
</body></html>
HTML

gen hebrew.pdf <<'HTML'
<html><body dir="rtl" style="font-family:'Noto Sans Hebrew',sans-serif;width:640px;margin:auto">
<h2>מבחן מערכת DonSheet</h2>
<p>מסמך זה בודק חילוץ טקסט בשפה העברית מתוך קובץ PDF שנוצר בדפדפן. הסדר הלוגי של המילים חייב להישמר גם בכיוון ימין־לשמאל.</p>
<p>מספרים 12345 ומועדים 2026-08-05 צריכים להישאר במקומם הנכון.</p>
</body></html>
HTML

gen korean.pdf <<'HTML'
<html><body style="font-family:'Noto Sans CJK KR',sans-serif;width:640px;margin:auto">
<h2>전자세금계산서 안내</h2>
<p>국세청은 전자세금계산서 발급 의무 대상을 법인사업자에서 일부 개인사업자로 확대하였습니다. 발급 기한은 공급일이 속하는 달의 다음 달 10일까지입니다.</p>
<p>Mixed: 금액 ₩1,234,000 부가세 10% 작성일자 2026-08-05 승인번호 20260805-1234.</p>
</body></html>
HTML

gen devanagari.pdf <<'HTML'
<html><body style="font-family:'Noto Sans Devanagari',sans-serif;width:640px;margin:auto">
<h2>नागरिक परीक्षा निर्देशिका</h2>
<p>लोक सेवा आयोगले खुला तथा आन्तरिक प्रतियोगितात्मक परीक्षाका लागि विज्ञापन प्रकाशित गरेको छ। आवेदन दिने अन्तिम मिति २०८३ साल बैशाख ३० गते भित्र हुनुपर्नेछ।</p>
<p>परिक्षार्थीले नागरिकता, शैक्षिक योग्यताको प्रमाणपत्र र पासपोर्ट साइजको फोटो अनिवार्य रूपमा संलग्न गर्नुपर्नेछ।</p>
</body></html>
HTML

echo "done"
