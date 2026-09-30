//! A tolerant tokenizer: it never fails on input that a repair pass can fix.

use alloc::borrow::Cow;
use alloc::string::String;

use crate::chars::{
    decode_html_entity, hex_value, is_delimiter, is_double_quote, is_func_name_char, is_quote,
    is_single_quote, is_special_ws, is_unquoted_delimiter, is_url_char, is_url_scheme, is_ws,
    simple_escape,
};
use crate::error::{Error, ErrorKind};
use crate::options::{Allow, Options, Repairs};

/// One lexical unit, tolerant of the mistakes the repair passes handle.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Token<'a> {
    OpenBrace,
    CloseBrace,
    OpenBracket,
    CloseBracket,
    Colon,
    Comma,
    Plus,
    Ellipsis,
    OpenParen,
    CloseParen,
    /// A `;`, which appears after JSONP calls and in hand-written data.
    Semicolon,
    /// A string, with typographic quotes normalized and escapes decoded.
    ///
    /// Plain strings borrow the input slice (zero allocation); only strings
    /// that needed escape/entity/quote repair own a `String`.
    Str {
        text: Cow<'a, str>,
        truncated: bool,
    },
    /// A number whose text is already valid JSON.
    ///
    /// Clean numbers borrow the input slice; repaired literals own their
    /// normalized text.
    Num {
        text: Cow<'a, str>,
        truncated: bool,
    },
    Bool(bool),
    Null,
    Undefined,
    /// A bare word: an unquoted key, an unquoted value, or a function name.
    Word {
        text: &'a str,
        truncated: bool,
    },
}

/// A string-opener: the source character that opened it plus how many bytes it spans.
struct Opener {
    ch: char,
    len: usize,
    from_entity: bool,
    family: QuoteFamily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuoteFamily {
    /// `"` and its typographic variants.
    Double,
    /// `'` and its typographic variants.
    Single,
}

impl Opener {
    /// An opener for the quote character `ch` spanning `len` source bytes.
    fn new(ch: char, len: usize) -> Self {
        Self {
            ch,
            len,
            from_entity: false,
            family: if is_single_quote(ch) {
                QuoteFamily::Single
            } else {
                QuoteFamily::Double
            },
        }
    }

    /// Whether `c` closes a string opened by this opener.
    fn is_end(&self, c: char) -> bool {
        match (self.family, self.ch) {
            (_, '"') => c == '"',
            (_, '\'') => c == '\'',
            (QuoteFamily::Double, _) => is_double_quote(c),
            (QuoteFamily::Single, _) => is_single_quote(c),
        }
    }
}

/// The tolerant lexer. Every method takes `&self` options so callers can run
/// repair passes selectively.
#[derive(Clone, Copy)]
pub(crate) struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    opts: Options,
    /// Whether an object/array/group frame is open around the current parse
    /// position. The parser keeps this in sync with its frame stack; it
    /// gates the top-level-only `isInsideUnclosedBracket` end-quote heuristic
    /// (inside a container the following `}`/`]` closes *that* container, it
    /// is not evidence that the quote was embedded).
    in_container: bool,
}

/// A token together with the byte offset where it starts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Spanned<'a> {
    pub(crate) token: Token<'a>,
    pub(crate) start: usize,
    /// Whether the skipped trivia before this token contained a newline, which
    /// is what separates NDJSON records.
    pub(crate) newline_before: bool,
}

impl<'a> Lexer<'a> {
    pub(crate) fn new(src: &'a str, opts: Options) -> Self {
        Self {
            src,
            pos: 0,
            opts,
            in_container: false,
        }
    }

    /// Syncs the container flag with the parser's frame stack.
    pub(crate) fn set_in_container(&mut self, in_container: bool) {
        self.in_container = in_container;
    }

    /// Byte offset of the next unread character.
    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    /// Whether the whole input has been consumed.
    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    /// Whether nothing but whitespace remains after `pos`.
    ///
    /// Lookaheads that skip trivia (notably `end_quote_is_real`) consult the
    /// next *non-ws* byte; if that byte does not exist yet, appending input
    /// can flip their decision. Stream checkpoints must treat "only trivia
    /// left" the same as EOF.
    pub(crate) fn at_effective_end(&self) -> bool {
        self.at_end() || self.rest().chars().all(is_ws)
    }

    /// Repositions the cursor (resume support). `pos` must be a position
    /// this lexer previously produced, i.e. a char boundary.
    pub(crate) fn set_pos(&mut self, pos: usize) {
        debug_assert!(self.src.is_char_boundary(pos));
        self.pos = pos;
    }

    /// The next non-trivia character, without producing a token or moving
    /// this lexer.
    ///
    /// The parser uses this to test for `+`/`(` after a value without
    /// caching a token that was lexed in value context when the next token
    /// is really an object key (`key_position` would be wrong).
    pub(crate) fn peek_significant_char(&self) -> Option<char> {
        let mut probe = *self;
        probe.skip_trivia();
        probe.peek_char()
    }

    /// The not-yet-consumed input.
    pub(crate) fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    /// The next character. ASCII (nearly every byte of real JSON) is read
    /// straight from the byte, skipping the slice and UTF-8 decode.
    fn peek_char(&self) -> Option<char> {
        match *self.src.as_bytes().get(self.pos)? {
            byte if byte.is_ascii() => Some(char::from(byte)),
            _ => self.rest().chars().next(),
        }
    }

    fn peek_second(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn bump_char(&mut self) -> Option<char> {
        let c = self.peek_char()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    /// Reads the next token, skipping trivia, comments, and fences first.
    ///
    /// `key_position` makes a bare word stop at `:` so that `{a: 1}` lexes the
    /// key and its value separately.
    pub(crate) fn next_token(&mut self, key_position: bool) -> Result<Option<Spanned<'a>>, Error> {
        let newline_before = self.skip_trivia();
        let start = self.pos;
        let Some(c) = self.peek_char() else {
            return Ok(None);
        };
        let token = if let Some(token) = punctuator(c) {
            self.pos += 1;
            token
        } else {
            self.scan_token(c, key_position)?
        };
        Ok(Some(Spanned {
            token,
            start,
            newline_before,
        }))
    }

    /// Lexes a token that is not a single-byte punctuator; `c` is the
    /// character at the cursor.
    fn scan_token(&mut self, c: char, key_position: bool) -> Result<Token<'a>, Error> {
        let start = self.pos;
        Ok(match c {
            '.' if self.rest().starts_with("...") => {
                self.pos += 3;
                self.skip_ws_only();
                if self.peek_char() == Some(',') {
                    self.pos += 1;
                }
                Token::Ellipsis
            }
            '/' => {
                if self.rest().starts_with("//") || self.rest().starts_with("/*") {
                    // Comments are only consumed by `skip_trivia` when enabled.
                    return Err(Error::new(ErrorKind::UnexpectedCharacter('/'), start));
                }
                if self.opts.repairs(Repairs::UNQUOTED) {
                    self.scan_regex()
                } else {
                    return Err(Error::new(ErrorKind::UnexpectedCharacter('/'), start));
                }
            }
            '\\' if self.opts.repairs(Repairs::UNQUOTED) => {
                // A redundant escape before a quote, as in `{\"a\": 1}`.
                self.pos += 1;
                match self.peek_char() {
                    Some(q) if is_quote(q) => {
                        self.scan_string_from(Opener::new(q, q.len_utf8()))?
                    }
                    _ => {
                        self.pos = start;
                        self.scan_word(key_position)
                    }
                }
            }
            '&' => match decode_html_entity(self.rest()) {
                Some((ch, len)) if is_quote(ch) && self.opts.repairs(Repairs::ENTITIES) => self
                    .scan_string_from(Opener {
                        from_entity: true,
                        ..Opener::new(ch, len)
                    })?,
                _ => self.scan_number_or_word(key_position)?,
            },
            c if is_quote(c) => {
                if c != '"' && !self.opts.repairs(Repairs::QUOTES) {
                    return Err(Error::new(ErrorKind::UnexpectedCharacter(c), start));
                }
                self.scan_string_from(Opener::new(c, c.len_utf8()))?
            }
            '-' | '0'..='9' | '.' => self.scan_number_or_word(key_position)?,
            _ => self.scan_word(key_position),
        })
    }
}

/// The token for a single-byte punctuator, or `None`.
fn punctuator(c: char) -> Option<Token<'static>> {
    Some(match c {
        '{' => Token::OpenBrace,
        '}' => Token::CloseBrace,
        '[' => Token::OpenBracket,
        ']' => Token::CloseBracket,
        ':' => Token::Colon,
        ',' => Token::Comma,
        ';' => Token::Semicolon,
        '+' => Token::Plus,
        '(' => Token::OpenParen,
        ')' => Token::CloseParen,
        _ => return None,
    })
}

impl<'a> Lexer<'a> {
    /// Skips whitespace, comments, and markdown fences.
    ///
    /// Returns whether a newline was skipped, which is what separates NDJSON
    /// records at the top level.
    fn skip_trivia(&mut self) -> bool {
        let mut saw_newline = false;
        loop {
            // Fast reject: classify the next byte (and, for `/`/`[`/`{`, the
            // one after it) before doing any whitespace/comment/fence work.
            // Everything else (JSON punctuators, quotes, digits, letters)
            // returns immediately — this runs once per token.
            let bytes = self.src.as_bytes();
            let next = bytes.get(self.pos + 1).copied();
            let can_start_trivia = match bytes.get(self.pos) {
                None => false,
                Some(&b) => match b {
                    b' ' | b'\n' | b'\r' | b'\t' => true,
                    b'/' => {
                        self.opts.repairs(Repairs::COMMENTS) && matches!(next, Some(b'/' | b'*'))
                    }
                    b'`' => self.opts.repairs(Repairs::FENCES),
                    // `[```` / `{```` fence openers: only when a backtick follows.
                    b'[' | b'{' => self.opts.repairs(Repairs::FENCES) && next == Some(b'`'),
                    // Special Unicode whitespace lives in multi-byte space;
                    // real non-ASCII tokens fall through to the loop, which
                    // consumes nothing and returns.
                    _ => b >= 0x80 && self.opts.repairs(Repairs::WHITESPACE),
                },
            };
            if !can_start_trivia {
                return saw_newline;
            }
            let before = self.pos;
            saw_newline |= self.skip_ws_only();
            if self.opts.repairs(Repairs::COMMENTS) {
                saw_newline |= self.skip_comment();
            }
            if self.opts.repairs(Repairs::FENCES) {
                self.skip_fence();
            }
            if self.pos == before {
                return saw_newline;
            }
        }
    }

    /// Skips one `//` comment (up to, not including, its newline) or one
    /// `/* */` comment at the cursor; returns whether a block comment spanned
    /// a newline. An unterminated block comment runs to the end of input.
    fn skip_comment(&mut self) -> bool {
        if self.src.as_bytes().get(self.pos) != Some(&b'/') {
            return false;
        }
        let rest = self.rest();
        if rest.starts_with("//") {
            self.pos += rest.find('\n').unwrap_or(rest.len());
        } else if let Some(body) = rest.strip_prefix("/*") {
            let (body, closer) = match body.find("*/") {
                Some(end) => (&body[..end], 2),
                None => (body, 0),
            };
            self.pos += 2 + body.len() + closer;
            return body.contains('\n');
        }
        false
    }

    /// Skips a markdown fence at the cursor: ```` ``` ```` plus an optional
    /// language specifier. Only the marker is consumed — the content stays
    /// for the parser, so inline fences like ```` ```{"a":1}``` ```` work as
    /// well as line fences (reference: `skipMarkdownCodeBlock`). The
    /// reference also treats `[```` / `{```` as openers and ```` ```] ```` /
    /// ```` ```} ```` as closers, so a bracket wrapped around a fence
    /// disappears with it (`[```\n{"a":1}\n```]` → `{"a": 1}`).
    fn skip_fence(&mut self) {
        let rest = &self.src.as_bytes()[self.pos..];
        let bracket = usize::from(matches!(rest, [b'[' | b'{', b'`', b'`', b'`', ..]));
        if !rest[bracket..].starts_with(b"```") {
            return;
        }
        self.pos += bracket + 3;
        while matches!(self.peek_char(), Some(c) if is_func_name_char(c)) {
            self.pos += 1;
        }
        if matches!(self.peek_char(), Some(']' | '}')) {
            self.pos += 1;
        }
    }

    /// Skips whitespace only; returns whether a newline was skipped.
    ///
    /// Byte-wise for ASCII (the overwhelmingly common case); non-ASCII is
    /// decoded only to test the special-Unicode-whitespace set.
    fn skip_ws_only(&mut self) -> bool {
        let mut saw_newline = false;
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b'\n' => {
                    saw_newline = true;
                    self.pos += 1;
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b if b >= 0x80 => {
                    if !self.opts.repairs(Repairs::WHITESPACE) {
                        break;
                    }
                    let Some(c) = self.src[self.pos..].chars().next() else {
                        break;
                    };
                    if is_special_ws(c) {
                        self.pos += c.len_utf8();
                    } else {
                        break;
                    }
                }
                _ => break,
            }
        }
        saw_newline
    }

    /// Scans a string opened by `opener`, normalizing quotes and escapes.
    ///
    /// Content that arrives verbatim from the input (bulk ASCII runs, kept
    /// quotes) never allocates: the token borrows `src[start..content_end]`.
    /// The first escape/entity/decode forces an owned buffer that the rest
    /// of the content is appended to.
    fn scan_string_from(&mut self, opener: Opener) -> Result<Token<'a>, Error> {
        self.pos += opener.len;
        let start = self.pos;
        let end_byte = match opener.family {
            QuoteFamily::Double => b'"',
            QuoteFamily::Single => b'\'',
        };
        // Fast path: clean content closed by the family's ASCII quote and
        // followed directly by a byte the end-quote rules always accept
        // (`,`/`:`/newline anywhere, a closer inside a container) — exactly
        // what the general loop below concludes, minus its bookkeeping.
        let bytes = self.src.as_bytes();
        let run_end = plain_run_end(bytes, start, end_byte);
        if !opener.from_entity && bytes.get(run_end) == Some(&end_byte) {
            let accept = match bytes.get(run_end + 1) {
                Some(b',' | b':' | b'\n') => true,
                Some(b'}' | b']') => self.in_container,
                _ => false,
            };
            if accept {
                self.pos = run_end + 1;
                return Ok(Token::Str {
                    text: Cow::Borrowed(&self.src[start..run_end]),
                    truncated: false,
                });
            }
        }
        // The general loop resumes after the run already scanned above.
        self.pos = run_end;
        // End of the verbatim prefix while `owned` is still `None`.
        let mut content_end = run_end;
        let mut owned: Option<String> = None;
        let mut truncated = false;
        let mut stop_at_delimiter = false;
        let mut stop_at_index: Option<usize> = None;
        // Whether the last pushed char came raw from the input and is a
        // delimiter (only then can the EOF two-pass rule fire — an escaped
        // `\]` is legitimate string content, not a swallowed closer).
        let mut raw_delim_tail = run_end > start && is_delimiter(char::from(bytes[run_end - 1]));
        loop {
            // Fast path: advance over a run of plain ASCII content (no escape,
            // no quote, no entity `&`, no non-ASCII). While nothing has been
            // decoded the run needs no buffer at all; afterwards it is one
            // memcpy into the owned buffer. Stop-mode runs are short by
            // definition (they end at the first delimiter), so they skip the
            // SWAR strides and also stop at unquoted-string delimiters.
            {
                let bytes = self.src.as_bytes();
                let mut i = self.pos;
                if stop_at_delimiter {
                    while i < bytes.len() {
                        let b = bytes[i];
                        if b >= 0x80
                            || b == b'&'
                            || b == b'\\'
                            || b == end_byte
                            || is_unquoted_delimiter(char::from(b))
                        {
                            break;
                        }
                        i += 1;
                    }
                } else {
                    i = plain_run_end(bytes, i, end_byte);
                }
                if let Some(stop) = stop_at_index {
                    if stop <= i {
                        i = stop;
                    }
                }
                if i > self.pos {
                    if let Some(text) = owned.as_mut() {
                        text.push_str(&self.src[self.pos..i]);
                    }
                    raw_delim_tail =
                        (bytes[i - 1] as char).is_ascii() && is_delimiter(bytes[i - 1] as char);
                    self.pos = i;
                    content_end = i;
                    continue;
                }
            }
            let Some(c) = self.peek_char() else {
                // Reference two-pass rule: when the text ends on a raw
                // delimiter (`{"a":"b}`, `["hello]`), that delimiter was
                // swallowed with the missing end quote — restart and end the
                // string at the first unquoted-string delimiter instead.
                if !stop_at_delimiter && raw_delim_tail {
                    stop_at_delimiter = true;
                    self.pos = start;
                    content_end = start;
                    owned = None;
                    continue;
                }
                truncated = true;
                break;
            };
            // Close at the comma that a missing end quote should precede
            // (`["hello,"world"]` → `"hello"` then the comma stays outside).
            if stop_at_index == Some(self.pos) {
                truncated = true;
                break;
            }
            if opener.from_entity && c == '&' {
                if let Some((decoded, len)) = decode_html_entity(self.rest()) {
                    if opener.is_end(decoded) {
                        self.pos += len;
                        break;
                    }
                    // An entity that does not close the string decodes to
                    // its character as content (`&quot;b &amp; c&quot;` →
                    // `b & c`); real-quote-opened strings keep `&…;` literal.
                    let text =
                        owned.get_or_insert_with(|| String::from(&self.src[start..content_end]));
                    text.push(decoded);
                    self.pos += len;
                    raw_delim_tail = is_delimiter(decoded);
                    continue;
                }
            }
            if c == '\\' {
                // Decode into an owned buffer; materialize the verbatim
                // prefix first so nothing decoded lands in a borrowed slice.
                let text = owned.get_or_insert_with(|| String::from(&self.src[start..content_end]));
                self.bump_char();
                // The escape target is the stop comma: close before it,
                // abandoning the backslash (`"y"\, …` → `"y"` + separator).
                if stop_at_index == Some(self.pos) {
                    truncated = true;
                    break;
                }
                if self.read_escape(text)? {
                    // Truncated `\u` escape at end of input: drop it and end
                    // the string here (reference: removing the unicode char
                    // and ending the string).
                    self.pos = self.src.len();
                    truncated = true;
                    break;
                }
                raw_delim_tail = false;
                continue;
            }
            if opener.is_end(c) {
                let quote_pos = self.pos;
                self.bump_char();
                // In stop mode any quote ends the string and is consumed as
                // the repaired closer (reference behavior).
                if stop_at_delimiter {
                    truncated = true;
                    break;
                }
                let content: Cow<'_, str> = match &owned {
                    Some(text) => Cow::Borrowed(text.as_str()),
                    None => Cow::Borrowed(&self.src[start..content_end]),
                };
                if self.end_quote_is_real(0, &content)? {
                    break;
                }
                // A quote immediately followed by `\` is a hard error in the
                // reference (`"y"\`); the fold policy keeps it as content and
                // skips the comma/delimiter restarts below.
                if self.peek_char() == Some('\\') {
                    self.pos = quote_pos;
                    self.bump_char();
                    if let Some(text) = owned.as_mut() {
                        text.push('"');
                    }
                    content_end = self.pos;
                    raw_delim_tail = false;
                    continue;
                }
                // A quote that is not a valid end quote triggers the
                // reference repair rules before falling back to keeping it
                // as content:
                //   quote preceded by a raw `,`  → close at that comma;
                //   quote preceded by any other raw delimiter → restart,
                //   stopping at the first unquoted-string delimiter.
                // Escaped characters (`\,`, `\:`) are string content, not
                // structure, so they never trigger a restart.
                let prev = self.src[..quote_pos]
                    .char_indices()
                    .rev()
                    .find(|&(_, ch)| !is_ws(ch));
                match prev {
                    Some((idx, ',')) if !escaped_at(self.src, idx) => {
                        if stop_at_index != Some(idx) {
                            stop_at_index = Some(idx);
                            self.pos = start;
                            content_end = start;
                            owned = None;
                            continue;
                        }
                    }
                    Some((idx, ch))
                        if is_delimiter(ch) && !escaped_at(self.src, idx) && !stop_at_delimiter =>
                    {
                        stop_at_delimiter = true;
                        self.pos = start;
                        content_end = start;
                        owned = None;
                        continue;
                    }
                    _ => {}
                }
                // Not an end quote: keep it as an escaped quote in the value
                // (verbatim: the consumed source quote is exactly the `"`
                // that lands in the content).
                self.pos = quote_pos;
                self.bump_char();
                if let Some(text) = owned.as_mut() {
                    text.push('"');
                }
                content_end = self.pos;
                raw_delim_tail = false;
                continue;
            }
            if stop_at_delimiter && is_unquoted_delimiter(c) {
                // A URL must not break on the `//` of its scheme
                // (`"https://…` → keep scanning url chars, as in scan_word).
                let is_url = {
                    let content: &str = match &owned {
                        Some(text) => text.as_str(),
                        None => &self.src[start..content_end],
                    };
                    c == '/' && content.ends_with(':') && is_url_scheme(content)
                };
                if is_url {
                    let url_start = self.pos;
                    while matches!(self.peek_char(), Some(u) if is_url_char(u)) {
                        self.bump_char();
                    }
                    let url = &self.src[url_start..self.pos];
                    if let Some(text) = owned.as_mut() {
                        text.push_str(url);
                    }
                    content_end = self.pos;
                    if let Some(&last) = url.as_bytes().last() {
                        raw_delim_tail = last < 0x80 && is_delimiter(last as char);
                    }
                    continue;
                }
                // The delimiter stays unconsumed for the parser:
                // `["hello]` closes the string before the `]`.
                truncated = true;
                break;
            }
            // Plain content char (typically non-ASCII): verbatim advance.
            // When `owned` is already materialized (an escape was decoded
            // earlier), the byte-range flushes above start at `self.pos`, so
            // this char must be appended directly or it would be skipped
            // (`"\u0000é"` must keep the `é`). Copy only this char —
            // `content_end` can still sit before the escape sequence.
            let char_start = self.pos;
            self.bump_char();
            if let Some(text) = owned.as_mut() {
                text.push_str(&self.src[char_start..self.pos]);
            }
            content_end = self.pos;
            raw_delim_tail = is_delimiter(c);
        }
        // The reference inserts the auto-close quote before trailing
        // whitespace when a string is truncated — trim it from the value.
        let text = match owned {
            Some(mut text) => {
                if truncated {
                    let keep = text.trim_end_matches(is_ws).len();
                    text.truncate(keep);
                }
                Cow::Owned(text)
            }
            None => {
                let raw = &self.src[start..content_end];
                if truncated {
                    Cow::Borrowed(raw.trim_end_matches(is_ws))
                } else {
                    Cow::Borrowed(raw)
                }
            }
        };
        Ok(Token::Str { text, truncated })
    }

    /// Whether a candidate closing quote is followed by a delimiter or by the
    /// end of input, which is the reference implementation's end-quote test.
    ///
    /// `content` is the string value decoded so far: a closing bracket that
    /// would land inside an unclosed bracket of the same kind (`(72")`) does
    /// not end the string (reference: `isInsideUnclosedBracket`).
    fn end_quote_is_real(&mut self, depth: u8, content: &str) -> Result<bool, Error> {
        let save = self.pos;
        self.skip_line_trivia();
        let real = match self.peek_char() {
            None => true,
            // Inside a container the closer after the quote (`}`, `]`, …)
            // belongs to that container, not to the string — `{"a": "x{"}`
            // is valid JSON. The unclosed-bracket heuristic only disambiguates
            // *top-level* strings whose embedded quote is followed by a stray
            // bracket (reference: `"the set {a, b"} more"`).
            Some(c) if is_delimiter(c) => {
                self.in_container || !is_inside_unclosed_bracket(content, c)
            }
            Some(c) if is_quote(c) && depth == 0 => {
                // `"The TV is 72""`: this quote is embedded when the next one
                // is a real end quote.
                self.bump_char();
                !self.end_quote_is_real(depth + 1, content)?
            }
            Some(_) => false,
        };
        self.pos = save;
        Ok(real)
    }

    /// Skips same-line whitespace and comments for the end-quote lookahead.
    ///
    /// Newlines are never consumed — a `\n` right after the quote is itself
    /// a delimiter (reference: `parseWhitespaceAndSkipComments(false)`), so
    /// `"John"\n lastName` closes the string instead of absorbing the next
    /// line.
    fn skip_line_trivia(&mut self) {
        loop {
            let before = self.pos;
            while let Some(c) = self.peek_char() {
                let skip = match c {
                    ' ' | '\r' | '\t' => true,
                    '\n' => false,
                    c => is_special_ws(c) && self.opts.repairs(Repairs::WHITESPACE),
                };
                if !skip {
                    break;
                }
                self.pos += c.len_utf8();
            }
            if self.opts.repairs(Repairs::COMMENTS) {
                self.skip_comment();
            }
            if self.pos == before {
                break;
            }
        }
    }
}

/// End of the run of plain string content starting at `i`: the first `&`,
/// `\`, `end_byte` (the family's ASCII quote), or non-ASCII byte (typographic
/// quotes), else the end of input. Long runs advance in 8-byte SWAR strides;
/// the scalar tail decides the exact stop byte.
fn plain_run_end(bytes: &[u8], mut i: usize, end_byte: u8) -> usize {
    use crate::swar::{broadcast, load_word, non_ascii_lanes, zero_lanes};
    let (amp, backslash, end) = (broadcast(b'&'), broadcast(b'\\'), broadcast(end_byte));
    while i + 8 <= bytes.len() {
        let word = load_word(bytes, i);
        // One branch for all four stop classes (see `zero_lanes`).
        let stop = zero_lanes(word ^ amp)
            | zero_lanes(word ^ backslash)
            | zero_lanes(word ^ end)
            | non_ascii_lanes(word);
        if stop != 0 {
            break;
        }
        i += 8;
    }
    while i < bytes.len() {
        let b = bytes[i];
        if b >= 0x80 || b == b'&' || b == b'\\' || b == end_byte {
            break;
        }
        i += 1;
    }
    i
}

/// Whether `content` contains more opening than closing copies of the
/// bracket that `close` would close — i.e. the closer would be inside an
/// unclosed bracket (reference: `isInsideUnclosedBracket`).
fn is_inside_unclosed_bracket(content: &str, close: char) -> bool {
    match close {
        ')' => content.matches('(').count() > content.matches(')').count(),
        ']' => content.matches('[').count() > content.matches(']').count(),
        '}' => content.matches('{').count() > content.matches('}').count(),
        _ => false,
    }
}

/// Whether the byte at `idx` is an escape target (preceded by an odd run of
/// backslashes), i.e. `\,` is an escaped comma but `",` is not.
fn escaped_at(src: &str, idx: usize) -> bool {
    src.as_bytes()[..idx]
        .iter()
        .rev()
        .take_while(|b| **b == b'\\')
        .count()
        % 2
        == 1
}

impl Lexer<'_> {
    /// Whether every repair pass is disabled.
    fn strict(&self) -> bool {
        self.opts.repairs == Repairs::NONE
    }

    /// Decodes one escape sequence, repairing the JavaScript-only ones.
    ///
    /// Returns `true` when a truncated `\u` escape at end of input was
    /// dropped and the surrounding string must end there.
    fn read_escape(&mut self, text: &mut String) -> Result<bool, Error> {
        let Some(c) = self.bump_char() else {
            // Trailing backslash at end of input: drop it (reference: an
            // unknown escape char is removed) and let the truncation repair
            // close the string.
            return Ok(false);
        };
        if let Some(decoded) = simple_escape(c) {
            text.push(decoded);
            return Ok(false);
        }
        match c {
            'u' => return self.read_unicode_escape(text),
            'x' => {
                let hi = self.peek_char().and_then(hex_value);
                let lo = self.peek_second().and_then(hex_value);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        self.bump_char();
                        self.bump_char();
                        push_code(text, hi * 16 + lo);
                    }
                    _ => {
                        if self.strict() {
                            return Err(Error::new(ErrorKind::InvalidEscape, self.pos));
                        }
                        text.push('x');
                    }
                }
            }
            other => text.push(other),
        }
        Ok(false)
    }

    /// Decodes `\uXXXX`, joining a surrogate pair when one follows.
    ///
    /// Returns `true` when fewer than four hex digits remain and the input
    /// ends right after them — the truncated escape is dropped and the
    /// string must end (reference behavior).
    fn read_unicode_escape(&mut self, text: &mut String) -> Result<bool, Error> {
        match self.read_hex4() {
            Some(code) => {
                if (0xD800..=0xDBFF).contains(&code) && self.rest().starts_with("\\u") {
                    let save = self.pos;
                    self.pos += 2;
                    match self.read_hex4() {
                        Some(low) if (0xDC00..=0xDFFF).contains(&low) => {
                            let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            push_code(text, combined);
                            return Ok(false);
                        }
                        _ => self.pos = save,
                    }
                }
                push_code(text, code);
                Ok(false)
            }
            None => {
                if self.strict() {
                    return Err(Error::new(ErrorKind::InvalidUnicodeEscape, self.pos));
                }
                let rest = self.rest();
                let hex_run = rest.chars().take_while(|c| hex_value(*c).is_some()).count();
                if hex_run == rest.len() && hex_run < 4 {
                    // Fewer than four hex digits and then EOF: drop the
                    // escape and end the string here.
                    return Ok(true);
                }
                text.push('u');
                Ok(false)
            }
        }
    }

    /// Reads exactly four hexadecimal digits, restoring the position on failure.
    fn read_hex4(&mut self) -> Option<u32> {
        let save = self.pos;
        let mut code = 0u32;
        for _ in 0..4 {
            match self.peek_char().and_then(hex_value) {
                Some(digit) => {
                    code = code * 16 + digit;
                    self.bump_char();
                }
                None => {
                    self.pos = save;
                    return None;
                }
            }
        }
        Some(code)
    }
}

impl<'a> Lexer<'a> {
    fn at_end_of_number(&self) -> bool {
        match self.peek_char() {
            None => true,
            Some(c) => is_delimiter(c) || is_ws(c),
        }
    }

    fn scan_number_or_word(&mut self, key_position: bool) -> Result<Token<'a>, Error> {
        match self.scan_number()? {
            Some(token) => Ok(token),
            None => Ok(self.scan_word(key_position)),
        }
    }

    /// Scans a number, normalizing its text into valid JSON.
    ///
    /// Returns `None` when the text turns out not to be a number, so the caller
    /// can retry it as a bare word.
    ///
    /// A number that is already valid JSON (the common case) is recognized by
    /// a cheap grammar pre-scan and **borrows** the input slice — no `String`
    /// is built. Anything needing repair falls into the slow path, which
    /// still borrows when the raw slice happens to equal the normalized text.
    fn scan_number(&mut self) -> Result<Option<Token<'a>>, Error> {
        if let Some(end) = self.clean_number_end() {
            let text = Cow::Borrowed(&self.src[self.pos..end]);
            self.pos = end;
            return Ok(Some(Token::Num {
                text,
                truncated: false,
            }));
        }
        let start = self.pos;
        let mut num = String::new();
        let mut invalid = false;
        let mut truncated = false;
        if self.peek_char() == Some('-') {
            num.push('-');
            self.bump_char();
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) && self.at_end_of_number()
            {
                num.push('0');
                truncated = self.at_end();
            }
        }
        if self.peek_char() == Some('0')
            && matches!(self.peek_second(), Some(c) if c.is_ascii_digit())
        {
            invalid = true;
        }
        self.take_digits(&mut num);
        if self.peek_char() == Some('.') {
            if num.is_empty() || num == "-" {
                num.push('0');
            }
            num.push('.');
            self.bump_char();
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                num.push('0');
                truncated = self.at_end();
            }
            self.take_digits(&mut num);
        }
        if self.pos == start {
            return Ok(None);
        }
        if matches!(self.peek_char(), Some('e' | 'E')) {
            if num == "-" {
                invalid = true;
            }
            if let Some(c) = self.bump_char() {
                num.push(c);
            }
            if matches!(self.peek_char(), Some('-' | '+')) {
                if let Some(c) = self.bump_char() {
                    num.push(c);
                }
            }
            if !matches!(self.peek_char(), Some(c) if c.is_ascii_digit()) {
                num.push('0');
                truncated = self.at_end();
            }
            self.take_digits(&mut num);
        }
        if !self.at_end_of_number() {
            // Not a number after all: let the caller scan it as a bare word.
            self.pos = start;
            return Ok(None);
        }
        let raw = &self.src[start..self.pos];
        if invalid {
            if self.strict() {
                return Err(Error::new(ErrorKind::UnexpectedCharacter('0'), start));
            }
            // Leading zeros cannot be respelled as a JSON number, so the
            // digits are kept verbatim as a string (borrowed, not copied).
            return Ok(Some(Token::Str {
                text: Cow::Borrowed(raw),
                truncated: false,
            }));
        }
        if raw == num {
            // The slow path recognized a number that needs no repair (e.g.
            // followed by special whitespace the fast path declined): borrow
            // the slice instead of keeping the temporary buffer.
            return Ok(Some(Token::Num {
                text: Cow::Borrowed(raw),
                truncated,
            }));
        }
        if !self.opts.repairs(Repairs::NUMBERS) {
            // The literal needed a repair that is switched off.
            return Err(Error::new(ErrorKind::UnexpectedCharacter('.'), start));
        }
        Ok(Some(Token::Num {
            text: Cow::Owned(num),
            truncated,
        }))
    }

    /// End position of a number at `self.pos` that is already valid JSON,
    /// or `None` when it needs the repair-oriented slow path.
    fn clean_number_end(&self) -> Option<usize> {
        let bytes = self.src.as_bytes();
        let mut i = self.pos;
        if i < bytes.len() && bytes[i] == b'-' {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        if bytes[i] == b'0' {
            i += 1;
            // A leading zero followed by more digits is invalid JSON (the
            // slow path demotes it to a string).
            if i < bytes.len() && bytes[i].is_ascii_digit() {
                return None;
            }
        } else if bytes[i].is_ascii_digit() {
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        } else {
            return None;
        }
        if i < bytes.len() && bytes[i] == b'.' {
            i += 1;
            let digits = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == digits {
                return None;
            }
        }
        if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
            i += 1;
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            let digits = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i == digits {
                return None;
            }
        }
        if i == self.pos {
            return None;
        }
        // Boundary check mirroring `at_end_of_number`; non-ASCII after the
        // number (e.g. special whitespace) defers to the slow path.
        match bytes.get(i) {
            None => Some(i),
            Some(&b) if b < 0x80 => {
                let c = b as char;
                if is_delimiter(c) || is_ws(c) {
                    Some(i)
                } else {
                    None
                }
            }
            Some(_) => None,
        }
    }

    fn take_digits(&mut self, out: &mut String) {
        let bytes = self.src.as_bytes();
        let begin = self.pos;
        while self.pos < bytes.len() && bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        if self.pos > begin {
            out.push_str(&self.src[begin..self.pos]);
        }
    }
}

impl<'a> Lexer<'a> {
    /// Scans a bare word: an unquoted key, an unquoted value, or a function name.
    ///
    /// Spaces and tabs are word *content* (matching the reference: `hello
    /// world` is one string, and numbers still stop at whitespace); only line
    /// breaks, quotes, and structural delimiters end a word.
    fn scan_word(&mut self, key_position: bool) -> Token<'a> {
        let start = self.pos;
        // Byte-wise inner loop: JSON/prose words are ASCII, so avoid
        // per-byte UTF-8 decoding. Typographic quotes (all non-ASCII) are
        // the only non-ASCII word terminators.
        let bytes = self.src.as_bytes();
        while self.pos < bytes.len() {
            let b = bytes[self.pos];
            if b < 0x80 {
                let stop = matches!(
                    b,
                    b'\n'
                        | b'\r'
                        | b'"'
                        | b'\''
                        | b'('
                        | b')'
                        | b','
                        | b'/'
                        | b'+'
                        | b'['
                        | b']'
                        | b'{'
                        | b'}'
                ) || (key_position && b == b':');
                if stop {
                    break;
                }
                self.pos += 1;
            } else {
                let Some(c) = self.src[self.pos..].chars().next() else {
                    break;
                };
                if is_quote(c) {
                    break;
                }
                self.pos += c.len_utf8();
            }
        }
        // Trailing whitespace is trivia, not word content: `true ` stays `true`.
        {
            let span = &self.src[start..self.pos];
            let trimmed = span.trim_end_matches(is_ws);
            self.pos = start + trimmed.len();
        }
        if self.pos == start {
            // Never return an empty token: that would stall the parser.
            self.bump_char();
        }
        // A URL must survive the `//` comment scanner; the scheme may follow
        // other words and spaces (`see https://…`), so test the last segment.
        let span = &self.src[start..self.pos];
        if span.ends_with(':') && self.rest().starts_with("//") {
            let scheme = span.rsplit(is_ws).next().unwrap_or(span);
            if is_url_scheme(scheme) {
                while matches!(self.peek_char(), Some(c) if is_url_char(c)) {
                    self.bump_char();
                }
            }
        }
        let text = &self.src[start..self.pos];
        // Trailing whitespace was rewound above, so `at_end()` alone misses
        // the EOF case (`foo  ` at end): if only whitespace remains, the word
        // was lexed at logical EOF and can still grow when more input arrives
        // (`foo  ` + `bar` is one word, not a finished value + a new token).
        let truncated = self.at_end() || self.rest().chars().all(is_ws);
        if self.peek_char() == Some('"') {
            // Missing start quote: in `abc"` the trailing `"` is the end
            // quote of a string that never opened — skip it (reference:
            // parseUnquotedString's trailing-quote repair). Skipped after
            // `text` is captured so the quote is not word content.
            self.bump_char();
        }
        let keywords = self.opts.repairs(Repairs::KEYWORDS);
        match text {
            "true" => Token::Bool(true),
            "false" => Token::Bool(false),
            "null" => Token::Null,
            "True" if keywords => Token::Bool(true),
            "False" if keywords => Token::Bool(false),
            "None" if keywords => Token::Null,
            "undefined" if keywords => Token::Undefined,
            // Truncated-keyword promotion is a *value*-position repair: a
            // cut-off key like `{"t` must stay a word so `Allow::KEY` (not
            // `Allow::BOOL`) decides whether it is kept — otherwise `t:`
            // would be rewritten to the key `"true"`.
            _ if truncated && !key_position && self.opts.repairs(Repairs::TRUNCATION) => {
                if self.opts.allows(Allow::BOOL) {
                    if "true".starts_with(text) {
                        return Token::Bool(true);
                    }
                    if "false".starts_with(text) {
                        return Token::Bool(false);
                    }
                }
                if self.opts.allows(Allow::NULL) && "null".starts_with(text) {
                    return Token::Null;
                }
                Token::Word { text, truncated }
            }
            _ => Token::Word { text, truncated },
        }
    }

    /// Scans a JavaScript regular expression literal as a string value.
    fn scan_regex(&mut self) -> Token<'a> {
        let start = self.pos;
        self.bump_char();
        while let Some(c) = self.peek_char() {
            if c == '\\' {
                self.bump_char();
                self.bump_char();
                continue;
            }
            self.bump_char();
            if c == '/' {
                break;
            }
        }
        Token::Str {
            text: Cow::Borrowed(&self.src[start..self.pos]),
            truncated: self.at_end(),
        }
    }
}

fn push_code(text: &mut String, code: u32) {
    text.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
}
