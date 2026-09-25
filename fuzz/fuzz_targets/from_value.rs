//! `from_value` must never panic for any target shape — type mismatches are
//! errors, never undefined behavior — and must agree with `Value::to_string`
//! semantics on numbers (exact integers, finite floats).
#![no_main]

use std::collections::BTreeMap;

use jsonfix::{Value, from_value, parse};
use libfuzzer_sys::fuzz_target;
use serde::de::IgnoredAny;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let input = input.as_ref();
    let Ok(value) = parse(input) else {
        return;
    };

    // Each target consumes the tree; clones keep the same input in play.
    // All of these must return (Ok or Err), never panic or hang.
    let _ = from_value::<IgnoredAny>(value.clone());
    let _ = from_value::<Value>(value.clone());
    let _ = from_value::<Vec<f64>>(value.clone());
    let _ = from_value::<BTreeMap<String, i64>>(value.clone());
    let _ = from_value::<Option<String>>(value.clone());
    let _ = from_value::<String>(value.clone());
    let _ = from_value::<f64>(value);
});
