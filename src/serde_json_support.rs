//! Optional `serde_json` bridge: `jsonfix` repairs, `serde_json` deserializes.

use alloc::string::{String, ToString};
use core::fmt;

use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;

use crate::error::Error;
use crate::options::Options;
use crate::value::Value;

/// Why [`deserialize`] failed: repair failure or a type mismatch.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DeserializeError {
    /// The document could not be repaired.
    Repair(Error),
    /// The repaired document did not match the target type.
    Json(String),
}

impl fmt::Display for DeserializeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeserializeError::Repair(error) => write!(f, "repair failed: {error}"),
            DeserializeError::Json(message) => write!(f, "deserialization failed: {message}"),
        }
    }
}

impl core::error::Error for DeserializeError {}

impl From<Error> for DeserializeError {
    fn from(error: Error) -> Self {
        DeserializeError::Repair(error)
    }
}

/// Repairs `input` and deserializes it into `T` (requires the `serde_json` feature).
///
/// ```
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct Reply { answer: String, score: f32 }
///
/// let reply: Reply = jsonfix::deserialize("{answer: 'yes', score: 0.9,}").unwrap();
/// assert_eq!(reply.answer, "yes");
/// ```
pub fn deserialize<T: DeserializeOwned>(input: &str) -> Result<T, DeserializeError> {
    deserialize_with(input, Options::all())
}

/// Like [`deserialize`], with explicit [`Options`].
pub fn deserialize_with<T: DeserializeOwned>(
    input: &str,
    opts: Options,
) -> Result<T, DeserializeError> {
    let repaired = crate::repair_with(input, opts)?;
    serde_json::from_str(&repaired).map_err(|error| DeserializeError::Json(error.to_string()))
}

/// Repairs `input` and returns a [`serde_json::Value`] in one call.
///
/// This is the common "just give me a JSON tree" entry point. It renders
/// through [`Value::to_serde_json`] rather than `serde_json`'s own parser, so
/// numbers follow `to_serde_json`'s documented rules: an integer that fits
/// `i64`/`u64` stays exact, anything outside the finite `f64` range (e.g.
/// `1e400`) becomes a JSON *string* keeping its text instead of erroring the
/// way `serde_json::from_str` does. Object keys follow `serde_json::Map`'s
/// ordering and duplicate rules.
///
/// ```
/// let value = jsonfix::loads("{name: 'Ada', age: 36,}").unwrap();
/// assert_eq!(value["name"], "Ada");
/// ```
pub fn loads(input: &str) -> Result<JsonValue, DeserializeError> {
    loads_with(input, Options::all())
}

/// Like [`loads`], with explicit [`Options`].
pub fn loads_with(input: &str, opts: Options) -> Result<JsonValue, DeserializeError> {
    let value = crate::parse_with(input, opts)?;
    Ok(value.to_serde_json())
}

impl Value {
    /// Converts this value into a `serde_json::Value`.
    ///
    /// Numbers that fit `i64`/`u64` or a finite `f64` become numbers. A
    /// number outside the finite `f64` range (e.g. `1e400`) becomes a JSON
    /// *string* keeping the original text, because `serde_json` cannot hold
    /// it as a number without its `arbitrary_precision` feature.
    ///
    /// Object keys follow `serde_json::Map`'s rules: sorted (BTreeMap)
    /// unless serde_json's `preserve_order` feature is enabled, and
    /// duplicate keys collapse to the last occurrence (jsonfix itself keeps
    /// duplicates and resolves `get` first-wins).
    pub fn to_serde_json(&self) -> JsonValue {
        match self {
            Value::Null => JsonValue::Null,
            Value::Bool(value) => JsonValue::Bool(*value),
            Value::Number(number) => {
                let text = number.as_str();
                if let Ok(value) = text.parse::<i64>() {
                    JsonValue::from(value)
                } else if let Ok(value) = text.parse::<u64>() {
                    JsonValue::from(value)
                } else {
                    match text
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                    {
                        Some(value) => JsonValue::Number(value),
                        None => JsonValue::String(String::from(text)),
                    }
                }
            }
            Value::String(text) => JsonValue::String(text.clone()),
            Value::Array(items) => {
                JsonValue::Array(items.iter().map(Value::to_serde_json).collect())
            }
            Value::Object(members) => JsonValue::Object(
                members
                    .iter()
                    .map(|(key, value)| (key.clone(), value.to_serde_json()))
                    .collect(),
            ),
        }
    }

    /// Builds a `jsonfix` value from a `serde_json::Value`.
    pub fn from_serde_json(value: &JsonValue) -> Value {
        match value {
            JsonValue::Null => Value::Null,
            JsonValue::Bool(value) => Value::Bool(*value),
            JsonValue::Number(number) => {
                Value::Number(crate::value::Number::from_normalized(number.to_string()))
            }
            JsonValue::String(text) => Value::String(text.clone()),
            JsonValue::Array(items) => {
                Value::Array(items.iter().map(Value::from_serde_json).collect())
            }
            JsonValue::Object(members) => Value::Object(
                members
                    .iter()
                    .map(|(key, value)| (key.clone(), Value::from_serde_json(value)))
                    .collect(),
            ),
        }
    }
}
