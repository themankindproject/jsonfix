//! Ergonomic `Value` access: indexing, comparisons, conversions, mutation.

use jsonfix::{Value, parse};

#[test]
fn index_chains_through_objects_and_arrays() {
    let value = parse("{user: {name: 'Ada', tags: ['math', 'code']}}").unwrap();
    assert_eq!(value["user"]["name"], "Ada");
    assert_eq!(value["user"]["tags"][1], "code");
}

#[test]
fn index_never_panics_on_missing_or_wrong_type() {
    let value = parse("{a: [1], s: 'x'}").unwrap();
    assert!(value["missing"].is_null());
    assert!(value["missing"]["deeper"][3].is_null());
    assert!(value["a"][5].is_null(), "out of bounds");
    assert!(value["s"]["k"].is_null(), "string is not an object");
    assert!(value[0].is_null(), "object is not an array");
}

#[test]
fn index_takes_the_first_duplicate_like_get() {
    let value = parse(r#"{"k": 1, "k": 2}"#).unwrap();
    assert_eq!(value["k"], 1);
    assert_eq!(value.get("k"), Some(&value["k"]));
}

#[test]
fn compares_with_primitives_in_both_directions() {
    let value =
        parse("{s: 'hi', t: true, n: 36, neg: -5, big: 18446744073709551615, f: 1.5}").unwrap();
    assert_eq!(value["s"], "hi");
    assert_eq!("hi", value["s"]);
    assert_eq!(value["s"], String::from("hi"));
    assert_eq!(value["t"], true);
    assert_eq!(value["n"], 36);
    assert_eq!(value["n"], 36_u8);
    assert_eq!(value["neg"], -5_i64);
    assert_eq!(value["big"], u64::MAX, "exact, never through f64");
    assert_eq!(value["f"], 1.5);
    // Mismatched kinds compare unequal rather than coercing.
    assert_ne!(value["n"], "36");
    assert_ne!(value["s"], true);
    assert_ne!(value["neg"], 5_u64);
}

#[test]
fn builds_values_from_rust_types() {
    let built = Value::from(vec![
        (String::from("id"), Value::from(7_u64)),
        (String::from("name"), Value::from("Ada")),
        (String::from("ok"), Value::from(true)),
        (String::from("none"), Value::from(())),
        (String::from("tags"), ["a", "b"].into_iter().collect()),
    ]);
    assert_eq!(
        built.to_json_string(),
        r#"{"id": 7, "name": "Ada", "ok": true, "none": null, "tags": ["a", "b"]}"#
    );
    assert!(jsonfix::validate(&built.to_json_string()).is_ok());
    let numbers: Value = (1..=3_i32).collect();
    assert_eq!(numbers.to_json_string(), "[1, 2, 3]");
    assert_eq!(Value::from(-9_i64).to_json_string(), "-9");
}

#[test]
fn mutable_access_edits_the_tree_in_place() {
    let mut value = parse("{a: 1, list: [1, 2]}").unwrap();
    *value.get_mut("a").unwrap() = Value::from("one");
    assert!(value.as_array_mut().is_none(), "root is an object");
    value
        .get_mut("list")
        .and_then(Value::as_array_mut)
        .unwrap()
        .push(Value::from(3));
    value
        .as_object_mut()
        .unwrap()
        .push((String::from("b"), Value::from(false)));
    assert_eq!(
        value.to_json_string(),
        r#"{"a": "one", "list": [1, 2, 3], "b": false}"#
    );
}

#[test]
fn take_moves_a_subtree_out() {
    let mut value = parse("{payload: {x: 1}}").unwrap();
    let payload = value.get_mut("payload").unwrap().take();
    assert_eq!(payload["x"], 1);
    assert!(value["payload"].is_null());
}
