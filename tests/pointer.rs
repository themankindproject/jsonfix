//! RFC 6901 JSON pointer conformance and related span/repair edge cases.

use jsonfix::{parse, repair};

#[test]
fn array_index_accepts_zero_and_plain_digits() {
    let value = parse("[10, 20, 30]").expect("parses");
    assert_eq!(value.pointer("/0").and_then(|v| v.as_i64()), Some(10));
    assert_eq!(value.pointer("/1").and_then(|v| v.as_i64()), Some(20));
    assert_eq!(value.pointer("/2").and_then(|v| v.as_i64()), Some(30));
}

#[test]
fn array_index_rejects_leading_zero_per_rfc6901() {
    // RFC 6901 section 4: array index tokens other than "0" must not have a
    // leading zero. `/01` is not a valid reference and resolves to nothing.
    let value = parse("[10, 20, 30]").expect("parses");
    assert_eq!(value.pointer("/01"), None);
    assert_eq!(value.pointer("/00"), None);
    assert_eq!(value.pointer("/007"), None);
}

#[test]
fn array_index_rejects_sign_and_non_digits() {
    let value = parse("[10, 20, 30]").expect("parses");
    assert_eq!(value.pointer("/-1"), None);
    assert_eq!(value.pointer("/+1"), None);
    assert_eq!(value.pointer("/1e0"), None);
    assert_eq!(value.pointer("/ 1"), None);
}

#[test]
fn empty_pointer_resolves_to_root() {
    let value = parse("{\"a\": 1}").expect("parses");
    assert_eq!(value.pointer(""), Some(&value));
}

#[test]
fn object_key_with_escaped_tokens() {
    // ~1 -> "/", ~0 -> "~"
    let value = parse("{\"a/b\": 1, \"m~n\": 2}").expect("parses");
    assert_eq!(value.pointer("/a~1b").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(value.pointer("/m~0n").and_then(|v| v.as_i64()), Some(2));
}

#[test]
fn bare_slash_repairs_to_a_string_not_data() {
    // A lone `/...` is prose-like: it becomes a JSON string, never a bare
    // token or a regex value. Locks in the documented "bare word -> string"
    // behavior for slash-led input.
    assert_eq!(repair("/abc").unwrap(), "\"/abc\"");
}

#[test]
fn non_empty_pointer_without_leading_slash_resolves_to_nothing() {
    // RFC 6901: a non-empty pointer starts with `/`. A missing slash is a
    // likely typo and must not silently return the whole document.
    let value = parse("{\"users\": [1, 2]}").expect("parses");
    assert_eq!(value.pointer("users"), None);
    assert_eq!(value.pointer("users/0"), None);
    assert!(value.pointer("/users").is_some());
}

#[test]
fn pointer_mut_follows_the_same_rules() {
    let mut value = parse("{\"a/b\": [1, 2], \"k\": 0}").expect("parses");
    *value.pointer_mut("/a~1b/1").expect("resolves") = jsonfix::Value::from(9);
    assert_eq!(value.to_json_string(), "{\"a/b\": [1, 9], \"k\": 0}");
    assert!(value.pointer_mut("/a~1b/01").is_none());
    assert!(value.pointer_mut("k").is_none());
    assert!(value.pointer_mut("").is_some());
}
