//! `parse`/`validate`/`parse_partial` must never panic, and every tree they
//! return must render back to strict-JSON text.

#![no_main]

use jsonfix::{Allow, Options, parse, parse_partial, validate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();

    // Strict parse: either an error with a byte position inside the input, or
    // a tree that renders to valid JSON.
    match validate(input) {
        Ok(value) => {
            let rendered = value.to_json_string();
            validate(&rendered).unwrap_or_else(|error| {
                panic!("validate({input:?}) rendered {rendered:?}: {error}")
            });
            // Rendering is deterministic: reparsing the render is a fixpoint.
            let again = parse(&rendered).expect("rendered text re-parses");
            assert_eq!(
                again.to_json_string(),
                rendered,
                "render/parse not stable for {input:?}"
            );
            // Accessors must not panic on any tree shape.
            walk(&value, input);
        }
        Err(error) => {
            assert!(
                error.position() <= input.len(),
                "error position {} beyond input length {} for {input:?}",
                error.position(),
                input.len()
            );
        }
    }

    // Partial policies: every kept value still renders as strict JSON.
    let policies = [
        Allow::NOTHING,
        Allow::STR,
        Allow::COLLECTION,
        Allow::STR | Allow::NUM | Allow::COLLECTION | Allow::KEY | Allow::ATOM,
        Allow::ALL,
    ];
    for allow in policies {
        if let Ok(value) = parse_partial(input, Options::partial(allow)) {
            let rendered = value.to_json_string();
            validate(&rendered).unwrap_or_else(|error| {
                panic!("parse_partial(allow={allow:?}) of {input:?} rendered {rendered:?}: {error}")
            });
        }
    }
});

/// Exercise every public accessor so a malformed tree panics here, not in
/// user code.
fn walk(value: &jsonfix::Value, input: &str) {
    let _ = value.len();
    let _ = value.is_empty();
    let _ = value.is_null();
    let _ = value.as_str();
    let _ = value.as_bool();
    let _ = value.as_i64();
    let _ = value.as_f64();
    let _ = value
        .as_number()
        .map(|n| (n.as_str().to_string(), n.as_i64(), n.as_f64()));
    let _ = value.as_array().map(|items| items.len());
    let _ = value.as_object().map(|members| {
        members.iter().for_each(|(key, _)| {
            let _ = value.get(key);
        })
    });
    let _ = value.index(0);
    let _ = value.pointer("");
    let _ = value.pointer("/0");
    let _ = value.pointer("/x");
    let _ = value.pointer("/x/0/y");
    let _ = value.to_json_string();
    let _ = format!("{value}"); // Display, if implemented — through to_string
    let _ = input; // referenced so refactors keep the connection obvious
}
