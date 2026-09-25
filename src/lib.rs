//! # jsonfix
//!
//! **Repair, extract, and partially parse malformed JSON** — the JSON that
//! language models emit, that logs contain, and that hand-written files slowly
//! turn into.
//!
//! Language models wrap JSON in prose and markdown fences, use single quotes,
//! leave trailing commas, and get cut off mid-string when a stream ends. Most
//! JSON parsers reject all of that. `jsonfix` accepts it, fixes it, and hands
//! back valid JSON — with zero dependencies, no `unsafe`, and no `std`.
//!
//! ## The four verbs
//!
//! | Function | Use it for |
//! |---|---|
//! | [`repair`] | turn broken JSON into a valid `String` |
//! | [`extract`] | pull the JSON value out of prose or a fenced block |
//! | [`parse`] / [`parse_partial`] | get a [`Value`] tree, complete or still streaming |
//! | [`StreamRepairer`] | repair a document that arrives chunk by chunk |
//!
//! ```
//! use jsonfix::{extract, parse, repair, repair_extract};
//!
//! let reply = "Sure! Here you go:\n```json\n{name: 'Ada', age: 36,}\n```";
//! // A chat reply has prose around the JSON: extract first, then repair.
//! assert_eq!(extract(reply), Some("{name: 'Ada', age: 36,}"));
//! assert_eq!(repair_extract(reply).unwrap(), r#"{"name": "Ada", "age": 36}"#);
//! // Without prose the same functions work on the bare document.
//! assert_eq!(repair("{name: 'Ada'}").unwrap(), r#"{"name": "Ada"}"#);
//! assert_eq!(parse("{age: 36}").unwrap().get("age").and_then(|v| v.as_i64()), Some(36));
//! ```
//!
//! ## Partial values while a stream is still being written
//!
//! [`Allow`] decides what may be kept from a value that is not finished yet.
//! This is what a chat UI needs while the model is still writing.
//!
//! ```
//! use jsonfix::{parse_partial, Allow, Options};
//!
//! let opts = Options::partial(Allow::OBJ | Allow::STR);
//! let value = parse_partial(r#"{"answer": "Hel"#, opts).unwrap();
//! assert_eq!(value.to_json_string(), r#"{"answer": "Hel"}"#);
//! ```
//!
//! ## Numbers keep their exact text
//!
//! A number is never routed through `f64`, so 64-bit IDs, timestamps, and long
//! decimals come back byte for byte:
//!
//! ```
//! use jsonfix::repair;
//!
//! assert_eq!(
//!     repair("{id: 1234567890123456789, price: 1.10}").unwrap(),
//!     r#"{"id": 1234567890123456789, "price": 1.10}"#
//! );
//! ```
//!
//! ## Choosing how much gets repaired
//!
//! Every repair pass has its own flag in [`Repairs`], and [`Options::strict`]
//! turns the crate into a plain validator:
//!
//! ```
//! use jsonfix::{validate, Options, Repairs, repair_with};
//!
//! // Fences and comments only: a bare word stays an error.
//! let opts = Options::all().with_repairs(Repairs::FENCES | Repairs::COMMENTS);
//! assert_eq!(repair_with("```json\n{\"a\": 1} // done\n```", opts).unwrap(), r#"{"a": 1}"#);
//! assert!(repair_with("{a: 1}", opts).is_err());
//!
//! assert!(validate(r#"{"a": 1}"#).is_ok());
//! assert!(validate("{a: 1}").is_err());
//! ```
//!
//! ## What gets repaired
//!
//! * markdown fences, surrounding prose, and JSONP wrappers (`cb({...})`)
//! * `//` and `/* */` comments, trailing commas, and missing commas
//! * single quotes and typographic quotes (`“...”`, `‘...’`)
//! * unquoted keys and values, `True`/`False`/`None`, `undefined`
//! * `NaN`/`Infinity` (kept as the strings `"NaN"`/`"Infinity"`), leading zeros
//!   (kept as a string), `.5`, `2.`, `2e`, and a lone `-`
//! * missing escapes, `\x41` escapes, HTML entities (`&quot;`), `"a" + "b"`
//! * `NumberLong(2)` / `ISODate("...")` wrappers and `[1, 2, ...]` ellipses
//! * truncated documents: open brackets and strings are closed
//! * several top-level values (NDJSON) become one array
//!
//! The behaviour follows the widely used JavaScript `jsonrepair` for the cases
//! it defines, expressed here as a tolerant lexer plus a lenient parser.
//!
//! ## No standard library required
//!
//! The crate is `#![no_std]` (it needs `alloc` only for the values it builds)
//! and forbids `unsafe`, which matters when the input comes from an untrusted
//! model or a hostile log line.
#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

mod chars;
mod error;
mod extract;
mod lexer;
mod options;
mod parser;
mod stream;
mod swar;
mod value;

#[cfg(feature = "serde_json")]
mod serde_json_support;
#[cfg(feature = "serde")]
mod serde_support;

use alloc::string::String;

pub use error::{Error, ErrorKind};
pub use extract::{extract, extract_all, extract_partial};
pub use options::{Allow, Options, Repairs};
pub use parser::MAX_NESTING_DEPTH;
pub use stream::{Delta, StreamRepairer};
pub use value::{Number, Value};

#[cfg(feature = "serde_json")]
pub use serde_json_support::{DeserializeError, deserialize, deserialize_with, loads, loads_with};
#[cfg(feature = "serde")]
pub use serde_support::from_value;

/// Parses `input` into a [`Value`], repairing anything repairable.
///
/// ```
/// use jsonfix::parse;
///
/// let value = parse("{name: 'Ada', tags: ['math', 'code',],}").unwrap();
/// assert_eq!(value.to_json_string(), r#"{"name": "Ada", "tags": ["math", "code"]}"#);
/// ```
pub fn parse(input: &str) -> Result<Value, Error> {
    parse_with(input, Options::all())
}

/// Like [`parse`], with explicit [`Options`].
pub fn parse_with(input: &str, opts: Options) -> Result<Value, Error> {
    // A whole document that was escaped once too often, such as `{\"a\": 1}`.
    if opts.repairs(Repairs::UNQUOTED) && looks_double_escaped(input) {
        let unescaped = unescape_once(input);
        return parser::Parser::new(&unescaped, opts).parse_document();
    }
    parser::Parser::new(input, opts).parse_document()
}

/// Like [`parse_with`], named for the partial-parsing use case.
///
/// Pass `Options::partial(Allow::OBJ | Allow::STR)` (or any other [`Allow`]
/// combination) to keep values that are still being written.
pub fn parse_partial(input: &str, opts: Options) -> Result<Value, Error> {
    parse_with(input, opts)
}

/// Validates that `input` is valid JSON and returns it as a [`Value`].
///
/// ```
/// use jsonfix::validate;
///
/// assert!(validate("[1, 2, 3]").is_ok());
/// assert!(validate("[1, 2, 3,]").is_err());
/// ```
pub fn validate(input: &str) -> Result<Value, Error> {
    parse_with(input, Options::strict())
}

/// Repairs `input` and returns canonical, valid JSON.
///
/// ```
/// use jsonfix::repair;
///
/// assert_eq!(repair("{a: 1, /* note */ b: 'two',}").unwrap(), r#"{"a": 1, "b": "two"}"#);
/// ```
pub fn repair(input: &str) -> Result<String, Error> {
    repair_with(input, Options::all())
}

/// Like [`repair`], with explicit [`Options`].
pub fn repair_with(input: &str, opts: Options) -> Result<String, Error> {
    let mut out = String::with_capacity(input.len());
    repair_into(input, &mut out, opts)?;
    Ok(out)
}

/// Like [`repair_with`], appending to a caller-provided buffer.
///
/// The buffer is left untouched (trimmed back to its previous length) when
/// repair fails.
pub fn repair_into(input: &str, out: &mut String, opts: Options) -> Result<(), Error> {
    repair_document_into(input, opts, out, None)
}

/// Byte-oriented [`repair`]: repairs `input` bytes and returns the repaired
/// document as bytes.
///
/// The input must be valid UTF-8; on invalid input the error carries the
/// exact byte offset of the first bad byte
/// ([`ErrorKind::InvalidUtf8`]). The output is byte-identical to [`repair`]
/// for the same input and options.
///
/// Available without the `std` feature (alloc only) — the byte entry point
/// for FFI/C-ABI sinks and `Vec<u8>` buffers.
///
/// ```
/// let out = jsonfix::repair_bytes(b"{a: 1,}").unwrap();
/// assert_eq!(out, br#"{"a": 1}"#);
/// ```
pub fn repair_bytes(input: &[u8]) -> Result<alloc::vec::Vec<u8>, Error> {
    repair_bytes_with(input, Options::all())
}

/// Like [`repair_bytes`], with explicit [`Options`].
pub fn repair_bytes_with(input: &[u8], opts: Options) -> Result<alloc::vec::Vec<u8>, Error> {
    let mut out = alloc::vec::Vec::with_capacity(input.len());
    repair_bytes_into(input, &mut out, opts)?;
    Ok(out)
}

/// Like [`repair_bytes_with`], appending to a caller-provided byte buffer.
///
/// The buffer is left untouched (trimmed back to its previous length) when
/// the repair fails or the input is not valid UTF-8.
pub fn repair_bytes_into(
    input: &[u8],
    out: &mut alloc::vec::Vec<u8>,
    opts: Options,
) -> Result<(), Error> {
    let input = str_from_utf8(input)?;
    let start = out.len();
    let text = repair_with(input, opts).inspect_err(|_| {
        out.truncate(start);
    })?;
    out.extend_from_slice(text.as_bytes());
    Ok(())
}

/// Validates UTF-8 up front, reporting the first invalid byte's offset —
/// positions in every other error are input byte offsets too.
fn str_from_utf8(input: &[u8]) -> Result<&str, Error> {
    core::str::from_utf8(input)
        .map_err(|error| Error::new(ErrorKind::InvalidUtf8, error.valid_up_to()))
}

/// Repairs `input` into `out` using the fastest sound strategy for `opts`.
///
/// Under [`Allow::ALL`] no value can be dropped mid-parse, so the parser
/// streams canonical JSON straight into the buffer — no [`Value`] tree is
/// built and plain strings/numbers stay borrowed from the input. Any other
/// option set falls back to parse-then-serialize (the tree path supports
/// `Allow`-gated drops). On error `out` is restored to its previous length.
///
/// When `cp` is provided (stream rendering), resume checkpoints recorded
/// during the parse are written back into it — unless the double-escape
/// pre-pass rewrites positions, in which case `cp` is ignored.
pub(crate) fn repair_document_into(
    input: &str,
    opts: Options,
    out: &mut String,
    cp: Option<&mut parser::ResumeCp>,
) -> Result<(), Error> {
    let start = out.len();
    let result = if opts.allows(Allow::ALL) {
        // A whole document that was escaped once too often, such as `{\"a\": 1}`.
        let unescaped;
        let double_escaped = opts.repairs(Repairs::UNQUOTED) && looks_double_escaped(input);
        let effective: &str = if double_escaped {
            unescaped = unescape_once(input);
            &unescaped
        } else {
            input
        };
        // Checkpoints index the raw input; the unescape pre-pass would
        // invalidate their offsets, so they are disabled for that path.
        let cp = if double_escaped { None } else { cp };
        let mut parser = parser::Parser::new_stream(effective, opts, out, cp);
        parser.parse_document_stream()
    } else {
        parse_with(input, opts).map(|value| value.write_to(out))
    };
    match result {
        Ok(()) => {
            // Post-condition: anything we emit must survive `validate`,
            // including its `MAX_NESTING_DEPTH` check (the NDJSON wrap can
            // add a level `enter()` never counted — also guard here so no
            // path can skip the explicit checks).
            if parser::structural_depth(&out[start..]) > parser::MAX_NESTING_DEPTH {
                out.truncate(start);
                return Err(Error::new(
                    crate::ErrorKind::DepthLimitExceeded,
                    out.len().min(input.len()),
                ));
            }
            Ok(())
        }
        Err(error) => {
            out.truncate(start);
            Err(error)
        }
    }
}

#[cfg(feature = "std")]
#[cfg(test)]
mod writer_tests {
    use super::*;
    use std::vec::Vec;

    /// A `Vec<u8>` sink collects the repair in one pass, no `String` handle.
    #[test]
    fn repair_to_writer_appends_canonical_json() {
        let mut out: Vec<u8> = b"[prefix] ".to_vec();
        repair_to_writer("{a: 1}", &mut out, Options::all()).expect("repairs");
        assert_eq!(out, b"[prefix] {\"a\": 1}");
    }

    /// On a repair failure the sink is left exactly as it was (the render is
    /// buffered first, so no partial bytes reach the writer).
    #[test]
    fn repair_to_writer_leaves_the_writer_untouched_on_repair_error() {
        let mut out: Vec<u8> = b"prefix".to_vec();
        let error = repair_to_writer("prose {\"a\": 1}", &mut out, Options::all())
            .expect_err("prose must not repair");
        assert!(matches!(error, WriteError::Repair(_)));
        assert_eq!(out, b"prefix");
    }
}

/// Byte-oriented repair: for FFI/C-ABI sinks, `Vec<u8>` buffers, and
/// `no_std` targets where a byte sink is the only output. Available without
/// the `std` feature (alloc only).
#[cfg(test)]
mod bytes_tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    /// A `Vec<u8>` sink collects the repair exactly like `repair_into`, and
    /// the output bytes match the `String` path byte for byte.
    #[test]
    fn repair_bytes_into_appends_canonical_json() {
        let mut out: Vec<u8> = b"[prefix] ".to_vec();
        repair_bytes_into(b"{a: 1, /* c */ b: 'two',}", &mut out, Options::all()).expect("repairs");
        assert_eq!(out, b"[prefix] {\"a\": 1, \"b\": \"two\"}");
    }

    /// The one-shot form returns just the repaired bytes.
    #[test]
    fn repair_bytes_returns_the_repaired_document() {
        let out = repair_bytes(b"{a: 1}").expect("repairs");
        assert_eq!(out, b"{\"a\": 1}");
    }

    /// Invalid UTF-8 fails with the exact byte offset of the first bad byte,
    /// not a position of `0` or a panic.
    #[test]
    fn repair_bytes_rejects_invalid_utf8_at_the_exact_offset() {
        // `b"{\"a\": \xff}"` — the bad byte is at offset 6.
        let error = repair_bytes(b"{\"a\": \xff}").expect_err("must reject invalid UTF-8");
        assert_eq!(error.kind(), &ErrorKind::InvalidUtf8);
        assert_eq!(error.position(), 6);
    }

    /// On any failure — repair or encoding — the sink is left exactly as it
    /// was (the render is buffered first, mirroring `repair_into`).
    #[test]
    fn repair_bytes_into_leaves_the_sink_untouched_on_error() {
        let mut out: Vec<u8> = b"prefix".to_vec();
        repair_bytes_into(b"prose {\"a\": 1}", &mut out, Options::all())
            .expect_err("must not repair");
        assert_eq!(out, b"prefix");
        repair_bytes_into(b"\xff", &mut out, Options::all())
            .expect_err("must reject invalid UTF-8");
        assert_eq!(out, b"prefix");
    }

    /// Byte output is byte-identical to the `String` API for the same input
    /// and options — both render paths must not drift.
    #[test]
    fn repair_bytes_matches_the_text_api() {
        for input in ["{a: 1", "'x', 'y'", "[1, 2, 3,]", "{\"k\": \"b\\\"}"] {
            let text = repair_with(input, Options::all()).expect("repairs");
            let bytes = repair_bytes_with(input.as_bytes(), Options::all()).expect("repairs");
            assert_eq!(bytes, text.as_bytes(), "input {input:?}");
        }
    }

    /// The tree path (restricted `Allow`) works through the byte API too.
    #[test]
    fn repair_bytes_supports_restricted_options() {
        let mut out = String::new();
        let mut bytes = Vec::new();
        let mut opts = Options::all();
        opts.allow = crate::Allow::NONE; // parse-then-serialize path
        repair_into("{a: 1,}", &mut out, opts).expect("repairs");
        repair_bytes_into(b"{a: 1,}", &mut bytes, opts).expect("repairs");
        assert_eq!(bytes, out.as_bytes());
    }
}

/// Why [`repair_to_writer`] failed: the repair itself, or the sink.
#[cfg(feature = "std")]
#[derive(Debug)]
#[non_exhaustive]
pub enum WriteError {
    /// The document could not be repaired; the sink was not touched.
    Repair(Error),
    /// The sink rejected or short-wrote bytes after a successful repair.
    Write(std::io::Error),
}

#[cfg(feature = "std")]
impl core::fmt::Display for WriteError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WriteError::Repair(error) => write!(f, "repair failed: {error}"),
            WriteError::Write(error) => write!(f, "write failed: {error}"),
        }
    }
}

#[cfg(feature = "std")]
impl core::error::Error for WriteError {}

#[cfg(feature = "std")]
impl From<Error> for WriteError {
    fn from(error: Error) -> Self {
        WriteError::Repair(error)
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for WriteError {
    fn from(error: std::io::Error) -> Self {
        WriteError::Write(error)
    }
}

/// Repairs `input` and writes canonical JSON to `sink` (requires the `std`
/// feature).
///
/// The repair is rendered in full before any bytes reach `sink`, so a repair
/// failure leaves the sink untouched; on success the JSON is written in one
/// `write_all` call. Use [`repair_into`] when the sink is a `String` — this
/// function exists for files, sockets, and other [`std::io::Write`] targets
/// without building an intermediate handle.
///
/// ```no_run
/// # fn main() -> Result<(), jsonfix::WriteError> {
/// let mut file = std::fs::File::create("repaired.json")?;
/// jsonfix::repair_to_writer("{a: 1,}", &mut file, jsonfix::Options::all())?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "std")]
pub fn repair_to_writer(
    input: &str,
    sink: &mut impl std::io::Write,
    opts: Options,
) -> Result<(), WriteError> {
    let mut buf = alloc::string::String::with_capacity(input.len());
    repair_into(input, &mut buf, opts)?;
    sink.write_all(buf.as_bytes()).map_err(WriteError::Write)
}

/// Extracts the first JSON value from `input` (prose, fences, logs) and repairs it.
///
/// Falls back to repairing the whole input when no value region is found
/// or when the extracted span itself cannot be repaired (e.g. a fenced
/// block whose body is not JSON at all).
///
/// ```
/// let reply = "The result is {\"ok\": true,} — done.";
/// assert_eq!(jsonfix::repair_extract(reply).unwrap(), r#"{"ok": true}"#);
/// ```
pub fn repair_extract(input: &str) -> Result<String, Error> {
    match extract(input) {
        Some(span) => repair(span).or_else(|_| repair(input)),
        None => repair(input),
    }
}

/// Whether the document looks like a JSON document that lost one escaping layer.
///
/// The test is deliberately conservative: `\"` must appear and no unescaped
/// `"` may be present, so a valid JSON string such as `"{\"a\": 1}"` is left
/// alone.
fn looks_double_escaped(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut escaped_quotes = 0usize;
    let mut raw_quotes = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'"' {
            continue;
        }
        let backslashes = bytes[..index]
            .iter()
            .rev()
            .take_while(|b| **b == b'\\')
            .count();
        if backslashes % 2 == 1 {
            escaped_quotes += 1;
        } else {
            raw_quotes += 1;
        }
    }
    escaped_quotes > 0 && raw_quotes == 0
}

/// Incremental [`looks_double_escaped`] verdict for streaming callers.
///
/// A quote's escaped/raw classification depends only on the run of
/// backslashes immediately before it, so the document-wide verdict folds per
/// byte: feed chunks as they arrive instead of rescanning the accumulated
/// input on every call. The verdict is exactly `looks_double_escaped` of
/// everything pushed so far.
#[derive(Debug, Clone, Default)]
pub(crate) struct DoubleEscapeScanner {
    escaped_quotes: usize,
    raw_quotes: usize,
    /// Length of the backslash run immediately before the scan position.
    backslashes: usize,
    /// State before the most recent [`push`](Self::push), restored by
    /// [`rollback`](Self::rollback) when that chunk is rejected.
    snapshot: (usize, usize, usize),
}

impl DoubleEscapeScanner {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Folds `chunk` into the verdict, keeping the previous state in
    /// `snapshot` so the chunk can be undone with [`rollback`](Self::rollback).
    pub(crate) fn push(&mut self, chunk: &str) {
        self.snapshot = (self.escaped_quotes, self.raw_quotes, self.backslashes);
        for &byte in chunk.as_bytes() {
            match byte {
                b'\\' => self.backslashes += 1,
                b'"' => {
                    if self.backslashes % 2 == 1 {
                        self.escaped_quotes += 1;
                    } else {
                        self.raw_quotes += 1;
                    }
                    self.backslashes = 0;
                }
                _ => self.backslashes = 0,
            }
        }
    }

    /// Undoes the most recent [`push`](Self::push).
    pub(crate) fn rollback(&mut self) {
        (self.escaped_quotes, self.raw_quotes, self.backslashes) = self.snapshot;
    }

    /// The verdict for everything pushed so far: exactly
    /// [`looks_double_escaped`] over that input.
    pub(crate) fn verdict(&self) -> bool {
        self.escaped_quotes > 0 && self.raw_quotes == 0
    }
}

#[cfg(all(test, feature = "serde_json"))]
mod loads_tests {
    use super::*;
    use alloc::string::String;
    use serde_json::Value as JsonValue;

    /// One call from broken input to a `serde_json::Value`.
    #[test]
    fn loads_repairs_straight_into_a_serde_json_value() {
        let value = serde_json_support::loads("{name: 'Ada', scores: [1, 2,], ok: True}")
            .expect("repairs and parses");
        assert_eq!(value["name"], "Ada");
        assert_eq!(value["scores"][1], 2);
        assert_eq!(value["ok"], true);
    }

    /// `loads` agrees with `deserialize::<serde_json::Value>` on ordinary
    /// documents (no duplicate keys, all numbers in range).
    #[test]
    fn loads_matches_deserialize() {
        let input = r#"{"a": 1, "b": [true, null, 1234567890123456789]}"#;
        let via_loads = serde_json_support::loads(input).expect("loads");
        let via_deserialize: JsonValue = crate::deserialize(input).expect("deserialize");
        assert_eq!(via_loads, via_deserialize);
    }

    /// Numbers outside the finite `f64` range survive as strings (documented
    /// `to_serde_json` fallback) — `loads` must not error or lose them.
    #[test]
    fn loads_keeps_out_of_range_numbers_as_strings() {
        let value = serde_json_support::loads("1e400").expect("repairs");
        assert_eq!(value, JsonValue::String(String::from("1e400")));
    }
}

/// Removes one level of escaping from a double-escaped document.
fn unescape_once(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('/') => out.push('/'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('b') => out.push('\u{08}'),
            Some('f') => out.push('\u{0C}'),
            Some('u') => out.push_str("\\u"),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod double_escape_scanner_tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Every chunk split must classify the document exactly like the
    /// whole-input [`looks_double_escaped`] test.
    #[test]
    fn incremental_scan_matches_the_whole_input_scan() {
        let inputs = [
            r#"\"a\": 1"#,
            r#""a\" b""#,
            "\\\\",
            "\\\\\\\"",
            "\"",
            "\\\"",
            "\\\\\"",
            "a\\\\\\",
            r#"\"a\": \"b\""#,
            r#"\"a\": \"b\\\""#,
            "no quotes at all",
        ];
        for input in inputs {
            let want = looks_double_escaped(input);
            let one_char_chunks: Vec<&str> = {
                let mut spans = Vec::new();
                let mut chars = input.char_indices().peekable();
                while let Some((start, _)) = chars.next() {
                    let end = chars.peek().map(|(next, _)| *next).unwrap_or(input.len());
                    spans.push(&input[start..end]);
                }
                spans
            };
            for split in 0..=input.len() {
                if !input.is_char_boundary(split) {
                    continue;
                }
                for chunks in [
                    vec![input],
                    vec![&input[..split], &input[split..]],
                    one_char_chunks.clone(),
                ] {
                    let mut scanner = DoubleEscapeScanner::new();
                    for chunk in &chunks {
                        scanner.push(chunk);
                    }
                    assert_eq!(
                        scanner.verdict(),
                        want,
                        "input {input:?} split {split:?} chunks {chunks:?}"
                    );
                }
            }
        }
    }

    /// Rolling a rejected chunk back must restore the exact verdict.
    #[test]
    fn rollback_restores_the_previous_verdict() {
        let mut scanner = DoubleEscapeScanner::new();
        scanner.push(r#"\"a\" b \"c"#);
        let before = scanner.verdict();
        scanner.push("\\\""); // would add an escaped quote
        scanner.rollback();
        assert_eq!(scanner.verdict(), before);
    }
}
