# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Nothing has been released yet: everything below ships with `0.1.0`, so it is
all listed as additions.

## [Unreleased]

### Added

**Core API**

- `parse`, `parse_with`, `parse_partial`, `validate`, `repair`, `repair_with`,
  `repair_into`, `repair_extract`, and `deserialize` at the crate root.
- `extract`, `extract_partial`, and `extract_all` for pulling JSON spans out of
  prose, markdown fences, and log lines.
- `StreamRepairer` with `push` / `push_delta` for incremental (LLM token)
  input, `Delta { keep, text }` for cheap re-renders.
- `Value` tree with lossless `Number` text, `pointer` (RFC 6901), and
  `write_to`/`to_json_string` rendering.
- `Repairs` bitmask (12 passes) and `Allow` bitmask (partial-json semantics).
- Byte-precise `Error`/`ErrorKind` with `Display` messages and stable
  `message()` text.
- `MAX_NESTING_DEPTH` cap and `ErrorKind::DepthLimitExceeded` so deeply nested
  input fails cleanly instead of overflowing the stack.

**Byte and writer sinks**

- `repair_bytes` / `repair_bytes_with` / `repair_bytes_into`: byte-buffer repair
  (alloc only, no `std` feature) for FFI/C-ABI sinks and `Vec<u8>` buffers.
  Output is byte-identical to `repair`; invalid UTF-8 fails with the exact
  offset of the first bad byte (`ErrorKind::InvalidUtf8`), and the sink is
  left untouched on any failure.
- `repair_to_writer` + `WriteError` (new `std` feature, off by default): write
  canonical JSON to any `std::io::Write` sink; the render is buffered first so
  a repair failure never touches the sink.

**Serde integration** (features `serde` / `serde_json`)

- `from_value` (feature `serde`): read a repaired `Value` into any serde data
  model without an intermediate `String` and without `serde_json`. Integer
  targets parse the number text exactly (`1234567890123456789` into `u64`
  never detours through `f64`), float targets reject non-finite text, and text
  that does not fit the target errors instead of being silently narrowed.
- `loads` / `loads_with` (feature `serde_json`): one call from broken text to a
  `serde_json::Value`, rendering through `Value::to_serde_json` so numbers
  outside the finite `f64` range survive as strings instead of erroring the
  way `serde_json::from_str` does.
- Conversions in both directions (`Value::to_serde_json` /
  `Value::from_serde_json`) and `Deserialize`/`Serialize` for `Value`;
  `Serialize` errors on non-finite numbers instead of letting the format
  silently emit `null` or a string.

**Correctness guarantees**

- Valid JSON documents whose string values contain unbalanced braces
  (`{"a": "x{"}`) parse correctly — the end-quote heuristic applies only
  outside object/array/group frames (`src/lexer.rs`, `Lexer::in_container`).
- Non-ASCII characters after a `\uXXXX` escape are kept (`"\u0000é"` retains
  both characters).
- `repair` output is always valid JSON that `validate` accepts and `repair` is
  idempotent on, including the NDJSON wrap path and after any repair
  combination.
- The NDJSON `[` wrap (stream and tree mode) counts against
  `MAX_NESTING_DEPTH`, and `repair_document_into` post-checks structural depth
  of its output — repair output never exceeds the depth `validate` accepts.
- `StreamRepairer` checkpoints are invalidated whenever a later chunk can
  change how earlier bytes parse: NDJSON `[` retrofit, first backtick (a
  fence can re-interpret earlier bytes), and failed pushes (the chunk is
  rolled back to the last state that parsed). Unstable scalars never
  checkpoint, truncated words count as growable, truncated-keyword promotions
  (`nu` → `null`) cannot become resume boundaries, and accepting any truncated
  scalar disables further checkpoints for that render (appended text can turn
  a previously closing comma back into string content).
- Strict mode reports the offending token's actual character
  (`UnexpectedCharacter`), rejects bare `(` (`(1)`, `(1`), and does not accept
  trailing `:`/`+`/`)` after the root value; repair mode still unwraps
  JSONP/serializer parentheses.
- Cut-off words follow the same `TRUNCATION`/`Allow` policy as cut-off strings,
  and a cut-off object key is never promoted to `true`/`false`/`null`
  (`{"t` keeps the key `t`, gated by `Allow::KEY`).
- A `+` not followed by a string is left to the collection's noise handling
  (`{"a": "x" + b: 1}` still finds the key `b`); with concatenation off,
  `"a" + "b"` errors cleanly.
- `repair_extract` falls back to repairing the whole input when the extracted
  span itself cannot be repaired.
- `extract` resumes past a fence's closing ``` instead of re-reading it as an
  opener (an empty fence followed by prose no longer mines the prose).

**Performance**

- Lexer hot loops (`skip_ws_only`, bare-word and digit scans, string content)
  and JSON string escaping scan bytes and bulk-copy ASCII runs instead of
  decoding UTF-8 per character.
- String and number tokens borrow the input slice when no repair touched them
  (`Cow<'a, str>`); clean numbers are recognized by a grammar pre-scan that
  never allocates.
- Under `Allow::ALL` (the default `repair*` path) the parser streams canonical
  JSON straight into the output buffer — no `Value` tree, no second walk —
  while `parse`/`validate`/`parse_partial` keep the tree path for `Allow`-gated
  drops. On a 13 KB LLM-reply fixture this cuts allocations from 1411 per
  repair to 2.
- Token-path slimming: `skip_trivia` classifies the next byte before doing any
  work (one load per token boundary), the lookahead caches its `Tag` instead of
  re-deriving it on every peek, and fence probing only fires when a backtick
  could actually follow.
- A tiny safe-SWAR module (`src/swar.rs`, no unsafe, no dependencies) skips
  long clean byte runs in 8-byte strides in string scanning and
  `write_escaped`, with a scalar tail for the exact stop byte.
- `StreamRepairer` renders incrementally: it checkpoints stable boundaries
  (consumed commas, real closers) and reparses only the newly appended tail
  instead of the whole document. Multi-chunk streaming of a 200-member object
  runs in 2.8 ms (was 29.0 ms without checkpoints) and 200-line NDJSON in
  1.5 ms (was 18.3 ms); EOF-invented repairs (truncation nulls) never
  checkpoint, so a later chunk can still supply the real value.
- The double-escape pre-pass check folds per chunk instead of rescanning the
  accumulated input on every `push`/`push_delta` (the fold is undone when a
  chunk is rolled back). Token-sized streaming of a 40 KB object pushed in
  8-byte chunks runs in 4.8 ms (152 ms without the fold), 160 KB in 16 ms
  (2.6 s); guarded by the `stream/token_chunks_8b` bench.
- Net on the one-shot bench fixture: `repair/fenced_body` 45.0 MiB/s (279 μs),
  `repair_extract` 43.4 MiB/s — measured on a single machine, ratios are the
  claim. Release `lto` is `thin` (own bench/example links ~4× faster than
  `fat`); benchmarks live in a `benchmarks` workspace member so plain
  `cargo test` does not compile the criterion dependency tree.

**Tooling**

- `cargo-fuzz` harness under `fuzz/` with seven targets (`repair`, `parse`,
  `extract`, `stream`, `options`, `bytes`, `from_value`) and a committed seed
  corpus. CI builds the harness and runs a short smoke pass on every PR.
- Criterion benchmarks (`benchmarks/benches/repair.rs`, `stream.rs`) for the
  one-shot repair and streaming paths.
