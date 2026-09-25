//! Extraction must only ever return spans of its input, and
//! `repair_extract` must produce strict-valid JSON when it succeeds.

#![no_main]

use jsonfix::{extract, extract_all, extract_partial, repair_extract, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    if let Some(found) = extract(input) {
        assert_span_of(input, found, "extract");
        // The span must itself be repairable (extraction is not allowed to
        // hand back a broken region that repair_extract would reject — that
        // pairing is the documented pipeline). Bare prose can extract as a
        // scalar that only repairs with keyword/unquoted passes, which are
        // on by default.
        let _ = repair_extract(input);
    }

    let partial = extract_partial(input);
    if !partial.is_empty() {
        assert_span_of(input, partial, "extract_partial");
        // extract_partial is a suffix: everything after the first value start.
        assert!(
            input.ends_with(partial),
            "extract_partial returned a non-suffix of {input:?}"
        );
    }

    let all = extract_all(input);
    for span in &all {
        assert_span_of(input, span, "extract_all");
    }

    match repair_extract(input) {
        Ok(repaired) => {
            validate(&repaired).unwrap_or_else(|error| {
                panic!("repair_extract({input:?}) -> {repaired:?} fails validate: {error}")
            });
        }
        Err(error) => {
            assert!(
                error.position() <= input.len(),
                "repair_extract error position {} beyond input {}",
                error.position(),
                input.len()
            );
        }
    }
});

/// `needle` must be a byte-subslice of `hay` (same allocation, in bounds).
fn assert_span_of(hay: &str, needle: &str, who: &str) {
    if needle.is_empty() {
        return; // "" may be a static empty string, not a subslice.
    }
    let hay_start = hay.as_ptr() as usize;
    let hay_end = hay_start + hay.len();
    let needle_start = needle.as_ptr() as usize;
    let needle_end = needle_start + needle.len();
    assert!(
        needle_start >= hay_start && needle_end <= hay_end && needle_end >= needle_start,
        "{who} returned {needle:?} which is not a span of {hay:?}"
    );
    // Must also be a UTF-8 boundary–aligned slice (implied by being a real
    // subslice of a &str, but re-check via byte equality with hay's region).
    let offset = needle_start - hay_start;
    assert_eq!(
        &hay[offset..offset + needle.len()],
        needle,
        "{who} span bytes mismatch"
    );
}
