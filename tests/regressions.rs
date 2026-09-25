//! Regression tests for the correctness audit fixes.

// --- 1. `+` not followed by a string -------------------------------------

#[test]
fn plus_before_a_non_string_is_left_to_the_callers_noise_handling() {
    // Object: the `+` is dropped as noise and the following key is still
    // scanned in key position (value-context peeking would swallow `b:`).
    assert_eq!(
        jsonfix::repair(r#"{"a": "x" + b: 1}"#).unwrap(),
        "{\"a\": \"x\", \"b\": 1}"
    );
    // Array: same drop; the next element still parses.
    assert_eq!(jsonfix::repair(r#"["a" + 5]"#).unwrap(), r#"["a", 5]"#);
}

#[test]
fn top_level_plus_still_rejects_a_second_value() {
    let error = jsonfix::repair("\"a\" + 5").expect_err("trailing value");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::TrailingValue);
}

#[test]
fn concatenation_off_errors_cleanly_on_a_real_concat() {
    let opts = jsonfix::Options::all()
        .with_repairs(jsonfix::Repairs::ALL.without(jsonfix::Repairs::CONCATENATION));
    let error = jsonfix::repair_with(r#"["a" + "b"]"#, opts).expect_err("concat off");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('+'));
}

// --- 2. truncated word bits ----------------------------------------------

#[test]
fn truncated_word_respects_truncation_and_allow() {
    // Truncation repair off: a cut-off word errors like a cut-off string.
    let opts = jsonfix::Options::all()
        .with_repairs(jsonfix::Repairs::ALL.without(jsonfix::Repairs::TRUNCATION));
    let error = jsonfix::parse_with("tru", opts).expect_err("truncation off");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedEnd);

    // Truncation on but strings not allowed: the partial word is dropped.
    let error = jsonfix::parse_partial("tru", jsonfix::Options::partial(jsonfix::Allow::NOTHING))
        .expect_err("nothing to keep");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::NoValueFound);

    // Both on: the word is kept as-is (a non-keyword prefix stays a string).
    assert_eq!(jsonfix::repair("hel").unwrap(), "\"hel\"");
}

// --- 3. truncated keys are not promoted to keywords ----------------------

#[test]
fn truncated_key_is_not_promoted_to_a_keyword() {
    let options = jsonfix::Options::partial(jsonfix::Allow::ALL);
    let value = jsonfix::parse_partial("{\"t", options).expect("key kept");
    assert_eq!(value.to_json_string(), "{\"t\": null}");

    // Value position still promotes: `[fa` → `[false]`.
    let value = jsonfix::parse_partial("[fa", options).expect("value kept");
    assert_eq!(value.to_json_string(), "[false]");
}

// --- 4. strict errors report the actual character ------------------------

#[test]
fn strict_errors_report_the_offending_character() {
    let error = jsonfix::validate("[...]").expect_err("ellipsis in array");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('.'));

    let error = jsonfix::validate("{\"a\": 1, ...}").expect_err("ellipsis in object");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('.'));

    let error = jsonfix::validate("{\"a\": 1}]").expect_err("stray bracket");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter(']'));

    let error = jsonfix::validate("[1 : 2]").expect_err("colon in array");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter(':'));

    let error = jsonfix::validate("[1 ; 2]").expect_err("semicolon in array");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter(';'));

    let error = jsonfix::validate("[1 + 2]").expect_err("plus in array");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('+'));
}

#[test]
fn trailing_punctuation_is_never_accepted_strictly() {
    for input in ["1 : 2", "1 + 2", "1)"] {
        assert!(
            jsonfix::validate(input).is_err(),
            "strict must reject trailing {input:?}"
        );
    }
    // Repair mode drops the noise when nothing value-like follows.
    assert_eq!(jsonfix::repair("1 )").unwrap(), "1");
}

// --- 5. parentheses are not JSON in strict mode --------------------------

#[test]
fn parentheses_are_rejected_in_strict_mode() {
    let error = jsonfix::validate("(1)").expect_err("closed parens");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('('));
    let error = jsonfix::validate("(1").expect_err("unclosed paren");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnexpectedCharacter('('));

    // Repair mode still unwraps them (JSONP / serializer leftovers).
    assert_eq!(jsonfix::repair("(1)").unwrap(), "1");
    assert_eq!(jsonfix::repair("cb({\"a\": 1});").unwrap(), "{\"a\": 1}");
}

// --- 6. repair_extract falls back when the span fails --------------------

#[test]
fn repair_extract_falls_back_when_the_span_is_unrepairable() {
    let input = "```x\n;\n```\n{\"a\": 1}";
    // `extract` prefers the (non-json) fence whose body alone is not repairable…
    assert_eq!(jsonfix::extract(input), Some(";"));
    assert!(jsonfix::repair(";").is_err());
    // …but repair_extract still succeeds via the whole input.
    assert_eq!(jsonfix::repair_extract(input).unwrap(), "{\"a\": 1}");
}

// --- 7. StreamRepairer rollback + push/push_delta interleave -------------

#[test]
fn failed_push_rolls_back_the_chunk() {
    let mut stream = jsonfix::StreamRepairer::with_options(jsonfix::Options::strict());
    assert!(stream.push("{").is_err());
    assert_eq!(stream.input(), "");
    assert_eq!(stream.len(), 0);
    // The stream still works after the rollback.
    assert_eq!(stream.push("[1]").unwrap(), "[1]");
}

#[test]
fn failed_push_delta_rolls_back_too() {
    let mut stream = jsonfix::StreamRepairer::with_options(jsonfix::Options::strict());
    assert!(stream.push("[1]").is_ok());
    let before = stream.input().to_string();
    assert!(stream.push_delta(", {").is_err());
    assert_eq!(stream.input(), before);
    assert_eq!(stream.output(), "[1]");
}

#[test]
fn mixed_push_and_push_delta_stay_consistent() {
    let mut stream = jsonfix::StreamRepairer::new();
    let mut buffer = stream.push("{\"name\": \"Ad").unwrap().to_string();
    let delta = stream.push_delta("a\"}").unwrap();
    buffer.truncate(delta.keep);
    buffer.push_str(delta.text);
    assert_eq!(buffer, r#"{"name": "Ada"}"#);
    assert_eq!(buffer, stream.output());

    // A plain push refreshes the output without corrupting the next delta.
    let _ = stream.push(" ").unwrap().to_string();
    let before = stream.output().to_string();
    let delta = stream.push_delta(" ").unwrap();
    let mut buffer = before;
    buffer.truncate(delta.keep);
    buffer.push_str(delta.text);
    assert_eq!(buffer, stream.output());
}

// --- 8. Serialize never retypes a number as a string ---------------------
// (covered under the serde_json feature; see tests/serde_bridge.rs)

// --- 9. extract does not re-read a closing fence as an opener ------------

#[test]
fn empty_fence_followed_by_prose_is_not_mined_as_a_candidate() {
    assert_eq!(jsonfix::extract("```\n```\nprose text\n```"), None);
    // Two real fences still work, json-tag preference intact.
    assert_eq!(
        jsonfix::extract("```python\nx = 1\n```\n```json\n{\"a\": 1}\n```"),
        Some("{\"a\": 1}")
    );
}

// --- 10. dead code removal is compile-time; behavior pinned above --------

// --- Fuzz-found: braces inside object string values ------------------------
// `{"a": "x{"}` is valid JSON; the top-level isInsideUnclosedBracket
// heuristic must not reject an end quote just because content has `{`.

#[test]
fn braces_inside_object_string_values_are_valid_json() {
    for doc in [
        r#"{"a": "{"}"#,
        r#"{"a": "x{"}"#,
        r#"{"a": "x{y"}"#,
        r#"["foo [ bar"]"#,
    ] {
        assert!(jsonfix::validate(doc).is_ok(), "validate rejected {doc:?}");
        assert_eq!(
            jsonfix::repair(doc).unwrap(),
            doc,
            "repair must be identity on {doc:?}"
        );
    }
    // Top-level embedded-quote + bracket case still repairs (reference).
    assert_eq!(
        jsonfix::repair(r#""the set {a, b"} more""#).unwrap(),
        r#""the set {a, b\"} more""#
    );
}

// --- Fuzz-found: non-ASCII after a `\uXXXX` escape was dropped -------------
// `owned` was materialized by the escape; the non-ASCII branch advanced
// `pos` without appending to it.

#[test]
fn non_ascii_survives_after_unicode_escape() {
    let cases: &[(&str, &[u8])] = &[
        ("\"\\u0000é\"", &[0x00, 0xC3, 0xA9]),
        ("\"\\u0001é\"", &[0x01, 0xC3, 0xA9]),
        ("\"x\\u0000😀\"", &[b'x', 0x00, 0xF0, 0x9F, 0x98, 0x80]),
    ];
    for (input, expected) in cases {
        let value = jsonfix::parse(input).expect("parses");
        let text = value.as_str().expect("string");
        assert_eq!(text.as_bytes(), *expected, "for {input:?}");
        // Fixpoint: rendering and reparsing must not lose the characters.
        let once = jsonfix::repair(input).unwrap();
        let twice = jsonfix::repair(&once).unwrap();
        assert_eq!(once, twice, "repair not stable for {input:?}");
        assert_eq!(
            jsonfix::parse(&once).unwrap().as_str().unwrap().as_bytes(),
            *expected
        );
    }
}

// --- Fuzz-found: NDJSON `[` insert rolled back with a stale length ---------
// After `insert('[')` the old `mark` pointed one byte short of the first
// value, so a dropped second value truncated mid-token (`["x` / `[`).

#[test]
fn dropped_second_ndjson_value_leaves_the_first_intact() {
    // `print())` parses to None under the stream path (stray `)`); the
    // rollback must restore the first value, not a bare `[`.
    for input in [
        "x\n```py\nprint())\n```",
        "1\n```py\nprint())\n```",
        "step\u{FFFD}one\n```py\nprint())\n```",
    ] {
        let out = jsonfix::repair(input).expect("repairable");
        jsonfix::validate(&out)
            .unwrap_or_else(|e| panic!("repair({input:?}) = {out:?} failed validate: {e}"));
    }
}
