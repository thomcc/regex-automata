/*!
TODO
*/

use core::{ascii, fmt, str};

#[cfg(feature = "alloc")]
use alloc::vec::Vec;

pub mod alphabet;
pub(crate) mod bytes;
#[cfg(feature = "alloc")]
pub(crate) mod determinize;
pub mod id;
#[cfg(feature = "alloc")]
pub(crate) mod lazy;
pub(crate) mod matchtypes;
pub mod prefilter;
#[cfg(feature = "alloc")]
pub(crate) mod sparse_set;
pub(crate) mod start;
#[cfg(feature = "alloc")]
pub(crate) mod syntax;

/// The offset, in bytes, that a match is delayed by in the DFAs generated by
/// this crate. (This includes lazy DFAs.)
///
/// The purpose of this delay is to support look-ahead such as \b (ASCII-only)
/// and $. In particular, both of these operators may require the
/// identification of the end of input in order to confirm a match. Not only
/// does this mean that all matches must therefore be delayed by a single byte,
/// but that a special EOI value is added to the alphabet of all DFAs. (Which
/// means that even though the alphabet of a DFA is typically all byte values,
/// the actual maximum alphabet size is 257 due to the extra EOI value.)
///
/// Since we delay matches by only 1 byte, this can't fully support a
/// Unicode-aware \b operator, which requires multi-byte look-ahead. Indeed,
/// DFAs in this crate do not support it. (It's not as simple as just
/// increasing the match offset to do it---otherwise we would---but building
/// the full Unicode-aware word boundary detection into an automaton is quite
/// tricky.)
pub(crate) const MATCH_OFFSET: usize = 1;

/// A type that wraps a single byte with a convenient fmt::Debug impl that
/// escapes the byte.
pub(crate) struct DebugByte(pub u8);

impl fmt::Debug for DebugByte {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // 10 bytes is enough to cover any output from ascii::escape_default.
        let mut bytes = [0u8; 10];
        let mut len = 0;
        for (i, mut b) in ascii::escape_default(self.0).enumerate() {
            // capitalize \xab to \xAB
            if i >= 2 && b'a' <= b && b <= b'f' {
                b -= 32;
            }
            bytes[len] = b;
            len += 1;
        }
        write!(f, "{}", str::from_utf8(&bytes[..len]).unwrap())
    }
}

/// Returns the smallest possible index of the next valid UTF-8 sequence
/// starting after `i`.
///
/// For all inputs, including invalid UTF-8 and any value of `i`, the return
/// value is guaranteed to be greater than `i`. (If there is no value greater
/// than `i` that fits in `usize`, then this panics.)
///
/// Generally speaking, this should only be called on `text` when it is
/// permitted to assume that it is valid UTF-8 and where either `i >=
/// text.len()` or where `text[i]` is a leading byte of a UTF-8 sequence.
#[inline(always)]
pub(crate) fn next_utf8(text: &[u8], i: usize) -> usize {
    let b = match text.get(i) {
        None => return i.checked_add(1).unwrap(),
        Some(&b) => b,
    };
    // For cases where we see an invalid UTF-8 byte, there isn't much we can do
    // other than just start at the next byte.
    let inc = utf8_len(b).unwrap_or(1);
    i.checked_add(inc).unwrap()
}

/// Returns true if and only if the given byte is considered a word character.
/// This only applies to ASCII.
///
/// This was copied from regex-syntax so that we can use it to determine the
/// starting DFA state while searching without depending on regex-syntax. The
/// definition is never going to change, so there's no maintenance/bit-rot
/// hazard here.
#[inline(always)]
pub(crate) fn is_word_byte(b: u8) -> bool {
    match b {
        b'_' | b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => true,
        _ => false,
    }
}

/// Decodes the next UTF-8 encoded codepoint from the given byte slice.
///
/// If no valid encoding of a codepoint exists at the beginning of the given
/// byte slice, then the first byte is returned instead.
///
/// This returns `None` if and only if `bytes` is empty.
#[inline(always)]
pub(crate) fn decode_utf8(bytes: &[u8]) -> Option<Result<char, u8>> {
    if bytes.is_empty() {
        return None;
    }
    let len = match utf8_len(bytes[0]) {
        None => return Some(Err(bytes[0])),
        Some(len) if len > bytes.len() => return Some(Err(bytes[0])),
        Some(1) => return Some(Ok(bytes[0] as char)),
        Some(len) => len,
    };
    match str::from_utf8(&bytes[..len]) {
        Ok(s) => Some(Ok(s.chars().next().unwrap())),
        Err(_) => Some(Err(bytes[0])),
    }
}

/// Decodes the last UTF-8 encoded codepoint from the given byte slice.
///
/// If no valid encoding of a codepoint exists at the end of the given byte
/// slice, then the last byte is returned instead.
///
/// This returns `None` if and only if `bytes` is empty.
#[inline(always)]
pub(crate) fn decode_last_utf8(bytes: &[u8]) -> Option<Result<char, u8>> {
    if bytes.is_empty() {
        return None;
    }
    let mut start = bytes.len() - 1;
    let limit = bytes.len().saturating_sub(4);
    while start > limit && !is_leading_or_invalid_utf8_byte(bytes[start]) {
        start -= 1;
    }
    match decode_utf8(&bytes[start..]) {
        None => None,
        Some(Ok(ch)) => Some(Ok(ch)),
        Some(Err(_)) => Some(Err(bytes[bytes.len() - 1])),
    }
}

/// Given a UTF-8 leading byte, this returns the total number of code units
/// in the following encoded codepoint.
///
/// If the given byte is not a valid UTF-8 leading byte, then this returns
/// `None`.
#[inline(always)]
fn utf8_len(byte: u8) -> Option<usize> {
    if byte <= 0x7F {
        return Some(1);
    } else if byte & 0b1100_0000 == 0b1000_0000 {
        return None;
    } else if byte <= 0b1101_1111 {
        Some(2)
    } else if byte <= 0b1110_1111 {
        Some(3)
    } else if byte <= 0b1111_0111 {
        Some(4)
    } else {
        None
    }
}

/// Returns true if and only if the given byte is either a valid leading UTF-8
/// byte, or is otherwise an invalid byte that can never appear anywhere in a
/// valid UTF-8 sequence.
#[inline(always)]
fn is_leading_or_invalid_utf8_byte(b: u8) -> bool {
    // In the ASCII case, the most significant bit is never set. The leading
    // byte of a 2/3/4-byte sequence always has the top two most significant
    // bits set. For bytes that can never appear anywhere in valid UTF-8, this
    // also returns true, since every such byte has its two most significant
    // bits set:
    //
    //     \xC0 :: 11000000
    //     \xC1 :: 11000001
    //     \xF5 :: 11110101
    //     \xF6 :: 11110110
    //     \xF7 :: 11110111
    //     \xF8 :: 11111000
    //     \xF9 :: 11111001
    //     \xFA :: 11111010
    //     \xFB :: 11111011
    //     \xFC :: 11111100
    //     \xFD :: 11111101
    //     \xFE :: 11111110
    //     \xFF :: 11111111
    (b & 0b1100_0000) != 0b1000_0000
}

#[cfg(feature = "alloc")]
#[inline(always)]
pub(crate) fn is_word_char_fwd(bytes: &[u8], mut at: usize) -> bool {
    use core::{ptr, sync::atomic::AtomicPtr};

    use crate::{
        dfa::{
            dense::{self, DFA},
            Automaton,
        },
        util::lazy,
    };

    static WORD: AtomicPtr<DFA<Vec<u32>>> = AtomicPtr::new(ptr::null_mut());

    let dfa = lazy::get_or_init(&WORD, || {
        // TODO: Should we use a lazy DFA here instead? It does complicate
        // things somewhat, since we then need a mutable cache, which probably
        // means a thread local.
        dense::Builder::new()
            .configure(dense::Config::new().anchored(true))
            .build(r"\w")
            .unwrap()
    });
    // This is OK since '\w' contains no look-around.
    let mut sid = dfa.universal_start_state();
    while at < bytes.len() {
        let byte = bytes[at];
        sid = dfa.next_state(sid, byte);
        at += 1;
        if dfa.is_special_state(sid) {
            if dfa.is_match_state(sid) {
                return true;
            } else if dfa.is_dead_state(sid) {
                return false;
            }
        }
    }
    dfa.is_match_state(dfa.next_eoi_state(sid))
}

#[cfg(feature = "alloc")]
#[inline(always)]
pub(crate) fn is_word_char_rev(bytes: &[u8], mut at: usize) -> bool {
    use core::{ptr, sync::atomic::AtomicPtr};

    use crate::{
        dfa::{
            dense::{self, DFA},
            Automaton,
        },
        nfa::thompson::NFA,
    };

    static WORD: AtomicPtr<DFA<Vec<u32>>> = AtomicPtr::new(ptr::null_mut());

    let dfa = lazy::get_or_init(&WORD, || {
        dense::Builder::new()
            .configure(dense::Config::new().anchored(true))
            .thompson(NFA::config().reverse(true).shrink(true))
            .build(r"\w")
            .unwrap()
    });

    // This is OK since '\w' contains no look-around.
    let mut sid = dfa.universal_start_state();
    while at > 0 {
        at -= 1;
        let byte = bytes[at];
        sid = dfa.next_state(sid, byte);
        if dfa.is_special_state(sid) {
            if dfa.is_match_state(sid) {
                return true;
            } else if dfa.is_dead_state(sid) {
                return false;
            }
        }
    }
    dfa.is_match_state(dfa.next_eoi_state(sid))
}
