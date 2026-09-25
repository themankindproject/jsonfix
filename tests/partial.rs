//! Partial parsing: what may be kept from a value that is still being written.

use jsonfix::{Allow, Options, parse_partial};

fn parsed(input: &str, allow: Allow) -> String {
    parse_partial(input, Options::partial(allow))
        .unwrap_or_else(|error| panic!("failed on {input:?}: {error}"))
        .to_json_string()
}

#[test]
fn objects_and_strings() {
    let obj = Allow::OBJ;
    assert_eq!(parsed("{\"a\": 1}", obj), "{\"a\": 1}");
    assert_eq!(parsed("{\"a\": 1", obj), "{\"a\": 1}");
    // The cut-off value is dropped and the object itself is closed.
    assert_eq!(parsed("{\"a\": 1, \"b\": \"xy", obj), "{\"a\": 1}");
    // The cut-off string is kept when it is allowed.
    assert_eq!(
        parsed("{\"a\": 1, \"b\": \"xy", Allow::OBJ | Allow::STR),
        "{\"a\": 1, \"b\": \"xy\"}"
    );
    // A cut-off key is dropped unless keys may be partial.
    assert_eq!(parsed("{\"a\": 1, \"b", obj), "{\"a\": 1}");
    assert_eq!(
        parsed("{\"a\": 1, \"b", Allow::OBJ | Allow::KEY),
        "{\"a\": 1, \"b\": null}"
    );
}

#[test]
fn arrays() {
    assert_eq!(parsed("[1, 2", Allow::ARR), "[1, 2]");
    assert_eq!(parsed("[1, 2, ", Allow::ARR), "[1, 2]");
    assert_eq!(parsed("[1, \"xy", Allow::ARR), "[1]");
    assert_eq!(parsed("[1, \"xy", Allow::ARR | Allow::STR), "[1, \"xy\"]");
    // Nested containers each need their own flag.
    assert_eq!(
        parsed("{\"a\": [1, 2", Allow::OBJ | Allow::ARR),
        "{\"a\": [1, 2]}"
    );
    assert_eq!(parsed("{\"a\": [1, 2", Allow::OBJ), "{}");
}

#[test]
fn numbers_and_keywords() {
    assert_eq!(parsed("[1, 2.", Allow::ARR), "[1]");
    assert_eq!(parsed("[1, 2.", Allow::ARR | Allow::NUM), "[1, 2.0]");
    assert_eq!(parsed("[tru", Allow::ARR), "[]");
    assert_eq!(parsed("[tru", Allow::ARR | Allow::BOOL), "[true]");
    assert_eq!(parsed("[fal", Allow::ARR | Allow::BOOL), "[false]");
    assert_eq!(parsed("[nul", Allow::ARR | Allow::NULL), "[null]");
}

#[test]
fn every_flag_together_keeps_the_most() {
    let all = Allow::ALL;
    assert_eq!(parsed("{\"a\": \"xy", all), "{\"a\": \"xy\"}");
    assert_eq!(parsed("{\"a\": 1.5e", all), "{\"a\": 1.5e0}");
    assert_eq!(parsed("[1, {\"k\": \"v", all), "[1, {\"k\": \"v\"}]");
}

#[test]
fn incomplete_values_can_be_refused_entirely() {
    // With no flags at all a cut-off string leaves nothing to return.
    let error =
        parse_partial("\"abc", Options::partial(Allow::NOTHING)).expect_err("nothing to keep");
    assert_eq!(error.kind(), &jsonfix::ErrorKind::NoValueFound);
    assert_eq!(parsed("\"abc", Allow::STR), "\"abc\"");
}
