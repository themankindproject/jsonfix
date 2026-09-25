//! Strict parsing: `validate` accepts exactly the JSON grammar.

/// Documents that are valid JSON and must be accepted.
const VALID: &[&str] = &[
    "{}",
    "[]",
    "null",
    "true",
    "false",
    "0",
    "1.5",
    "-1",
    "1234567890123456789",
    "\"\"",
    "\"text\"",
    "\"escaped \\\" quote\"",
    "\"back\\\\slash\"",
    "\"\\u0041\\u00e9\"",
    "\"\\ud83d\\ude00\"",
    "\"line\\nbreak\"",
    "[1, 2, 3]",
    "[[1], [2, [3]]]",
    "[true, false, null]",
    "{\"a\": 1}",
    "{\"\": 1}",
    "{\"a\": {\"b\": {\"c\": []}}}",
    "{\"a\": [1, {\"b\": null}], \"c\": \"d\"}",
    "  {  \"a\"  :  1  }  ",
    "{\"a\":\"\\u00e9\"}",
    "\n[\n1,\n2\n]\n",
];

/// Valid numbers whose exact text does not survive a trip through `f64`, and
/// therefore are only compared with themselves.
const VALID_NUMBER_TEXT: &[&str] = &["-0", "1e5", "1.5e-3", "-1.5E+3", "0.10", "1E0"];

/// Documents that are not valid JSON and must be rejected.
const INVALID: &[&str] = &[
    "",
    "   ",
    "{",
    "}",
    "[",
    "]",
    ",",
    ":",
    "{a: 1}",
    "{'a': 1}",
    "{\"a\": 1,}",
    "[1, 2,]",
    "[1 2]",
    "{\"a\": 1 \"b\": 2}",
    "{\"a\" 1}",
    "{\"a\"}",
    "{\"a\":}",
    "{\"a\": }",
    "{\"a\": 1",
    "[1, 2",
    "{\"a\": [1, 2}",
    "\"unterminated",
    "{\"a\": 1}}",
    "01",
    ".5",
    "1.",
    "1e",
    "1e+",
    "+1",
    "--1",
    "NaN",
    "Infinity",
    "undefined",
    "TRUE",
    "nullx",
    "{\"a\": True}",
    "{\"a\": None}",
    "{\"a\": 1} trailing",
    "1 2",
    "[] []",
    "{\"a\": 1} // comment",
    "/* comment */ {\"a\": 1}",
    "```json\n{\"a\": 1}\n```",
    "&quot;x&quot;",
    "{\"a\":\u{00A0}1}",
    "{\u{201C}a\u{201D}: 1}",
    "{\"a\": \"x\" + \"y\"}",
    "{\"a\": 1, }",
    "[1,,2]",
    "{,}",
    "{\"a\":1,,\"b\":2}",
    "(1)",
    "(1",
];

#[test]
fn accepts_valid_json() {
    for input in VALID {
        let value = jsonfix::validate(input)
            .unwrap_or_else(|error| panic!("rejected valid JSON {input:?}: {error}"));
        // Prove the value matches what an independent parser reads: re-parse
        // jsonfix's own rendering through serde_json and compare. (Deliberately
        // avoids Value::from_serde_json so this test compiles without the
        // serde_json feature.)
        let expected: serde_json::Value = serde_json::from_str(input).expect("serde_json agrees");
        let actual: serde_json::Value = serde_json::from_str(&value.to_json_string())
            .unwrap_or_else(|error| {
                panic!("jsonfix output for {input:?} is not valid JSON: {error}");
            });
        assert_eq!(actual, expected, "for {input:?}");
    }
}

#[test]
fn rejects_invalid_json() {
    for input in INVALID {
        assert!(
            jsonfix::validate(input).is_err(),
            "accepted invalid JSON {input:?}"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(input).is_err(),
            "the test expectation is wrong for {input:?}"
        );
    }
}

#[test]
fn number_text_is_preserved_exactly() {
    for input in VALID_NUMBER_TEXT {
        let value = jsonfix::validate(input)
            .unwrap_or_else(|error| panic!("rejected valid number {input:?}: {error}"));
        assert_eq!(value.to_json_string(), *input);
    }
}

#[test]
fn strict_errors_point_at_the_input() {
    let error = jsonfix::validate("[1, 2,]").expect_err("trailing comma");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::TrailingComma);
    assert!(error.position() <= "[1, 2,]".len());
    assert!(error.to_string().contains("trailing comma"));

    let error = jsonfix::validate("[1 2]").expect_err("missing comma");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::ExpectedComma);

    let error = jsonfix::validate("{a: 1}").expect_err("bare key");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::UnquotedValue);
}
