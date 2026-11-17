//! Tree objects (docs/ARCHITECTURE.md, "Trees"): a directory snapshot as a
//! standard object.
//!
//! A tree is `magic "O5STREE1" | entry*`, every entry
//! `mode u8 | name_len u16 (little endian) | name | hash [32]`, with nothing
//! after the last entry. It is canonical: there is exactly one encoding of a
//! given directory, so equal directories have equal hashes. The rules:
//! - mode is 0 (regular file), 1 (executable file), 2 (symlink; the hash is a
//!   blob of the link target's bytes) or 3 (subtree; the hash is a tree);
//! - a name is one path segment as raw bytes: 1 to 65,535 bytes, no `/`, no
//!   NUL, not `.` or `..`;
//! - names are strictly increasing in byte order (so they are unique);
//! - the whole encoding is at most [`MAX_TREE`] bytes.
//!
//! [`decode`] accepts exactly the canonical encodings and returns their
//! entries, [`encode`] produces them and refuses entry lists that are not
//! canonical, and [`children`] is the GC tracer's view: every entry's hash.
//! Entries are not names, so they need no name encoding.

use crate::Hash;
use crate::names::cmp;

/// Header length.
pub const MAGIC_LEN: usize = 8;
/// Largest encoded tree (bytes).
pub const MAX_TREE: usize = 0x8000_0000;
/// Largest name (bytes): what the u16 length can say.
pub const MAX_NAME: usize = 65535;
/// Mode of a regular file.
pub const REGULAR: u8 = 0;
/// Mode of an executable file.
pub const EXECUTABLE: u8 = 1;
/// Mode of a symlink.
pub const SYMLINK: u8 = 2;
/// Mode of a subtree.
pub const SUBTREE: u8 = 3;

/// One directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// [`REGULAR`], [`EXECUTABLE`], [`SYMLINK`] or [`SUBTREE`].
    pub mode: u8,
    /// One path segment, raw bytes.
    pub name: Vec<u8>,
    /// The blob (file contents, link target) or subtree.
    pub hash: Hash,
}

/// Why bytes are not a canonical tree (or entries can't be encoded as one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeError {
    /// Shorter than the header, or not `"O5STREE1"`.
    BadMagic,
    /// Over [`MAX_TREE`] bytes.
    TooLarge,
    /// An entry runs past the end.
    Truncated,
    /// A mode above 3.
    BadMode,
    /// An empty or over-long name, or one with `/` or NUL, or `.` / `..`.
    BadName,
    /// A name not greater than the one before it.
    Unsorted,
}

/// Whether `b` starts with `"O5STREE1"` (the caller checks `b.len() >= 8`).
pub fn magic_ok(b: &[u8]) -> bool {
    b[0] == b'O'
        && b[1] == b'5'
        && b[2] == b'S'
        && b[3] == b'T'
        && b[4] == b'R'
        && b[5] == b'E'
        && b[6] == b'E'
        && b[7] == b'1'
}

/// Whether `name` is a valid entry name.
pub fn name_ok(name: &Vec<u8>) -> bool {
    let n = name.len();
    if n == 0 || n > MAX_NAME {
        return false;
    }
    if n == 1 && name[0] == b'.' {
        return false;
    }
    if n == 2 && name[0] == b'.' && name[1] == b'.' {
        return false;
    }
    no_bad_byte(name)
}

/// Whether `name` has no NUL and no `/`.
pub fn no_bad_byte(name: &Vec<u8>) -> bool {
    let mut i: usize = 0;
    while i < name.len() {
        if name[i] == 0 {
            return false;
        }
        if name[i] == b'/' {
            return false;
        }
        i += 1;
    }
    true
}

/// `b[start..start + n]` (the caller checks the range).
pub fn copy_range(b: &[u8], start: usize, n: usize) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < n {
        out.push(b[start + i]);
        i += 1;
    }
    out
}

/// The 32 bytes at `b[at..at + 32]` (the caller checks the range).
pub fn hash_from(b: &[u8], at: usize) -> Hash {
    let mut h = [0u8; 32];
    let mut i: usize = 0;
    while i < 32 {
        h[i] = b[at + i];
        i += 1;
    }
    h
}

/// Decodes the entry at `b[at..]` and checks its mode and name. Returns it
/// and the offset after it.
pub fn parse_entry(b: &[u8], at: usize) -> Result<(Entry, usize), TreeError> {
    if at > b.len() {
        return Err(TreeError::Truncated);
    }
    let rest = b.len() - at;
    if rest < 3 {
        return Err(TreeError::Truncated);
    }
    let mode = b[at];
    let n = (b[at + 1] as usize) + (b[at + 2] as usize) * 256;
    if n > rest - 3 || 32 > rest - 3 - n {
        return Err(TreeError::Truncated);
    }
    if mode > 3 {
        return Err(TreeError::BadMode);
    }
    let name = copy_range(b, at + 3, n);
    if !name_ok(&name) {
        return Err(TreeError::BadName);
    }
    let hash = hash_from(b, at + 3 + n);
    Ok((Entry { mode, name, hash }, at + 3 + n + 32))
}

/// Decodes a tree: the entries, if `b` is a canonical tree encoding.
pub fn decode(b: &[u8]) -> Result<Vec<Entry>, TreeError> {
    if b.len() < MAGIC_LEN || !magic_ok(b) {
        return Err(TreeError::BadMagic);
    }
    if b.len() > MAX_TREE {
        return Err(TreeError::TooLarge);
    }
    let mut out: Vec<Entry> = Vec::new();
    let mut at: usize = MAGIC_LEN;
    while at < b.len() {
        match parse_entry(b, at) {
            Err(e) => return Err(e),
            Ok((e, next)) => {
                if out.len() > 0 {
                    if cmp(&out[out.len() - 1].name, &e.name) >= 0 {
                        return Err(TreeError::Unsorted);
                    }
                }
                out.push(e);
                at = next;
            }
        }
    }
    Ok(out)
}

/// Checks that `es` is canonical (valid modes and names, strictly
/// increasing names, encoding within [`MAX_TREE`]) and returns the length
/// of its encoding.
pub fn check(es: &Vec<Entry>) -> Result<usize, TreeError> {
    let mut size: usize = MAGIC_LEN;
    let mut i: usize = 0;
    while i < es.len() {
        let e = &es[i];
        if e.mode > 3 {
            return Err(TreeError::BadMode);
        }
        if !name_ok(&e.name) {
            return Err(TreeError::BadName);
        }
        if i > 0 {
            if cmp(&es[i - 1].name, &e.name) >= 0 {
                return Err(TreeError::Unsorted);
            }
        }
        // size <= MAX_TREE here, and an entry is at most 35 + MAX_NAME bytes: no overflow.
        let len = 35 + e.name.len();
        if len > MAX_TREE - size {
            return Err(TreeError::TooLarge);
        }
        size = size + len;
        i += 1;
    }
    Ok(size)
}

/// Appends `src` to `out`.
pub fn extend(mut out: Vec<u8>, src: &Vec<u8>) -> Vec<u8> {
    let mut i: usize = 0;
    while i < src.len() {
        out.push(src[i]);
        i += 1;
    }
    out
}

/// Appends a hash to `out`.
pub fn extend_hash(mut out: Vec<u8>, h: &Hash) -> Vec<u8> {
    let mut i: usize = 0;
    while i < 32 {
        out.push(h[i]);
        i += 1;
    }
    out
}

/// Encodes canonical entries (see [`check`]).
pub fn encode(es: &Vec<Entry>) -> Result<Vec<u8>, TreeError> {
    match check(es) {
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let mut out: Vec<u8> = Vec::new();
    out.push(b'O');
    out.push(b'5');
    out.push(b'S');
    out.push(b'T');
    out.push(b'R');
    out.push(b'E');
    out.push(b'E');
    out.push(b'1');
    let mut i: usize = 0;
    while i < es.len() {
        let e = &es[i];
        let n = e.name.len();
        out.push(e.mode);
        out.push((n % 256) as u8);
        out.push((n / 256) as u8);
        out = extend(out, &e.name);
        out = extend_hash(out, &e.hash);
        i += 1;
    }
    Ok(out)
}

/// The GC tracer's view of a tree: every entry's hash, in order.
pub fn children(b: &[u8]) -> Result<Vec<Hash>, TreeError> {
    match decode(b) {
        Err(e) => Err(e),
        Ok(es) => {
            let mut out: Vec<Hash> = Vec::new();
            let mut i: usize = 0;
            while i < es.len() {
                out.push(es[i].hash);
                i += 1;
            }
            Ok(out)
        }
    }
}
