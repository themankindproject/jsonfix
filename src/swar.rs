//! Safe SIMD-within-a-register (SWAR) byte helpers.
//!
//! Pure `core` arithmetic on `u64` lanes — no `unsafe`, no dependencies, no
//! intrinsics — used to skip over long runs of ordinary bytes instead of
//! testing one byte at a time. Lane operations here are endian-independent
//! (broadcast/xor/and treat all lanes identically).

/// Byte-lane broadcast: replicates `byte` into all eight lanes.
#[inline(always)]
pub(crate) fn broadcast(byte: u8) -> u64 {
    // No carries: `byte <= 0xFF` fits in every 8-bit column of the product.
    byte as u64 * 0x0101_0101_0101_0101
}

#[inline(always)]
fn haszero(v: u64) -> bool {
    // Classic haszero: (v - 1 per lane) sets a lane's high bit iff that
    // lane was zero (borrow from a zero lane flips its high bit, and the
    // `!v` term requires the original lane to have been zero).
    v.wrapping_sub(0x0101_0101_0101_0101) & !v & 0x8080_8080_8080_8080 != 0
}

/// Whether any byte of `word` equals `needle_mask`'s lane value (`< 0x80`).
#[inline(always)]
pub(crate) fn hasbyte(word: u64, needle_mask: u64) -> bool {
    haszero(word ^ needle_mask)
}

/// Whether any byte of `word` is `< limit_mask`'s lane value (limit ≤ 0x80).
///
/// Borrow trick: a lane below `limit` borrows, setting its high bit while
/// the original high bit was clear; lanes ≥ 0x80 can never match because
/// `!word` clears their high-bit term.
#[inline(always)]
pub(crate) fn hasless(word: u64, limit_mask: u64) -> bool {
    word.wrapping_sub(limit_mask) & !word & 0x8080_8080_8080_8080 != 0
}

/// Whether any byte of `word` is non-ASCII (`>= 0x80`).
#[inline(always)]
pub(crate) fn has_non_ascii(word: u64) -> bool {
    word & 0x8080_8080_8080_8080 != 0
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
