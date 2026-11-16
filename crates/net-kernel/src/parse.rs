//! Wire parsers for untrusted input. Each is total (never panics) and strict: every value has
//! exactly one accepted spelling. Specs and proofs: `verify/net/proofs/ParseSpec.lean`.

/// Value of an ASCII hex digit, or 16 if `c` is not one. With `lower`, only `0-9a-f`.
pub fn hex_nibble(c: u8, lower: bool) -> u8 {
    if c >= 48 && c <= 57 {
        c - 48
    } else if c >= 97 && c <= 102 {
        c - 87
    } else if !lower && c >= 65 && c <= 70 {
        c - 55
    } else {
        16
    }
}

/// Exactly `2 * n` hex digits → `n` bytes. With `lower`, lowercase only (canonical ids: OCI
/// digests, git oids). `None` on any other input.
pub fn decode_hex(s: &[u8], n: usize, lower: bool) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || s.len() / 2 != n {
        return None;
    }
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < n {
        let a = hex_nibble(s[2 * i], lower);
        let b = hex_nibble(s[2 * i + 1], lower);
        if a > 15 || b > 15 {
            return None;
        }
        out.push(a * 16 + b);
        i += 1;
    }
    Some(out)
}

/// A git pkt-line length prefix: exactly four hex digits (either case, as git writes
/// lowercase but reads both).
pub fn pkt_len(b: &[u8]) -> Option<u16> {
    if b.len() != 4 {
        return None;
    }
    let mut v: u16 = 0;
    let mut i: usize = 0;
    while i < 4 {
        let d = hex_nibble(b[i], false);
        if d > 15 {
            return None;
        }
        v = v * 16 + (d as u16);
        i += 1;
    }
    Some(v)
}

/// Strict decimal `u64`: nonempty, digits only, no leading zero (except "0"), no overflow.
pub fn parse_dec_u64(s: &[u8]) -> Option<u64> {
    if s.len() == 0 {
        return None;
    }
    if s.len() > 1 && s[0] == 48 {
        return None;
    }
    let mut v: u64 = 0;
    let mut i: usize = 0;
    while i < s.len() {
        let c = s[i];
        if c < 48 || c > 57 {
            return None;
        }
        let d = (c - 48) as u64;
        if v > (u64::MAX - d) / 10 {
            return None;
        }
        v = v * 10 + d;
        i += 1;
    }
    Some(v)
}

/// The fixed first line of a Git LFS pointer.
pub const LFS_VERSION_LINE: &[u8] = b"version https://git-lfs.github.com/spec/v1";
/// Length of [`LFS_VERSION_LINE`].
pub const LFS_VERSION_LEN: usize = 42;
/// Largest pointer file accepted (git-lfs writes ~130 bytes).
pub const LFS_MAX_POINTER: usize = 512;

/// End of the line `[start, i)` without a trailing `\r`.
fn trim_cr(body: &[u8], start: usize, i: usize) -> usize {
    if i > start && body[i - 1] == 13 { i - 1 } else { i }
}

/// Scan state over a pointer: only positions and scalars, so the loop is first order. The
/// oid is decoded once, from the last valid `oid` line, after the scan.
#[derive(Clone, Copy)]
pub struct LfsScan {
    /// Start of the current line.
    pub start: usize,
    /// The last valid `oid sha256:` line's value, `[oid_s, oid_e)`, if any.
    pub has_oid: bool,
    pub oid_s: usize,
    pub oid_e: usize,
    /// The last valid `size` line's value, if any.
    pub has_size: bool,
    pub size: u64,
}

/// The scan after the line `body[start..end]` (without its `\r`): `oid sha256:<64 hex>` or
/// `size <decimal>` updates it; other lines (extensions, malformed values) say nothing.
pub fn lfs_line(sc: LfsScan, body: &[u8], start: usize, end: usize) -> LfsScan {
    let oid_p: &[u8] = b"oid sha256:";
    let size_p: &[u8] = b"size ";
    if starts_at(body, start, end, oid_p) {
        let vs = start + oid_p.len();
        if end - vs == 64 && all_hex(body, vs, end) {
            LfsScan { start: sc.start, has_oid: true, oid_s: vs, oid_e: end, has_size: sc.has_size, size: sc.size }
        } else {
            sc
        }
    } else if starts_at(body, start, end, size_p) {
        let v = slice_of(body, start + size_p.len(), end);
        match parse_dec_u64(&v) {
            Some(n) => LfsScan {
                start: sc.start,
                has_oid: sc.has_oid,
                oid_s: sc.oid_s,
                oid_e: sc.oid_e,
                has_size: true,
                size: n,
            },
            None => sc,
        }
    } else {
        sc
    }
}

/// Are `body[s..e]` all hex digits (either case)?
fn all_hex(body: &[u8], s: usize, e: usize) -> bool {
    let mut i = s;
    while i < e {
        if hex_nibble(body[i], false) > 15 {
            return false;
        }
        i += 1;
    }
    true
}

/// One position `i` of the scan: at a newline (or the end), the line `[sc.start, i)` is read.
pub fn lfs_scan(sc: LfsScan, body: &[u8], i: usize) -> LfsScan {
    if i == body.len() || body[i] == 10 {
        let start = sc.start;
        let r = if i > start || i < body.len() { lfs_line(sc, body, start, trim_cr(body, start, i)) } else { sc };
        LfsScan { start: i + 1, has_oid: r.has_oid, oid_s: r.oid_s, oid_e: r.oid_e, has_size: r.has_size, size: r.size }
    } else {
        sc
    }
}

/// Does `body[start..end]` begin with `p`?
fn starts_at(body: &[u8], start: usize, end: usize, p: &[u8]) -> bool {
    if end < start || end - start < p.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < p.len() {
        if body[start + i] != p[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `body[start..end]` as a new vector.
fn slice_of(body: &[u8], start: usize, end: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut i = start;
    while i < end && i < body.len() {
        out.push(body[i]);
        i += 1;
    }
    out
}

/// Does `body` begin with [`LFS_VERSION_LINE`] and a newline?
pub fn lfs_version_ok(body: &[u8]) -> bool {
    body.len() > LFS_VERSION_LEN && starts_at(body, 0, body.len(), LFS_VERSION_LINE) && body[LFS_VERSION_LEN] == 10
}

/// A Git LFS pointer file: `(sha256 bytes, size)`, or `None` if `body` is not one. The first
/// line must be [`LFS_VERSION_LINE`]; both `oid` and `size` must be present. A later line
/// overrides an earlier one.
pub fn parse_lfs_pointer(body: &[u8]) -> Option<(Vec<u8>, u64)> {
    if body.len() > LFS_MAX_POINTER {
        return None;
    }
    // (Global byte strings are read in helpers: Aeneas finds no loop fixed point in a function
    // that borrows one before its loop.)
    if !lfs_version_ok(body) {
        return None;
    }
    let vl = LFS_VERSION_LEN;
    let mut sc = LfsScan { start: vl + 1, has_oid: false, oid_s: 0, oid_e: 0, has_size: false, size: 0 };
    let mut i = vl + 1;
    while i <= body.len() {
        sc = lfs_scan(sc, body, i);
        i += 1;
    }
    if !sc.has_oid || !sc.has_size {
        return None;
    }
    let v = slice_of(body, sc.oid_s, sc.oid_e);
    match decode_hex(&v, 32, false) {
        Some(h) => Some((h, sc.size)),
        None => None,
    }
}

/// Largest uncompressed Xet chunk.
pub const XET_MAX_CHUNK: u32 = 128 * 1024;

/// A Xet xorb chunk header (8 bytes at `b[i..]`): version 0, compressed length (u24 LE),
/// scheme (0 none, 1 LZ4, 2 byte-grouped LZ4), uncompressed length (u24 LE). Returns
/// `(compressed_len, scheme, uncompressed_len)` if the header is present and acceptable: the
/// lengths are bounded and the compressed data fits in `b`.
pub fn xet_chunk_header(b: &[u8], i: usize) -> Option<(u32, u8, u32)> {
    if i > b.len() || b.len() - i < 8 {
        return None;
    }
    if b[i] != 0 {
        return None;
    }
    let clen = (b[i + 1] as u32) | ((b[i + 2] as u32) << 8) | ((b[i + 3] as u32) << 16);
    let scheme = b[i + 4];
    let ulen = (b[i + 5] as u32) | ((b[i + 6] as u32) << 8) | ((b[i + 7] as u32) << 16);
    if scheme > 2 {
        return None;
    }
    if ulen > XET_MAX_CHUNK || clen > 2 * XET_MAX_CHUNK {
        return None;
    }
    if scheme == 0 && clen != ulen {
        return None;
    }
    if (b.len() - i - 8) < (clen as usize) {
        return None;
    }
    Some((clen, scheme, ulen))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_pkt() {
        assert_eq!(decode_hex(b"ab01", 2, true), Some(vec![0xab, 0x01]));
        assert_eq!(decode_hex(b"AB01", 2, true), None);
        assert_eq!(decode_hex(b"AB01", 2, false), Some(vec![0xab, 0x01]));
        assert_eq!(decode_hex(b"+a", 1, false), None);
        assert_eq!(decode_hex("\u{27e}a".as_bytes(), 1, false), None);
        assert_eq!(decode_hex(b"abc", 1, false), None);
        assert_eq!(pkt_len(b"0010"), Some(16));
        assert_eq!(pkt_len(b"ffff"), Some(65535));
        assert_eq!(pkt_len(b"+00f"), None);
        assert_eq!(pkt_len(b"001"), None);
    }

    #[test]
    fn decimal() {
        assert_eq!(parse_dec_u64(b"0"), Some(0));
        assert_eq!(parse_dec_u64(b"18446744073709551615"), Some(u64::MAX));
        assert_eq!(parse_dec_u64(b"18446744073709551616"), None);
        for bad in [&b""[..], b"01", b"+1", b"-1", b" 1", b"1 "] {
            assert_eq!(parse_dec_u64(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn lfs_pointer() {
        let oid = "a".repeat(64);
        let p = format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 12345\n");
        assert_eq!(parse_lfs_pointer(p.as_bytes()), Some((vec![0xaa; 32], 12345)));
        // As before: the version line ends in a bare LF; later lines may end in CRLF.
        let crlf = p.replace('\n', "\r\n").replacen("v1\r\n", "v1\n", 1);
        assert_eq!(parse_lfs_pointer(crlf.as_bytes()), Some((vec![0xaa; 32], 12345)));
        assert_eq!(parse_lfs_pointer(p.replace('\n', "\r\n").as_bytes()), None);
        let no_nl = p.trim_end().to_string();
        assert_eq!(parse_lfs_pointer(no_nl.as_bytes()), Some((vec![0xaa; 32], 12345)));
        assert_eq!(parse_lfs_pointer(p.replace("size 12345", "size +5").as_bytes()), None);
        assert_eq!(parse_lfs_pointer(p.replace("oid sha256:", "oid sha1:").as_bytes()), None);
        assert_eq!(parse_lfs_pointer(&p.as_bytes()[1..]), None);
        let mut big = p.clone().into_bytes();
        big.extend(vec![b'x'; 600]);
        assert_eq!(parse_lfs_pointer(&big), None);
    }

    #[test]
    fn version_len() {
        assert_eq!(LFS_VERSION_LINE.len(), LFS_VERSION_LEN);
    }

    #[test]
    fn xet_header() {
        let mut b = vec![0u8, 4, 0, 0, 0, 4, 0, 0];
        b.extend_from_slice(b"abcd");
        assert_eq!(xet_chunk_header(&b, 0), Some((4, 0, 4)));
        assert_eq!(xet_chunk_header(&b[..10], 0), None);
        assert_eq!(xet_chunk_header(&b, 5), None);
        assert_eq!(xet_chunk_header(&b, 100), None);
        b[0] = 1;
        assert_eq!(xet_chunk_header(&b, 0), None);
    }
}
