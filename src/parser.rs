//! The tolerant recursive-descent parser.
//!
//! It is deliberately forgiving: the grammar accepts anything a repair pass can
//! fix, and each repair is gated on a [`Repairs`] flag so strict parsing is the
//! same code path with those flags cleared.

use alloc::borrow::Cow;
use alloc::string::String;
use alloc::vec;
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
}

impl<'a, 'b> Parser<'a, 'b> {
    pub(crate) fn new(input: &'a str, opts: Options) -> Self {
        Self {
            lexer: Lexer::new(input, opts),
            peeked: None,
            opts,
            frames: Vec::new(),
            top: TopState::default(),
            out: None,
            cp_slot: None,
            resume_phase: None,
            eof_dependent: false,
        }
    }

    /// A stream-mode parser that appends repaired JSON to `out` and records
    /// resume checkpoints into `cp` (when provided).
    pub(crate) fn new_stream(
        input: &'a str,
        opts: Options,
        out: &'b mut String,
        cp: Option<&'b mut ResumeCp>,
    ) -> Self {
        Self {
            lexer: Lexer::new(input, opts),
            peeked: None,
            opts,
            frames: Vec::new(),
            top: TopState::default(),
            out: Some(out),
            cp_slot: cp,
            resume_phase: None,
            eof_dependent: false,
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
        let phase = cp.phase;
        let mut lexer = Lexer::new(input, opts);
        lexer.set_pos(cp.input_pos);
        Self {
            lexer,
            peeked: None,
            opts,
            frames: cp.frames.clone(),
            top: cp.top,
            out: Some(out),
            cp_slot: Some(cp),
            resume_phase: Some(phase),
            eof_dependent: false,
        }
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
        if self.out.is_none() {
            return;
        }
        if self.eof_dependent {
            return;
        }
        // `stable == false` means this boundary is *not* safe to resume from
        // (growable scalar, two-pass string end, EOF-invented null, …).
        // Callers already fold `at_end` into the flag where relevant — never
        // save an unstable checkpoint just because input happens to follow.
        if !stable {
            return;
        }
        let frames_len = match phase {
            CpPhase::ContinueParent => self.frames.len().saturating_sub(1),
            _ => self.frames.len(),
        };
        if self.frames[..frames_len]
            .iter()
            .any(|f| f.kind == ContainerKind::Group)
        {
            return;
        }
        let pos = self.lexer.pos();
        let out_len = match &self.out {
            Some(out) => out.len(),
            None => return,
        };
        let top = self.top;
        let Some(slot) = self.cp_slot.as_deref_mut() else {
            return;
        };
        slot.valid = true;
        slot.input_pos = pos;
        slot.output_len = out_len;
        slot.phase = phase;
        slot.frames.clear();
        slot.frames.extend_from_slice(&self.frames[..frames_len]);
        slot.top = top;
    }

    /// Whether every repair pass is disabled.
    fn strict(&self) -> bool {
        self.opts.repairs == Repairs::NONE
    }

    /// Enters one container; fails once [`MAX_NESTING_DEPTH`] is reached.
    fn enter(&mut self, kind: ContainerKind) -> Result<(), Error> {
        if self.frames.len() >= MAX_NESTING_DEPTH {
            return Err(Error::new(ErrorKind::DepthLimitExceeded, self.position()));
        }
        self.frames.push(Frame {
            kind,
            first: true,
            comma_pending: false,
        });
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
        let mut values = Vec::new();
        self.parse_top(&mut values)?;
        if values.len() == 1 {
            Ok(values.pop().expect("one value parsed"))
        } else {
            // Wrapping several top-level values in one array adds a level
            // that `enter()` never counted (the deepest value may already
            // use the full budget). Reject before emitting a tree whose
            // serialization fails `validate` / `MAX_NESTING_DEPTH`.
            let max_member = values.iter().map(value_depth).max().unwrap_or(0);
            if max_member >= MAX_NESTING_DEPTH {
                return Err(Error::new(ErrorKind::DepthLimitExceeded, self.lexer.pos()));
            }
            Ok(Value::Array(values))
        }
    }

    /// Parses the whole document in stream mode: bytes are appended to
    /// `self.out` and no tree is built (returned values are placeholders).
    pub(crate) fn parse_document_stream(&mut self) -> Result<(), Error> {
        let mut scratch = Vec::new();
        self.parse_top(&mut scratch)
    }

    /// Continues a stream parse from a saved checkpoint (see
    /// [`Parser::new_stream_resumed`]). Rebuilds the suspended call chain:
    /// innermost container loop → parent value-arms → top-level driver.
    pub(crate) fn parse_resume(&mut self) -> Result<(), Error> {
        let phase = self.resume_phase.unwrap_or(CpPhase::TopContinue);
        let mut values: Vec<Value> = vec![Value::Null; self.top.count];
        match phase {
            CpPhase::TopContinue => self.top_continue(&mut values),
            CpPhase::InContainer | CpPhase::ContinueParent => {
                // ContinueParent: the innermost frame is already finished —
                // apply its parent's value-arm tail before running that loop.
                let mut need_tail = matches!(phase, CpPhase::ContinueParent);
                loop {
                    if self.frames.is_empty() {
                        // The suspended top-level value is complete.
                        values.push(Value::Null);
                        self.top.count = values.len();
                        return self.top_continue(&mut values);
                    }
                    if need_tail {
                        let idx = self.frames.len() - 1;
                        self.frames[idx].first = false;
                        self.frames[idx].comma_pending = false;
                    }
                    need_tail = true;
                    let kind = self.frames[self.frames.len() - 1].kind;
                    match kind {
                        ContainerKind::Object => {
                            let _ = self.run_object_loop()?;
                        }
                        ContainerKind::Array => {
                            let _ = self.run_array_loop()?;
                        }
                        // Groups never create checkpoints.
                        ContainerKind::Group => {
                            return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
                        }
                    }
                    self.leave();
                }
            }
        }
    }

    /// Shared top-level driver for both modes. `values` collects the parsed
    /// values in tree mode; stream mode pushes placeholder `Null`s so the
    /// value *count* (used for NDJSON `[...]` wrapping) stays identical.
    fn parse_top(&mut self, values: &mut Vec<Value>) -> Result<(), Error> {
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
        let first_pos = self.emit().map(|out| out.len()).unwrap_or(0);
        self.top = TopState {
            first_pos,
            prose_first,
            count: 0,
        };
        // A truncated first value (unclosed string/number/word, or the
        // two-pass string rule that stops on a raw delimiter at "logical"
        // EOF while `pos` still sits before trailing input) is growable:
        // a later chunk continues *this* value rather than starting another.
        // `!at_end` alone is not enough — `"a#\n` stops at the newline with
        // pos < len and would otherwise checkpoint as a finished value.
        let Some(first) = self.parse_value()? else {
            return Err(Error::new(ErrorKind::NoValueFound, position));
        };
        values.push(first);
        self.top.count = 1;
        self.save_cp(CpPhase::TopContinue, false);
        self.top_continue(values)
    }

    /// The top-level loop after the first value has been parsed (also the
    /// resume entry for [`CpPhase::TopContinue`]).
    fn top_continue(&mut self, values: &mut Vec<Value>) -> Result<(), Error> {
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
                            return Err(Error::new(
                                ErrorKind::UnexpectedCharacter(tag.punct_char()),
                                self.position(),
                            ));
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
            let inserted_bracket = self.emit().is_some() && values.len() == 1;
            if inserted_bracket {
                // The wrap adds one nesting level that `enter()` never counted
                // (the first value already used the full depth budget).
                if let Some(out) = self.emit() {
                    if structural_depth(out) >= MAX_NESTING_DEPTH {
                        return Err(Error::new(ErrorKind::DepthLimitExceeded, self.position()));
                    }
                }
            }
            if let Some(out) = self.emit() {
                if inserted_bracket {
                    out.insert(first_pos, '[');
                }
                out.push_str(", ");
            }
            if inserted_bracket {
                self.invalidate_cp_from(first_pos);
            }
            // The value tag is already peeked (starts_value check above).
            match self.parse_value()? {
                Some(value) => {
                    values.push(value);
                    self.top.count = values.len();
                    self.save_cp(CpPhase::TopContinue, false);
                }
                None => {
                    if let (Some(out), Some(base)) = (self.emit(), base) {
                        // Drop `, ` plus anything `parse_value` wrote, keep
                        // the bracket (if any) for the explicit remove below.
                        out.truncate(base + usize::from(inserted_bracket));
                        if inserted_bracket {
                            out.remove(first_pos);
                        }
                    }
                    if inserted_bracket {
                        self.invalidate_cp_from(first_pos);
                    }
                    break;
                }
            }
        }
        if let Some(out) = self.emit() {
            if values.len() > 1 {
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
            let spanned = self.lexer.next_token(key_position)?;
            self.peeked = spanned.map(|spanned| {
                let tag = Tag::of(&spanned.token);
                (spanned, tag)
            });
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
    /// Parses one value.
    ///
    /// Returns `None` when there is no value at the cursor, and also when the
    /// value was incomplete and the partial policy (see [`Allow`]) dropped it.
    fn parse_value(&mut self) -> Result<Option<Value>, Error> {
        // Stray separators before a value are dropped (but not in strict mode).
        while let Some(tag @ (Tag::Ellipsis | Tag::Semicolon | Tag::Plus)) = self.peek_tag(false)? {
            if self.strict() {
                let c = match tag {
                    Tag::Ellipsis => '.',
                    Tag::Semicolon => ';',
                    _ => '+',
                };
                return Err(Error::new(
                    ErrorKind::UnexpectedCharacter(c),
                    self.position(),
                ));
            }
            self.take();
        }
        let Some(tag) = self.peek_tag(false)? else {
            return Ok(None);
        };
        match tag {
            Tag::OpenBrace => self.parse_object(),
            Tag::OpenBracket => self.parse_array(),
            Tag::OpenParen => self.parse_group(),
            Tag::Str { truncated } => self.parse_string_value(truncated),
            Tag::Num { truncated } => {
                let spanned = self.take();
                if truncated && !self.opts.allows(Allow::NUM) {
                    return Ok(None);
                }
                if truncated {
                    self.eof_dependent = true;
                }
                match spanned.token {
                    Token::Num { text, .. } => {
                        if let Some(out) = self.emit() {
                            // Stream mode: append the number text (already
                            // valid JSON) and report a placeholder.
                            out.push_str(&text);
                            Ok(Some(Value::Null))
                        } else {
                            Ok(Some(Value::Number(Number::from_normalized(
                                text.into_owned(),
                            ))))
                        }
                    }
                    _ => Ok(None),
                }
            }
            Tag::Bool => {
                let spanned = self.take();
                match spanned.token {
                    Token::Bool(value) => {
                        if let Some(out) = self.emit() {
                            out.push_str(if value { "true" } else { "false" });
                            Ok(Some(Value::Null))
                        } else {
                            Ok(Some(Value::Bool(value)))
                        }
                    }
                    _ => Ok(None),
                }
            }
            Tag::Null | Tag::Undefined => {
                self.take();
                if let Some(out) = self.emit() {
                    out.push_str("null");
                }
                Ok(Some(Value::Null))
            }
            Tag::Word { .. } => self.parse_word_value(),
            Tag::CloseBrace
            | Tag::CloseBracket
            | Tag::CloseParen
            | Tag::Colon
            | Tag::Comma
            | Tag::Semicolon
            | Tag::Plus
            | Tag::Ellipsis => Ok(None),
        }
    }

    /// A parenthesised value, as produced by JavaScript-ish serializers.
    fn parse_group(&mut self) -> Result<Option<Value>, Error> {
        self.enter(ContainerKind::Group)?;
        let result = self.parse_group_inner();
        self.leave();
        result
    }

    fn parse_group_inner(&mut self) -> Result<Option<Value>, Error> {
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
                Some(tag) if tag.starts_value() && inner.is_none() => {
                    inner = self.parse_value()?;
                }
                _ => break,
            }
        }
        Ok(inner)
    }

    fn parse_string_value(&mut self, truncated: bool) -> Result<Option<Value>, Error> {
        let spanned = self.take();
        let start = spanned.start;
        let Token::Str { text, .. } = spanned.token else {
            return Ok(None);
        };
        if truncated && !self.opts.repairs(Repairs::TRUNCATION) {
            return Err(Error::new(ErrorKind::UnexpectedEnd, start));
        }
        if truncated && !self.opts.allows(Allow::STR) {
            return Ok(None);
        }
        if truncated {
            // Two-pass / EOF string end: following separators are not stable.
            self.eof_dependent = true;
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
                Some(Tag::Str { .. }) => {
                    if let Token::Str { text: next, .. } = self.take().token {
                        text.to_mut().push_str(&next);
                    }
                }
                _ => break,
            }
        }
        if let Some(out) = self.emit() {
            write_escaped(out, &text);
            Ok(Some(Value::Null))
        } else {
            Ok(Some(Value::String(text.into_owned())))
        }
    }

    /// A bare word: either a function-call wrapper or an unquoted string.
    fn parse_word_value(&mut self) -> Result<Option<Value>, Error> {
        let spanned = self.take();
        let start = spanned.start;
        let Token::Word { text, truncated } = spanned.token else {
            return Ok(None);
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
        // A word cut off by truncation is treated like a cut-off string:
        // an error when truncation repair is off, a drop when the partial
        // policy does not allow strings.
        if truncated && !self.opts.repairs(Repairs::TRUNCATION) {
            return Err(Error::new(ErrorKind::UnexpectedEnd, start));
        }
        if truncated && !self.opts.allows(Allow::STR) {
            return Ok(None);
        }
        if truncated {
            self.eof_dependent = true;
        }
        if let Some(out) = self.emit() {
            write_escaped(out, text);
            Ok(Some(Value::Null))
        } else {
            Ok(Some(Value::String(String::from(text))))
        }
    }
}

/// What the parser found where an object key belongs.
enum KeyStep<'a> {
    /// A usable key, plus whether it was cut off at end of input.
    ///
    /// Quoted/numeric keys borrow the input slice; word/keyword keys own a
    /// short `String`.
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
        match tag {
            Tag::Str { truncated } => {
                let spanned = self.take();
                match spanned.token {
                    Token::Str { text, .. } => Ok(KeyStep::Key(text, truncated)),
                    _ => Ok(KeyStep::Skip),
                }
            }
            Tag::Word { truncated } => {
                let spanned = self.take();
                let start = spanned.start;
                let Token::Word { text, .. } = spanned.token else {
                    return Ok(KeyStep::Skip);
                };
                if !self.opts.repairs(Repairs::UNQUOTED) {
                    return Err(Error::new(ErrorKind::UnquotedValue, start));
                }
                Ok(KeyStep::Key(Cow::Owned(String::from(text)), truncated))
            }
            Tag::Num { truncated } => {
                let spanned = self.take();
                match spanned.token {
                    Token::Num { text, .. } => Ok(KeyStep::Key(text, truncated)),
                    _ => Ok(KeyStep::Skip),
                }
            }
            Tag::Bool => {
                let spanned = self.take();
                match spanned.token {
                    Token::Bool(value) => Ok(KeyStep::Key(
                        Cow::Owned(String::from(if value { "true" } else { "false" })),
                        false,
                    )),
                    _ => Ok(KeyStep::Skip),
                }
            }
            Tag::Null | Tag::Undefined => {
                self.take();
                Ok(KeyStep::Key(Cow::Borrowed("null"), false))
            }
            _ => {
                if self.strict() {
                    return Err(Error::new(ErrorKind::ExpectedObjectKey, self.position()));
                }
                self.take();
                Ok(KeyStep::Skip)
            }
        }
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

    fn parse_object(&mut self) -> Result<Option<Value>, Error> {
        self.enter(ContainerKind::Object)?;
        self.take();
        // Fresh parses write the opener here; a resume finds it already in
        // the output prefix (the wrapper is not used on resume).
        if let Some(out) = self.emit() {
            out.push('{');
        }
        let result = self.run_object_loop();
        self.leave();
        result
    }

    /// The member loop. The caller has consumed `{` (fresh) or the cursor
    /// sits just past it (resume); the frame for this object is already on
    /// `self.frames`.
    fn run_object_loop(&mut self) -> Result<Option<Value>, Error> {
        let idx = self.frames.len() - 1;
        let mut members: Vec<(String, Value)> = Vec::new();
        let mut closed = false;
        loop {
            let first = self.frames[idx].first;
            let comma_pending = self.frames[idx].comma_pending;
            match self.peek_tag(true)? {
                None => break,
                Some(tag @ (Tag::CloseBrace | Tag::CloseBracket)) => {
                    if !matches!(tag, Tag::CloseBrace) && self.strict() {
                        return Err(Error::new(
                            ErrorKind::UnexpectedCharacter(tag.punct_char()),
                            self.position(),
                        ));
                    }
                    if comma_pending && self.strict() {
                        return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                    }
                    self.take();
                    closed = true;
                    break;
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
                    continue;
                }
                Some(tag @ (Tag::Semicolon | Tag::Ellipsis | Tag::Colon | Tag::Plus)) => {
                    if self.strict() {
                        return Err(Error::new(
                            ErrorKind::UnexpectedCharacter(tag.punct_char()),
                            self.position(),
                        ));
                    }
                    self.take();
                    continue;
                }
                _ => {}
            }
            if !first && !comma_pending && self.strict() {
                return Err(Error::new(ErrorKind::ExpectedComma, self.position()));
            }
            // A structural value at key position ends this object; the
            // parent decides what to do with it (`[{"i":1,{"i":2}]` → two
            // array elements). Reference behavior: parseKey fails on `{` and
            // the object loop breaks (trailing comma stripped).
            if matches!(
                self.peek_tag(true)?,
                Some(Tag::OpenBrace | Tag::OpenBracket)
            ) {
                break;
            }
            let (key, key_truncated) = match self.parse_key()? {
                KeyStep::Key(key, truncated) => (key, truncated),
                KeyStep::Skip => continue,
                KeyStep::Stop => break,
            };
            if key_truncated {
                // A key growable at EOF: later checkpoints are unsafe, and a
                // cut-off key follows the same policy as a cut-off value.
                self.eof_dependent = true;
                if !self.opts.repairs(Repairs::TRUNCATION) {
                    return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
                }
                if !self.opts.allows(Allow::KEY) {
                    break;
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
                    break;
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
            // Colon consumed but end of input: `{"foo":` → `{"foo": null}`.
            // (A value token that exists but is dropped by the Allow policy
            // is handled below, not here.)
            if self.peek_tag(false)?.is_none() {
                if self.opts.repairs(Repairs::TRUNCATION) {
                    // The *null* is invented from EOF — appending input can
                    // replace it with a real value, so this is NOT a stable
                    // checkpoint even though the key was bounded by `:`.
                    self.emit_null_member(&mut members, key, false);
                }
                break;
            }
            // The next token cannot start a value: the value is missing
            // entirely — insert `null` (`{"a":}` → `{"a": null}`), the
            // reference repairs missing object values unconditionally.
            if matches!(
                self.peek_tag(false)?,
                Some(Tag::CloseBrace | Tag::CloseBracket | Tag::Comma | Tag::Semicolon)
            ) && !self.strict()
            {
                self.emit_null_member(&mut members, key, false);
                continue;
            }
            // Stream: write `key: ` first; the value appends itself, and a
            // `None` result (impossible under Allow::ALL) rolls the member
            // back so no dangling `key: ` remains.
            self.peek_tag(false)?; // fill the value token before inspecting it
            let mark = if let Some(out) = self.emit() {
                let mark = out.len();
                if !first {
                    out.push_str(", ");
                }
                write_escaped(out, &key);
                out.push_str(": ");
                Some(mark)
            } else {
                None
            };
            match self.parse_value()? {
                Some(value) => {
                    if mark.is_none() {
                        members.push((key.into_owned(), value));
                    }
                }
                None => {
                    if let (Some(out), Some(mark)) = (self.emit(), mark) {
                        out.truncate(mark);
                    }
                    break;
                }
            }
            self.frames[idx].first = false;
            self.frames[idx].comma_pending = false;
            // Member finished; checkpoint when input follows and the value
            // was not growable (truncated scalar / two-pass string end).
            self.save_cp(CpPhase::InContainer, false);
        }
        let keep = self.finish_collection(closed, Allow::OBJ)?;
        if let Some(out) = self.emit() {
            // Stream mode only runs under Allow::ALL, where `keep` is always
            // true — the closer is written once, whether the object closed
            // explicitly or was closed by truncation repair.
            debug_assert!(keep);
            out.push('}');
        }
        if closed && self.streaming() {
            // Closed by a real `}`: a stable boundary for the parent's
            // value-arm resume, even when `}` is the last input byte.
            self.save_cp(CpPhase::ContinueParent, true);
        }
        if keep {
            if self.streaming() {
                Ok(Some(Value::Null))
            } else {
                Ok(Some(Value::Object(members)))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_array(&mut self) -> Result<Option<Value>, Error> {
        self.enter(ContainerKind::Array)?;
        self.take();
        if let Some(out) = self.emit() {
            out.push('[');
        }
        let result = self.run_array_loop();
        self.leave();
        result
    }

    /// The element loop (see [`Self::run_object_loop`]; same resume rules).
    fn run_array_loop(&mut self) -> Result<Option<Value>, Error> {
        let idx = self.frames.len() - 1;
        let mut items: Vec<Value> = Vec::new();
        let mut closed = false;
        loop {
            let first = self.frames[idx].first;
            let comma_pending = self.frames[idx].comma_pending;
            match self.peek_tag(false)? {
                None => break,
                Some(tag @ (Tag::CloseBracket | Tag::CloseBrace)) => {
                    if !matches!(tag, Tag::CloseBracket) && self.strict() {
                        return Err(Error::new(
                            ErrorKind::UnexpectedCharacter(tag.punct_char()),
                            self.position(),
                        ));
                    }
                    if comma_pending && self.strict() {
                        return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                    }
                    self.take();
                    closed = true;
                    break;
                }
                Some(Tag::Comma) => {
                    self.take();
                    if (first || comma_pending) && self.strict() {
                        return Err(Error::new(ErrorKind::TrailingComma, self.position()));
                    }
                    self.frames[idx].comma_pending = true;
                    self.save_cp(CpPhase::InContainer, true);
                    continue;
                }
                Some(tag @ (Tag::Semicolon | Tag::Ellipsis | Tag::Colon | Tag::Plus)) => {
                    if self.strict() {
                        return Err(Error::new(
                            ErrorKind::UnexpectedCharacter(tag.punct_char()),
                            self.position(),
                        ));
                    }
                    self.take();
                    continue;
                }
                _ => {}
            }
            if !first && !comma_pending && self.strict() {
                return Err(Error::new(ErrorKind::ExpectedComma, self.position()));
            }
            // Stream: write the separator first; roll it back if the element
            // does not materialize.
            let mark = if let Some(out) = self.emit() {
                let mark = out.len();
                if !first {
                    out.push_str(", ");
                }
                Some(mark)
            } else {
                None
            };
            match self.parse_value()? {
                Some(value) => {
                    if mark.is_none() {
                        items.push(value);
                    }
                    self.frames[idx].first = false;
                    self.frames[idx].comma_pending = false;
                    self.save_cp(CpPhase::InContainer, false);
                }
                None => {
                    if let (Some(out), Some(mark)) = (self.emit(), mark) {
                        out.truncate(mark);
                    }
                    break;
                }
            }
        }
        let keep = self.finish_collection(closed, Allow::ARR)?;
        if let Some(out) = self.emit() {
            debug_assert!(keep);
            out.push(']');
        }
        if closed && self.streaming() {
            self.save_cp(CpPhase::ContinueParent, true);
        }
        if keep {
            if self.streaming() {
                Ok(Some(Value::Null))
            } else {
                Ok(Some(Value::Array(items)))
            }
        } else {
            Ok(None)
        }
    }

    /// Applies the truncation policy to a collection that never closed.
    ///
    /// Returns `true` when the caller should keep what it collected.
    fn finish_collection(&self, closed: bool, flag: Allow) -> Result<bool, Error> {
        if closed {
            return Ok(true);
        }
        if !self.opts.repairs(Repairs::TRUNCATION) {
            return Err(Error::new(ErrorKind::UnexpectedEnd, self.position()));
        }
        Ok(self.opts.allows(flag))
    }
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
    for c in text.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '[' | '{' => {
                depth += 1;
                max = max.max(depth);
            }
            ']' | '}' => depth = depth.saturating_sub(1),
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
