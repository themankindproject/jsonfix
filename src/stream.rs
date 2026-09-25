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
    pub fn new() -> Self {
        Self::with_options(Options::all())
    }

    /// A stream repairer with explicit options, e.g. a partial policy.
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
    pub fn push(&mut self, chunk: &str) -> Result<&str, Error> {
        let before = self.input.len();
        self.note_chunk(chunk);
        self.input.push_str(chunk);
        if self.can_resume() {
            // Keep the stable prefix, drop the old tail, parse only the rest.
            let keep = self.cp.output_len.min(self.current.len());
            self.current.truncate(keep);
            let resumed = {
                let mut parser = parser::Parser::new_stream_resumed(
                    &self.input,
                    self.opts,
                    &mut self.current,
                    &mut self.cp,
                );
                parser.parse_resume()
            };
            if resumed.is_ok() {
                return Ok(&self.current);
            }
            // Fall through to a full render with the (possibly extended) cp.
        }
        self.current.clear();
        let result = if self.cp_enabled() {
            crate::repair_document_into(
                &self.input,
                self.opts,
                &mut self.current,
                Some(&mut self.cp),
            )
        } else {
            crate::repair_document_into(&self.input, self.opts, &mut self.current, None)
        };
        match result {
            Ok(()) => Ok(&self.current),
            Err(error) => {
                // The failed parse may have recorded checkpoints past
                // `before` (or mid-tail); they must not be resumed after
                // the rollback.
                self.cp.valid = false;
                self.escape_scan.rollback();
                self.input.truncate(before);
                self.current.clear();
                // Restore the last successful rendering of the rolled-back input.
                let _ =
                    crate::repair_document_into(&self.input, self.opts, &mut self.current, None);
                Err(error)
            }
        }
    }

    /// Appends `chunk` and returns only the changed tail.
    ///
    /// Apply it by truncating your buffer to [`Delta::keep`] and appending
    /// [`Delta::text`]. On error the chunk is rolled back, as in [`push`].
    ///
    /// [`push`]: Self::push
    pub fn push_delta(&mut self, chunk: &str) -> Result<Delta<'_>, Error> {
        let before = self.input.len();
        self.note_chunk(chunk);
        self.input.push_str(chunk);
        if self.can_resume() {
            // `previous` ← last output (diff base); `current` ← stable
            // prefix (copied out of it) + the resumed tail.
            core::mem::swap(&mut self.previous, &mut self.current);
            self.current.clear();
            let prefix_len = self.cp.output_len.min(self.previous.len());
            self.current.push_str(&self.previous[..prefix_len]);
            let resumed = {
                let mut parser = parser::Parser::new_stream_resumed(
                    &self.input,
                    self.opts,
                    &mut self.current,
                    &mut self.cp,
                );
                parser.parse_resume()
            };
            match resumed {
                Ok(()) => {
                    let keep = common_prefix_len(&self.previous, &self.current);
                    return Ok(Delta {
                        keep,
                        text: &self.current[keep..],
                    });
                }
                Err(error) => {
                    // Roll back: `previous` currently holds the last good
                    // output; swap it back into `current`.
                    self.cp.valid = false;
                    self.escape_scan.rollback();
                    self.current.clear();
                    core::mem::swap(&mut self.previous, &mut self.current);
                    self.input.truncate(before);
                    return Err(error);
                }
            }
        }
        // Full path: render the new output into `previous` (whose old content
        // is a stale diff base), then swap so `previous` becomes the last
        // output and `current` the new one. On error only `input` rolls back.
        self.previous.clear();
        let result = if self.cp_enabled() {
            crate::repair_document_into(
                &self.input,
                self.opts,
                &mut self.previous,
                Some(&mut self.cp),
            )
        } else {
            crate::repair_document_into(&self.input, self.opts, &mut self.previous, None)
        };
        match result {
            Ok(()) => {
                core::mem::swap(&mut self.previous, &mut self.current);
                let keep = common_prefix_len(&self.previous, &self.current);
                Ok(Delta {
                    keep,
                    text: &self.current[keep..],
                })
            }
            Err(error) => {
                self.cp.valid = false;
                self.escape_scan.rollback();
                self.input.truncate(before);
                self.previous.clear();
                Err(error)
            }
        }
    }

    /// Parses everything pushed so far.
    pub fn value(&self) -> Result<Value, Error> {
        crate::parse_with(&self.input, self.opts)
    }

    /// The raw input accumulated so far.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The repaired output produced by the last [`push`](Self::push).
    pub fn output(&self) -> &str {
        &self.current
    }

    /// Number of input bytes accumulated.
    pub fn len(&self) -> usize {
        self.input.len()
    }

    /// Whether nothing has been pushed yet.
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
    let (left, right) = (a.as_bytes(), b.as_bytes());
    let mut len = 0usize;
    while len < left.len() && len < right.len() && left[len] == right[len] {
        len += 1;
    }
    while len > 0 && !b.is_char_boundary(len) {
        len -= 1;
    }
    len
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

    #[test]
    fn crash2546_exact_bytes() {
        // Full fuzz input: first byte steers chunk size, rest is the document.
        let data: &[u8] = &[
            34, 123, 97, 58, 255, 92, 117, 48, 48, 101, 57, 26, 32, 32, 92, 255, 255, 255,
        ];
        let (&steer, rest) = data.split_first().expect("nonempty");
        let step = 1 + usize::from(steer % 32);
        let text = alloc::string::String::from_utf8_lossy(rest);
        let text = text.as_ref();
        let mut s = StreamRepairer::new();
        let mut acc = alloc::string::String::new();
        let mut start = 0;
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            if end < text.len() {
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
            }
            let chunk = &text[start..end];
            acc.push_str(chunk);
            let got = s.push(chunk).map(alloc::string::String::from);
            let want = crate::repair(&acc);
            if got.is_err() && want.is_err() {
                acc.truncate(acc.len() - chunk.len());
            } else {
                assert_eq!(got, want, "at prefix {acc:?} (chunk {chunk:?})");
            }
            start = end;
        }
    }

    #[test]
    fn probe_cp_after_each_chunk() {
        let data: &[u8] = &[
            34, 123, 97, 58, 255, 92, 117, 48, 48, 101, 57, 26, 32, 32, 92, 255, 255, 255,
        ];
        let (&steer, rest) = data.split_first().expect("nonempty");
        let step = 1 + usize::from(steer % 32);
        let text = String::from_utf8_lossy(rest);
        let text = text.as_ref();
        let mut s = StreamRepairer::new();
        let mut start = 0;
        let mut n = 0;
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            if end < text.len() {
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
            }
            let chunk = &text[start..end];
            let out = String::from(s.push(chunk).expect("push"));
            if s.cp.valid {
                panic!(
                    "after chunk {n} [{start}..{end}] {chunk:?} out={out:?} cp={:?}",
                    s.cp
                );
            }
            start = end;
            n += 1;
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

    #[test]
    fn probe_af2a_cp() {
        let data: &[u8] = &[99, 123, 34, 97, 34, 0, 34, 0, 58, 32, 50, 169, 41];
        let (&steer, rest) = data.split_first().expect("x");
        let step = 1 + usize::from(steer % 32u8);
        let text = String::from_utf8_lossy(rest);
        let text = text.as_ref();
        let mut s = StreamRepairer::new();
        let mut start = 0;
        let mut n = 0;
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            if end < text.len() {
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
            }
            let chunk = &text[start..end];
            let out = String::from(s.push(chunk).expect("p"));
            if s.cp.valid {
                panic!(
                    "chunk {n} [{start}..{end}] {chunk:?} out={out:?} cp={:?}",
                    s.cp
                );
            }
            start = end;
            n += 1;
        }
    }

    #[test]
    fn probe_24f5_cp() {
        let data: &[u8] = &[
            99, 123, 34, 97, 34, 58, 32, 49, 38, 50, 169, 41, 123, 97, 10, 10, 10, 10, 10, 10, 10,
            86, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 236, 255, 255, 255, 10, 0, 0, 0, 0,
            0, 0, 0, 0, 0,
        ];
        let (&steer, rest) = data.split_first().expect("x");
        let step = 1 + usize::from(steer as u16 % 32);
        let text = String::from_utf8_lossy(rest);
        let text = text.as_ref();
        let mut s = StreamRepairer::new();
        let mut start = 0;
        let mut n = 0;
        while start < text.len() {
            let mut end = (start + step).min(text.len());
            if end < text.len() {
                while end < text.len() && !text.is_char_boundary(end) {
                    end += 1;
                }
            }
            let chunk = &text[start..end];
            if s.push(chunk).is_err() {
                // rolled back; continue probing subsequent chunks
            }
            if s.cp.valid {
                panic!("after chunk {n} [{start}..{end}] cp={:?}", s.cp);
            }
            start = end;
            n += 1;
        }
    }
}
