//! Streaming repair for input that arrives in chunks: LLM tokens, pipes, logs.

use alloc::string::String;

use crate::error::Error;
use crate::options::{Allow, Options, Repairs};
use crate::parser::{self, ResumeCp};
use crate::value::Value;

/// What changed between two consecutive repaired outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Delta<'a> {
    /// Keep this many bytes of your previous buffer.
    pub keep: usize,
    /// Then append this text.
    pub text: &'a str,
}

/// Repairs a document that is revealed one chunk at a time.
///
/// The whole input is retained so that every chunk produces the same result as
/// repairing the complete document in one call. Feed chunks with [`push`] and
/// render the returned text; call [`reset`] when a document is finished.
///
/// # Incremental rendering
///
/// Under the default [`Options::all`] policy, renders resume from the last
/// *stable checkpoint* (a comma or closer boundary) and reparse only the
/// newly appended tail, so feeding a document in many chunks is roughly
/// linear in total size instead of quadratic. Checkpoints cannot advance
/// inside a still-growing value (a number/string at EOF may gain more
/// characters), so a single huge member without separators falls back to
/// reparsing its tail each time; option sets that can drop values
/// ([`Options::strict`], partial policies) always take the full-reparse path.
/// Once a backtick has been seen the full document is reparsed on every
/// chunk, because a fence marker can change how *earlier* bytes parse.
///
/// [`push`]: StreamRepairer::push
/// [`reset`]: StreamRepairer::reset
///
/// ```
/// use jsonfix::StreamRepairer;
///
/// let mut stream = StreamRepairer::new();
/// assert_eq!(stream.push("{\"name\": \"Ad").unwrap(), r#"{"name": "Ad"}"#);
/// assert_eq!(stream.push("a\"}").unwrap(), r#"{"name": "Ada"}"#);
/// ```
#[derive(Debug, Clone)]
pub struct StreamRepairer {
    input: String,
    previous: String,
    current: String,
    opts: Options,
    /// Last stable resume point recorded by the renderer.
    cp: ResumeCp,
    /// Whether any pushed chunk contained a backtick. A later ` ``` ` fence
    /// can re-interpret bytes that were already checkpointed (`v,` +
    /// `` ```js `` repairs differently than `v,\`` alone), so resume is
    /// disabled from the first backtick onward.
    saw_backtick: bool,
    /// Incremental [`crate::looks_double_escaped`] verdict over everything
    /// pushed so far — folded per chunk (undone per rejected chunk) so a push
    /// never rescans the accumulated input.
    escape_scan: crate::DoubleEscapeScanner,
}

impl StreamRepairer {
    /// A stream repairer that runs every repair pass.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(Options::all())
    }

    /// A stream repairer with explicit options, e.g. a partial policy.
    #[must_use]
    pub fn with_options(opts: Options) -> Self {
        Self {
            input: String::new(),
            previous: String::new(),
            current: String::new(),
            opts,
            cp: ResumeCp::new(),
            saw_backtick: false,
            escape_scan: crate::DoubleEscapeScanner::new(),
        }
    }

    /// Checkpoints apply only when no value can be dropped (`Allow::ALL`)
    /// and the input has no double-escape pre-pass (it would rewrite the
    /// offsets the checkpoints index).
    fn cp_enabled(&self) -> bool {
        self.opts.allows(Allow::ALL)
            && !(self.opts.repairs(Repairs::UNQUOTED) && self.escape_scan.verdict())
            && !self.saw_backtick
    }

    fn can_resume(&self) -> bool {
        self.cp.valid && self.cp_enabled()
    }

    /// Notes fence-relevant and double-escape-relevant bytes in `chunk` before
    /// any resume decision.
    fn note_chunk(&mut self, chunk: &str) {
        self.escape_scan.push(chunk);
        if !self.saw_backtick && chunk.contains('`') {
            self.saw_backtick = true;
            self.cp.valid = false;
        }
    }

    /// Appends `chunk` and returns the whole repaired document so far.
    ///
    /// The returned text is complete and valid JSON, so a caller can render it
    /// directly on every token. If the accumulated input cannot be repaired,
    /// the chunk is rolled back and the error is returned — the stream still
    /// holds the last state that parsed.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the accumulated input cannot be repaired.
    pub fn push(&mut self, chunk: &str) -> Result<&str, Error> {
        self.render(chunk, false)?;
        Ok(&self.current)
    }

    /// Appends `chunk` and returns only the changed tail.
    ///
    /// Apply it by truncating your buffer to [`Delta::keep`] and appending
    /// [`Delta::text`]. On error the chunk is rolled back, as in [`push`].
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the accumulated input cannot be repaired.
    ///
    /// [`push`]: Self::push
    pub fn push_delta(&mut self, chunk: &str) -> Result<Delta<'_>, Error> {
        let keep = self.render(chunk, true)?;
        Ok(Delta {
            keep,
            text: &self.current[keep..],
        })
    }

    /// Appends `chunk` and re-renders `current`, resuming from the last stable
    /// checkpoint when possible. With `delta`, returns the length of the
    /// longest char-aligned prefix the new output shares with the previous one.
    ///
    /// Invariant: `cp.valid` implies `cp` was recorded by the render (or the
    /// resumes continuing it) that produced `current`.
    fn render(&mut self, chunk: &str, delta: bool) -> Result<usize, Error> {
        let before = self.input.len();
        self.note_chunk(chunk);
        self.input.push_str(chunk);
        if self.can_resume() {
            let keep = self.cp.output_len.min(self.current.len());
            if delta {
                // The diff base past the stable prefix (the prefix is shared).
                self.previous.clear();
                self.previous.push_str(&self.current[keep..]);
            }
            self.current.truncate(keep);
            let mut parser = parser::Parser::new_stream_resumed(
                &self.input,
                self.opts,
                &mut self.current,
                &mut self.cp,
            );
            let (resumed, shift) = (parser.parse_resume(), parser.shifted_at());
            if resumed.is_ok() {
                return Ok(if delta {
                    resumed_prefix_len(&self.current, keep, &self.previous, shift)
                } else {
                    0
                });
            }
            if delta {
                // Put the previous output back: it is the diff base below.
                if let Some(at) = shift {
                    self.current.remove(at);
                }
                self.current.truncate(keep);
                self.current.push_str(&self.previous);
            }
            // Fall through: the full render reports the canonical error.
        }
        // A full render records its own checkpoints (or none, when disabled);
        // nothing from an earlier render may survive it.
        self.cp.valid = false;
        let cp = if self.cp_enabled() {
            Some(&mut self.cp)
        } else {
            None
        };
        // Delta renders into the spare buffer so `current` stays the diff base.
        let target = if delta {
            &mut self.previous
        } else {
            &mut self.current
        };
        target.clear();
        match crate::repair_document_into(&self.input, self.opts, target, cp) {
            Ok(()) if delta => {
                core::mem::swap(&mut self.previous, &mut self.current);
                Ok(common_prefix_len(&self.previous, &self.current))
            }
            Ok(()) => Ok(0),
            Err(error) => {
                self.cp.valid = false;
                self.escape_scan.rollback();
                self.input.truncate(before);
                if !delta {
                    // `current` was the render target: restore the last good
                    // rendering of the rolled-back input.
                    self.current.clear();
                    let _ = crate::repair_document_into(
                        &self.input,
                        self.opts,
                        &mut self.current,
                        None,
                    );
                }
                Err(error)
            }
        }
    }

    /// Parses everything pushed so far.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] if the accumulated input cannot be repaired.
    pub fn value(&self) -> Result<Value, Error> {
        crate::parse_with(&self.input, self.opts)
    }

    /// The raw input accumulated so far.
    #[must_use]
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The repaired output produced by the last [`push`](Self::push).
    #[must_use]
    pub fn output(&self) -> &str {
        &self.current
    }

    /// Number of input bytes accumulated.
    #[must_use]
    pub fn len(&self) -> usize {
        self.input.len()
    }

    /// Whether nothing has been pushed yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// Forgets the current document so the next chunk starts a new one.
    pub fn reset(&mut self) {
        self.input.clear();
        self.previous.clear();
        self.current.clear();
        self.cp = ResumeCp::new();
        self.saw_backtick = false;
        self.escape_scan = crate::DoubleEscapeScanner::new();
    }
}

impl Default for StreamRepairer {
    fn default() -> Self {
        Self::new()
    }
}

/// Length of the shared prefix of `a` and `b`, rounded to a character boundary.
fn common_prefix_len(a: &str, b: &str) -> usize {
    char_floor(b, shared_bytes(a.as_bytes(), b.as_bytes()))
}

/// [`common_prefix_len`] of the previous output and `new`, without touching
/// the stable prefix: the previous output was `new[..keep] + old_tail`,
/// except that a retrofitted NDJSON `[` may since have been inserted at
/// `shift` (old prefix byte `i >= shift` now sits at `new[i + 1]`).
fn resumed_prefix_len(new: &str, keep: usize, old_tail: &str, shift: Option<usize>) -> usize {
    let bytes = new.as_bytes();
    let mut len = shift.unwrap_or(keep);
    while len < keep && bytes[len + 1] == bytes[len] {
        len += 1;
    }
    if len == keep {
        len += shared_bytes(old_tail.as_bytes(), &bytes[keep..]);
    }
    char_floor(new, len)
}

/// Length of the shared prefix of two byte strings, compared a word at a time.
fn shared_bytes(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n && crate::swar::load_word(a, i) == crate::swar::load_word(b, i) {
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// The largest character boundary of `text` at or below `index`.
fn char_floor(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_after_a_comma_boundary() {
        let mut stream = StreamRepairer::new();
        stream.push("{\"a\": 1,").unwrap();
        assert!(stream.cp.valid, "consumed comma must checkpoint");
        assert_eq!(stream.cp.phase, parser::CpPhase::InContainer);
    }

    #[test]
    fn scalar_at_eof_does_not_checkpoint() {
        let mut stream = StreamRepairer::new();
        stream.push("12").unwrap();
        assert!(
            !stream.cp.valid,
            "growable token at EOF must not checkpoint"
        );
    }

    #[test]
    fn closed_container_checkpoints_even_at_eof() {
        let mut stream = StreamRepairer::new();
        stream.push("{\"a\": 1}").unwrap();
        assert!(stream.cp.valid, "a real closing brace must checkpoint");
    }

    #[test]
    fn truncated_string_at_eof_does_not_checkpoint() {
        // `"a#\n` at EOF is a growable string: a later chunk continues the
        // same value, it does not start a second top-level value.
        let mut stream = StreamRepairer::new();
        let out = String::from(stream.push("\"a#\n").expect("chunk0"));
        assert_eq!(out, "\"a#\"");
        assert!(
            !stream.cp.valid,
            "truncated string at EOF must not leave a resume point (cp={:?})",
            stream.cp
        );
        let full = String::from(stream.push("\0\0\0\x32").expect("chunk1"));
        assert_eq!(full, crate::repair("\"a#\n\0\0\0\x32").unwrap());
    }
}

#[cfg(test)]
mod fuzz_regressions2 {
    use super::*;

    #[test]
    fn unquoted_value_spaces_then_more_does_not_checkpoint() {
        let mut s = StreamRepairer::new();
        let p0 = "{\"a:\u{FFFD}\\u00e9\u{1a}  ";
        let o0 = String::from(s.push(p0).unwrap());
        assert_eq!(o0, crate::repair(p0).unwrap());
        assert!(
            !s.cp.valid,
            "truncated unquoted/str value at EOF must not checkpoint: {:?}",
            s.cp
        );
        let p1 = "\u{FFFD}\u{FFFD}";
        let mut acc = String::from(p0);
        acc.push_str(p1);
        let o1 = String::from(s.push(p1).unwrap());
        assert_eq!(o1, crate::repair(&acc).unwrap());
    }

    /// Fuzz crash input 2546; the stream target reads it as (steer, text).
    const CRASH_2546: &[u8] = &[
        34, 123, 97, 58, 255, 92, 117, 48, 48, 101, 57, 26, 32, 32, 92, 255, 255, 255,
    ];

    /// Replays fuzz bytes the way `fuzz/fuzz_targets/stream.rs` does: the
    /// first byte steers the chunk size (1..=32), the rest is the
    /// lossy-decoded document, split forward onto char boundaries.
    fn fuzz_chunks(data: &[u8]) -> alloc::vec::Vec<String> {
        let (&steer, rest) = data.split_first().expect("nonempty");
        let step = 1 + usize::from(steer % 32);
        let text = String::from_utf8_lossy(rest);
        let mut chunks = alloc::vec::Vec::new();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            while !text.is_char_boundary(end) {
                end += 1;
            }
            chunks.push(String::from(&text[start..end]));
            start = end;
        }
        chunks
    }

    #[test]
    fn crash2546_exact_bytes() {
        let mut s = StreamRepairer::new();
        let mut acc = String::new();
        for chunk in fuzz_chunks(CRASH_2546) {
            acc.push_str(&chunk);
            let got = s.push(&chunk).map(String::from);
            let want = crate::repair(&acc);
            if got.is_err() && want.is_err() {
                acc.truncate(acc.len() - chunk.len());
            } else {
                assert_eq!(got, want, "at prefix {acc:?} (chunk {chunk:?})");
            }
        }
    }

    /// Fuzz inputs whose every render ends on an EOF-dependent value: no
    /// push may leave a resume checkpoint behind (`must_repair`: every chunk
    /// is also expected to repair).
    #[test]
    fn eof_dependent_renders_never_checkpoint() {
        let af2a: &[u8] = &[99, 123, 34, 97, 34, 0, 34, 0, 58, 32, 50, 169, 41];
        let f245: &[u8] = &[
            99, 123, 34, 97, 34, 58, 32, 49, 38, 50, 169, 41, 123, 97, 10, 10, 10, 10, 10, 10, 10,
            86, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 236, 255, 255, 255, 10, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        for (data, must_repair) in [(CRASH_2546, true), (af2a, true), (f245, false)] {
            let mut s = StreamRepairer::new();
            for (n, chunk) in fuzz_chunks(data).iter().enumerate() {
                let pushed = s.push(chunk).map(String::from);
                assert!(
                    !must_repair || pushed.is_ok(),
                    "chunk {n} {chunk:?}: {pushed:?}"
                );
                assert!(!s.cp.valid, "after chunk {n} {chunk:?} cp={:?}", s.cp);
            }
        }
    }

    #[test]
    fn truncated_keyword_promotion_does_not_checkpoint() {
        // `nu` at EOF promotes to null; appending `ll` must re-repair as one
        // word, not resume after an invented null.
        let mut s = StreamRepairer::new();
        let o0 = String::from(s.push("nu").expect("chunk0"));
        assert_eq!(o0, crate::repair("nu").unwrap());
        assert!(
            !s.cp.valid,
            "promoted truncated keyword must not checkpoint: {:?}",
            s.cp
        );
        let o1 = String::from(s.push("ll").expect("chunk1"));
        assert_eq!(o1, crate::repair("null").unwrap());
    }
}
