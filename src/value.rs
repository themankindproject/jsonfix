//! A dependency-free JSON value tree with lossless number handling.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A JSON number kept as its original (normalized) text.
///
/// Numbers are never converted through `f64`, so 64-bit identifiers,
/// timestamps, and long decimals survive a repair round-trip unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Number(String);

impl Number {
    /// Wraps already-normalized JSON number text.
    pub(crate) fn from_normalized(text: String) -> Self {
        Self(text)
    }

    /// The number exactly as it will be emitted.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parses the number as `f64`.
    pub fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }

    /// Parses the number as `i64` when it is a plain integer.
    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }

    /// Parses the number as `u64` when it is a plain non-negative integer.
    pub fn as_u64(&self) -> Option<u64> {
        self.0.parse().ok()
    }
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A repaired, parsed JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number, preserved as text.
    Number(Number),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object; insertion order is preserved and duplicate keys are kept.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Returns the string contents when this is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the boolean when this is a [`Value::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the number when this is a [`Value::Number`].
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Returns the number as `f64` when this is a [`Value::Number`].
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number()?.as_f64()
    }

    /// Returns the number as `i64` when this is an integer [`Value::Number`].
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number()?.as_i64()
    }

    /// Returns the elements when this is a [`Value::Array`].
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns the key/value pairs when this is a [`Value::Object`].
    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Looks up a key in an object (first match wins).
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?
            .iter()
            .find_map(|(k, v)| (k == key).then_some(v))
    }

    /// Looks up an index in an array.
    pub fn index(&self, index: usize) -> Option<&Value> {
        self.as_array()?.get(index)
    }

    /// Resolves an RFC 6901 JSON pointer such as `/choices/0/text`.
    ///
    /// An empty pointer resolves to `self`.
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        if pointer.is_empty() {
            return Some(self);
        }
        let mut current = self;
        for raw in pointer.split('/').skip(1) {
            let token = raw.replace("~1", "/").replace("~0", "~");
            current = match current {
                Value::Object(_) => current.get(&token)?,
                Value::Array(items) => items.get(token.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(current)
    }

    /// Number of elements or members; `0` for scalars.
    pub fn len(&self) -> usize {
        match self {
            Value::Array(a) => a.len(),
            Value::Object(o) => o.len(),
            _ => 0,
        }
    }

    /// Whether this value is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Whether this value is an empty array, an empty object, or a scalar.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Appends the canonical compact rendering of this value to `out`.
    pub fn write_to(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Number(n) => out.push_str(n.as_str()),
            Value::String(s) => write_escaped(out, s),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_to(out);
                }
                out.push(']');
            }
            Value::Object(members) => {
                out.push('{');
                for (i, (key, value)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_escaped(out, key);
                    out.push_str(": ");
                    value.write_to(out);
                }
                out.push('}');
            }
        }
    }

    /// Renders this value as canonical compact JSON.
    pub fn to_json_string(&self) -> String {
        let mut out = String::with_capacity(32);
        self.write_to(&mut out);
        out
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(32);
        self.write_to(&mut out);
        f.write_str(&out)
    }
}

/// Writes `text` as a JSON string, escaping the minimum the grammar requires.
pub(crate) fn write_escaped(out: &mut String, text: &str) {
    out.push('"');
    let bytes = text.as_bytes();
    let n_quote = crate::swar::broadcast(b'"');
    let n_backslash = crate::swar::broadcast(b'\\');
    let n_control = crate::swar::broadcast(0x20);
    let mut plain_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        // SWAR stride: skip 8 bytes at a time while none needs escaping
        // (`"`, `\`, or a C0 control). The scalar scan below then finds the
        // exact byte — SWAR only ever advances over proven-clean bytes.
        while i + 8 <= bytes.len() {
            let word = crate::swar::load_word(bytes, i);
            if crate::swar::hasbyte(word, n_quote)
                || crate::swar::hasbyte(word, n_backslash)
                || crate::swar::hasless(word, n_control)
            {
                break;
            }
            i += 8;
        }
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'"' || b == b'\\' || b < 0x20 {
                break;
            }
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Flush the plain run (one memcpy) and escape this byte.
        if i > plain_start {
            out.push_str(&text[plain_start..i]);
        }
        match bytes[i] {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            b => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push_str("\\u00");
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xF) as usize] as char);
            }
        }
        i += 1;
        plain_start = i;
    }
    if plain_start < bytes.len() {
        out.push_str(&text[plain_start..]);
    }
    out.push('"');
}
