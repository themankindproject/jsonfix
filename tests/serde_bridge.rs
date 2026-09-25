//! The optional `serde` and `serde_json` integration.
#![cfg(feature = "serde_json")]

use jsonfix::{DeserializeError, Value};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, PartialEq)]
struct Reply {
    answer: String,
    score: f32,
}

#[test]
fn deserializes_model_output() {
    let reply: Reply = jsonfix::deserialize("{answer: 'yes', score: 0.9,}").expect("deserializes");
    assert_eq!(
        reply,
        Reply {
            answer: String::from("yes"),
            score: 0.9,
        }
    );
}

#[test]
fn deserializes_from_a_fenced_reply() {
    let text = "Sure:\n```json\n{\"answer\": \"yes\", \"score\": 1.5}\n```";
    let reply: Reply = jsonfix::deserialize(jsonfix::extract(text).expect("fence")).expect("ok");
    assert_eq!(reply.score, 1.5);
}

#[test]
fn reports_a_type_mismatch_as_a_json_error() {
    let error = jsonfix::deserialize::<Reply>("{answer: 'yes', score: 'many'}").expect_err("fails");
    assert!(matches!(error, DeserializeError::Json(_)));
    assert!(error.to_string().contains("deserialization failed"));
}

#[test]
fn reports_a_repair_error() {
    let error =
        jsonfix::deserialize::<Reply>("prose before {\"answer\": \"x\"}").expect_err("fails");
    assert!(matches!(error, DeserializeError::Repair(_)));
    assert!(error.to_string().contains("repair failed"));
}

/// `from_serde_json` walks a sorted map, so member order can differ from the
/// source text; each value is still equal, key for key.
#[test]
fn converts_between_value_types() {
    let text = r#"{"a": [1, 2], "b": "text", "c": true, "d": null, "big": 1234567890123456789}"#;
    let mine = jsonfix::parse(text).expect("parses");
    let theirs = mine.to_serde_json();
    assert_eq!(theirs["a"][1], serde_json::json!(2));
    assert_eq!(theirs["b"], serde_json::json!("text"));
    assert_eq!(theirs["big"], serde_json::json!(1234567890123456789u64));
    let back = Value::from_serde_json(&theirs);
    for (key, value) in mine.as_object().expect("an object") {
        assert_eq!(back.get(key), Some(value), "key {key}");
    }
}

#[test]
fn values_serialize_with_serde() {
    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Wrapper {
        payload: Value,
    }

    let value = jsonfix::parse("{payload: {list: [1, 2.5, 'x'], flag: True}}").expect("parses");
    let wrapper = Wrapper {
        payload: value.pointer("/payload").expect("payload").clone(),
    };
    let json = serde_json::to_string(&wrapper).expect("serializes");
    assert_eq!(json, r#"{"payload":{"list":[1,2.5,"x"],"flag":true}}"#);
}

#[test]
fn non_finite_numbers_error_on_serialize_instead_of_becoming_strings() {
    // `1e400` is a legal JSON number whose value overflows f64. Serializing
    // must fail with a clear message — never silently become `null` (what
    // serde_json does with a raw non-finite f64) or a JSON string.
    let value = jsonfix::parse("1e400").expect("parses");
    let error = serde_json::to_string(&value).expect_err("non-finite number");
    let message = error.to_string();
    assert!(
        message.contains("1e400") && message.contains("finite"),
        "unexpected error: {message}"
    );

    // Finite numbers still round-trip exactly through i64/u64.
    let value = jsonfix::parse("1234567890123456789").expect("parses");
    assert_eq!(
        serde_json::to_string(&value).unwrap(),
        "1234567890123456789"
    );
}
