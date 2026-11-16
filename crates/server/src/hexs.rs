//! Strict hex decoding for untrusted input (URLs, protocol lines, config): thin adapters over
//! `o5s_net_kernel::parse`, whose code is extracted to Lean and proven to compute the
//! strict spec on every input (`verify/net/proofs/ParseSpec.lean`: `decode_hex_spec`,
//! `pkt_len_spec`).
//!
//! `u8::from_str_radix` accepts a leading `+` ("+f" is 15), and slicing a `&str` by byte
//! offsets panics inside a multi-byte character. These helpers accept only ASCII hex digits,
//! so every id has exactly one spelling and no input can panic.

use o5s_net_kernel::parse;

/// Decodes `s` (exactly `2 * out.len()` hex digits, either case) into `out`.
pub(crate) fn decode_into(s: &[u8], out: &mut [u8]) -> Option<()> {
    copy(parse::decode_hex(s, out.len(), false)?, out)
}

/// Like [`decode_into`], lowercase only (canonical ids: OCI digests, git oids on the wire).
pub(crate) fn decode_lower_into(s: &[u8], out: &mut [u8]) -> Option<()> {
    copy(parse::decode_hex(s, out.len(), true)?, out)
}

fn copy(v: Vec<u8>, out: &mut [u8]) -> Option<()> {
    (v.len() == out.len()).then(|| out.copy_from_slice(&v))
}

/// 64 hex digits (either case) → 32 bytes.
pub(crate) fn decode32(s: &str) -> Option<[u8; 32]> {
    let mut o = [0u8; 32];
    decode_into(s.as_bytes(), &mut o)?;
    Some(o)
}

/// A pkt-line length: exactly four hex digits.
pub(crate) fn pkt_len(b: &[u8; 4]) -> Option<usize> {
    parse::pkt_len(b).map(usize::from)
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
