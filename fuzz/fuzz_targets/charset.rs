//! Fuzz charset decoding: arbitrary bytes + arbitrary
//! content-type charset labels → String. The v2.5 multi-byte
//! double-decode panic (#35) class lives here: GB18030, Shift-JIS,
//! EUC-KR and malformed/truncated sequences.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Split input: first quarter is the content-type charset
    // label, the rest is the body.
    let split = data.len() / 4;
    let (ct_bytes, body) = data.split_at(split);
    let ct = format!("text/html; charset={}", String::from_utf8_lossy(ct_bytes));
    let _ = donsetch::extract::charset::decode(body, &ct);
    // BOM sniffing + meta charset detection run inside decode;
    // also exercise the plain path with no label.
    let _ = donsetch::extract::charset::decode(body, "text/html");
});
