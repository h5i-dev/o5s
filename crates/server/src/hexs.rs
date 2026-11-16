//! Strict hex decoding for untrusted input (URLs, protocol lines, config).
//!
//! `u8::from_str_radix` accepts a leading `+` ("+f" is 15), and slicing a `&str` by byte
//! offsets panics inside a multi-byte character. These helpers accept only ASCII hex digits,
//! so every id has exactly one spelling and no input can panic.

fn nib(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn nib_lower(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'F' => None,
        _ => nib(c),
    }
}

/// Decodes `s` (exactly `2 * out.len()` hex digits, either case) into `out`.
pub(crate) fn decode_into(s: &[u8], out: &mut [u8]) -> Option<()> {
    decode_with(s, out, nib)
}

/// Like [`decode_into`], lowercase only (canonical ids: OCI digests, git oids on the wire).
pub(crate) fn decode_lower_into(s: &[u8], out: &mut [u8]) -> Option<()> {
    decode_with(s, out, nib_lower)
}

fn decode_with(s: &[u8], out: &mut [u8], f: fn(u8) -> Option<u8>) -> Option<()> {
    if s.len() != 2 * out.len() {
        return None;
    }
    for (o, p) in out.iter_mut().zip(s.as_chunks::<2>().0) {
        *o = (f(p[0])? << 4) | f(p[1])?;
    }
    Some(())
}

/// 64 hex digits (either case) → 32 bytes.
pub(crate) fn decode32(s: &str) -> Option<[u8; 32]> {
    let mut o = [0u8; 32];
    decode_into(s.as_bytes(), &mut o)?;
    Some(o)
}

/// A pkt-line length: exactly four hex digits.
pub(crate) fn pkt_len(b: &[u8; 4]) -> Option<usize> {
    let mut o = [0u8; 2];
    decode_into(b, &mut o)?;
    Some(u16::from_be_bytes(o) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict() {
        assert_eq!(decode32(&"ab".repeat(32)), Some([0xab; 32]));
        assert_eq!(decode32(&"AB".repeat(32)), Some([0xab; 32]));
        let mut o = [0u8; 32];
        assert!(decode_lower_into("AB".repeat(32).as_bytes(), &mut o).is_none());
        assert!(decode32(&"+a".repeat(32)).is_none(), "from_str_radix would accept '+'");
        // A multi-byte character at a byte offset that would split it: no panic, just None.
        let s = format!("{}\u{27e}{}", "7".repeat(17), "7".repeat(45));
        assert_eq!(s.len(), 64);
        assert!(decode32(&s).is_none());
        assert!(decode32(&"a".repeat(63)).is_none());
        assert_eq!(pkt_len(b"0010"), Some(16));
        assert_eq!(pkt_len(b"+00f"), None);
    }
}
