//! Safe SIMD-within-a-register (SWAR) byte helpers.
//!
//! Pure `core` arithmetic on `u64` lanes — no `unsafe`, no dependencies, no
//! intrinsics — used to skip over long runs of ordinary bytes instead of
//! testing one byte at a time. Lane operations here are endian-independent
//! (broadcast/xor/and treat all lanes identically).

const LO: u64 = 0x0101_0101_0101_0101;
const HI: u64 = 0x8080_8080_8080_8080;

/// Byte-lane broadcast: replicates `byte` into all eight lanes.
#[inline(always)]
pub(crate) fn broadcast(byte: u8) -> u64 {
    // No carries: `byte <= 0xFF` fits in every 8-bit column of the product.
    byte as u64 * LO
}

/// Nonzero iff some lane of `v` is zero. Classic haszero: (v - 1 per lane)
/// sets a lane's high bit iff that lane was zero (borrow from a zero lane
/// flips its high bit, and the `!v` term requires the original lane to have
/// been zero). Individual flagged lanes may be off after the first zero lane
/// (borrow propagation), but "any lane" is exact — and so is the OR of
/// several such masks, which lets callers test many needles with one branch.
#[inline(always)]
pub(crate) fn zero_lanes(v: u64) -> u64 {
    v.wrapping_sub(LO) & !v & HI
}

/// Nonzero iff some lane of `word` is `< limit_mask`'s lane value (limit ≤
/// 0x80). Borrow trick: a lane below `limit` borrows, setting its high bit
/// while the original high bit was clear; lanes ≥ 0x80 never match because
/// `!word` clears their high-bit term.
#[inline(always)]
pub(crate) fn less_lanes(word: u64, limit_mask: u64) -> u64 {
    word.wrapping_sub(limit_mask) & !word & HI
}

/// Nonzero iff some lane of `word` is non-ASCII (`>= 0x80`).
#[inline(always)]
pub(crate) fn non_ascii_lanes(word: u64) -> u64 {
    word & HI
}

/// Loads eight bytes at `bytes[i..i + 8]` as a `u64` (caller checks length).
///
/// `i + 8 <= bytes.len()` must hold; unaligned loads are fine for `u64::from_ne_bytes`.
#[inline(always)]
pub(crate) fn load_word(bytes: &[u8], i: usize) -> u64 {
    let mut chunk = [0u8; 8];
    chunk.copy_from_slice(&bytes[i..i + 8]);
    u64::from_ne_bytes(chunk)
}

/// The first index at or after `i` holding `a` or `b` (both `< 0x80`), or
/// `bytes.len()`. Clean stretches advance eight bytes at a time.
pub(crate) fn find_either(bytes: &[u8], mut i: usize, a: u8, b: u8) -> usize {
    let (wide_a, wide_b) = (broadcast(a), broadcast(b));
    while i + 8 <= bytes.len() {
        let word = load_word(bytes, i);
        if zero_lanes(word ^ wide_a) | zero_lanes(word ^ wide_b) != 0 {
            break;
        }
        i += 8;
    }
    while i < bytes.len() && bytes[i] != a && bytes[i] != b {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hasbyte(word: u64, needle: u64) -> bool {
        zero_lanes(word ^ needle) != 0
    }

    fn hasless(word: u64, limit: u64) -> bool {
        less_lanes(word, limit) != 0
    }

    fn has_non_ascii(word: u64) -> bool {
        non_ascii_lanes(word) != 0
    }

    /// The OR-combined stop mask the scanners use flags a word iff some byte
    /// matches one of the needles — checked against a scalar reference over
    /// every needle position and every lane value around the boundaries.
    #[test]
    fn combined_masks_match_a_scalar_reference() {
        let needles = *b"\"\\&";
        for fill in [b'a', b' ', b'~', 0x21, 0x7F, b'"' + 1, b'\\' - 1] {
            for pos in 0..8 {
                for probe in (0u8..=0xFF)
                    .step_by(3)
                    .chain([b'"', b'\\', b'&', 0x1F, 0x20, 0x80])
                {
                    let mut bytes = [fill; 8];
                    bytes[pos] = probe;
                    let word = load_word(&bytes, 0);
                    let stop = needles.iter().fold(
                        non_ascii_lanes(word) | less_lanes(word, broadcast(0x20)),
                        |acc, n| acc | zero_lanes(word ^ broadcast(*n)),
                    );
                    let want = bytes
                        .iter()
                        .any(|b| *b >= 0x80 || *b < 0x20 || needles.contains(b));
                    assert_eq!(stop != 0, want, "bytes {bytes:?}");
                }
            }
        }
    }

    #[test]
    fn detects_each_byte_value() {
        let word = load_word(b"a\"b\\c\x01de", 0); // 8 bytes
        assert!(hasbyte(word, broadcast(b'"')));
        assert!(hasbyte(word, broadcast(b'\\')));
        assert!(!hasbyte(word, broadcast(b'x')));
    }

    #[test]
    fn detects_controls_and_non_ascii() {
        let word = load_word(b"abcdefg\x01", 0);
        assert!(hasless(word, broadcast(0x20)));
        let word = load_word("日本語です!".as_bytes(), 0);
        assert!(has_non_ascii(word));
        let word = load_word(b"abcdefgh", 0);
        assert!(!hasless(word, broadcast(0x20)));
        assert!(!has_non_ascii(word));
        assert!(!hasbyte(word, broadcast(b'"')));
    }

    #[test]
    fn space_is_not_a_control() {
        let word = load_word(b"abc defg", 0);
        assert!(!hasless(word, broadcast(0x20)));
    }

    #[test]
    fn find_either_matches_a_scalar_scan() {
        let text = "plain ascii then \u{e9}\u{201c} and a quote\" far \\ away".repeat(3);
        let bytes = text.as_bytes();
        for start in 0..bytes.len() {
            let want = (start..bytes.len())
                .find(|&i| bytes[i] == b'"' || bytes[i] == b'\\')
                .unwrap_or(bytes.len());
            assert_eq!(find_either(bytes, start, b'"', b'\\'), want, "from {start}");
        }
        assert_eq!(find_either(b"", 0, b'"', b'\\'), 0);
    }
}
