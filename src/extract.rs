//! Pulling JSON values out of prose, markdown, and log lines.

use alloc::vec::Vec;

use crate::chars::{is_double_quote, is_single_quote, is_ws};

/// Characters that end a bare scalar during extraction.
fn is_scalar_delimiter(c: char) -> bool {
    c == ',' || c == '}' || c == ']' || c == ')' || c == ';' || is_ws(c)
}

/// Whether a candidate start character can begin a JSON value.
///
/// Bare words are deliberately excluded: prose must not be mistaken for data.
/// A `.` only counts when it starts a decimal like `.5` — otherwise prose such
/// as an ellipsis (`...`) would be mistaken for data. `next` is the character
/// immediately after `c`.
fn is_value_start(c: char, next: Option<char>) -> bool {
    match c {
        '{' | '[' | '-' | '+' => true,
        '.' => matches!(next, Some('0'..='9')),
        // U+0060/U+00B4 are repair-only quote pairings (`` `foo´ ``); as a
        // value start they would mistake markdown backticks for data.
        c => {
            c.is_ascii_digit()
                || is_double_quote(c)
                || (is_single_quote(c) && c != '\u{0060}' && c != '\u{00B4}')
        }
    }
}

/// Returns the first JSON value found in `input`, trimmed of surrounding prose.
///
/// A fenced markdown block wins over loose text; inside loose text the first
/// balanced `{...}` or `[...]` wins.
///
/// ```
/// let text = "Sure! Here is the data:\n```json\n{\"ok\": true}\n```\nEnjoy.";
/// assert_eq!(jsonfix::extract(text), Some("{\"ok\": true}"));
/// ```
#[must_use]
pub fn extract(input: &str) -> Option<&str> {
    if let Some((start, end)) = fenced_block(input) {
        return Some(input[start..end].trim());
    }
    let start = first_value_start(input)?;
    let (_, end) = scan_value(input, start, false)?;
    Some(input[start..end].trim())
}

/// Returns everything from the first JSON value to the end of the input.
///
/// This is what a streaming caller wants: as more tokens arrive the tail keeps
/// growing. Returns an empty string while no value has started yet.
///
/// ```
/// assert_eq!(jsonfix::extract_partial("partial: {\"a\": [1, 2"), "{\"a\": [1, 2");
/// assert_eq!(jsonfix::extract_partial("no json yet"), "");
/// ```
#[must_use]
pub fn extract_partial(input: &str) -> &str {
    match first_value_start(input) {
        Some(start) => &input[start..],
        None => "",
    }
}

/// Returns every top-level value in `input`, in order (NDJSON and log lines).
///
/// ```
/// let log = "{\"n\": 1}\n{\"n\": 2}";
/// assert_eq!(jsonfix::extract_all(log), vec!["{\"n\": 1}", "{\"n\": 2}"]);
/// ```
#[must_use]
pub fn extract_all(input: &str) -> Vec<&str> {
    let mut values = Vec::new();
    let mut pos = 0usize;
    // The first `{`/`[` at or after `pos`, cached across iterations: a
    // stretch with no bracket is searched once, not once per value in it
    // (prose full of bare scalars would otherwise rescan to the end each time).
    let mut bracket = first_bracket(input);
    while pos < input.len() {
        if bracket.is_some_and(|at| at < pos) {
            bracket = first_bracket(&input[pos..]).map(|at| pos + at);
        }
        let start = match bracket {
            Some(at) => at,
            None => match first_scalar_start(&input[pos..]) {
                Some(offset) => pos + offset,
                None => break,
            },
        };
        let Some((_, end)) = scan_value(input, start, false) else {
            break;
        };
        values.push(input[start..end].trim());
        pos = end.max(start + 1);
    }
    values
}

/// The byte offset of the first character that can start a value.
///
/// Structural values (`{`, `[`) are preferred; a bare scalar is only used when
/// there is nothing better in the input.
fn first_value_start(input: &str) -> Option<usize> {
    first_bracket(input).or_else(|| first_scalar_start(input))
}

/// The byte offset of the first `{` or `[`.
fn first_bracket(input: &str) -> Option<usize> {
    input.bytes().position(|b| b == b'{' || b == b'[')
}

/// The byte offset of the first character that can start a bare scalar.
fn first_scalar_start(input: &str) -> Option<usize> {
    let mut chars = input.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        let next = chars.peek().map(|&(_, next)| next);
        if is_value_start(c, next) {
            return Some(offset);
        }
    }
    None
}

/// Finds the first non-empty fenced code block, preferring one tagged `json`.
fn fenced_block(input: &str) -> Option<(usize, usize)> {
    let mut first = None;
    let mut search = 0usize;
    while let Some(offset) = input[search..].find("```") {
        let open = search + offset + 3;
        let line_end = input[open..]
            .find('\n')
            .map_or(input.len(), |o| open + o + 1);
        let spec = input[open..line_end].trim();
        // Case-insensitive `json` tag check without allocating a lowercased
        // copy per fence: compare the first four bytes in place. A non-ASCII
        // boundary at byte 4 makes `get(..4)` return `None` (not a json tag).
        let is_json = spec
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("json"));
        let body_start = line_end.min(input.len());
        let body_end = input[body_start..]
            .find("```")
            .map_or(input.len(), |o| body_start + o);
        if body_end > body_start && !input[body_start..body_end].trim().is_empty() {
            if is_json {
                return Some((body_start, body_end));
            }
            first.get_or_insert((body_start, body_end));
        }
        if body_end <= search {
            break;
        }
        // Resume *after* this fence's closing ``` — landing on the closer
        // itself would re-read it as an opener and mine the prose between
        // fences as a fake candidate body.
        search = body_end.saturating_add(3).min(input.len());
    }
    first
}

/// Scans the value starting at `start`, returning its `[start, end)` span.
///
/// When `partial` is set, an unterminated value extends to the end of input.
fn scan_value(input: &str, start: usize, partial: bool) -> Option<(usize, usize)> {
    let first = input[start..].chars().next()?;
    match first {
        '{' | '[' => scan_container(input, start, partial),
        c if is_double_quote(c) || is_single_quote(c) => {
            scan_string(input, start, c, partial).map(|end| (start, end))
        }
        _ => {
            let end = input[start..]
                .char_indices()
                .find(|(_, c)| is_scalar_delimiter(*c))
                .map_or(input.len(), |(offset, _)| start + offset);
            (end > start).then_some((start, end))
        }
    }
}

/// Bracket-matching scan that is aware of strings and comments.
///
/// Byte-wise: every structural byte is ASCII, so only non-ASCII bytes (which
/// may be typographic quotes) are decoded.
fn scan_container(input: &str, start: usize, partial: bool) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut i = start;
    while i < bytes.len() {
        if let Some(open) = quote {
            if open == '"' || open == '\'' {
                // Only the quote itself and `\` matter inside a string opened
                // by an ASCII quote: skip everything else in bulk.
                i = crate::swar::find_either(bytes, i, open as u8, b'\\');
                if i == bytes.len() {
                    break;
                }
            }
            let c = char_at(input, i);
            let next = i + c.len_utf8();
            if c == '\\' && next < bytes.len() {
                // Skip the escaped character, whatever its width.
                i = next + char_at(input, next).len_utf8();
                continue;
            }
            if closes(open, c) {
                quote = None;
            }
            i = next;
            continue;
        }
        let c = char_at(input, i);
        let next = i + c.len_utf8();
        match c {
            c if is_double_quote(c) || is_single_quote(c) => quote = Some(c),
            '/' if matches!(bytes.get(next), Some(b'/' | b'*')) => {
                i = comment_end(input, i);
                continue;
            }
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start, next));
                }
            }
            _ => {}
        }
        i = next;
    }
    partial.then_some((start, input.len()))
}

/// The character starting at byte `i` (a char boundary before the end).
fn char_at(input: &str, i: usize) -> char {
    match input.as_bytes()[i] {
        byte if byte.is_ascii() => char::from(byte),
        _ => input[i..].chars().next().unwrap_or('\0'),
    }
}

/// Scans a quoted string, returning the offset just past its closing quote.
fn scan_string(input: &str, start: usize, open: char, partial: bool) -> Option<usize> {
    let mut escaped = false;
    for (offset, c) in input[start..].char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
            continue;
        }
        if closes(open, c) {
            return Some(start + offset + c.len_utf8());
        }
    }
    partial.then_some(input.len())
}

/// Whether `c` closes a string opened by `open`.
fn closes(open: char, c: char) -> bool {
    if open == '"' {
        c == '"'
    } else if open == '\'' {
        c == '\''
    } else if is_double_quote(open) {
        is_double_quote(c) || c == '"'
    } else {
        is_single_quote(c) || c == '\''
    }
}

/// End of the `//` or `/* */` comment starting at byte `at`: just past the
/// line's newline or the closing `*/`, or the end of input when unterminated.
fn comment_end(input: &str, at: usize) -> usize {
    let closer = if input[at..].starts_with("//") {
        "\n"
    } else {
        "*/"
    };
    input[at + 2..]
        .find(closer)
        .map_or(input.len(), |end| at + 2 + end + closer.len())
}
