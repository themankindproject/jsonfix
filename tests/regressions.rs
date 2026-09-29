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

// --- Double-escape errors report positions in the caller's bytes ----------
// The pre-pass feeds the parser an unescaped copy; without remapping, error
// offsets pointed into that copy instead of the input the caller passed.

#[test]
fn double_escape_error_positions_map_back_to_the_input() {
    // Byte 10 is the `{` of the second top-level value.
    let input = r#"{\"a\": 1}{\"b\": 2}"#;
    let error = jsonfix::repair(input).expect_err("two values, no separator");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::TrailingValue);
    assert_eq!(error.position(), 10, "must point at the second value");
    assert_eq!(input.as_bytes()[error.position()], b'{');

    // `parse` takes the same path.
    let error = jsonfix::parse(input).expect_err("two values, no separator");
    assert_eq!(error.position(), 10);

    // Trailing text after the value: byte 11 is the `x`.
    let input = r#"{\"a\": 1} x"#;
    let error = jsonfix::parse(input).expect_err("trailing text");
    assert_eq!(error.position(), 11, "must point at the trailing `x`");
    assert_eq!(input.as_bytes()[error.position()], b'x');
}

#[test]
fn depth_scan_skip_does_not_weaken_the_limit() {
    // The gate skips the post-render scan when the parser's depth bound
    // proves the output fits; at and past the limit it must still run.
    // Deep-but-fine input repairs; the NDJSON wrap past the limit is still
    // rejected (covered by tests/depth.rs, re-asserted here for the gate).
    let deep = format!(
        "{}1{}",
        "[".repeat(jsonfix::MAX_NESTING_DEPTH),
        "]".repeat(jsonfix::MAX_NESTING_DEPTH)
    );
    jsonfix::repair(&deep).expect("depth == limit still repairs");
    let ndjson = format!("{deep}\n2");
    let err = jsonfix::repair(&ndjson).expect_err("wrap past limit rejected");
    assert_eq!(err.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
}

// --- Value accessor/predicate surface ---------------------------------------
// `as_u64` and the `is_*` predicates round out the accessor set; they must
// agree with the variants they report on.

#[test]
fn value_predicates_and_u64_accessor_agree_with_variants() {
    let doc = jsonfix::parse(
        r#"{"null": null, "bool": true, "num": 18446744073709551615, "neg": -1, "str": "s", "arr": [], "obj": {}}"#,
    )
    .expect("repairs");

    let get = |key: &str| doc.get(key).expect("member");

    assert!(get("null").is_null() && !get("null").is_bool());
    assert!(get("bool").is_bool() && !get("bool").is_null());
    assert!(get("num").is_number() && get("num").as_u64() == Some(u64::MAX));
    assert!(get("neg").as_u64().is_none(), "-1 is not a u64");
    assert!(get("str").is_string() && get("str").as_str() == Some("s"));
    assert!(get("arr").is_array() && get("arr").as_array() == Some(&[][..]));
    assert!(get("obj").is_object() && get("obj").as_object() == Some(&[][..]));

    // Exactly one predicate is true per value.
    for key in ["null", "bool", "num", "str", "arr", "obj"] {
        let value = get(key);
        let hits = [
            value.is_null(),
            value.is_bool(),
            value.is_number(),
            value.is_string(),
            value.is_array(),
            value.is_object(),
        ]
        .iter()
        .filter(|hit| **hit)
        .count();
        assert_eq!(hits, 1, "predicate count for {key}");
    }
}

// --- cut-off string after `+` (corpus sweep) -----------------------------

#[test]
fn cut_off_concatenated_segment_follows_the_truncation_policy() {
    use jsonfix::{Allow, ErrorKind, Options, Repairs};
    // Truncation repair off: `"a" + "b` errors exactly like a lone `"b`.
    let no_trunc = Options::all().with_repairs(Repairs::ALL.without(Repairs::TRUNCATION));
    let error = jsonfix::repair_with("\"a\" + \"b", no_trunc).expect_err("cut-off segment");
    assert_eq!(error.kind(), &ErrorKind::UnexpectedEnd);
    assert_eq!(error.position(), 6, "points at the cut-off segment");
    // A partial policy without `Allow::STR` drops the incomplete value
    // instead of keeping a half-joined string.
    let arr_only = Options::partial(Allow::ARR);
    let value = jsonfix::parse_with("[\"x\" + \"a", arr_only).expect("partial parse");
    assert_eq!(value.to_json_string(), "[]");
}

/// Streaming must equal one-shot repair at every prefix, even when a
/// concatenated segment is still growing (minimized from the fuzz corpus).
#[test]
fn streaming_a_cut_off_concatenation_matches_repair() {
    for doc in [
        "[[\"\"+\u{201E}]\u{FFFD}",
        "{\u{FFFD}\r\u{FFFD}''''+''''[''+']'",
    ] {
        let mut stream = jsonfix::StreamRepairer::new();
        let mut accepted = String::new();
        for ch in doc.chars() {
            let chunk = ch.to_string();
            accepted.push_str(&chunk);
            let got = stream.push(&chunk).map(String::from);
            let want = jsonfix::repair(&accepted);
            match (&got, &want) {
                (Ok(g), Ok(w)) => assert_eq!(g, w, "prefix {accepted:?}"),
                (Err(_), Err(_)) => accepted.truncate(accepted.len() - chunk.len()),
                _ => panic!("parity broke at {accepted:?}: {got:?} vs {want:?}"),
            }
        }
    }
}

// --- NDJSON wrap depth accounting ----------------------------------------

fn nested(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

#[test]
fn depth_errors_agree_between_repair_and_parse() {
    for doc in [
        format!("1\n{}", nested(256)),
        format!("{}\n1", nested(256)),
        nested(257),
    ] {
        let repaired = jsonfix::repair(&doc).expect_err("too deep once wrapped");
        let parsed = jsonfix::parse(&doc).expect_err("too deep once wrapped");
        assert_eq!(repaired.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
        assert_eq!(
            repaired,
            parsed,
            "same error, same byte, for {} bytes",
            doc.len()
        );
    }
    // A later value deeper than the wrap allows fails at its offending
    // opener, not at the start or end of the document.
    let error = jsonfix::repair(&format!("1\n{}", nested(256))).unwrap_err();
    assert_eq!(error.position(), 2 + 255);
    // One level less fits exactly (wrap + 255 = 256) and validates.
    let ok = jsonfix::repair(&format!("1\n{}", nested(255))).expect("fits");
    assert!(jsonfix::validate(&ok).is_ok());
}

#[test]
fn repair_into_ignores_brackets_already_in_the_callers_buffer() {
    // The depth check must measure this document only; text the caller
    // already put in the buffer is not part of it.
    let mut out = "[".repeat(300);
    jsonfix::repair_into("1\n2", &mut out, jsonfix::Options::all()).expect("repairs");
    assert!(out.ends_with("[1, 2]"));
    assert_eq!(out.len(), 300 + "[1, 2]".len());
}
