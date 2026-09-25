//! Parse errors with byte-accurate positions.

use core::fmt;

/// What went wrong while repairing or parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The input ended before a complete value was read.
    UnexpectedEnd,
    /// A character appeared where the grammar does not allow it.
    UnexpectedCharacter(char),
    /// An object key was expected but something else was found.
    ExpectedObjectKey,
    /// A `:` was expected between an object key and its value.
    ExpectedColon,
    /// The input does not contain a JSON value at all.
    NoValueFound,
    /// An escape sequence was malformed and repairs were disabled.
    InvalidEscape,
    /// A `\u` escape did not contain four hexadecimal digits.
    InvalidUnicodeEscape,
    /// Two values were not separated by a comma.
    ExpectedComma,
    /// A comma appeared before a closing bracket or at the end of the input.
    TrailingComma,
    /// A second value appeared at the top level while NDJSON repair was off.
    TrailingValue,
    /// A bare word appeared as a value while unquoted-value repair was off.
    UnquotedValue,
    /// The input nested objects, arrays, or groups deeper than
    /// [`MAX_NESTING_DEPTH`](crate::MAX_NESTING_DEPTH). Raised instead of
    /// risking a stack overflow, regardless of which repair passes are enabled.
    DepthLimitExceeded,
    /// The byte input was not valid UTF-8; the position is the offset of the
    /// first byte that does not decode.
    InvalidUtf8,
}

/// A repair or parse failure, carrying the byte offset where it was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    position: usize,
}

impl Error {
    /// Builds an error from a kind and a byte offset into the input.
    pub const fn new(kind: ErrorKind, position: usize) -> Self {
        Self { kind, position }
    }

    /// Returns the class of failure.
    pub const fn kind(&self) -> &ErrorKind {
        &self.kind
    }

    /// Returns the byte offset (0-based) into the input where the failure was detected.
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Returns a stable, human-readable message for this error.
    pub fn message(&self) -> &'static str {
        match self.kind {
            ErrorKind::UnexpectedEnd => "unexpected end of input",
            ErrorKind::UnexpectedCharacter(_) => "unexpected character",
            ErrorKind::ExpectedObjectKey => "object key expected",
            ErrorKind::ExpectedColon => "colon expected",
            ErrorKind::NoValueFound => "no JSON value found",
            ErrorKind::InvalidEscape => "invalid escape sequence",
            ErrorKind::InvalidUnicodeEscape => "invalid unicode escape",
            ErrorKind::ExpectedComma => "comma expected",
            ErrorKind::TrailingComma => "trailing comma",
            ErrorKind::TrailingValue => {
                "unexpected text around the first top-level value (try extract())"
            }
            ErrorKind::UnquotedValue => "unquoted value",
            ErrorKind::DepthLimitExceeded => "maximum nesting depth exceeded",
            ErrorKind::InvalidUtf8 => "input is not valid UTF-8",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message(), self.position)?;
        if let ErrorKind::UnexpectedCharacter(c) = self.kind {
            write!(f, " ({c:?})")?;
        }
        Ok(())
    }
}

impl core::error::Error for Error {}
