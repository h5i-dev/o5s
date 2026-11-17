//! File paths as names: a canonical, reversible encoding.
//!
//! Names allow only `[A-Za-z0-9._+-]` in `/`-separated segments, so arbitrary file
//! paths (spaces, `[ ]`, `~`, `%`, non-ASCII, `.`/`..` segments) cannot be names
//! as they are. The convention, per `/`-separated segment:
//! - every byte outside `[A-Za-z0-9._-]` becomes `+XX` (two uppercase hex digits),
//!   so `+` itself is `+2B`;
//! - a segment that is exactly `.` or `..` has its dots encoded (`+2E`, `+2E+2E`);
//! - an empty path, an empty segment (a leading, trailing or doubled `/`), and a
//!   result over [`names::MAX_NAME`] bytes are errors.
//!
//! Decoding is strict: an escape must be `+` and two uppercase hex digits, and the
//! input must be exactly what encoding its result gives back (no lowercase hex, no
//! escape of a byte that needn't be escaped, no literal `.`/`..` segment). So
//! encoding is a bijection onto its image (proved in `verify/core`, `PathName.lean`).

use crate::names;

/// Why a path or an encoded name is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathError {
    /// The empty path or name.
    Empty,
    /// A leading, trailing or doubled `/`.
    EmptySegment,
    /// The encoded name would exceed [`names::MAX_NAME`] bytes.
    TooLong,
    /// A byte that is neither allowed literally nor an escape (decoding).
    BadByte,
    /// `+` not followed by two uppercase hex digits (decoding).
    BadEscape,
    /// Decodable, but not the canonical encoding of its result (decoding).
    NonCanonical,
    /// The encoded result is not a valid name (never happens: proved).
    Invalid,
}

/// A byte kept literally: `[A-Za-z0-9._-]`.
pub fn plain(b: u8) -> bool {
    if b >= b'a' {
        return b <= b'z';
    }
    if b >= b'A' {
        if b <= b'Z' {
            return true;
        }
        return b == b'_';
    }
    if b >= b'0' {
        return b <= b'9';
    }
    if b == b'.' {
        return true;
    }
    b == b'-'
}

/// A byte allowed literally in an encoded name: `/` or a plain byte.
pub fn literal(c: u8) -> bool {
    if c == b'/' {
        return true;
    }
    plain(c)
}

/// The uppercase hex digit of `v` (`v < 16`).
pub fn hex_digit(v: u8) -> u8 {
    if v < 10 {
        return b'0' + v;
    }
    b'A' + (v - 10)
}

/// The value of an uppercase hex digit, or 16 if `c` is not one.
pub fn hex_value(c: u8) -> u8 {
    if c >= b'0' {
        if c <= b'9' {
            return c - b'0';
        }
    }
    if c >= b'A' {
        if c <= b'F' {
            return c - b'A' + 10;
        }
    }
    16
}

/// Whether a segment starts at `i` (`i < p.len()`).
fn starts_segment(p: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    p[i - 1] == b'/'
}

/// Whether a segment ends at `i` (`i < p.len()`).
fn ends_segment(p: &[u8], i: usize) -> bool {
    if i + 1 == p.len() {
        return true;
    }
    p[i + 1] == b'/'
}

/// Whether byte `i` is a dot of a segment that is exactly `.` or `..`.
pub fn dot_segment(p: &[u8], i: usize) -> bool {
    if p[i] != b'.' {
        return false;
    }
    if starts_segment(p, i) {
        if ends_segment(p, i) {
            return true; // "."
        }
        if i + 1 < p.len() {
            if p[i + 1] == b'.' {
                return ends_segment(p, i + 1); // ".." (first dot)
            }
        }
        return false;
    }
    if p[i - 1] == b'.' {
        if starts_segment(p, i - 1) {
            return ends_segment(p, i); // ".." (second dot)
        }
    }
    false
}

/// Whether the `/` at `i` borders an empty segment (`i < p.len()`).
fn empty_segment_at(p: &[u8], i: usize) -> bool {
    if i == 0 {
        return true;
    }
    if i + 1 == p.len() {
        return true;
    }
    p[i + 1] == b'/'
}

/// Whether byte `i` (not `/`) is kept literally.
fn keep(p: &[u8], i: usize) -> bool {
    if plain(p[i]) {
        return !dot_segment(p, i);
    }
    false
}

/// Appends the escape `+HH` of `b`.
fn push_escape(out: Vec<u8>, b: u8) -> Vec<u8> {
    let mut out = out;
    out.push(b'+');
    out.push(hex_digit(b / 16));
    out.push(hex_digit(b % 16));
    out
}

/// Encodes a file path as a name.
pub fn encode(p: &[u8]) -> Result<Vec<u8>, PathError> {
    if p.len() == 0 {
        return Err(PathError::Empty);
    }
    // Encoding never shrinks, so a longer path cannot fit (and the work stays bounded).
    if p.len() > names::MAX_NAME {
        return Err(PathError::TooLong);
    }
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < p.len() {
        let b = p[i];
        if b == b'/' {
            if empty_segment_at(p, i) {
                return Err(PathError::EmptySegment);
            }
            out.push(b'/');
        } else if keep(p, i) {
            out.push(b);
        } else {
            out = push_escape(out, b);
        }
        i += 1;
    }
    if out.len() > names::MAX_NAME {
        return Err(PathError::TooLong);
    }
    match names::validate(&out) {
        Ok(()) => Ok(out),
        Err(_) => Err(PathError::Invalid),
    }
}

/// Decodes the escapes of an encoded name (one byte per step: normal, after `+`,
/// after the first hex digit), without the canonical check.
pub fn decode_raw(n: &[u8]) -> Result<Vec<u8>, PathError> {
    let mut out: Vec<u8> = Vec::new();
    let mut st: u8 = 0;
    let mut hi: u8 = 0;
    let mut i: usize = 0;
    while i < n.len() {
        let c = n[i];
        if st == 0 {
            if c == b'+' {
                st = 1;
            } else if literal(c) {
                out.push(c);
            } else {
                return Err(PathError::BadByte);
            }
        } else {
            let v = hex_value(c);
            if v >= 16 {
                return Err(PathError::BadEscape);
            }
            if st == 1 {
                hi = v;
                st = 2;
            } else {
                out.push(hi * 16 + v);
                st = 0;
            }
        }
        i += 1;
    }
    if st != 0 {
        return Err(PathError::BadEscape);
    }
    Ok(out)
}

/// Whether two byte strings are equal.
pub fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Decodes a name back to the file path it encodes. Strict: only the canonical
/// encoding of a path decodes, so `encode` is a bijection onto what `decode` accepts.
pub fn decode(n: &[u8]) -> Result<Vec<u8>, PathError> {
    if n.len() == 0 {
        return Err(PathError::Empty);
    }
    let p = match decode_raw(n) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };
    match encode(&p) {
        Ok(e) => {
            if bytes_eq(&e, n) {
                Ok(p)
            } else {
                Err(PathError::NonCanonical)
            }
        }
        Err(_) => Err(PathError::NonCanonical),
    }
}
