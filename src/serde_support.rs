//! Optional `serde` support: [`Value`] serializes and deserializes with any
//! serde data format.

use alloc::string::{String, ToString};
use core::fmt;

use serde::de::value::Error as DeError;
use serde::de::{
    DeserializeOwned, Deserializer, Error as _, IntoDeserializer, MapAccess, SeqAccess, Visitor,
};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

use crate::value::{Number, Value};

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Value::Null => serializer.serialize_unit(),
            Value::Bool(value) => serializer.serialize_bool(*value),
            Value::Number(number) => {
                let text = number.as_str();
                if let Ok(value) = text.parse::<i64>() {
                    return serializer.serialize_i64(value);
                }
                if let Ok(value) = text.parse::<u64>() {
                    return serializer.serialize_u64(value);
                }
                match number.as_f64() {
                    Some(value) if value.is_finite() => serializer.serialize_f64(value),
                    // Never retype a number: JSON serializers turn non-finite
                    // floats into `null` (serde_json) or would need a string
                    // fallback — both silently change the type. Error instead.
                    _ => Err(<S::Error as serde::ser::Error>::custom(alloc::format!(
                        "number `{text}` is not a finite float and cannot be represented"
                    ))),
                }
            }
            Value::String(text) => serializer.serialize_str(text),
            Value::Array(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Value::Object(members) => {
                let mut map = serializer.serialize_map(Some(members.len()))?;
                for (key, value) in members {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(ValueVisitor)
    }
}

/// A `Visitor` that builds a [`Value`] from any serde data format.
///
/// Numbers become their canonical text, so `serde` round-trips never lose
/// precision.
struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(number(value.to_string()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(number(value.to_string()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Value, E> {
        Ok(number(value.to_string()))
    }

    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(String::from(value)))
    }

    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_none<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut items = alloc::vec::Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut members = alloc::vec::Vec::new();
        while let Some((key, value)) = map.next_entry::<String, Value>()? {
            members.push((key, value));
        }
        Ok(Value::Object(members))
    }
}

fn number(text: String) -> Value {
    Value::Number(Number::from_normalized(text))
}

/// Deserializes any serde data model from a [`Value`], consuming the tree.
///
/// The typed counterpart to [`Value::to_serde_json`]: parse broken input,
/// then read it into structs without an intermediate repaired `String` and
/// without the `serde_json` feature:
///
/// ```
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct Reply { answer: String, score: f32 }
///
/// let value = jsonfix::parse("{answer: 'yes', score: 0.9,}").unwrap();
/// let reply: Reply = jsonfix::from_value(value).unwrap();
/// assert_eq!(reply.answer, "yes");
/// ```
///
/// Numbers: an integer-typed target parses the number *text* as that integer
/// (`1234567890123456789` into `u64` is exact — never a detour through
/// `f64`), a float target rejects non-finite values (`1e400`), and text that
/// does not fit the target errors instead of being silently narrowed. For
/// the string-keeping variant see [`Value::to_serde_json`](crate::Value::to_serde_json),
/// which powers [`loads`](crate::loads).
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T, serde::de::value::Error> {
    T::deserialize(value)
}

/// Typed integer/float targets: the number *text* must parse as that type,
/// so `1234567890123456789` reads into `u64` exactly and text that does not
/// fit errors instead of being narrowed through a cast. Float targets and
/// float-valued text additionally reject non-finite values (e.g. `1e400`).
macro_rules! parse_number_method {
    ($method:ident, $visit:ident, $ty:ty) => {
        fn $method<V>(self, visitor: V) -> Result<V::Value, Self::Error>
        where
            V: Visitor<'de>,
        {
            match self {
                Value::Number(text) => {
                    let raw = text.as_str();
                    match raw.parse::<$ty>() {
                        Ok(value) => match raw.parse::<f64>() {
                            Ok(f) if !f.is_finite() => Err(DeError::custom(alloc::format!(
                                "number `{raw}` is not a finite float and cannot be represented"
                            ))),
                            _ => visitor.$visit(value),
                        },
                        Err(_) => Err(DeError::custom(alloc::format!(
                            "number `{raw}` does not fit `{}`",
                            core::stringify!($ty)
                        ))),
                    }
                }
                other => Err(type_error(core::stringify!($ty), other)),
            }
        }
    };
}

/// [`Deserializer`] for [`Value`]: nested values are consumed and
/// deserialized directly.
impl<'de> Deserializer<'de> for Value {
    type Error = DeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Null => visitor.visit_unit(),
            Value::Bool(value) => visitor.visit_bool(value),
            Value::Number(text) => visit_number(text, visitor),
            Value::String(text) => visitor.visit_string(text),
            Value::Array(items) => visitor.visit_seq(SeqDeserializer(items.into_iter())),
            Value::Object(members) => visitor.visit_map(MapDeserializer::new(members)),
        }
    }

    fn deserialize_bool<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Bool(value) => visitor.visit_bool(value),
            other => Err(type_error("a boolean", other)),
        }
    }

    parse_number_method!(deserialize_i8, visit_i8, i8);
    parse_number_method!(deserialize_i16, visit_i16, i16);
    parse_number_method!(deserialize_i32, visit_i32, i32);
    parse_number_method!(deserialize_i64, visit_i64, i64);
    parse_number_method!(deserialize_u8, visit_u8, u8);
    parse_number_method!(deserialize_u16, visit_u16, u16);
    parse_number_method!(deserialize_u32, visit_u32, u32);
    parse_number_method!(deserialize_u64, visit_u64, u64);
    parse_number_method!(deserialize_f32, visit_f32, f32);
    parse_number_method!(deserialize_f64, visit_f64, f64);

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::String(text) => {
                let mut chars = text.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => visitor.visit_char(c),
                    _ => Err(DeError::custom(alloc::format!(
                        "expected one character, found the string `{text}`"
                    ))),
                }
            }
            other => Err(type_error("a character", other)),
        }
    }

    fn deserialize_str<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::String(text) => visitor.visit_str(&text),
            other => Err(type_error("a string", other)),
        }
    }

    fn deserialize_string<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::String(text) => visitor.visit_string(text),
            other => Err(type_error("a string", other)),
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Null => visitor.visit_none(),
            value => visitor.visit_some(value),
        }
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    // The generic form drives typed structures and externally tagged enums
    // (`{"Note": "x"}`), and keeps object keys and number text intact.
    fn deserialize_map<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Object(members) => visitor.visit_map(MapDeserializer::new(members)),
            other => Err(type_error("a map", other)),
        }
    }

    fn deserialize_struct<V>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_map(visitor)
    }

    fn deserialize_seq<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Array(items) => visitor.visit_seq(SeqDeserializer(items.into_iter())),
            other => Err(type_error("a sequence", other)),
        }
    }

    fn deserialize_tuple<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_seq(visitor)
    }

    fn deserialize_unit<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::Null => visitor.visit_unit(),
            other => Err(type_error("`()`", other)),
        }
    }

    fn deserialize_unit_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_unit(visitor)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self {
            Value::String(variant) => visitor.visit_enum(variant.into_deserializer()),
            Value::Object(members) => visitor.visit_enum(EnumDeserializer(members.into_iter())),
            other => Err(type_error("an enum", other)),
        }
    }

    fn deserialize_identifier<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.deserialize_string(visitor)
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        // `IgnoredAny`'s visitor accepts any shape.
        self.deserialize_any(visitor)
    }

    serde::forward_to_deserialize_any! { bytes byte_buf }
}

impl Value {
    /// The JSON kind name, for type-mismatch error messages.
    fn kind_name(&self) -> &'static str {
        match self {
            Value::Null => "`null`",
            Value::Bool(_) => "a boolean",
            Value::Number(_) => "a number",
            Value::String(_) => "a string",
            Value::Array(_) => "an array",
            Value::Object(_) => "an object",
        }
    }
}

fn type_error(expected: &str, found: Value) -> DeError {
    DeError::custom(alloc::format!(
        "expected {expected}, found {}",
        found.kind_name()
    ))
}

/// Reads one number for `deserialize_any`: exact `i64`/`u64` text first
/// (the [`Serialize`] mapping), then a finite `f64`; anything else is an
/// error, never a silent retype.
fn visit_number<'de, V>(text: Number, visitor: V) -> Result<V::Value, DeError>
where
    V: Visitor<'de>,
{
    let raw = text.as_str();
    if let Ok(value) = raw.parse::<i64>() {
        return visitor.visit_i64(value);
    }
    if let Ok(value) = raw.parse::<u64>() {
        return visitor.visit_u64(value);
    }
    match raw.parse::<f64>() {
        Ok(value) if value.is_finite() => visitor.visit_f64(value),
        _ => Err(DeError::custom(alloc::format!(
            "number `{raw}` is not a finite float and cannot be represented"
        ))),
    }
}

struct SeqDeserializer(alloc::vec::IntoIter<Value>);

impl<'de> SeqAccess<'de> for SeqDeserializer {
    type Error = DeError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        self.0
            .next()
            .map(|value| seed.deserialize(value))
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

/// Member iteration for [`MapAccess`]; the value of the last returned key is
/// kept until `next_value_seed` consumes it.
struct MapDeserializer {
    iter: alloc::vec::IntoIter<(String, Value)>,
    value: Option<Value>,
}

impl MapDeserializer {
    fn new(members: alloc::vec::Vec<(String, Value)>) -> Self {
        Self {
            iter: members.into_iter(),
            value: None,
        }
    }
}

impl<'de> MapAccess<'de> for MapDeserializer {
    type Error = DeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: serde::de::DeserializeSeed<'de>,
    {
        self.iter
            .next()
            .map(|(key, value)| {
                self.value = Some(value);
                seed.deserialize(key.into_deserializer())
            })
            .transpose()
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        match self.value.take() {
            Some(value) => seed.deserialize(value),
            None => Err(DeError::custom("value is missing")),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

/// A one-member object as an externally tagged enum (`{"Note": "x"}`); the
/// variant name is the key and the content is the value.
struct EnumDeserializer(alloc::vec::IntoIter<(String, Value)>);

impl<'de> serde::de::EnumAccess<'de> for EnumDeserializer {
    type Error = DeError;
    type Variant = EnumVariantDeserializer;

    fn variant_seed<V>(mut self, seed: V) -> Result<(V::Value, Self::Variant), Self::Error>
    where
        V: serde::de::DeserializeSeed<'de>,
    {
        match self.0.next() {
            Some((variant, value)) => {
                let variant = seed.deserialize(variant.into_deserializer())?;
                Ok((variant, EnumVariantDeserializer(value)))
            }
            None => Err(DeError::custom("enum must have exactly one member")),
        }
    }
}

struct EnumVariantDeserializer(Value);

impl<'de> serde::de::VariantAccess<'de> for EnumVariantDeserializer {
    type Error = DeError;

    fn unit_variant(self) -> Result<(), Self::Error> {
        match self.0 {
            Value::Null => Ok(()),
            other => Err(type_error("`null` content for a unit variant", other)),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, Self::Error>
    where
        T: serde::de::DeserializeSeed<'de>,
    {
        seed.deserialize(self.0)
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Array(items) => visitor.visit_seq(SeqDeserializer(items.into_iter())),
            other => Err(type_error("a tuple variant content", other)),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.0 {
            Value::Object(members) => visitor.visit_map(MapDeserializer::new(members)),
            other => Err(type_error("a struct variant content", other)),
        }
    }
}

#[cfg(test)]
mod from_value_tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Reply {
        answer: String,
        score: f32,
    }

    /// The documented pipeline: broken text → `parse` → `from_value`, with no
    /// intermediate repaired `String` and no `serde_json`.
    #[test]
    fn from_value_reads_a_struct_from_repaired_input() {
        let value = crate::parse("{answer: 'yes', score: 0.9,}").expect("repairs");
        let reply: Reply = from_value(value).expect("deserializes");
        assert_eq!(
            reply,
            Reply {
                answer: String::from("yes"),
                score: 0.9
            }
        );
    }

    /// Number text maps to `i64`/`u64` exactly (the `Serialize` mapping) —
    /// 64-bit identifiers never drift through `f64`.
    #[test]
    fn from_value_keeps_64_bit_integer_text_exact() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Ids {
            big: u64,
            small: i64,
        }
        let value = crate::parse("{big: 1234567890123456789, small: -9223372036854775808}")
            .expect("repairs");
        let ids: Ids = from_value(value).expect("deserializes");
        assert_eq!(
            ids,
            Ids {
                big: 1234567890123456789,
                small: i64::MIN
            }
        );
    }

    /// `null` deserializes as `None` through `Option` fields.
    #[test]
    fn from_value_treats_null_as_none() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Maybe {
            a: Option<i32>,
            b: Option<i32>,
        }
        let value = crate::parse(r#"{"a": null, "b": 2}"#).expect("parses");
        let got: Maybe = from_value(value).expect("deserializes");
        assert_eq!(
            got,
            Maybe {
                a: None,
                b: Some(2)
            }
        );
    }

    /// Sequences walk in order.
    #[test]
    fn from_value_walks_sequences() {
        let value = crate::parse("[1, 2, 3]").expect("parses");
        let items: Vec<i64> = from_value(value).expect("deserializes");
        assert_eq!(items, vec![1, 2, 3]);
    }

    /// Externally tagged enums (the JSON shape: `{"Note": "x"}` / `"Done"`)
    /// read through `from_value`.
    #[test]
    fn from_value_supports_externally_tagged_enums() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        enum Step {
            Done,
            Note(String),
        }
        let value = crate::parse(r#"{"Note": "x"}"#).expect("parses");
        let step: Step = from_value(value).expect("deserializes");
        assert_eq!(step, Step::Note(String::from("x")));
        let value = crate::parse(r#""Done""#).expect("parses");
        let step: Step = from_value(value).expect("deserializes");
        assert_eq!(step, Step::Done);
    }

    /// The typed data model errors on number text outside the finite `f64`
    /// range (mirroring `Serialize`); the document model (`to_serde_json`,
    /// `loads`) keeps it as a string instead.
    #[test]
    fn from_value_errors_on_non_finite_numbers() {
        let value = crate::parse("1e400").expect("repairs");
        let error = from_value::<f64>(value).expect_err("non-finite");
        assert!(alloc::format!("{error}").contains("finite"));
    }

    /// An integer that does not fit the target width errors instead of
    /// wrapping through a cast (serde_json's contract).
    #[test]
    fn from_value_errors_instead_of_overflowing_integer_targets() {
        // 12345678901234567890 fits `u64` but not `i64`.
        let value = crate::parse("12345678901234567890").expect("repairs");
        assert!(from_value::<i64>(value).is_err());
        let value = crate::parse("-1").expect("parses");
        assert!(from_value::<u64>(value).is_err());
    }
}
