//! Fuzz pagination: hostile (offset, max_chars) pairs over
//! arbitrary text, including multi-byte CJK content. The v2.5
//! paginate overflow panic (unclamped max_chars/offset from tool
//! args) must never come back.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: (Vec<u8>, u8, u8)| {
    let text = String::from_utf8_lossy(&data.0).to_string();
    // Scale the u8 knobs up to sizes that overflow usize arithmetic
    // if anyone reintroduces unchecked math.
    let offset = (data.1 as usize) * 8_388_608; // up to ~2.1B
    let max_chars = (data.2 as usize).max(1) * 8_388_608;
    let (slice, next) = donsetch::extract::paginate_public(&text, offset, max_chars);
    // Invariant: the slice must be valid UTF-8 (guaranteed by type)
    // and next_offset must never point inside a char boundary.
    if let Some(n) = next {
        assert!(n <= text.len());
        assert!(text.is_char_boundary(n), "next_offset split a char");
    }
    assert!(slice.len() <= max_chars.max(text.len()));
});
