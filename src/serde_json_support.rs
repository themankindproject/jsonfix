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
///
/// # Errors
///
/// Returns [`DeserializeError::Repair`] when `input` cannot be repaired, or
/// [`DeserializeError::Json`] when the repaired JSON does not match `T`.
pub fn deserialize<T: DeserializeOwned>(input: &str) -> Result<T, DeserializeError> {
    deserialize_with(input, Options::all())
}

/// Like [`deserialize`], with explicit [`Options`].
///
/// # Errors
///
/// Returns [`DeserializeError`] when `input` cannot be repaired under `opts`
/// or the repaired JSON does not match `T`.
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
///
/// # Errors
///
/// Returns [`DeserializeError`] only when `input` cannot be repaired —
/// rendering to `serde_json::Value` itself does not fail.
pub fn loads(input: &str) -> Result<JsonValue, DeserializeError> {
    loads_with(input, Options::all())
}

/// Like [`loads`], with explicit [`Options`].
///
/// # Errors
///
/// Returns [`DeserializeError`] when `input` cannot be repaired under `opts`;
/// rendering itself does not fail.
pub fn loads_with(input: &str, opts: Options) -> Result<JsonValue, DeserializeError> {
    Ok(crate::parse_with(input, opts)?.into())
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
    ///
    /// This clones every string; `JsonValue::from(value)` (the [`From`]
    /// conversion) consumes the tree and moves them instead.
    pub fn to_serde_json(&self) -> JsonValue {
        JsonValue::from(self.clone())
    }

    /// Builds a `jsonfix` value from a `serde_json::Value`.
    ///
    /// This clones every string; `Value::from(json)` (the [`From`]
    /// conversion) consumes the tree and moves them instead.
    pub fn from_serde_json(value: &JsonValue) -> Value {
        Value::from(value.clone())
    }
}

/// Consuming conversion: strings and keys move into the `serde_json` tree
/// without being copied. Numbers follow [`Value::to_serde_json`]'s rules.
///
/// ```
/// let value = jsonfix::parse("{name: 'Ada', big: 1e400}").unwrap();
/// let json = serde_json::Value::from(value);
/// assert_eq!(json["name"], "Ada");
/// assert_eq!(json["big"], "1e400"); // out-of-range text survives as a string
/// ```
impl From<Value> for JsonValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => JsonValue::Null,
            Value::Bool(value) => JsonValue::Bool(value),
            Value::Number(number) => number_to_json(number),
            Value::String(text) => JsonValue::String(text),
            Value::Array(items) => JsonValue::Array(items.into_iter().map(Into::into).collect()),
            Value::Object(members) => JsonValue::Object(
                members
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect(),
            ),
        }
    }
}

/// Consuming conversion from a `serde_json` tree; strings and keys move.
impl From<JsonValue> for Value {
    fn from(value: JsonValue) -> Self {
        match value {
            JsonValue::Null => Value::Null,
            JsonValue::Bool(value) => Value::Bool(value),
            JsonValue::Number(number) => {
                Value::Number(crate::value::Number::from_normalized(number.to_string()))
            }
            JsonValue::String(text) => Value::String(text),
            JsonValue::Array(items) => Value::Array(items.into_iter().map(Into::into).collect()),
            JsonValue::Object(members) => Value::Object(
                members
                    .into_iter()
                    .map(|(key, value)| (key, value.into()))
                    .collect(),
            ),
        }
    }
}

/// An exact `i64`/`u64`, else a finite `f64`, else the number text as a
/// string (never a silent `null`).
fn number_to_json(number: crate::value::Number) -> JsonValue {
    use crate::value::Classified;
    match number.classify() {
        Some(Classified::I64(value)) => JsonValue::from(value),
        Some(Classified::U64(value)) => JsonValue::from(value),
        Some(Classified::F64(value)) => JsonValue::from(value),
        None => JsonValue::String(number.into_string()),
    }
}
