//! The tolerant recursive-descent parser.
//!
//! It is deliberately forgiving: the grammar accepts anything a repair pass can
//! fix, and each repair is gated on a [`Repairs`] flag so strict parsing is the
//! same code path with those flags cleared.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::{Error, ErrorKind};
use crate::lexer::{Lexer, Spanned, Token};
use crate::options::{Allow, Options, Repairs};
use crate::value::{Number, Value, write_escaped};

/// Maximum container/group nesting depth accepted by the parser.
///
/// Input nested deeper than this fails with
/// [`ErrorKind::DepthLimitExceeded`](crate::ErrorKind::DepthLimitExceeded)
/// instead of risking a stack overflow, no matter which repair passes are
/// enabled. The limit is generous for real-world JSON (which rarely exceeds
/// double digits) while keeping worst-case stack use around half a
/// megabyte even in unoptimized debug builds on small-stack threads.
pub const MAX_NESTING_DEPTH: usize = 256;

/// Frames pre-reserved on the container stack. Real-world JSON rarely nests
/// past this, so one upfront allocation covers the common case and skips the
/// early doublings a `Vec::new()` would pay while descending.
const FRAME_PREALLOC: usize = 16;

/// A payload-free view of a token, cheap to compare and copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Colon,
    Comma,
    Semicolon,
    Plus,
    Ellipsis,
    OpenParen,
    CloseParen,
    Str { truncated: bool },
    Num { truncated: bool },
    Bool,
    Null,
    Undefined,
    Word { truncated: bool },
}

impl Tag {
    fn of(token: &Token<'_>) -> Self {
        match token {
            Token::OpenBrace => Tag::OpenBrace,
            Token::CloseBrace => Tag::CloseBrace,
            Token::OpenBracket => Tag::OpenBracket,
            Token::CloseBracket => Tag::CloseBracket,
            Token::Colon => Tag::Colon,
            Token::Comma => Tag::Comma,
            Token::Semicolon => Tag::Semicolon,
            Token::Plus => Tag::Plus,
            Token::Ellipsis => Tag::Ellipsis,
            Token::OpenParen => Tag::OpenParen,
            Token::CloseParen => Tag::CloseParen,
            Token::Str { truncated, .. } => Tag::Str {
                truncated: *truncated,
            },
            Token::Num { truncated, .. } => Tag::Num {
                truncated: *truncated,
            },
            Token::Bool(_) => Tag::Bool,
            Token::Null => Tag::Null,
            Token::Undefined => Tag::Undefined,
            Token::Word { truncated, .. } => Tag::Word {
                truncated: *truncated,
            },
        }
    }

    /// Whether this token can begin a JSON value.
    fn starts_value(self) -> bool {
        matches!(
            self,
            Tag::OpenBrace
                | Tag::OpenBracket
                | Tag::Str { .. }
                | Tag::Num { .. }
                | Tag::Bool
                | Tag::Null
                | Tag::Undefined
                | Tag::Word { .. }
                | Tag::OpenParen
        )
    }

    /// The character this token reports in `UnexpectedCharacter` errors.
    ///
    /// Only meaningful for punctuation tags; value tags are never reported
    /// this way and return NUL.
    fn punct_char(self) -> char {
        match self {
            Tag::OpenBrace => '{',
            Tag::CloseBrace => '}',
            Tag::OpenBracket => '[',
            Tag::CloseBracket => ']',
            Tag::Colon => ':',
            Tag::Comma => ',',
            Tag::Semicolon => ';',
            Tag::Plus => '+',
            Tag::Ellipsis => '.',
            Tag::OpenParen => '(',
            Tag::CloseParen => ')',
            _ => '\0',
        }
    }
}

/// What kind of container an open parse frame represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerKind {
    Object,
    Array,
    /// Parenthesised groups never create checkpoints (JSONP is rare in
    /// streams; their contents are parsed fresh).
    Group,
}

/// One open container: its kind plus the loop locals needed to resume it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Frame {
    kind: ContainerKind,
    first: bool,
    comma_pending: bool,
}

/// Top-level driver state carried across a resume.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TopState {
    /// Absolute output offset where the first value's rendering starts
    /// (stream mode retrofits `[` here if more values follow).
    first_pos: usize,
    prose_first: bool,
    /// Completed top-level values (excludes an in-progress value when the
    /// checkpoint sits inside a container).
    count: usize,
}

/// Where a resume should continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CpPhase {
    /// Inside the innermost container's member loop.
    InContainer,
    /// A container just closed; finish the parent's value-arm first.
    ContinueParent,
    /// No container open: continue the top-level driver.
    TopContinue,
}

/// A stable point from which a later parse can resume.
///
/// Only positions where every preceding token was lexed against a real
/// boundary (comma/closer/etc. — never "growable at EOF") are recorded, so
/// appending input can never change how the prefix was interpreted.
#[derive(Debug, Clone)]
pub(crate) struct ResumeCp {
    pub(crate) valid: bool,
    pub(crate) input_pos: usize,
    pub(crate) output_len: usize,
    pub(crate) phase: CpPhase,
    pub(crate) frames: Vec<Frame>,
    pub(crate) top: TopState,
}

impl ResumeCp {
    pub(crate) fn new() -> Self {
        Self {
            valid: false,
            input_pos: 0,
            output_len: 0,
            phase: CpPhase::TopContinue,
            frames: Vec::new(),
            top: TopState::default(),
        }
    }
}

/// The tolerant parser: `Lexer` plus one token of lookahead.
///
/// In *tree* mode (`out: None`, used by `parse`/`validate`/`parse_partial`)
/// values are assembled into a [`Value`]. In *stream* mode (`out: Some`,
/// used by `repair`) every value is serialized straight into the output
/// buffer as it is recognized — no tree, no second walk — which is only
/// sound when no value can be dropped mid-parse, i.e. under [`Allow::ALL`].
/// Stream mode additionally records [`ResumeCp`] checkpoints so
/// `StreamRepairer` can reparse only the newly appended tail.
pub(crate) struct Parser<'a, 'b> {
    lexer: Lexer<'a>,
    /// One token of lookahead plus its precomputed [`Tag`] — the tag is
    /// derived once at fill time instead of on every `peek_tag` call
    /// (the object loop peeks the same token many times per member).
    peeked: Option<(Spanned<'a>, Tag)>,
    opts: Options,
    /// Open containers (also the depth counter for [`MAX_NESTING_DEPTH`]).
    frames: Vec<Frame>,
    /// High-water mark of `frames.len()`: the deepest the renderer could have
    /// nested. Groups count here but emit no brackets, so this can only
    /// over-state the rendered structural depth — a safe basis for skipping
    /// the post-render depth scan.
    max_depth: usize,
    top: TopState,
    out: Option<&'b mut String>,
    /// Where checkpoints are recorded (stream mode with a live renderer).
    cp_slot: Option<&'b mut ResumeCp>,
    /// Set only when constructed by [`Parser::new_stream_resumed`].
    resume_phase: Option<CpPhase>,
    /// Set once a truncated scalar is accepted: later structure (especially
    /// commas after a two-pass string end) depends on where the input
    /// happens to end, so no further stream checkpoints are safe.
    eof_dependent: bool,
    /// Output offset of a retrofitted NDJSON `[` that is currently inserted
    /// (the only write that lands *before* bytes already rendered).
    shifted_at: Option<usize>,
    /// Tree mode: completed values waiting for their parent to collect them
    /// (top-level values stay here until `parse_document` returns them).
    values: Vec<Value>,
}

impl<'a, 'b> Parser<'a, 'b> {
    pub(crate) fn new(input: &'a str, opts: Options) -> Self {
        Self::with_sink(input, opts, None, None)
    }

    /// A stream-mode parser that appends repaired JSON to `out` and records
    /// resume checkpoints into `cp` (when provided).
    pub(crate) fn new_stream(
        input: &'a str,
        opts: Options,
        out: &'b mut String,
        cp: Option<&'b mut ResumeCp>,
    ) -> Self {
        Self::with_sink(input, opts, Some(out), cp)
    }

    fn with_sink(
        input: &'a str,
        opts: Options,
        out: Option<&'b mut String>,
        cp_slot: Option<&'b mut ResumeCp>,
    ) -> Self {
        Self {
            lexer: Lexer::new(input, opts),
            peeked: None,
            opts,
            frames: Vec::with_capacity(FRAME_PREALLOC),
            max_depth: 0,
            top: TopState::default(),
            out,
            cp_slot,
            resume_phase: None,
            eof_dependent: false,
            shifted_at: None,
            values: Vec::new(),
        }
    }

    /// A stream-mode parser positioned at a saved checkpoint: the lexer
    /// starts at `cp.input_pos`, frames/top are restored, and new saves go
    /// back into the same `cp`.
    pub(crate) fn new_stream_resumed(
        input: &'a str,
        opts: Options,
        out: &'b mut String,
        cp: &'b mut ResumeCp,
    ) -> Self {
        let (phase, input_pos, top) = (cp.phase, cp.input_pos, cp.top);
        let mut parser = Self::with_sink(input, opts, Some(out), Some(cp));
        if let Some(cp) = parser.cp_slot.as_deref() {
            parser.frames.extend_from_slice(&cp.frames);
        }
        parser.lexer.set_pos(input_pos);
        parser.max_depth = parser.frames.len();
        parser.top = top;
        parser.resume_phase = Some(phase);
        parser
    }

    /// The deepest `frames.len()` reached so far (see the field docs).
    pub(crate) fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Where a retrofitted NDJSON `[` currently sits in the output, if one was
    /// inserted by this parse: bytes before it are untouched, bytes after it
    /// shifted right by one.
    pub(crate) fn shifted_at(&self) -> Option<usize> {
        self.shifted_at
    }

    /// Whether values are serialized directly instead of built into a tree.
    fn streaming(&self) -> bool {
        self.out.is_some()
    }

    /// The output buffer, when in stream mode.
    ///
    /// Borrows must stay short: never hold the result across another
    /// `&mut self` call (peek/take/parse).
    fn emit(&mut self) -> Option<&mut String> {
        self.out.as_deref_mut()
    }

    /// Invalidates checkpoints whose `output_len` sits at or after `first_pos`
    /// — the byte range the retroactive NDJSON `[` insert/remove rewrote.
    ///
    /// Shifting the length is not enough: a `TopContinue` saved *before* the
    /// bracket still has `count == 1`, so resume would truncate to `[1…` and
    /// then insert a second `[`. Dropping the checkpoint forces a full
    /// re-render (or a fresh save after the wrap is in place).
    fn invalidate_cp_from(&mut self, first_pos: usize) {
        let Some(slot) = self.cp_slot.as_deref_mut() else {
            return;
        };
        if slot.valid && slot.output_len >= first_pos {
            slot.valid = false;
        }
    }

    /// Records a resume checkpoint (stream mode only).
    ///
    /// `stable` must be true when every token before the position was lexed
    /// against a real input boundary (a consumed comma/closer); positions at
    /// EOF after growable tokens (numbers, words, strings) are skipped so a
    /// later append can never reinterpret the prefix.
    ///
    /// The recorded frame list follows `phase`: [`CpPhase::ContinueParent`]
    /// excludes the container that just closed (its parent chain only).
    fn save_cp(&mut self, phase: CpPhase, stable: bool) {
        // `stable == false` means this boundary is *not* safe to resume from
        // (growable scalar, two-pass string end, EOF-invented null, …).
        // Callers already fold `at_end` into the flag where relevant — never
        // save an unstable checkpoint just because input happens to follow.
        // Cheap exits first: `repair` (no slot) never records checkpoints.
        if !stable || self.eof_dependent || self.cp_slot.is_none() {
            return;
        }
        let Some(out_len) = self.out.as_ref().map(|out| out.len()) else {
            return;
        };
        let frames_len = match phase {
            CpPhase::ContinueParent => self.frames.len().saturating_sub(1),
            _ => self.frames.len(),
        };
        let frames = &self.frames[..frames_len];
        if frames.iter().any(|f| f.kind == ContainerKind::Group) {
            return;
        }
        let Some(slot) = self.cp_slot.as_deref_mut() else {
            return;
        };
        slot.valid = true;
        slot.input_pos = self.lexer.pos();
        slot.output_len = out_len;
        slot.phase = phase;
        slot.frames.clear();
        slot.frames.extend_from_slice(frames);
        slot.top = self.top;
    }

    /// Whether every repair pass is disabled.
    fn strict(&self) -> bool {
        self.opts.repairs == Repairs::NONE
    }

    /// Enters one container; fails once [`MAX_NESTING_DEPTH`] is reached.
    ///
    /// Once a top-level value has completed, any later top-level value is
    /// being wrapped into the NDJSON `[...]` array, which renders one extra
    /// level `frames` never holds. That level is reserved here so the error
    /// fires at the offending opener — the same byte in tree and stream mode —
    /// instead of surfacing later from a post-render scan with no useful
    /// position.
    fn enter(&mut self, kind: ContainerKind) -> Result<(), Error> {
        let wrap_level = usize::from(self.top.count >= 1);
        if self.frames.len() + wrap_level >= MAX_NESTING_DEPTH {
            return Err(Error::new(ErrorKind::DepthLimitExceeded, self.position()));
        }
        self.frames.push(Frame {
            kind,
            first: true,
            comma_pending: false,
        });
        self.max_depth = self.max_depth.max(self.frames.len());
        Ok(())
    }

    /// Leaves one container. Must be paired with every successful `enter`.
    fn leave(&mut self) {
        self.frames.pop();
    }

    fn peek_newline(&mut self) -> Result<bool, Error> {
        self.fill(false)?;
        Ok(self
            .peeked
            .as_ref()
            .is_some_and(|(spanned, _)| spanned.newline_before))
    }

    /// Parses the whole input into one value, wrapping several top-level values
    /// into an array when NDJSON repair is enabled. Tree mode only.
    pub(crate) fn parse_document(&mut self) -> Result<Value, Error> {
        self.parse_top()?;
        if self.top.count == 1 {
            return Ok(self.pop_value());
        }
        // Wrapping several top-level values in one array adds a level that
        // `enter()` never counted (the deepest value may already use the full
        // budget). Reject before emitting a tree whose serialization fails
        // `validate` / `MAX_NESTING_DEPTH`.
        if self.values.iter().map(value_depth).max().unwrap_or(0) >= MAX_NESTING_DEPTH {
            return Err(Error::new(ErrorKind::DepthLimitExceeded, self.lexer.pos()));
        }
        Ok(Value::Array(core::mem::take(&mut self.values)))
    }

    /// Parses the whole document in stream mode: bytes are appended to
    /// `self.out` and no tree is built.
    pub(crate) fn parse_document_stream(&mut self) -> Result<(), Error> {
        self.parse_top()
    }

    /// Continues a stream parse from a saved checkpoint (see
    /// [`Parser::new_stream_resumed`]). Rebuilds the suspended call chain:
    /// innermost container loop → parent value-arms → top-level driver.
    pub(crate) fn parse_resume(&mut self) -> Result<(), Error> {
        // ContinueParent: the innermost frame is already finished — apply its
        // parent's value-arm tail before running that loop.
        let mut need_tail = match self.resume_phase.unwrap_or(CpPhase::TopContinue) {
            CpPhase::TopContinue => return self.top_continue(),
            CpPhase::InContainer => false,
            CpPhase::ContinueParent => true,
        };
        while let Some(frame) = self.frames.last_mut() {
            if need_tail {
                frame.first = false;
                frame.comma_pending = false;
            }
            need_tail = true;
            let kind = frame.kind;
            match kind {
                ContainerKind::Object => self.run_object_loop()?,
                ContainerKind::Array => self.run_array_loop()?,
                // Groups never create checkpoints.
                ContainerKind::Group => {
                    return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
                }
            };
            self.leave();
        }
        // The suspended top-level value is complete.
        self.top.count += 1;
        self.top_continue()
    }

    /// Shared top-level driver for both modes. Completed top-level values are
    /// counted in `top.count` (used for NDJSON `[...]` wrapping); tree mode
    /// also keeps them on the value stack.
    fn parse_top(&mut self) -> Result<(), Error> {
        let position = self.position();
        // A first value that is a multi-word bare string is prose (the
        // document contract: prose must go through `extract`, not wrap into
        // an NDJSON array). Single words like `a\nb` still wrap.
        self.peek_tag(false)?;
        let prose_first = matches!(
            self.peeked.as_ref(),
            Some((spanned, _))
                if matches!(
                    &spanned.token,
                    Token::Word { text, .. } if text.chars().any(|c| c.is_whitespace())
                )
        );
        // Byte offset where the first value's output starts; stream mode
        // retrofits `[` here if a second top-level value appears.
        let first_pos = self.out.as_ref().map_or(0, |out| out.len());
        self.top = TopState {
            first_pos,
            prose_first,
            count: 0,
        };
        // No checkpoint after a top-level value: a scalar at EOF may still
        // grow (`"a#\n` stops at the newline with pos < len), and a closed
        // container already saved its own ContinueParent boundary.
        if !self.parse_value()? {
            return Err(Error::new(ErrorKind::NoValueFound, position));
        }
        self.top.count = 1;
        self.top_continue()
    }

    /// Pops the value the last successful `parse_value` pushed (tree mode).
    fn pop_value(&mut self) -> Value {
        self.values
            .pop()
            .expect("tree mode pushes one value per parsed value")
    }

    /// The top-level loop after the first value has been parsed (also the
    /// resume entry for [`CpPhase::TopContinue`]).
    fn top_continue(&mut self) -> Result<(), Error> {
        let first_pos = self.top.first_pos;
        let prose_first = self.top.prose_first;
        loop {
            // Between top-level values only separators and stray closers appear.
            let mut pending_comma = false;
            loop {
                match self.peek_tag(false)? {
                    Some(Tag::Comma) => {
                        self.take();
                        if pending_comma && self.strict() {
                            return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                        }
                        pending_comma = true;
                    }
                    // Between top-level values only separators, stray closers,
                    // and punctuation noise appear. The reference throws on
                    // anything but separators/newlines/values; repair mode
                    // drops the noise. Each token reports its own character.
                    Some(
                        tag @ (Tag::Semicolon
                        | Tag::Ellipsis
                        | Tag::Plus
                        | Tag::Colon
                        | Tag::CloseParen
                        | Tag::CloseBrace
                        | Tag::CloseBracket),
                    ) => {
                        if self.strict() {
                            return Err(self.unexpected(tag));
                        }
                        self.take();
                    }
                    _ => break,
                }
            }
            let next_is_value = matches!(self.peek_tag(false)?, Some(tag) if tag.starts_value());
            if !next_is_value {
                if pending_comma && self.strict() {
                    return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                }
                break;
            }
            // A second top-level value needs a comma or newline before it
            // (reference: newline/comma-separated top-level values, including
            // bare words like `a\nb`, wrap into an array) — unless the first
            // value was prose.
            let separated = pending_comma || self.peek_newline()?;
            if !separated || prose_first || !self.opts.repairs(Repairs::NDJSON) {
                return Err(Error::new(ErrorKind::TrailingValue, self.position()));
            }
            // Stream mode: retrofit `[` before the first value and join with
            // `, ` — byte-identical to `Value::write_to`'s array rendering.
            // If the second value parses to `None` (dropped incomplete value),
            // undo both the bracket and the separator: a bare `[` would not be
            // valid JSON. `insert` shifts the buffer, so rollback is explicit
            // rather than a truncate to a pre-insert length.
            let base = self.emit().map(|out| out.len());
            let wraps_first = self.top.count == 1;
            let inserted_bracket = self.streaming() && wraps_first;
            if wraps_first {
                // The wrap adds one nesting level that `enter()` never counted
                // for the first value (it may already use the full budget).
                // Measure only this document's rendering (`first_pos..`): the
                // buffer may hold caller-provided text before it.
                let first_depth = match self.out.as_deref() {
                    Some(out) => structural_depth(&out[first_pos..]),
                    None => value_depth(&self.values[0]),
                };
                if first_depth >= MAX_NESTING_DEPTH {
                    return Err(Error::new(ErrorKind::DepthLimitExceeded, self.position()));
                }
            }
            if let Some(out) = self.emit() {
                if inserted_bracket {
                    out.insert(first_pos, '[');
                }
                out.push_str(", ");
            }
            if inserted_bracket {
                self.shifted_at = Some(first_pos);
                self.invalidate_cp_from(first_pos);
            }
            // The value tag is already peeked (starts_value check above).
            match self.parse_value()? {
                true => self.top.count += 1,
                false => {
                    if let (Some(out), Some(base)) = (self.emit(), base) {
                        // Drop `, ` plus anything `parse_value` wrote, keep
                        // the bracket (if any) for the explicit remove below.
                        out.truncate(base + usize::from(inserted_bracket));
                        if inserted_bracket {
                            out.remove(first_pos);
                        }
                    }
                    if inserted_bracket {
                        self.shifted_at = None;
                        self.invalidate_cp_from(first_pos);
                    }
                    break;
                }
            }
        }
        if self.top.count > 1 {
            if let Some(out) = self.emit() {
                out.push(']');
            }
        }
        Ok(())
    }

    /// The byte offset of the token under the cursor.
    fn position(&self) -> usize {
        match &self.peeked {
            Some((spanned, _)) => spanned.start,
            None => self.lexer.pos(),
        }
    }

    fn fill(&mut self, key_position: bool) -> Result<(), Error> {
        if self.peeked.is_none() {
            self.lexer.set_in_container(!self.frames.is_empty());
            if let Some(spanned) = self.lexer.next_token(key_position)? {
                let tag = Tag::of(&spanned.token);
                // `get_or_insert` (the slot is known empty) skips the drop of
                // a previous token that a plain assignment would emit.
                self.peeked.get_or_insert((spanned, tag));
            }
        }
        Ok(())
    }

    fn peek_tag(&mut self, key_position: bool) -> Result<Option<Tag>, Error> {
        self.fill(key_position)?;
        Ok(self.peeked.as_ref().map(|(_, tag)| *tag))
    }

    fn take(&mut self) -> Spanned<'a> {
        self.peeked
            .take()
            .map(|(spanned, _)| spanned)
            .expect("peek was called before take")
    }
}

impl Parser<'_, '_> {
    /// Parses one value: renders it (stream mode) or pushes it onto the value
    /// stack (tree mode).
    ///
    /// Returns `false` when there is no value at the cursor, and also when the
    /// value was incomplete and the partial policy (see [`Allow`]) dropped it.
    fn parse_value(&mut self) -> Result<bool, Error> {
        // Stray separators before a value are dropped (but not in strict mode).
        while let Some(tag @ (Tag::Ellipsis | Tag::Semicolon | Tag::Plus)) = self.peek_tag(false)? {
            if self.strict() {
                return Err(self.unexpected(tag));
            }
            self.take();
        }
        let Some(tag) = self.peek_tag(false)? else {
            return Ok(false);
        };
        match tag {
            Tag::OpenBrace => self.parse_object(),
            Tag::OpenBracket => self.parse_array(),
            Tag::OpenParen => self.parse_group(),
            Tag::Str { truncated } => self.parse_string_value(truncated),
            Tag::Num { truncated } => {
                let Token::Num { text, .. } = self.take().token else {
                    return Ok(false);
                };
                if truncated && !self.opts.allows(Allow::NUM) {
                    return Ok(false);
                }
                self.eof_dependent |= truncated;
                // The number text is already valid JSON.
                match self.out.as_deref_mut() {
                    Some(out) => out.push_str(&text),
                    None => self
                        .values
                        .push(Value::Number(Number::from_normalized(text.into_owned()))),
                }
                Ok(true)
            }
            Tag::Bool => {
                let Token::Bool(value) = self.take().token else {
                    return Ok(false);
                };
                Ok(self.produce(if value { "true" } else { "false" }, Value::Bool(value)))
            }
            Tag::Null | Tag::Undefined => {
                self.take();
                Ok(self.produce("null", Value::Null))
            }
            Tag::Word { .. } => self.parse_word_value(),
            Tag::CloseBrace
            | Tag::CloseBracket
            | Tag::CloseParen
            | Tag::Colon
            | Tag::Comma
            | Tag::Semicolon
            | Tag::Plus
            | Tag::Ellipsis => Ok(false),
        }
    }

    /// Produces a non-string scalar: its JSON text (stream) or `value` (tree).
    fn produce(&mut self, json: &str, value: Value) -> bool {
        match self.out.as_deref_mut() {
            Some(out) => out.push_str(json),
            None => self.values.push(value),
        }
        true
    }

    /// Produces a string value: escaped into the output (stream) or pushed as
    /// a tree node.
    fn produce_str(&mut self, text: Cow<'_, str>) -> bool {
        match self.out.as_deref_mut() {
            Some(out) => write_escaped(out, &text),
            None => self.values.push(Value::String(text.into_owned())),
        }
        true
    }

    /// A parenthesised value, as produced by JavaScript-ish serializers.
    fn parse_group(&mut self) -> Result<bool, Error> {
        self.enter(ContainerKind::Group)?;
        let result = self.parse_group_inner();
        self.leave();
        result
    }

    fn parse_group_inner(&mut self) -> Result<bool, Error> {
        if self.strict() {
            // Parenthesised values are not JSON: reject them in strict mode
            // (both `(1)` and a cut-off `(1`), like any other repair pass.
            return Err(Error::new(
                ErrorKind::UnexpectedCharacter('('),
                self.position(),
            ));
        }
        self.take();
        let mut inner = self.parse_value()?;
        loop {
            match self.peek_tag(false)? {
                Some(Tag::CloseParen) => {
                    self.take();
                    break;
                }
                Some(Tag::Semicolon) => {
                    self.take();
                }
                Some(tag) if tag.starts_value() && !inner => {
                    inner = self.parse_value()?;
                }
                _ => break,
            }
        }
        Ok(inner)
    }

    fn parse_string_value(&mut self, truncated: bool) -> Result<bool, Error> {
        let spanned = self.take();
        let Token::Str { text, .. } = spanned.token else {
            return Ok(false);
        };
        // A two-pass / EOF string end: following separators are not stable.
        if !self.accept_truncated(truncated, spanned.start)? {
            return Ok(false);
        }
        // `"long text" + "more text"`: string concatenation across a line break.
        // The lookahead only lexes when needed (context-free `+`, then a
        // string after it): peeking unconditionally would cache the next
        // token in value context and swallow a following object key
        // (`"John"\n lastName: …`). A `+` not followed by a string is left
        // in the buffer for the caller's noise handling — the reference
        // drops it the same way. `text` stays borrowed until a real `+`
        // forces an owned buffer via `to_mut`.
        let mut text = text;
        loop {
            if self.peeked.is_none() && self.lexer.peek_significant_char() != Some('+') {
                break;
            }
            if !matches!(self.peek_tag(false)?, Some(Tag::Plus)) {
                break;
            }
            if !self.opts.repairs(Repairs::CONCATENATION) {
                return Err(Error::new(
                    ErrorKind::UnexpectedCharacter('+'),
                    self.position(),
                ));
            }
            // Only lex the token after `+` when it looks like a string;
            // otherwise leave the `Plus` for the collection/top-level parser.
            match self.lexer.peek_significant_char() {
                Some(c) if crate::chars::is_quote(c) || c == '&' || c == '\\' => {}
                _ => break,
            }
            self.take();
            match self.peek_tag(false)? {
                Some(Tag::Str {
                    truncated: segment_truncated,
                }) => {
                    let spanned = self.take();
                    let segment_start = spanned.start;
                    if let Token::Str { text: next, .. } = spanned.token {
                        // A cut-off segment after `+` is a cut-off string and
                        // follows the same policy as a lone one: an error
                        // without TRUNCATION, the whole (incomplete) value
                        // dropped without `Allow::STR`, and no stable stream
                        // checkpoint after it.
                        if !self.accept_truncated(segment_truncated, segment_start)? {
                            return Ok(false);
                        }
                        text.to_mut().push_str(&next);
                    }
                }
                _ => break,
            }
        }
        Ok(self.produce_str(text))
    }

    /// The policy for a string-like token cut off at end of input: an error
    /// without `TRUNCATION`, dropped (`false`) without `Allow::STR`, and
    /// otherwise kept — after which no stream checkpoint is stable. Complete
    /// tokens always pass.
    fn accept_truncated(&mut self, truncated: bool, start: usize) -> Result<bool, Error> {
        if !truncated {
            return Ok(true);
        }
        if !self.opts.repairs(Repairs::TRUNCATION) {
            return Err(Error::new(ErrorKind::UnexpectedEnd, start));
        }
        if !self.opts.allows(Allow::STR) {
            return Ok(false);
        }
        self.eof_dependent = true;
        Ok(true)
    }

    /// A bare word: either a function-call wrapper or an unquoted string.
    fn parse_word_value(&mut self) -> Result<bool, Error> {
        let spanned = self.take();
        let start = spanned.start;
        let Token::Word { text, truncated } = spanned.token else {
            return Ok(false);
        };
        // Same lookahead discipline as parse_string_value: only lex `(` —
        // any other next char must stay unlexed so a following object key
        // is scanned with key_position.
        let next_is_call =
            if self.peeked.is_none() && self.lexer.peek_significant_char() != Some('(') {
                false
            } else {
                matches!(self.peek_tag(false)?, Some(Tag::OpenParen))
            };
        if next_is_call {
            if !self.opts.repairs(Repairs::CALLS) {
                return Err(Error::new(ErrorKind::UnexpectedCharacter('('), start));
            }
            // `NumberLong(2)` and `cb({...})` both reduce to the inner value.
            return self.parse_group();
        }
        if !self.opts.repairs(Repairs::UNQUOTED) {
            return Err(Error::new(ErrorKind::UnquotedValue, start));
        }
        // A word cut off by truncation is treated like a cut-off string.
        if !self.accept_truncated(truncated, start)? {
            return Ok(false);
        }
        Ok(self.produce_str(Cow::Borrowed(text)))
    }
}

/// What the parser found where an object key belongs.
enum KeyStep<'a> {
    /// A usable key, plus whether it was cut off at end of input.
    ///
    /// Keys borrow the input (or a static keyword) unless a repair decoded
    /// them into an owned buffer.
    Key(Cow<'a, str>, bool),
    /// Nothing key-like: one token was consumed, keep looking.
    Skip,
    /// End of input: stop the object.
    Stop,
}

impl<'a> Parser<'a, '_> {
    fn parse_key(&mut self) -> Result<KeyStep<'a>, Error> {
        let Some(tag) = self.peek_tag(true)? else {
            return Ok(KeyStep::Stop);
        };
        if !matches!(
            tag,
            Tag::Str { .. }
                | Tag::Word { .. }
                | Tag::Num { .. }
                | Tag::Bool
                | Tag::Null
                | Tag::Undefined
        ) {
            if self.strict() {
                return Err(Error::new(ErrorKind::ExpectedObjectKey, self.position()));
            }
            self.take();
            return Ok(KeyStep::Skip);
        }
        let spanned = self.take();
        Ok(match spanned.token {
            Token::Str { text, truncated } | Token::Num { text, truncated } => {
                KeyStep::Key(text, truncated)
            }
            Token::Word { text, truncated } => {
                if !self.opts.repairs(Repairs::UNQUOTED) {
                    return Err(Error::new(ErrorKind::UnquotedValue, spanned.start));
                }
                KeyStep::Key(Cow::Borrowed(text), truncated)
            }
            Token::Bool(value) => {
                KeyStep::Key(Cow::Borrowed(if value { "true" } else { "false" }), false)
            }
            _ => KeyStep::Key(Cow::Borrowed("null"), false),
        })
    }

    /// Tree: push a null-valued member. Stream: emit `, ` + `key: null`.
    /// Updates the frame (`first`/`comma_pending` become false) and records
    /// a member-boundary checkpoint.
    ///
    /// `stable_tail` marks emissions whose last token was already bounded by
    /// a delimiter in the input (safe even at EOF); otherwise the checkpoint
    /// is only kept when input follows.
    fn emit_null_member(
        &mut self,
        members: &mut Vec<(String, Value)>,
        key: Cow<'_, str>,
        stable_tail: bool,
    ) {
        let idx = self.frames.len() - 1;
        let first = self.frames[idx].first;
        if let Some(out) = self.emit() {
            if !first {
                out.push_str(", ");
            }
            write_escaped(out, &key);
            out.push_str(": null");
        } else {
            members.push((key.into_owned(), Value::Null));
        }
        self.frames[idx].first = false;
        self.frames[idx].comma_pending = false;
        let stable = stable_tail || !self.lexer.at_effective_end();
        self.save_cp(CpPhase::InContainer, stable);
    }

    fn parse_object(&mut self) -> Result<bool, Error> {
        self.open_container(ContainerKind::Object, '{')?;
        let result = self.run_object_loop();
        self.leave();
        result
    }

    fn parse_array(&mut self) -> Result<bool, Error> {
        self.open_container(ContainerKind::Array, '[')?;
        let result = self.run_array_loop();
        self.leave();
        result
    }

    /// Enters a container and consumes its opener. Fresh parses write the
    /// opener here; a resume finds it already in the output prefix (the loops
    /// are entered directly on resume).
    fn open_container(&mut self, kind: ContainerKind, opener: char) -> Result<(), Error> {
        self.enter(kind)?;
        self.take();
        if let Some(out) = self.emit() {
            out.push(opener);
        }
        Ok(())
    }

    /// Handles what may sit between members/elements — separators, closers,
    /// and noise — for both container loops. `own` is the closer that
    /// matches the container; strict mode rejects the other one.
    fn separator_step(&mut self, idx: usize, key_position: bool, own: Tag) -> Result<Step, Error> {
        let Frame {
            first,
            comma_pending,
            ..
        } = self.frames[idx];
        match self.peek_tag(key_position)? {
            None => return Ok(Step::End),
            Some(tag @ (Tag::CloseBrace | Tag::CloseBracket)) => {
                if tag != own && self.strict() {
                    return Err(self.unexpected(tag));
                }
                if comma_pending && self.strict() {
                    return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                }
                self.take();
                return Ok(Step::Closed);
            }
            Some(Tag::Comma) => {
                self.take();
                // A leading or doubled comma is dropped when repairing.
                if (first || comma_pending) && self.strict() {
                    return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                }
                self.frames[idx].comma_pending = true;
                // A consumed comma is a stable boundary even at EOF.
                self.save_cp(CpPhase::InContainer, true);
                return Ok(Step::Again);
            }
            Some(tag @ (Tag::Semicolon | Tag::Ellipsis | Tag::Colon | Tag::Plus)) => {
                if self.strict() {
                    return Err(self.unexpected(tag));
                }
                self.take();
                return Ok(Step::Again);
            }
            _ => {}
        }
        if !first && !comma_pending && self.strict() {
            return Err(Error::new(ErrorKind::ExpectedComma, self.position()));
        }
        Ok(Step::Member)
    }

    /// `UnexpectedCharacter` for a punctuation token at the cursor.
    fn unexpected(&self, tag: Tag) -> Error {
        Error::new(
            ErrorKind::UnexpectedCharacter(tag.punct_char()),
            self.position(),
        )
    }

    /// Stream mode: writes the `, ` separator (unless this is the first
    /// member) and returns the rollback mark for a value that does not
    /// materialize. Tree mode: `None`.
    fn separator_mark(&mut self, idx: usize) -> Option<usize> {
        let first = self.frames[idx].first;
        let out = self.out.as_deref_mut()?;
        let mark = out.len();
        if !first {
            out.push_str(", ");
        }
        Some(mark)
    }

    /// Undoes a stream-mode member rendering back to `mark`.
    fn rollback(&mut self, mark: Option<usize>) {
        if let (Some(out), Some(mark)) = (self.out.as_deref_mut(), mark) {
            out.truncate(mark);
        }
    }

    /// Marks a member/element as done for the separator rules.
    fn member_done(&mut self, idx: usize) {
        self.frames[idx].first = false;
        self.frames[idx].comma_pending = false;
    }

    /// The member loop. The caller has consumed `{` (fresh) or the cursor
    /// sits just past it (resume); the frame for this object is already on
    /// `self.frames`.
    fn run_object_loop(&mut self) -> Result<bool, Error> {
        let idx = self.frames.len() - 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        let closed = loop {
            match self.separator_step(idx, true, Tag::CloseBrace)? {
                Step::Again => continue,
                Step::Closed => break true,
                Step::End => break false,
                Step::Member => {}
            }
            // A structural value at key position ends this object; the
            // parent decides what to do with it (`[{"i":1,{"i":2}]` → two
            // array elements). Reference behavior: parseKey fails on `{` and
            // the object loop breaks (trailing comma stripped).
            if matches!(
                self.peek_tag(true)?,
                Some(Tag::OpenBrace | Tag::OpenBracket)
            ) {
                break false;
            }
            let (key, key_truncated) = match self.parse_key()? {
                KeyStep::Key(key, truncated) => (key, truncated),
                KeyStep::Skip => continue,
                KeyStep::Stop => break false,
            };
            if key_truncated {
                // A key growable at EOF: later checkpoints are unsafe, and a
                // cut-off key follows the same policy as a cut-off value.
                self.eof_dependent = true;
                if !self.opts.repairs(Repairs::TRUNCATION) {
                    return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
                }
                if !self.opts.allows(Allow::KEY) {
                    break false;
                }
            }
            match self.peek_tag(false)? {
                Some(Tag::Colon) => {
                    self.take();
                }
                Some(Tag::Comma | Tag::Semicolon) => {
                    // Last token is the delimiter after the key: stable even
                    // when it sits at EOF.
                    self.emit_null_member(&mut members, key, true);
                    continue;
                }
                None => {
                    if self.opts.repairs(Repairs::TRUNCATION) && self.opts.allows(Allow::KEY) {
                        // Key itself may have been lexed at EOF: unstable.
                        self.emit_null_member(&mut members, key, false);
                    }
                    break false;
                }
                Some(other) if other.starts_value() => {
                    if self.strict() {
                        return Err(Error::new(ErrorKind::ExpectedColon, self.position()));
                    }
                    // A missing colon is repaired by inserting one.
                }
                Some(_) => {
                    self.take();
                    self.emit_null_member(&mut members, key, false);
                    continue;
                }
            }
            match self.peek_tag(false)? {
                // Colon consumed but end of input: `{"foo":` → `{"foo": null}`.
                // The *null* is invented from EOF — appending input can
                // replace it with a real value, so this is NOT a stable
                // checkpoint even though the key was bounded by `:`.
                None => {
                    if self.opts.repairs(Repairs::TRUNCATION) {
                        self.emit_null_member(&mut members, key, false);
                    }
                    break false;
                }
                // The next token cannot start a value: the value is missing
                // entirely — insert `null` (`{"a":}` → `{"a": null}`), the
                // reference repairs missing object values unconditionally.
                Some(Tag::CloseBrace | Tag::CloseBracket | Tag::Comma | Tag::Semicolon)
                    if !self.strict() =>
                {
                    self.emit_null_member(&mut members, key, false);
                    continue;
                }
                _ => {}
            }
            // Stream: write `key: ` first; the value appends itself, and a
            // dropped value (impossible under Allow::ALL) rolls the member
            // back so no dangling `key: ` remains.
            let mark = self.separator_mark(idx);
            if let Some(out) = self.emit() {
                write_escaped(out, &key);
                out.push_str(": ");
            }
            if !self.parse_value()? {
                self.rollback(mark);
                break false;
            }
            if mark.is_none() {
                let value = self.pop_value();
                members.push((key.into_owned(), value));
            }
            // No checkpoint after a value: it may be a growable scalar.
            self.member_done(idx);
        };
        self.close_container(closed, Allow::OBJ, '}', || Value::Object(members))
    }

    /// The element loop (see [`Self::run_object_loop`]; same resume rules).
    fn run_array_loop(&mut self) -> Result<bool, Error> {
        let idx = self.frames.len() - 1;
        let mut items: Vec<Value> = Vec::new();
        let closed = loop {
            match self.separator_step(idx, false, Tag::CloseBracket)? {
                Step::Again => continue,
                Step::Closed => break true,
                Step::End => break false,
                Step::Member => {}
            }
            let mark = self.separator_mark(idx);
            if !self.parse_value()? {
                self.rollback(mark);
                break false;
            }
            if mark.is_none() {
                let value = self.pop_value();
                items.push(value);
            }
            self.member_done(idx);
        };
        self.close_container(closed, Allow::ARR, ']', || Value::Array(items))
    }

    /// Finishes a container: applies the truncation policy to one that
    /// never closed, then renders the closer (stream) or pushes the node
    /// (tree). A real closer is a stable boundary for the parent's value-arm
    /// resume, even when it is the last input byte.
    fn close_container(
        &mut self,
        closed: bool,
        flag: Allow,
        closer: char,
        node: impl FnOnce() -> Value,
    ) -> Result<bool, Error> {
        let keep = closed || {
            if !self.opts.repairs(Repairs::TRUNCATION) {
                return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
            }
            self.opts.allows(flag)
        };
        match self.out.as_deref_mut() {
            Some(out) => {
                // Stream mode only runs under Allow::ALL, where `keep` is
                // always true — the closer is written once, whether the
                // container closed explicitly or by truncation repair.
                debug_assert!(keep);
                out.push(closer);
                if closed {
                    self.save_cp(CpPhase::ContinueParent, true);
                }
            }
            None if keep => self.values.push(node()),
            None => {}
        }
        Ok(keep)
    }
}

/// What [`Parser::separator_step`] found.
enum Step {
    /// A member/element should be parsed next.
    Member,
    /// A separator or noise token was consumed; look again.
    Again,
    /// The container's closer was consumed.
    Closed,
    /// End of input.
    End,
}

/// Maximum structural nesting of `text` (brackets/braces outside strings).
///
/// Used to keep the NDJSON `[` wrap from pushing an already-at-limit first
/// value one level past [`MAX_NESTING_DEPTH`], and as a post-condition on
/// repaired output.
pub(crate) fn structural_depth(text: &str) -> usize {
    let mut depth = 0usize;
    let mut max = 0usize;
    let mut in_str = false;
    let mut esc = false;
    // Only ASCII structural bytes matter; iterate bytes to skip UTF-8
    // decoding. Non-ASCII bytes (>= 0x80) only ever appear as string content
    // or inside tokens, none of which this scan acts on.
    for &b in text.as_bytes() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Maximum nesting of a parsed [`Value`] tree (same accounting as
/// [`structural_depth`]).
fn value_depth(value: &crate::value::Value) -> usize {
    use crate::value::Value;
    match value {
        Value::Array(items) => 1 + items.iter().map(value_depth).max().unwrap_or(0),
        Value::Object(members) => {
            1 + members
                .iter()
                .map(|(_, v)| value_depth(v))
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    }
}

#[cfg(test)]
mod truncated_peek_tests {
    use super::*;

    #[test]
    fn two_pass_string_is_truncated() {
        let opts = Options::all();
        let mut lexer = Lexer::new("\"a#\n", opts);
        let tok = lexer.next_token(false).expect("token").expect("some");
        match tok.token {
            Token::Str { truncated, .. } => assert!(truncated, "expected truncated Str"),
            other => panic!("expected Str, got {:?}", other),
        }
    }
}
