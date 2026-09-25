//! Character classification shared by the lexer, the parser, and the extractor.
//!
//! The sets mirror the ones the reference JavaScript implementation
//! (`jsonrepair`) uses, so repairing the same document yields the same result.

/// ASCII whitespace that JSON itself allows between tokens.
pub(crate) fn is_json_ws(c: char) -> bool {
    matches!(c, ' ' | '\n' | '\r' | '\t')
}

/// Unicode whitespace that JSON does not allow but documents routinely contain.
pub(crate) fn is_special_ws(c: char) -> bool {
    matches!(
        c,
        '\u{00A0}' | '\u{180E}' | '\u{2000}'
            ..='\u{200B}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// Any whitespace the lexer skips: ASCII whitespace plus special whitespace.
pub(crate) fn is_ws(c: char) -> bool {
    is_json_ws(c) || is_special_ws(c)
}

/// Double-quote-like characters: `"` and the typographic variants.
pub(crate) fn is_double_quote(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}')
}

/// Single-quote-like characters: `'`, the typographic variants, and the
/// non-normalized left/right quotes `` ` `` (U+0060) and ´ (U+00B4)
/// (mirroring the reference's `isSingleQuoteLike`).
pub(crate) fn is_single_quote(c: char) -> bool {
    matches!(
        c,
        '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{0060}' | '\u{00B4}'
    )
}

/// Whether the character can open or close a string.
pub(crate) fn is_quote(c: char) -> bool {
    is_double_quote(c) || is_single_quote(c)
}

/// Characters that may follow a closing quote, per the reference implementation.
pub(crate) fn is_delimiter(c: char) -> bool {
    matches!(
        c,
        ',' | ':' | '[' | ']' | '/' | '{' | '}' | '(' | ')' | '\n' | '+'
    )
}

/// Characters that terminate an unquoted word (note: `:` is *not* included,
/// because `:` ends a key but may appear inside a bare value such as a URL).
pub(crate) fn is_unquoted_delimiter(c: char) -> bool {
    matches!(c, ',' | '[' | ']' | '/' | '{' | '}' | '\n' | '+')
}

/// Whether the character can continue a function name.
pub(crate) fn is_func_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Characters allowed in the tail of a URL, mirroring the reference regex.
pub(crate) fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.'
                | '_'
                | '~'
                | ':'
                | '/'
                | '?'
                | '#'
                | '@'
                | '!'
                | '$'
                | '&'
                | '\''
                | '('
                | ')'
                | '*'
                | '+'
                | ';'
                | '='
        )
}

/// Whether the scanned span looks like the start of a URL (`https://`).
pub(crate) fn is_url_scheme(span: &str) -> bool {
    matches!(
        span,
        "http:" | "https:" | "ftp:" | "mailto:" | "file:" | "data:" | "irc:"
    )
}

/// Decodes a hexadecimal digit.
pub(crate) fn hex_value(c: char) -> Option<u32> {
    c.to_digit(16)
}

/// Decodes a named or numeric HTML entity at the start of `fragment`.
///
/// Returns the decoded character and the number of bytes consumed, or `None`
/// when there is no complete entity. Mirrors the reference implementation's
/// `matchHtmlEntity`.
pub(crate) fn decode_html_entity(fragment: &str) -> Option<(char, usize)> {
    if !fragment.starts_with('&') {
        return None;
    }
    let semi = fragment.find(';')?;
    let entity = &fragment[..=semi];
    let named = match entity {
        "&quot;" => Some('"'),
        "&amp;" => Some('&'),
        "&lt;" => Some('<'),
        "&gt;" => Some('>'),
        "&apos;" => Some('\''),
        _ => None,
    };
    if let Some(c) = named {
        return Some((c, entity.len()));
    }
    let body = fragment.get(1..semi)?;
    let body = body.strip_prefix('#')?;
    let (radix, digits) = match body.strip_prefix(['x', 'X']) {
        Some(hex) => (16, hex),
        None => (10, body),
    };
    if digits.is_empty() {
        return None;
    }
    let code = u32::from_str_radix(digits, radix).ok()?;
    let c = char::from_u32(code)?;
    Some((c, entity.len()))
}
