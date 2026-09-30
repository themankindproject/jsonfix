# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `StreamRepairer` could resume a checkpoint recorded *before* the
  double-escape verdict flipped on: the unescaped render records no
  checkpoints, so when a later raw `"` flipped the verdict back off, the stale
  checkpoint spliced a raw tail onto the unescaped prefix (`[\n1, ` +
  `\"a\", ` + `"b"]` rendered `[1, "a", "a\"", "b"]` instead of
  `repair`'s `["\\n1", "a\"", "b"]`). Every full render now starts from a
  cleared checkpoint.
- Deserializing a non-finite float into a `Value` (from any serde source)
  produced the number text `NaN`/`inf`, which renders as invalid JSON. It now
  becomes `null`, as in `serde_json`.

### Changed

- Floats deserialized into a `Value` keep their shortest round-trip *float*
  spelling (`1.0`, `1e300`) instead of the integer-looking `Display` text
  (`1`, `1000…0`), so `from_value::<Value>` is an exact round trip and a
  float never re-serializes as an integer.

### Added

- Consuming conversions `From<Value> for serde_json::Value` and
  `From<serde_json::Value> for Value` (feature `serde_json`): strings and keys
  move instead of being cloned. `loads` uses them.
- `From<f64>` / `From<f32>` (non-finite → `null`) and `From<Option<T>>`
  (`None` → `null`) for `Value`; `FromIterator<(K, V)>` collects key/value
  pairs into an object (arrays still collect from plain values).
- `Number::into_string`.
- 128-bit integers (feature `serde`): `from_value` reads `i128`/`u128` targets
  from the number text exactly, and `Value` deserializes from `i128`/`u128`
  sources.

### Performance

Criterion medians against a baseline recorded on 0.1.0, same machine
(`cargo bench -p jsonfix-benchmarks`); ratios are the claim, absolute times
move with the machine.

| Benchmark | 0.1.0 | Unreleased | Time |
|---|---|---|---|
| `repair/fenced_body` (13 KB LLM reply) | 263.5 μs | 193.8 μs | −26% |
| `repair/repair_extract` | 268.8 μs | 210.5 μs | −22% |
| `partial/parse_full` | 224.5 μs | 180.7 μs | −20% |
| `partial/parse_partial` | 222.8 μs | 180.4 μs | −19% |
| `bytes/repair` | 265.0 μs | 194.7 μs | −26% |
| `from_value/parse_then_from_value` | 482.1 μs | 408.4 μs | −15% |
| `stream/token_chunks_8b/flat_object_800` | 4.20 ms | 3.10 ms | −26% |
| `stream/chunks_64b/flat_object_200` | 400.3 μs | 308.4 μs | −24% |
| `stream/ndjson_lines/200_lines` | 349.3 μs | 147.1 μs | −58% |
| `stream/one_char_at_a_time/small_doc` | 18.9 μs | 15.3 μs | −20% |

`extract/fenced` and `from_value/from_value_only` are unchanged (within ±2%).

- **`StreamRepairer::push_delta` is linear.** It used to copy and
  byte-compare the whole output on every chunk; it now diffs only the bytes
  after the resumed checkpoint, a word at a time. A 51 KB object pushed in
  8-byte chunks drops from 134 ms to 3.4 ms (≈ `push`'s 3.2 ms), and the new
  `stream/token_chunks_8b/flat_object_800_delta` bench guards it.
- **NDJSON streaming is linear.** Resuming no longer allocates a placeholder
  per completed top-level value: 2000 lines pushed line by line drop from
  25.9 ms to 1.5 ms.
- **`extract_all` is linear.** Its bracket search is cached across values, so
  prose full of bare scalars no longer rescans to the end per value (16 000
  values: 4.3 s → 1.9 ms).
- `loads` is 27–35% faster: it converts the tree by value instead of cloning
  every string.
- Lexer: an ASCII fast path in character peeking, a punctuator table, a
  tighter trivia pre-check, `find`-based comment skipping, and a string fast
  path that returns clean, well-terminated strings without entering the
  repair loop.
- Parser: each lookahead token is moved into place once (no dead drop of the
  previous one), unquoted and keyword object keys borrow instead of
  allocating, and checkpoint bookkeeping exits early on the one-shot `repair`
  path.
- SWAR scans (string content, `write_escaped`, `extract` string bodies)
  combine all stop classes into one mask per 8-byte word — one branch instead
  of three or four.
- `extract` matches brackets byte-wise, skips string bodies in 8-byte strides,
  and picks the preferred fence in a single pass without a candidate list.

### Internal

- Object and array loops share their separator/closer handling
  (`separator_step`, `close_container`); tree mode keeps parsed values on one
  stack instead of threading `Option<Value>` through every call.
- Implemented once instead of two or three times: the double-escape
  pre-pass, RFC 6901 token parsing (`pointer` / `pointer_mut`), comment and
  fence skipping, the one-character escape table, number classification for
  `Serialize` / `deserialize_any` / `serde_json`, and the stream
  `push` / `push_delta` render path.
- Non-test code in `src/` shrinks from 4052 to 3943 lines despite the
  additions above; the duplicated fuzz-replay loops in the stream tests
  collapse into one helper.
- New regression tests cover every fix above, `Delta::keep` maximality,
  `extract_all` scaling, the consuming `serde_json` conversions, and the
  combined SWAR masks against a scalar reference. Verified against 0.1.0 by
  replaying the full fuzz corpus (155 840 inputs) through every public API
  with zero output differences.

## [0.1.0] - 2026-09-29

First public release: everything below is new in `0.1.0`.

### Added

**Core API**

- `parse`, `parse_with`, `parse_partial`, `validate`, `repair`, `repair_with`,
  `repair_into`, `repair_extract`, and `deserialize` at the crate root.
- `extract`, `extract_partial`, and `extract_all` for pulling JSON spans out of
  prose, markdown fences, and log lines.
- `StreamRepairer` with `push` / `push_delta` for incremental (LLM token)
  input, `Delta { keep, text }` for cheap re-renders.
- `Value` tree with lossless `Number` text, `pointer` (RFC 6901),
  `write_to`/`to_json_string` rendering, and a closed set of six kinds —
  exhaustive `match` over it is stable.
- `Value` accessors and predicates: `as_u64` joins `as_i64`/`as_f64`, and
  `is_bool`/`is_number`/`is_string`/`is_array`/`is_object` join `is_null`.
- Ergonomic `Value` access in the `serde_json` style: `value["key"]` and
  `value[i]` via `Index` (never panic — a missing key, out-of-range index,
  or wrong kind yields a shared `null`, so lookups chain freely);
  `PartialEq` against `str`/`String`/`bool`/every integer width (exact, via
  the number text) and floats, in both directions (`value["n"] == 36`);
  `From` for `bool`, `&str`/`String`, every integer width, `()` (`null`),
  `Vec<Value>`, and `Vec<(String, Value)>`, plus `FromIterator` for arrays.
- Mutable access: `get_mut`, `pointer_mut`, `as_array_mut`, `as_object_mut`,
  and `take` (move a subtree out, e.g. into `from_value`, without cloning).
- docs.rs badges every feature-gated item with the feature that enables it
  (`doc_cfg` under `--cfg docsrs`).
- `Repairs` bitmask (12 passes) and `Allow` bitmask (partial-json semantics).
- Byte-precise `Error`/`ErrorKind` with `Display` messages and stable
  `message()` text.
- Every public accessor and builder is `#[must_use]`, and every fallible
  function documents its failure modes under `# Errors`.
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
  never detours through `f64`), float targets reject any value that is not
  finite in the *target* type — including one that overflows `f32` to infinity
  while still finite as `f64` (`1e40` into an `f32` field errors rather than
  yielding `inf`) — and text that does not fit the target errors instead of
  being silently narrowed.
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
- Errors on double-escaped documents (`{\"a\": 1}`) report byte offsets in the
  caller's input; positions from the parser's unescaped copy are mapped back.
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
- `Value::pointer` array references follow RFC 6901 §4: only `0` or a
  leading-zero-free digit run indexes an array, so `/01`, `/-1`, and `/1e0`
  resolve to nothing instead of being coerced through `usize::parse`.
- `Value::pointer` rejects a non-empty pointer that does not start with `/`
  (RFC 6901): `pointer("users")` resolves to nothing instead of silently
  returning the whole document.
- A cut-off string segment after `+` (`"a" + "b`) follows the same policy as
  a lone cut-off string: it errors without `TRUNCATION`, drops the value
  without `Allow::STR`, and blocks stream checkpoints. Previously the joined
  string was accepted unconditionally, and a checkpoint after it let
  `StreamRepairer` diverge from `repair` once the segment kept growing
  (found by replaying the fuzz corpus through a prefix-parity sweep: 74
  prefix divergences and 64 `push_delta` mismatches, now zero).
- The NDJSON `[` wrap's extra nesting level is reserved when entering a
  container, so an over-deep later value fails at its offending opener with
  the same byte offset from `repair`, `parse`, and `StreamRepairer`
  (previously `repair` reported byte 0 from a post-render scan while `parse`
  reported the end of input). The wrap check also measures only the current
  document's output, so `repair_into` no longer fails with
  `DepthLimitExceeded` when the caller's buffer already holds `[` characters.

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
- Repair no longer pays two whole-buffer scans per call: the post-render
  depth check is skipped when the parser's high-water nesting (plus one
  reserved NDJSON `[` level) proves the output fits `MAX_NESTING_DEPTH`, and
  the double-escape pre-pass stops at the first raw quote instead of scanning
  the whole input. Back-to-back A/B on `repair/fenced_body`: ~310–313 μs →
  ~276–279 μs (≈10–12%).
- `repair_bytes` / `repair_bytes_into` no longer build an intermediate
  `String` and copy it into the caller's `Vec<u8>`: when the sink's current
  bytes are valid UTF-8 (always true for an empty sink, i.e. `repair_bytes`)
  the repair renders straight into the sink's own allocation, moved through a
  `String` and back with `into_bytes` (no copy). A sink already holding
  non-UTF-8 bytes keeps the buffered fallback. On the 13 KB LLM-reply fixture
  this drops `repair_bytes` from 4 allocations / 26.5 KB to 3 / 13.3 KB —
  byte-for-byte identical to `repair`'s own allocation profile (measured with
  a counting global allocator).
- `Value::to_json_string` / `Display` size the output buffer from a cheap
  one-pass length estimate (`render_len_hint`) instead of starting at 32 bytes
  and doubling. Rendering the same 13 KB tree drops from 10 allocations /
  32.7 KB to a single 12.8 KB allocation with no reallocation.
- `extract` no longer allocates a lowercased copy of each fence's info string
  to test for the `json` tag; the first four bytes are compared in place with
  `eq_ignore_ascii_case`. Extracting from a reply with four fences drops from
  five allocations (one per fence tag plus the candidate list) to one (the
  candidate list alone), scaling with the number of fences.
- `Value::pointer` only allocates an unescaped copy of a reference token when
  it actually contains a `~` escape; ordinary tokens index the tree with the
  borrowed slice. Resolving `/users/1/name` drops from six allocations (two
  per segment for the `~1`/`~0` replaces) to zero; escaped tokens keep their
  single allocation.
- The parser pre-reserves its container-frame stack (`FRAME_PREALLOC = 16`),
  so descending into nested input no longer pays the early `Vec` doublings
  (0→1→2→4→8…); one upfront allocation covers essentially all real-world
  nesting. Flat input is unaffected (one allocation either way).
- `structural_depth` (the post-render nesting check) scans bytes instead of
  `chars()`: every byte it acts on (`"`, `\`, `[`, `{`, `]`, `}`) is ASCII, so
  UTF-8 decoding was pure overhead on the output-validation pass.

**Tooling**

- `cargo-fuzz` harness under `fuzz/` with seven targets (`repair`, `parse`,
  `extract`, `stream`, `options`, `bytes`, `from_value`) and a committed seed
  corpus. CI builds the harness and runs a short smoke pass on every PR;
  `.github/workflows/fuzz-scheduled.yml` runs 10-minute passes per target
  nightly (or on demand via `workflow_dispatch`) and uploads crash artifacts.
- Criterion benchmarks (`benchmarks/benches/repair.rs`, `stream.rs`) for the
  one-shot repair and streaming paths.
- CI covers the feature matrix explicitly: default, `--all-features`,
  `--no-default-features` (check + test), and each optional feature alone
  (`std`, `serde`, `serde_json`).
- The README's Rust examples run as doctests (`#[doc = include_str!(...)]`
  under `cfg(doctest)`), so they cannot rot silently.
- The published crate is an explicit allowlist (`include` in `Cargo.toml`):
  library source, manifest, README, and licenses only. Tests, examples,
  benches, fuzz harnesses, and CI/tooling config never reach dependents,
  and the default feature set pulls in no dependencies at all.
- The optional `serde` / `serde_json` dependencies declare the oldest
  releases verified to build the crate (`serde >= 1.0.100`,
  `serde_json >= 1.0.45`, the first with an `alloc` feature), checked with
  `cargo update -Z direct-minimal-versions`.

[Unreleased]: https://github.com/themankindproject/jsonfix/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/themankindproject/jsonfix/releases/tag/v0.1.0
