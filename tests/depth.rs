//! Nesting depth is capped so hostile input cannot blow the stack.

/// `[` × `depth` + `1` + `]` × `depth`.
fn nested_arrays(depth: usize) -> String {
    format!("{}1{}", "[".repeat(depth), "]".repeat(depth))
}

#[test]
fn nesting_at_the_limit_is_accepted() {
    let input = nested_arrays(jsonfix::MAX_NESTING_DEPTH);
    jsonfix::parse(&input).expect("depth == limit parses");
    jsonfix::validate(&input).expect("depth == limit validates");
    jsonfix::repair(&input).expect("depth == limit repairs");
}

#[test]
fn nesting_past_the_limit_is_rejected() {
    let input = nested_arrays(jsonfix::MAX_NESTING_DEPTH + 1);

    let error = jsonfix::parse(&input).expect_err("depth > limit must fail");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);

    let error = jsonfix::validate(&input).expect_err("strict mode is capped too");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);

    let error = jsonfix::repair(&input).expect_err("repair is capped too");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
    assert!(
        error.to_string().contains("maximum nesting depth"),
        "unexpected message: {error}"
    );
}

#[test]
fn hostile_deep_input_errors_instead_of_overflowing() {
    // Far beyond the limit: must unwind cleanly, not crash the test process.
    for input in [
        nested_arrays(100_000),
        nested_arrays(100_000).replace(']', ")"),
    ] {
        let error = jsonfix::parse(&input).expect_err("capped");
        assert_eq!(error.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
    }
}

#[test]
fn nesting_limit_does_not_fire_on_realistic_shapes() {
    let mut input = String::from("[1, {\"a\": [2, {\"b\": [[3]]}]}]");
    // 4 real levels plus wrapping prose-level depth well under the cap.
    for _ in 0..50 {
        input = format!("[{input}]");
    }
    jsonfix::parse(&input).expect("50-level nesting is fine");
}

#[test]
fn ndjson_wrap_cannot_exceed_max_nesting_depth() {
    // The first value uses the full depth budget; retrofitting `[` would
    // make validate see MAX+1 levels. repair must refuse instead of
    // emitting output that fails its own validate.
    let deep = format!(
        "{}1{}",
        "[".repeat(jsonfix::MAX_NESTING_DEPTH),
        "]".repeat(jsonfix::MAX_NESTING_DEPTH)
    );
    assert!(
        jsonfix::validate(&deep).is_ok(),
        "deep alone is within the limit"
    );
    let ndjson = format!("{deep}\n2");
    let err = jsonfix::repair(&ndjson).expect_err("wrap must be rejected");
    assert_eq!(err.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
}

#[test]
fn tree_mode_ndjson_wrap_cannot_exceed_max_nesting_depth() {
    let deep = format!(
        "{}1{}",
        "[".repeat(jsonfix::MAX_NESTING_DEPTH),
        "]".repeat(jsonfix::MAX_NESTING_DEPTH)
    );
    let ndjson = format!("{deep}\n2");
    let err = jsonfix::parse(&ndjson).expect_err("array wrap must be rejected");
    assert_eq!(err.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
    // and repair must not emit un-validatable output
    let err = jsonfix::repair(&ndjson).expect_err("repair wrap must be rejected");
    assert_eq!(err.kind(), &jsonfix::ErrorKind::DepthLimitExceeded);
}
