//! A dependency-free JSON value tree with lossless number handling.

use alloc::borrow::Cow;
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
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The number text, by value.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// Parses the number as `f64`.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.0.parse().ok()
    }

    /// Parses the number as `i64` when it is a plain integer.
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }

    /// Parses the number as `u64` when it is a plain non-negative integer.
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.0.parse().ok()
    }

    /// The serde data-model reading of the text: an exact `i64`, else an
    /// exact `u64`, else a finite `f64`; `None` when none fits (e.g. `1e400`).
    #[cfg(feature = "serde")]
    pub(crate) fn classify(&self) -> Option<Classified> {
        if let Some(value) = self.as_i64() {
            return Some(Classified::I64(value));
        }
        if let Some(value) = self.as_u64() {
            return Some(Classified::U64(value));
        }
        self.as_f64()
            .filter(|value| value.is_finite())
            .map(Classified::F64)
    }
}

/// See [`Number::classify`].
#[cfg(feature = "serde")]
#[derive(Clone, Copy)]
pub(crate) enum Classified {
    I64(i64),
    U64(u64),
    F64(f64),
}

impl fmt::Display for Number {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A repaired, parsed JSON value.
///
/// The six kinds mirror JSON itself and are complete: `Value` is a closed
/// enum, so exhaustive matches are stable.
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
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the boolean when this is a [`Value::Bool`].
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Returns the number when this is a [`Value::Number`].
    #[must_use]
    pub fn as_number(&self) -> Option<&Number> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Returns the number as `f64` when this is a [`Value::Number`].
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number()?.as_f64()
    }

    /// Returns the number as `i64` when this is an integer [`Value::Number`].
    #[must_use]
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number()?.as_i64()
    }

    /// Returns the number as `u64` when this is a non-negative integer
    /// [`Value::Number`].
    #[must_use]
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number()?.as_u64()
    }

    /// Returns the elements when this is a [`Value::Array`].
    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Returns the key/value pairs when this is a [`Value::Object`].
    #[must_use]
    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Looks up a key in an object (first match wins).
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?
            .iter()
            .find_map(|(k, v)| (k == key).then_some(v))
    }

    /// Looks up an index in an array.
    #[must_use]
    pub fn index(&self, index: usize) -> Option<&Value> {
        self.as_array()?.get(index)
    }

    /// Resolves an RFC 6901 JSON pointer such as `/choices/0/text`.
    ///
    /// An empty pointer resolves to `self`; any other pointer must start with
    /// `/` (so `"users"` resolves to nothing rather than the root). Array
    /// reference tokens follow RFC 6901 §4: only `0` or a leading-zero-free
    /// run of digits indexes an array, so `/01` and `/-1` resolve to nothing.
    #[must_use]
    pub fn pointer(&self, pointer: &str) -> Option<&Value> {
        pointer_tokens(pointer)?.try_fold(self, |current, token| match current {
            Value::Object(_) => current.get(&token),
            Value::Array(items) => items.get(array_index(&token)?),
            _ => None,
        })
    }

    /// Mutable elements when this is a [`Value::Array`].
    #[must_use]
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Mutable key/value pairs when this is a [`Value::Object`].
    #[must_use]
    pub fn as_object_mut(&mut self) -> Option<&mut Vec<(String, Value)>> {
        match self {
            Value::Object(o) => Some(o),
            _ => None,
        }
    }

    /// Mutable lookup of a key in an object (first match wins, like [`get`]).
    ///
    /// [`get`]: Value::get
    #[must_use]
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.as_object_mut()?
            .iter_mut()
            .find_map(|(k, v)| (k == key).then_some(v))
    }

    /// Mutable counterpart of [`pointer`](Value::pointer), with the same
    /// RFC 6901 token rules.
    ///
    /// ```
    /// let mut value = jsonfix::parse("{a: {b: [1, 2]}}").unwrap();
    /// *value.pointer_mut("/a/b/1").unwrap() = jsonfix::Value::from(20);
    /// assert_eq!(value.to_json_string(), r#"{"a": {"b": [1, 20]}}"#);
    /// ```
    #[must_use]
    pub fn pointer_mut(&mut self, pointer: &str) -> Option<&mut Value> {
        pointer_tokens(pointer)?.try_fold(self, |current, token| match current {
            Value::Object(_) => current.get_mut(&token),
            Value::Array(items) => items.get_mut(array_index(&token)?),
            _ => None,
        })
    }

    /// Moves the value out, leaving [`Value::Null`] in its place.
    ///
    /// Handy for handing a subtree to [`from_value`](crate::from_value)
    /// without cloning it.
    ///
    /// ```
    /// let mut value = jsonfix::parse("{payload: [1, 2]}").unwrap();
    /// let payload = value.get_mut("payload").unwrap().take();
    /// assert_eq!(payload.len(), 2);
    /// assert!(value["payload"].is_null());
    /// ```
    #[must_use]
    pub fn take(&mut self) -> Value {
        core::mem::replace(self, Value::Null)
    }

    /// Number of elements or members; `0` for scalars.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Value::Array(a) => a.len(),
            Value::Object(o) => o.len(),
            _ => 0,
        }
    }

    /// Whether this value is [`Value::Null`].
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Whether this value is [`Value::Bool`].
    #[must_use]
    pub fn is_bool(&self) -> bool {
        matches!(self, Value::Bool(_))
    }

    /// Whether this value is [`Value::Number`].
    #[must_use]
    pub fn is_number(&self) -> bool {
        matches!(self, Value::Number(_))
    }

    /// Whether this value is [`Value::String`].
    #[must_use]
    pub fn is_string(&self) -> bool {
        matches!(self, Value::String(_))
    }

    /// Whether this value is [`Value::Array`].
    #[must_use]
    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    /// Whether this value is [`Value::Object`].
    #[must_use]
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    /// Whether this value is an empty array, an empty object, or a scalar.
    #[must_use]
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
    #[must_use]
    pub fn to_json_string(&self) -> String {
        let mut out = String::with_capacity(self.render_len_hint());
        self.write_to(&mut out);
        out
    }

    /// A cheap lower-bound estimate of the byte length [`write_to`] produces,
    /// used to size the output buffer up front and avoid reallocations while
    /// rendering. Computed in one allocation-free pass; being an estimate, it
    /// never needs to be exact.
    fn render_len_hint(&self) -> usize {
        match self {
            Value::Null => 4,
            Value::Bool(true) => 4,
            Value::Bool(false) => 5,
            Value::Number(n) => n.as_str().len(),
            // `+2` for the quotes; escaping may add more, hence "lower bound".
            Value::String(s) => s.len() + 2,
            Value::Array(items) => {
                // `[` + `]` + `, ` between items + each element.
                2 + items.len().saturating_sub(1) * 2
                    + items.iter().map(Value::render_len_hint).sum::<usize>()
            }
            Value::Object(members) => {
                // `{` + `}` + `, ` between members + each `"key": value`.
                2 + members.len().saturating_sub(1) * 2
                    + members
                        .iter()
                        .map(|(key, value)| key.len() + 4 + value.render_len_hint())
                        .sum::<usize>()
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(self.render_len_hint());
        self.write_to(&mut out);
        f.write_str(&out)
    }
}

/// Shared `null` returned by indexing a missing key or index.
static NULL: Value = Value::Null;

/// `value["key"]`: the member's value, or `null` when `value` is not an
/// object or has no such key (first match wins, like [`Value::get`]).
///
/// Indexing never panics, mirroring `serde_json`'s shared-index behavior, so
/// lookups chain freely: `value["user"]["name"]`.
///
/// ```
/// let value = jsonfix::parse("{user: {name: 'Ada', tags: ['math']}}").unwrap();
/// assert_eq!(value["user"]["name"], "Ada");
/// assert_eq!(value["user"]["tags"][0], "math");
/// assert!(value["missing"]["deeper"].is_null());
/// ```
impl core::ops::Index<&str> for Value {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        self.get(key).unwrap_or(&NULL)
    }
}

/// `value[i]`: the element, or `null` when `value` is not an array or `i` is
/// out of bounds. Never panics.
impl core::ops::Index<usize> for Value {
    type Output = Value;

    fn index(&self, index: usize) -> &Value {
        self.index(index).unwrap_or(&NULL)
    }
}

impl PartialEq<str> for Value {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for Value {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<String> for Value {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == Some(other.as_str())
    }
}

impl PartialEq<Value> for str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for &str {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<Value> for String {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

impl PartialEq<bool> for Value {
    fn eq(&self, other: &bool) -> bool {
        self.as_bool() == Some(*other)
    }
}

impl PartialEq<Value> for bool {
    fn eq(&self, other: &Value) -> bool {
        other == self
    }
}

/// Integer comparisons parse the number text exactly (never through `f64`);
/// every width is implemented so an untyped literal (`value == 36`) resolves.
macro_rules! eq_integer {
    ($($ty:ty => $wide:ty, $as_wide:ident);* $(;)?) => {$(
        impl PartialEq<$ty> for Value {
            fn eq(&self, other: &$ty) -> bool {
                self.$as_wide() == Some(<$wide>::from(*other))
            }
        }

        impl PartialEq<Value> for $ty {
            fn eq(&self, other: &Value) -> bool {
                other == self
            }
        }
    )*};
}

eq_integer! {
    i8 => i64, as_i64;
    i16 => i64, as_i64;
    i32 => i64, as_i64;
    i64 => i64, as_i64;
    u8 => u64, as_u64;
    u16 => u64, as_u64;
    u32 => u64, as_u64;
    u64 => u64, as_u64;
}

impl PartialEq<isize> for Value {
    fn eq(&self, other: &isize) -> bool {
        i64::try_from(*other).is_ok_and(|wide| self.as_i64() == Some(wide))
    }
}

impl PartialEq<usize> for Value {
    fn eq(&self, other: &usize) -> bool {
        u64::try_from(*other).is_ok_and(|wide| self.as_u64() == Some(wide))
    }
}

/// Float comparisons go through [`Value::as_f64`] (the convenience path;
/// compare [`Number::as_str`] when exactness matters).
impl PartialEq<f64> for Value {
    fn eq(&self, other: &f64) -> bool {
        self.as_f64() == Some(*other)
    }
}

impl PartialEq<f32> for Value {
    fn eq(&self, other: &f32) -> bool {
        self.as_f64() == Some(f64::from(*other))
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::String(String::from(value))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::String(value)
    }
}

impl From<Vec<Value>> for Value {
    fn from(items: Vec<Value>) -> Self {
        Value::Array(items)
    }
}

impl From<Vec<(String, Value)>> for Value {
    fn from(members: Vec<(String, Value)>) -> Self {
        Value::Object(members)
    }
}

/// `()` converts to `null`.
impl From<()> for Value {
    fn from((): ()) -> Self {
        Value::Null
    }
}

macro_rules! from_integer {
    ($($ty:ty),* $(,)?) => {$(
        impl From<$ty> for Value {
            fn from(value: $ty) -> Self {
                Value::Number(Number::from_normalized(alloc::string::ToString::to_string(&value)))
            }
        }
    )*};
}

from_integer!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

/// Floats render with Rust's shortest round-trip text (`0.1`, `1.0`,
/// `1e300`); a non-finite float has no JSON spelling and becomes `null`, as
/// in `serde_json`.
impl From<f64> for Value {
    fn from(value: f64) -> Self {
        if value.is_finite() {
            Value::Number(Number::from_normalized(alloc::format!("{value:?}")))
        } else {
            Value::Null
        }
    }
}

/// Widens to `f64` first (as `serde_json` does), so `Value::from(x) == x`
/// holds under the `f64`-based float comparison.
impl From<f32> for Value {
    fn from(value: f32) -> Self {
        Value::from(f64::from(value))
    }
}

/// `None` converts to `null`.
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(value: Option<T>) -> Self {
        value.map_or(Value::Null, Into::into)
    }
}

impl<T: Into<Value>> FromIterator<T> for Value {
    /// Collects into an array.
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Value::Array(iter.into_iter().map(Into::into).collect())
    }
}

impl<K: Into<String>, V: Into<Value>> FromIterator<(K, V)> for Value {
    /// Collects key/value pairs into an object, in order (duplicates kept).
    ///
    /// ```
    /// use jsonfix::Value;
    ///
    /// let value: Value = [("a", 1), ("b", 2)].into_iter().collect();
    /// assert_eq!(value.to_json_string(), r#"{"a": 1, "b": 2}"#);
    /// ```
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        Value::Object(
            iter.into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        )
    }
}

/// The unescaped reference tokens of an RFC 6901 pointer, or `None` for a
/// non-empty pointer that does not start with `/` (without this guard
/// `pointer("users")`, a likely typo, would yield the root document). The
/// empty pointer has no tokens. Escaping is rare, so only a token that
/// actually contains `~` allocates an unescaped copy.
fn pointer_tokens(pointer: &str) -> Option<impl Iterator<Item = Cow<'_, str>>> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return None;
    }
    Some(pointer.split('/').skip(1).map(|raw| {
        if raw.contains('~') {
            Cow::Owned(raw.replace("~1", "/").replace("~0", "~"))
        } else {
            Cow::Borrowed(raw)
        }
    }))
}

/// Parses an RFC 6901 array-index reference token.
///
/// Per RFC 6901 §4, an array index is either `0` or a sequence of digits with
/// no leading zero. Tokens such as `01`, `+1`, `-1`, or `1e0` are not valid
/// array references and resolve to no element.
fn array_index(token: &str) -> Option<usize> {
    if token != "0" && token.starts_with('0') {
        return None;
    }
    if !token.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    token.parse::<usize>().ok()
}

/// Writes `text` as a JSON string, escaping the minimum the grammar requires.
pub(crate) fn write_escaped(out: &mut String, text: &str) {
    use crate::swar::{broadcast, less_lanes, load_word, zero_lanes};
    out.push('"');
    let bytes = text.as_bytes();
    let n_quote = broadcast(b'"');
    let n_backslash = broadcast(b'\\');
    let n_control = broadcast(0x20);
    let mut plain_start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        // SWAR stride: skip 8 bytes at a time while none needs escaping
        // (`"`, `\`, or a C0 control) — one branch for all three classes.
        // The scalar scan below then finds the exact byte; SWAR only ever
        // advances over proven-clean bytes.
        while i + 8 <= bytes.len() {
            let word = load_word(bytes, i);
            let stop = zero_lanes(word ^ n_quote)
                | zero_lanes(word ^ n_backslash)
                | less_lanes(word, n_control);
            if stop != 0 {
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
