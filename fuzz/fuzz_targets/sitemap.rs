//! Fuzz sitemap parsing: arbitrary bytes (raw XML or gzip) through
//! maybe_gunzip (the decompression-bomb cap) and parse_sitemap.
//! The 64 MiB cap and entry caps must hold for any input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let gunzipped = donsetch::crawl::sitemap::maybe_gunzip(data);
    // Invariant: gunzip output is bounded no matter the input.
    assert!(
        gunzipped.len() <= 64 * 1024 * 1024 + 4096,
        "gunzip cap violated: {} bytes",
        gunzipped.len()
    );
    let xml = String::from_utf8_lossy(&gunzipped);
    let mut out = Vec::new();
    donsetch::crawl::sitemap::parse_sitemap(&xml, &mut out, 50_000);
});
