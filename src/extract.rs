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
pub fn extract_all(input: &str) -> Vec<&str> {
    let mut values = Vec::new();
    let mut pos = 0usize;
    while pos < input.len() {
        let Some(offset) = first_value_start(&input[pos..]) else {
            break;
        };
        let start = pos + offset;
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
    let mut fallback = None;
    let mut chars = input.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        if c == '{' || c == '[' {
            return Some(offset);
        }
        let next = chars.peek().copied().map(|(_, next)| next);
        if fallback.is_none() && is_value_start(c, next) {
            fallback = Some(offset);
        }
    }
    fallback
}

/// Finds the first non-empty fenced code block, preferring one tagged `json`.
fn fenced_block(input: &str) -> Option<(usize, usize)> {
    let mut candidates: Vec<(bool, usize, usize)> = Vec::new();
    let mut search = 0usize;
    while let Some(offset) = input[search..].find("```") {
        let open = search + offset + 3;
        let line_end = input[open..]
            .find('\n')
            .map_or(input.len(), |o| open + o + 1);
        let spec = input[open..line_end].trim();
        let is_json = spec.to_ascii_lowercase().starts_with("json");
        let body_start = line_end.min(input.len());
        let body_end = input[body_start..]
            .find("```")
            .map_or(input.len(), |o| body_start + o);
        if body_end > body_start && !input[body_start..body_end].trim().is_empty() {
            candidates.push((is_json, body_start, body_end));
        }
        if body_end <= search {
            break;
        }
        // Resume *after* this fence's closing ``` — landing on the closer
        // itself would re-read it as an opener and mine the prose between
        // fences as a fake candidate body.
        search = body_end.saturating_add(3).min(input.len());
    }
    let chosen = candidates
        .iter()
        .find(|(is_json, _, _)| *is_json)
        .or_else(|| candidates.first())?;
    Some((chosen.1, chosen.2))
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
fn scan_container(input: &str, start: usize, partial: bool) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut chars = input[start..].char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        if let Some(open) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if closes(open, c) {
                quote = None;
            }
            continue;
        }
        match c {
            c if is_double_quote(c) || is_single_quote(c) => quote = Some(c),
            '/' if matches!(chars.peek(), Some((_, '/' | '*'))) => {
                skip_comment(input, start + offset, &mut chars);
            }
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((start, start + offset + c.len_utf8()));
                }
            }
            _ => {}
        }
    }
    partial.then_some((start, input.len()))
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

/// Skips a `//` or `/* */` comment while scanning.
fn skip_comment(
    input: &str,
    at: usize,
    chars: &mut core::iter::Peekable<core::str::CharIndices<'_>>,
) {
    if input[at..].starts_with("//") {
        for (_, c) in chars.by_ref() {
            if c == '\n' {
                break;
            }
        }
    } else {
        chars.next();
        while let Some((_, c)) = chars.next() {
            if c == '*' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                chars.next();
                break;
            }
        }
    }
}
