//! Arbitrary `Allow`/`Repairs` masks must never panic; outputs stay
//! strict-valid; repair stays a fixpoint; strict masks agree with `validate`.

#![no_main]

use jsonfix::{Allow, Options, Repairs, parse_partial, repair_with, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let allow = allow_from(data[0]);
    let repairs = repairs_from(u16::from_le_bytes([data[1], data[2]]));
    let opts = Options { allow, repairs };
    let input = String::from_utf8_lossy(&data[3..]);
    let input = input.as_ref();

    match repair_with(input, opts) {
        Ok(repaired) => {
            validate(&repaired).unwrap_or_else(|error| {
                panic!(
                    "repair_with(allow={allow:?}, repairs={repairs:?}) of {input:?} \
                     produced {repaired:?} which fails validate: {error}"
                )
            });
            // Whatever the mask, a second pass over complete JSON is a no-op:
            // every repair only fires on invalid shapes, and `allow` only
            // affects values cut off at EOF (there are none in `repaired`).
            match repair_with(&repaired, opts) {
                Ok(again) => assert_eq!(
                    again, repaired,
                    "repair_with not a fixpoint under {opts:?}: {input:?} -> {repaired:?} -> {again:?}"
                ),
                Err(error) => panic!(
                    "repair_with({repaired:?}) under {opts:?} failed after succeeding: {error}"
                ),
            }
        }
        Err(error) => assert!(
            error.position() <= input.len(),
            "repair_with error position {} beyond input {}",
            error.position(),
            input.len()
        ),
    }

    if let Ok(value) = parse_partial(input, opts) {
        let rendered = value.to_json_string();
        validate(&rendered).unwrap_or_else(|error| {
            panic!("parse_partial({opts:?}) of {input:?} rendered {rendered:?}: {error}")
        });
    }

    // A strict mask is exactly `validate`: same acceptance, same rendering.
    if allow == Allow::NOTHING && repairs == Repairs::NONE {
        let via_repair = repair_with(input, opts);
        let via_validate = validate(input);
        match (via_repair, via_validate) {
            (Ok(a), Ok(b)) => {
                let b = b.to_json_string();
                assert_eq!(a, b, "strict repair/render diverged for {input:?}");
            }
            (Err(a), Err(b)) => assert_eq!(
                a.kind(),
                b.kind(),
                "strict repair and validate disagree for {input:?}"
            ),
            (a, b) => panic!("strict repair {a:?} vs validate {b:?} for {input:?}"),
        }
    }
});

/// Map 8 bits onto the seven defined `Allow` flags.
fn allow_from(byte: u8) -> Allow {
    const FLAGS: [Allow; 7] = [
        Allow::STR,
        Allow::NUM,
        Allow::ARR,
        Allow::OBJ,
        Allow::KEY,
        Allow::BOOL,
        Allow::NULL,
    ];
    FLAGS
        .iter()
        .enumerate()
        .filter(|(i, _)| byte & (1 << i) != 0)
        .fold(Allow::NOTHING, |acc, (_, flag)| acc | *flag)
}

/// Map 16 bits onto the twelve defined `Repairs` flags.
fn repairs_from(bits: u16) -> Repairs {
    const FLAGS: [Repairs; 12] = [
        Repairs::FENCES,
        Repairs::COMMENTS,
        Repairs::UNQUOTED,
        Repairs::KEYWORDS,
        Repairs::NUMBERS,
        Repairs::CONCATENATION,
        Repairs::CALLS,
        Repairs::ENTITIES,
        Repairs::QUOTES,
        Repairs::WHITESPACE,
        Repairs::TRUNCATION,
        Repairs::NDJSON,
    ];
    FLAGS
        .iter()
        .enumerate()
        .filter(|(i, _)| bits & (1 << i) != 0)
        .fold(Repairs::NONE, |acc, (_, flag)| acc | *flag)
}
