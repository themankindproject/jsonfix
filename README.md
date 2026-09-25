# jsonfix

`jsonfix` repairs, extracts, and parses the JSON that language models, logs,
and hand-edited files *almost* produce: trailing commas, `'single quotes'`,
markdown fences, prose around the document, `None` instead of `null`, or a
stream cut off mid-token. One call turns it into valid JSON — or into the best
prefix of it — and tells you byte-exactly what it could not save.

Zero dependencies, `#![no_std]` + `alloc`, `#![forbid(unsafe_code)]`, lossless
numbers. Modeled on the excellent JavaScript
[`jsonrepair`](https://github.com/josdejong/jsonrepair) library.

## Install

```toml
[dependencies]
jsonfix = "0.1"
```

| Feature | Adds | Default |
|---|---|---|
| *(none)* | repair, extract, parse, stream | ✔ |
| `std` | `repair_to_writer` (`io::Write` sinks) | — |
| `serde` | `from_value`: `Value` → your own types | — |
| `serde_json` | `loads` / `deserialize` (implies `serde`) | — |

Works with `default-features = false` — the crate needs only `alloc`.

## The five verbs

| Function | Use it for |
|---|---|
| [`repair`](#repairing) | turn broken JSON into a valid `String` |
| [`extract`](#finding-json-in-prose) | pull the JSON span out of prose or a fence |
| [`parse`](#parsing-to-a-tree) / [`parse_partial`](#partial-parsing-and-truncated-input) | get a `Value` tree, complete or still streaming |
| [`StreamRepairer`](#streaming-token-by-token) | repair a document that arrives chunk by chunk |
| [`validate`](#choosing-what-gets-repaired) | strict check with byte-precise errors |

```rust
use jsonfix::{extract, parse, repair, repair_extract};

let reply = "Sure! Here you go:\n```json\n{name: 'Ada', age: 36,}\n```";
// A chat reply has prose around the JSON: extract first, then repair.
assert_eq!(extract(reply), Some("{name: 'Ada', age: 36,}"));
assert_eq!(repair_extract(reply).unwrap(), r#"{"name": "Ada", "age": 36}"#);
// Without prose the same functions work on the bare document.
assert_eq!(repair("{name: 'Ada'}").unwrap(), r#"{"name": "Ada"}"#);
assert_eq!(parse("{age: 36}").unwrap().get("age").and_then(|v| v.as_i64()), Some(36));
```

## Repairing

`repair` returns canonical JSON text; `repair_with` takes [`Options`](#choosing-what-gets-repaired).
All of the damage below is repaired by default (each is a toggleable pass):

| Damage | Input | Output |
|---|---|---|
| unquoted keys/values | `{name: 'Ada'}` | `{"name": "Ada"}` |
| trailing / missing commas | `[1, 2,]` | `[1, 2]` |
| comments | `{"a": 1 /* note */}` | `{"a": 1}` |
| `True`/`False`/`None`/`undefined` | `{a: True, b: None}` | `{"a": true, "b": null}` |
| `NaN`/`Infinity` | `{"a": NaN}` | `{"a": "NaN"}` |
| numeric oddities | `[.5, 2., 2e, -, 01]` | `[0.5, 2.0, 2e0, -0, "01"]` |
| typographic quotes | `{"a": “v”}` | `{"a": "v"}` |
| missing escapes | `{"a": "x"y"z"}` | `{"a": "x\"y\"z"}` |
| string concatenation | `{"a": "x" + "y"}` | `{"a": "xy"}` |
| wrapper calls | `NumberLong(2)`, `ISODate("d")` | `2`, `"d"` |
| ellipses | `[1, 2, ...]` | `[1, 2]` |
| truncation | `{"a": [1, 2` | `{"a": [1, 2]}` |
| NDJSON / several values | `1\n2\n3` | `[1, 2, 3]` |
| markdown fences + prose | ```` ```json\n{...}\n``` ```` | the document |
| JSONP | `cb({"a": 1})` | `{"a": 1}` |
| one escaping layer too many | `{\"a\": 1}` | `{"a": 1}` |

```rust
use jsonfix::repair;

assert_eq!(repair("{a: 1, /* note */ b: 'two',}").unwrap(), r#"{"a": 1, "b": "two"}"#);
assert_eq!(repair("{\"a\": [1, 2").unwrap(), r#"{"a": [1, 2]}"#);
assert_eq!(repair("1\n2\n3").unwrap(), "[1, 2, 3]");
```

## Finding JSON in prose

`extract` returns the raw span (unrepaired) of the first value; `extract_all`
returns every top-level span; `extract_partial` returns from the first value to
the end of input (useful while a stream is still arriving). `repair_extract` is
the convenience combo: extract, repair, and fall back to repairing the whole
input if the span is not JSON.

```rust
use jsonfix::{extract, extract_all, extract_partial, repair_extract};

assert_eq!(extract(r#"The result is {"ok": true,} — done."#), Some(r#"{"ok": true,}"#));
assert_eq!(extract("no json here"), None);
assert_eq!(extract_all("{\"a\": 1}\n{\"b\": 2}"), ["{\"a\": 1}", "{\"b\": 2}"]);
assert_eq!(extract_partial(r#"prose {"a": 1} tail"#), r#"{"a": 1} tail"#);

// Combo: prose + fence + damage in one call.
let reply = "Sure!\n```json\n{\"answer\": 42,}\n```\nDone.";
assert_eq!(repair_extract(reply).unwrap(), r#"{"answer": 42}"#);
```

## Parsing to a tree

`parse` gives a `Value` with accessor helpers, RFC 6901 pointers, and a
lossless `Number` that keeps the original digit text — 64-bit IDs and trailing
zeros survive a round trip byte for byte.

```rust
use jsonfix::parse;

let value = parse("{id: 1234567890123456789, price: 1.10}").unwrap();

// Numbers keep their exact text; parse them only if they fit.
let id = value.get("id").and_then(|v| v.as_number()).unwrap();
assert_eq!(id.as_str(), "1234567890123456789");
assert_eq!(id.as_u64(), Some(1234567890123456789)); // exact, never via f64
assert_eq!(
    value.get("price").and_then(|v| v.as_number()).unwrap().as_str(),
    "1.10"
);

// Trees: get / index / pointer (RFC 6901) / len.
let doc = parse(r#"{"users":[{"name":"ada"},{"name":"bob"}]}"#).unwrap();
assert_eq!(doc.pointer("/users/1/name").and_then(|v| v.as_str()), Some("bob"));
assert_eq!(
    doc.get("users").and_then(|u| u.index(0)).and_then(|u| u.get("name")).and_then(|v| v.as_str()),
    Some("ada")
);
assert_eq!(doc.get("users").map(|u| u.len()), Some(2));

// Render back to JSON text.
assert_eq!(value.to_json_string(), r#"{"id": 1234567890123456789, "price": 1.10}"#);
```

## Partial parsing and truncated input

While a model is still writing, the document ends mid-value. `Allow` says which
cut-off constructs may be kept; `parse_partial` (or `Options::partial`) enables
the repair passes *and* the policy.

| Flag | Keeps a cut-off… |
|---|---|
| `Allow::STR` | string (`"Hel`) |
| `Allow::NUM` | number (`1.`, `2e`, `-`) |
| `Allow::ARR` / `Allow::OBJ` | array / object |
| `Allow::KEY` | object key (`"x` → `"x": null`) |
| `Allow::BOOL` / `Allow::NULL` | `tru`, `fal` / `nul` |
| `Allow::ATOM` / `Allow::COLLECTION` / `Allow::ALL` | shorthands |

A member whose key is cut off is kept (as `null`) only with `Allow::KEY`;
otherwise the unfinished member is dropped:

```rust
use jsonfix::{parse_partial, Allow, Options};

let cut = r#"{"answer": "Hel"#;
let opts = Options::partial(Allow::OBJ | Allow::STR);
assert_eq!(parse_partial(cut, opts).unwrap().to_json_string(), r#"{"answer": "Hel"}"#);

let torn = r#"{"key": "v, "x"#;
// Unfinished member "x is dropped ...
assert_eq!(
    parse_partial(torn, Options::partial(Allow::OBJ | Allow::STR)).unwrap().to_json_string(),
    r#"{"key": "v"}"#
);
// ... unless Allow::KEY keeps it, as null.
assert_eq!(
    parse_partial(torn, Options::partial(Allow::ALL)).unwrap().to_json_string(),
    r#"{"key": "v", "x": null}"#
);
```

## Streaming token by token

`StreamRepairer` accepts the document chunk by chunk (LLM tokens) and, after
every chunk, returns exactly what repairing the whole input so far would
return. `push` re-renders the full repaired document; `push_delta` returns just
the change (`keep` this many bytes of your buffer, then append `text`) — which
is what a chat UI wants. `reset()` starts a new document.

```rust
use jsonfix::StreamRepairer;

let mut stream = StreamRepairer::new();
assert_eq!(stream.push(r#"{"name": "Ad"#).unwrap(), r#"{"name": "Ad"}"#);
assert_eq!(stream.push("a\", \"age\": ").unwrap(), r#"{"name": "Ada", "age": null}"#);
assert_eq!(stream.push("36}").unwrap(), r#"{"name": "Ada", "age": 36}"#);
assert_eq!(stream.value().unwrap().to_json_string(), r#"{"name": "Ada", "age": 36}"#);

// Deltas: patch an on-screen buffer in place.
let mut stream = StreamRepairer::new();
let mut shown = String::new();
for chunk in [r#"{"name": "Ad"#, "a\", \"age\": ", "36}"] {
    let delta = stream.push_delta(chunk).unwrap();
    shown.truncate(delta.keep);
    shown.push_str(delta.text);
}
assert_eq!(shown, r#"{"name": "Ada", "age": 36}"#);
```

## Choosing what gets repaired

Every repair pass is a bit in `Repairs` (`FENCES`, `COMMENTS`, `UNQUOTED`,
`KEYWORDS`, `NUMBERS`, `CONCATENATION`, `CALLS`, `ENTITIES`, `QUOTES`,
`WHITESPACE`, `TRUNCATION`, `NDJSON`; `ALL` / `NONE` shorthands), and
`Options::strict()` is a plain validator. With repairs off, `repair` reports
byte-precise errors instead of guessing.

```rust
use jsonfix::{repair_with, validate, Options, Repairs};

// Fences and comments only: a bare word stays an error.
let opts = Options::all().with_repairs(Repairs::FENCES | Repairs::COMMENTS);
assert_eq!(repair_with("```json\n{\"a\": 1} // done\n```", opts).unwrap(), r#"{"a": 1}"#);
assert!(repair_with("{a: 1}", opts).is_err());

assert!(validate(r#"{"a": 1}"#).is_ok());
assert!(validate("{a: 1}").is_err());
```

## Errors

Failures carry the class, a stable message, and a 0-based byte offset:

```rust
use jsonfix::{validate, ErrorKind};

let err = validate("[1, 2, 3,]").unwrap_err();
assert_eq!(err.kind(), &ErrorKind::TrailingComma);
assert_eq!(err.position(), 9);
assert_eq!(err.message(), "trailing comma");       // stable, no position
assert_eq!(err.to_string(), "trailing comma at byte 9");

// Wrapper errors display their inner error.
let mut sink = Vec::new();
let err = jsonfix::repair_to_writer("prose {\"a\": 1}", &mut sink, jsonfix::Options::all()).unwrap_err();
assert_eq!(
    err.to_string(),
    "repair failed: unexpected text around the first top-level value (try extract()) at byte 6"
);

let err = jsonfix::deserialize::<i32>("{a: 1}").unwrap_err();
assert_eq!(err.to_string(), "deserialization failed: invalid type: map, expected i32 at line 1 column 0");
```

## serde integration

`loads` returns a `serde_json::Value` in one call, `deserialize` reads straight
into your own types, and `from_value` does the same without `serde_json`.
Numbers outside the finite `f64` range survive as strings instead of erroring
the way `serde_json::from_str` does.

```rust
#[derive(serde::Deserialize, Debug, PartialEq)]
struct Reply { answer: String, score: f32 }

let reply: Reply = jsonfix::deserialize("{answer: 'yes', score: 0.9,}").unwrap();
assert_eq!(reply, Reply { answer: "yes".into(), score: 0.9 });

let value = jsonfix::loads("{n: 1e400}").unwrap();
assert_eq!(value["n"], "1e400"); // out-of-range text survives as a string

// `serde` only: no serde_json needed.
let reply: Reply = jsonfix::from_value(jsonfix::parse("{answer: 'yes', score: 0.9}").unwrap()).unwrap();
assert_eq!(reply, Reply { answer: "yes".into(), score: 0.9 });

// Value <-> serde_json::Value conversions (feature `serde_json`).
use jsonfix::Value;
assert_eq!(
    jsonfix::parse(r#"{"id": 1234567890123456789}"#).unwrap().to_serde_json()["id"],
    1234567890123456789u64
);
assert_eq!(Value::from_serde_json(&serde_json::json!({"a": 1})).to_json_string(), r#"{"a": 1}"#);
```

## Byte and writer APIs

For FFI/C-ABI sinks and `Vec<u8>` buffers: `repair_bytes` / `repair_bytes_with`
/ `repair_bytes_into` are alloc-only (no `std`) and byte-identical to `repair`.
For files and sockets: `repair_to_writer` (feature `std`) writes in one
`write_all` and never touches the sink on failure. `Value::write_to` appends
JSON text to any `String`.

```rust
// alloc only — no `std` needed.
assert_eq!(jsonfix::repair_bytes(b"{a: 1,}").unwrap(), br#"{"a": 1}"#);
let err = jsonfix::repair_bytes(b"{\"a\": \xff}").unwrap_err();
assert_eq!(err.position(), 6); // exact offset of the first bad byte

// Feature `std`: files, sockets, any io::Write.
let mut sink = Vec::new();
jsonfix::repair_to_writer("{a: 1,}", &mut sink, jsonfix::Options::all()).unwrap();
assert_eq!(sink, br#"{"a": 1}"#);

use jsonfix::parse;
let mut buf = String::from("prefix ");
parse("{a: 1}").unwrap().write_to(&mut buf);
assert_eq!(buf, r#"prefix {"a": 1}"#);
```

## Guarantees and edge cases

* **Lossless numbers**: `Number` never routes text through `f64`. `as_u64` /
  `as_i64` parse exactly (`None` if the text does not fit); `as_str` is always
  the original text; `as_f64` saturates (`1e400` → `inf`), so use `as_str`
  when exactness matters. Valid input round-trips unchanged (`1.10` stays
  `1.10`).
* **Depth cap**: nesting deeper than `MAX_NESTING_DEPTH` errors with
  `ErrorKind::DepthLimitExceeded` instead of overflowing the stack — even
  under full repair.
* **Prose is never invented, but bare words become strings**: `hello world`
  repairs to the JSON string `"hello world"` (the `UNQUOTED` pass doing its
  job); empty input errors with `NoValueFound`. Use `extract` first when only
  a JSON region should be considered.
* **Every pass is opt-out**, and `Options::strict()` accepts nothing.

## The rest of the surface

Tree accessors, streaming state, and bitmask algebra — the pieces the sections
above use but do not enumerate:

```rust
use jsonfix::{Allow, Repairs, StreamRepairer, Value};

// Every Value kind is matchable; objects keep insertion order and duplicates.
let kinds: Vec<&str> = jsonfix::parse(r#"[null, true, 1, "s", [], {}]"#)
    .unwrap().as_array().unwrap().iter().map(|v| match v {
        Value::Null => "Null", Value::Bool(_) => "Bool", Value::Number(_) => "Number",
        Value::String(_) => "String", Value::Array(_) => "Array", Value::Object(_) => "Object",
    }).collect();
assert_eq!(kinds, ["Null", "Bool", "Number", "String", "Array", "Object"]);
assert!(jsonfix::parse("null").unwrap().is_null());
assert!(jsonfix::parse("{}").unwrap().is_empty());
assert_eq!(jsonfix::parse(r#"{"a":1,"b":2}"#).unwrap().len(), 2);

// Number accessors are exact-or-None; f64 is the saturating convenience.
assert_eq!(jsonfix::parse("123456789012345678901234").unwrap().as_number().unwrap().as_i64(), None);
assert_eq!(jsonfix::parse("1.5").unwrap().as_f64(), Some(1.5));

// Stream state: raw input, repaired output, byte count.
let mut stream = StreamRepairer::new();
stream.push(r#"{"a": 1"#).unwrap();
assert_eq!(stream.input(), r#"{"a": 1"#);
assert_eq!(stream.output(), r#"{"a": 1}"#);
assert_eq!(stream.len(), 7);
assert!(!stream.is_empty());

// Bitmask algebra: |, union, without, contains, bits, is_empty.
assert_eq!((Allow::STR | Allow::NUM).bits(), 3);
assert!(!Allow::ALL.without(Allow::KEY).contains(Allow::KEY));
assert_eq!(Repairs::FENCES.union(Repairs::COMMENTS).bits(), 3);
assert!(!Repairs::ALL.without(Repairs::NDJSON).contains(Repairs::NDJSON));
assert!(Allow::NOTHING.is_empty());
```

## Complete API reference

Every public item, in one place. `Options` fields (`allow`, `repairs`) are
public; `ErrorKind`, `WriteError`, and `DeserializeError` are
`#[non_exhaustive]`, so match them with a wildcard arm.

| Item | Returns | Notes |
|---|---|---|
| `parse(input)` / `parse_with(input, Options)` | `Result<Value, Error>` | tree from possibly-broken input |
| `parse_partial(input, Options)` | `Result<Value, Error>` | alias of `parse_with`, named for the partial case |
| `validate(input)` | `Result<Value, Error>` | strict JSON check |
| `repair(input)` / `repair_with(input, Options)` | `Result<String, Error>` | canonical JSON text |
| `repair_into(input, &mut String, Options)` | `Result<(), Error>` | append to a buffer; untouched on error |
| `repair_extract(input)` | `Result<String, Error>` | extract + repair, whole-input fallback |
| `extract(input)` | `Option<&str>` | first JSON span, unrepaired |
| `extract_all(input)` | `Vec<&str>` | every top-level span |
| `extract_partial(input)` | `&str` | first span through end of input |
| `repair_bytes` / `repair_bytes_with` | `Result<Vec<u8>, Error>` | byte output, alloc-only |
| `repair_bytes_into(input, &mut Vec<u8>, Options)` | `Result<(), Error>` | byte buffer, untouched on error |
| `repair_to_writer(input, &mut impl Write, Options)` | `Result<(), WriteError>` | *(feature `std`)* |
| `deserialize` / `deserialize_with` | `Result<T, DeserializeError>` | *(feature `serde_json`)* |
| `loads` / `loads_with` | `Result<serde_json::Value, DeserializeError>` | *(feature `serde_json`)* |
| `from_value(Value)` | `Result<T, serde::de::value::Error>` | *(feature `serde`)* |
| `MAX_NESTING_DEPTH` | `usize` constant | nesting cap (deeper input errors) |

| Type | Shape | Methods / fields |
|---|---|---|
| `Value` | `Null`, `Bool`, `Number`, `String`, `Array(Vec<Value>)`, `Object(Vec<(String, Value)>)` | `as_str`, `as_bool`, `as_number`, `as_f64`, `as_i64`, `as_array`, `as_object`, `get`, `index`, `pointer`, `len`, `is_null`, `is_empty`, `write_to`, `to_json_string`, `to_serde_json` / `from_serde_json` *(serde_json)* |
| `Number` | the original digit text | `as_str`, `as_f64`, `as_i64`, `as_u64` |
| `Options` | `allow: Allow`, `repairs: Repairs` | `all`, `strict`, `partial`, `with_repairs`, `with_allow`, `repairs`, `allows` |
| `Allow` | `NOTHING`/`NONE`, `STR`, `NUM`, `ARR`, `OBJ`, `KEY`, `BOOL`, `NULL`, `ATOM`, `COLLECTION`, `ALL` | `contains`, `is_empty`, `bits`, `union`, `without`, `BitOr` |
| `Repairs` | `NONE`, `FENCES`, `COMMENTS`, `UNQUOTED`, `KEYWORDS`, `NUMBERS`, `CONCATENATION`, `CALLS`, `ENTITIES`, `QUOTES`, `WHITESPACE`, `TRUNCATION`, `NDJSON`, `ALL` | `contains`, `bits`, `union`, `without`, `BitOr` |
| `Error` | kind + byte offset | `new`, `kind`, `position`, `message`; `Display`, `core::error::Error` |
| `ErrorKind` | `UnexpectedEnd`, `UnexpectedCharacter(char)`, `ExpectedObjectKey`, `ExpectedColon`, `NoValueFound`, `InvalidEscape`, `InvalidUnicodeEscape`, `ExpectedComma`, `TrailingComma`, `TrailingValue`, `UnquotedValue`, `DepthLimitExceeded`, `InvalidUtf8` | `#[non_exhaustive]` |
| `StreamRepairer` | accumulated input + rendered output | `new`, `with_options`, `push`, `push_delta`, `value`, `input`, `output`, `len`, `is_empty`, `reset` |
| `Delta<'a>` | `keep: usize`, `text: &'a str` | — |
| `WriteError` *(std)* | `Repair(Error)` \| `Write(io::Error)` | `#[non_exhaustive]` |
| `DeserializeError` *(serde_json)* | `Repair(Error)` \| `Json(String)` | `#[non_exhaustive]` |

## How it compares

Feature surface vs the other JSON-repair crates on crates.io (sources and
READMEs checked 2026-09; "—" means the crate does not ship the feature):

| | `jsonfix` | `jsonrepair` | `jsonrepair-rs` | `llm_json` | `repair_json` |
|---|---|---|---|---|---|
| Dependencies | 0 | 2 (`memchr`, `thiserror`) | 0 (opt. `serde*`) | 3 (`serde_json`, `clap`, ...) | 1 (`thiserror`) |
| `no_std` + `alloc` | ✔ | — | — | — | — |
| Lossless number text (u64 IDs, `1.10`) | ✔ | — | — | — | — |
| Repair passes you can toggle | 12 passes | 10 options | 1 (`strict`) | 4 bools | — |
| Partial-parse policy (per value kind) | 7 flags | — | — | 1 global toggle | streaming only |
| Incremental `Delta` output (`push_delta`) | ✔ | — | — | — | — |
| Byte-precise error positions | ✔ | — | — | — | — |
| Streaming API | ✔ | ✔ | ✔ | ✔ | ✔ |

The row that matters for LLM pipelines: when a stream truncates mid-number,
`jsonfix` keeps the digits it saw and reports byte-exact offsets — no other
crate in the table promises either.

## No standard library required

`#![no_std]` with `alloc` only, and `#![forbid(unsafe_code)]` — which matters
when the input comes from an untrusted model or a hostile log line. The
optional `serde`/`serde_json` features are declared `default-features = false`
(alloc-only), so they do not pull `std` in.

## Development

```console
$ cargo test --all-features                   # full suite + doctests
$ cargo test --no-default-features --lib      # alloc-only surface
$ cargo clippy --all-targets --all-features -- -D warnings
$ cargo run --release -p jsonfix-benchmarks --bench repair   # criterion
$ cargo +nightly fuzz run repair -- -runs=50000              # harness in fuzz/
```

Behaviour follows the widely used JavaScript `jsonrepair` for the cases it
defines, expressed here as a tolerant lexer plus a lenient parser; the
streaming and partial-parse policies go beyond it.

## License

Dual-licensed under MIT or Apache-2.0, at your option.
