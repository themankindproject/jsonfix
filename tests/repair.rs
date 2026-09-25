//! Repair cases: every documented repair pass, one table.

/// Repairs that must succeed, with the exact canonical output.
const CASES: &[(&str, &str)] = &[
    // --- fences, prose, comments ---
    ("```json\n{\"a\": 1}\n```", "{\"a\": 1}"),
    ("```\n[1, 2]\n```", "[1, 2]"),
    ("```json\n{\"a\": 1}", "{\"a\": 1}"),
    ("{\"a\": 1} // done", "{\"a\": 1}"),
    ("/* lead */ {\"a\": 1}", "{\"a\": 1}"),
    ("{\"a\": /* inner */ 1}", "{\"a\": 1}"),
    ("// only a comment\n{\"a\": 1}", "{\"a\": 1}"),
    // --- quotes ---
    ("{name: 'John'}", "{\"name\": \"John\"}"),
    ("{\u{201C}a\u{201D}: \u{2018}b\u{2019}}", "{\"a\": \"b\"}"),
    ("{'a': \"it's fine\"}", "{\"a\": \"it's fine\"}"),
    ("{a: 'say \"hi\"'}", "{\"a\": \"say \\\"hi\\\"\"}"),
    (
        "{\"a\": \"The TV is 72\"\"}",
        "{\"a\": \"The TV is 72\\\"\"}",
    ),
    ("{\"a\": \"multi\nline\"}", "{\"a\": \"multi\\nline\"}"),
    ("{\"a\": 'tab\there'}", "{\"a\": \"tab\\there\"}"),
    // --- keywords and numbers ---
    (
        "{a: True, b: False, c: None, d: undefined, e: true, f: null}",
        "{\"a\": true, \"b\": false, \"c\": null, \"d\": null, \"e\": true, \"f\": null}",
    ),
    (
        "{n: NaN, i: Infinity, j: -Infinity}",
        "{\"n\": \"NaN\", \"i\": \"Infinity\", \"j\": \"-Infinity\"}",
    ),
    (
        "{x: .5, y: 2., z: 2e, w: -, v: 01, u: -0}",
        "{\"x\": 0.5, \"y\": 2.0, \"z\": 2e0, \"w\": -0, \"v\": \"01\", \"u\": -0}",
    ),
    (
        "{big: 1234567890123456789, price: 1.10, tiny: 2.5e-9}",
        "{\"big\": 1234567890123456789, \"price\": 1.10, \"tiny\": 2.5e-9}",
    ),
    ("{a: 1e5}", "{\"a\": 1e5}"),
    // --- commas and brackets ---
    ("[1, 2, 3,]", "[1, 2, 3]"),
    ("{\"a\": 1, \"b\": 2,}", "{\"a\": 1, \"b\": 2}"),
    ("[1 2 3]", "[1, 2, 3]"),
    ("{, \"a\": 1}", "{\"a\": 1}"),
    ("{\"a\": 1 \"b\": 2}", "{\"a\": 1, \"b\": 2}"),
    ("{\"a\" \"b\"}", "{\"a\": \"b\"}"),
    ("{\"a\": 1}}", "{\"a\": 1}"),
    ("[1, 2,]", "[1, 2]"),
    ("[1, 2, ...]", "[1, 2]"),
    ("[1, 2, ..., 9]", "[1, 2, 9]"),
    ("[..., 7, 8, 9]", "[7, 8, 9]"),
    ("{a: 1, ...}", "{\"a\": 1}"),
    // --- truncation ---
    ("{\"a\": 1", "{\"a\": 1}"),
    ("[1, 2", "[1, 2]"),
    ("{\"a\": \"xy", "{\"a\": \"xy\"}"),
    ("{\"a\": ", "{\"a\": null}"),
    ("{\"a\": 1, \"b\": \"xy", "{\"a\": 1, \"b\": \"xy\"}"),
    ("[1, \"xy", "[1, \"xy\"]"),
    ("{\"a\": [1, {\"b\": 2", "{\"a\": [1, {\"b\": 2}]}"),
    // --- concatenation, calls, entities, escaping ---
    (
        "{\"a\": \"hello \" + \"world\"}",
        "{\"a\": \"hello world\"}",
    ),
    ("{a: 'x' +\n  'y'}", "{\"a\": \"xy\"}"),
    ("{n: NumberLong(\"2\")}", "{\"n\": \"2\"}"),
    (
        "{d: ISODate(\"2012-12-19T06:01:17.171Z\")}",
        "{\"d\": \"2012-12-19T06:01:17.171Z\"}",
    ),
    ("callback({\"a\": 1});", "{\"a\": 1}"),
    ("{\"a\": &quot;x&quot;}", "{\"a\": \"x\"}"),
    ("{\"a\": \"\\x41\\x42\"}", "{\"a\": \"AB\"}"),
    ("{\\\"a\\\": 1}", "{\"a\": 1}"),
    ("{a: \"\\u00e9\\u2605\"}", "{\"a\": \"\u{e9}\u{2605}\"}"),
    // --- NDJSON and scalars ---
    ("{\"a\": 1}\n{\"b\": 2}", "[{\"a\": 1}, {\"b\": 2}]"),
    ("{\"a\": 1}, {\"b\": 2}", "[{\"a\": 1}, {\"b\": 2}]"),
    ("1\n2\n3", "[1, 2, 3]"),
    ("1", "1"),
    ("\"just a string\"", "\"just a string\""),
    // --- whitespace, URLs, regex literals ---
    ("{\u{00A0}\"a\":\u{3000}1}", "{\"a\": 1}"),
    (
        "{url: https://example.com/x?y=1}",
        "{\"url\": \"https://example.com/x?y=1\"}",
    ),
    ("{a: /re+/ , b: 2}", "{\"a\": \"/re+/\", \"b\": 2}"),
    // --- escapes in strings ---
    ("{\"a\": \"back\\\\slash\"}", "{\"a\": \"back\\\\slash\"}"),
    ("{'a': 'don\\'t'}", "{\"a\": \"don't\"}"),
];

/// Inputs that must fail rather than produce a value.
const ERRORS: &[&str] = &[
    "Sure! Here you go:\n{\"a\": 1}",
    "prose before\n{\"a\": 1}\nprose after",
];

#[test]
fn repairs_to_expected_output() {
    for (input, expected) in CASES {
        let repaired = jsonfix::repair(input)
            .unwrap_or_else(|error| panic!("failed to repair {input:?}: {error}"));
        assert_eq!(&repaired, expected, "wrong repair for {input:?}");
        // The output must be valid JSON according to an independent parser.
        serde_json::from_str::<serde_json::Value>(&repaired)
            .unwrap_or_else(|error| panic!("repaired {input:?} to invalid JSON: {error}"));
        // Repairing again must be a no-op.
        assert_eq!(jsonfix::repair(&repaired).unwrap(), repaired);
    }
}

#[test]
fn prose_needs_extraction_first() {
    for input in ERRORS {
        let error = jsonfix::repair(input).expect_err("prose must not be repaired silently");
        assert_eq!(error.kind(), &jsonfix::ErrorKind::TrailingValue);
        // `repair_extract` is the documented entry point for prose.
        assert!(jsonfix::repair_extract(input).is_ok());
    }
}

#[test]
fn parse_matches_repair_byte_for_byte() {
    for (input, expected) in CASES {
        let value = jsonfix::parse(input).expect("parses");
        assert_eq!(&value.to_json_string(), expected, "for {input:?}");
    }
}

#[test]
fn repair_into_appends() {
    let mut out = String::from("[prefix] ");
    jsonfix::repair_into("{a: 1}", &mut out, jsonfix::Options::all()).unwrap();
    assert_eq!(out, "[prefix] {\"a\": 1}");
}
