//! `repair` must never panic, always emit valid JSON, be a fixpoint, and
//! restore the caller's buffer when it fails.

#![no_main]

use jsonfix::{Options, repair, repair_into, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    // repair_into appends on success and trims back to the previous length on
    // failure — the sentinel must survive either way.
    const SENTINEL: &str = "⟦sentinel⟧";
    let mut buf = String::from(SENTINEL);
    match repair_into(input, &mut buf, Options::all()) {
        Ok(()) => {
            let repaired = &buf[SENTINEL.len()..];
            let standalone = repair(input).expect("repair_into and repair agree");
            assert_eq!(
                repaired, standalone,
                "repair_into appended {repaired:?} but repair returned {standalone:?}"
            );
            assert_valid_and_fixpoint(input, repaired);
        }
        Err(into_error) => {
            assert_eq!(
                buf, SENTINEL,
                "repair_into must restore the buffer on error ({into_error})"
            );
            let standalone =
                repair(input).expect_err("repair_into failed, so repair must fail too");
            assert_eq!(
                into_error.kind(),
                standalone.kind(),
                "repair_into and repair disagree on {input:?}"
            );
        }
    }
});

/// The rendered text must parse as strict JSON, and repairing it again must
/// be a no-op (the property-test `repair_is_a_fixpoint`, under adversarial
/// bytes instead of a generator).
fn assert_valid_and_fixpoint(input: &str, repaired: &str) {
    validate(repaired).unwrap_or_else(|error| {
        panic!("repair({input:?}) produced {repaired:?} which fails validate: {error}")
    });
    let twice = repair(repaired)
        .unwrap_or_else(|error| panic!("repair of its own output {repaired:?} failed: {error}"));
    assert_eq!(
        repaired, twice,
        "repair not idempotent: {input:?} -> {repaired:?} -> {twice:?}"
    );
}
