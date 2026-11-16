//! Name grammar and namespace governance.
//!
//! A name (and a namespace prefix) is 1..=1024 bytes of `/`-separated
//! segments; each segment is non-empty, not `.` or `..`, and made of
//! `[A-Za-z0-9._+-]`. Prefixes govern the names under them segment by
//! segment: `a/b` covers `a/b` and `a/b/c`, not `a/bc`.

use crate::{Mobility, Rule};

/// Maximum name length in bytes.
pub const MAX_NAME: usize = 1024;

/// Why a name is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// Zero bytes.
    Empty,
    /// More than [`MAX_NAME`] bytes.
    TooLong,
    /// Two `/` in a row, or one at either end.
    EmptySegment,
    /// A `.` or `..` segment.
    DotSegment,
    /// A byte outside `[A-Za-z0-9._+-/]`.
    BadByte,
}

/// Whether `c` may appear in a segment.
pub fn segment_byte(c: u8) -> bool {
    (c >= b'a' && c <= b'z')
        || (c >= b'A' && c <= b'Z')
        || (c >= b'0' && c <= b'9')
        || c == b'.'
        || c == b'_'
        || c == b'-'
        || c == b'+'
}

/// Checks the grammar. The first problem found (scanning left to right,
/// segment by segment) is reported.
pub fn validate(s: &Vec<u8>) -> Result<(), Invalid> {
    if s.len() == 0 {
        return Err(Invalid::Empty);
    }
    if s.len() > MAX_NAME {
        return Err(Invalid::TooLong);
    }
    // One segment at a time: [start, i) is the current segment.
    let mut start: usize = 0;
    let mut i: usize = 0;
    while i <= s.len() {
        if i == s.len() || s[i] == b'/' {
            let n = i - start;
            if n == 0 {
                return Err(Invalid::EmptySegment);
            }
            if (n == 1 && s[start] == b'.') || (n == 2 && s[start] == b'.' && s[start + 1] == b'.') {
                return Err(Invalid::DotSegment);
            }
            start = i + 1;
        } else if !segment_byte(s[i]) {
            return Err(Invalid::BadByte);
        }
        i += 1;
    }
    Ok(())
}

/// True if `prefix` is a byte prefix of `s`.
pub fn starts_with(s: &Vec<u8>, prefix: &Vec<u8>) -> bool {
    if prefix.len() > s.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < prefix.len() {
        if s[i] != prefix[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// True if `s` equals `prefix` or lies under `prefix/` (segment-aligned).
/// The empty prefix covers everything.
pub fn is_under(s: &Vec<u8>, prefix: &Vec<u8>) -> bool {
    if prefix.len() == 0 {
        return true;
    }
    if !starts_with(s, prefix) {
        return false;
    }
    s.len() == prefix.len() || s[prefix.len()] == b'/'
}

/// The rule governing `name`: the rule of the longest prefix in `rules`
/// that covers it, or `Mutable` under the empty prefix if none does.
/// Returns the rule and the index of its prefix in `rules` (`None` for the
/// default). The first of several equally long covering prefixes wins
/// (well-formed rule lists have distinct prefixes).
pub fn governing(rules: &Vec<(Vec<u8>, Rule)>, name: &Vec<u8>) -> (Rule, Option<usize>) {
    let mut best: Option<usize> = None;
    let mut best_len: usize = 0;
    let mut i: usize = 0;
    while i < rules.len() {
        let covers = is_under(name, &rules[i].0);
        let r = pick(best, best_len, i, rules[i].0.len(), covers);
        best = r.0;
        best_len = r.1;
        i += 1;
    }
    match best {
        Some(j) => (rules[j].1, Some(j)),
        None => (Rule::Mutable, None),
    }
}

/// One step of [`governing`]: rule `i`, with a prefix of `plen` bytes that
/// covers the name or not, replaces the best so far if it is the first
/// covering one or strictly longer.
pub fn pick(best: Option<usize>, best_len: usize, i: usize, plen: usize, covers: bool) -> (Option<usize>, usize) {
    if !covers {
        return (best, best_len);
    }
    match best {
        None => (Some(i), plen),
        Some(_) => {
            if plen > best_len {
                (Some(i), plen)
            } else {
                (best, best_len)
            }
        }
    }
}

const HEADS: [u8; 6] = [b'h', b'e', b'a', b'd', b's', b'/'];
const RELEASES: [u8; 9] = [b'r', b'e', b'l', b'e', b'a', b's', b'e', b's', b'/'];

/// True if `s[from..]` starts with `lit` and has at least one more byte.
fn rest_has(s: &Vec<u8>, from: usize, lit: &[u8]) -> bool {
    // `from + lit.len() >= s.len()`, without overflow.
    if lit.len() >= s.len() || from >= s.len() - lit.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < lit.len() {
        if s[from + i] != lit[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// How `name` may move under `rule`, whose namespace is `prefix` (which
/// covers `name`).
pub fn mobility(rule: Rule, prefix: &Vec<u8>, name: &Vec<u8>) -> Mobility {
    match rule {
        Rule::Mutable => Mobility::Mutable,
        Rule::Immutable => Mobility::CreateOnce,
        Rule::HeadRelease => {
            // The part of the name below the prefix starts at `from` (and is
            // empty if the name is the prefix: then nothing may be written).
            if prefix.len() > 0 && prefix.len() >= name.len() {
                return Mobility::Forbidden;
            }
            let from = if prefix.len() == 0 { 0 } else { prefix.len() + 1 };
            if rest_has(name, from, &HEADS) {
                return Mobility::Mutable;
            }
            if rest_has(name, from, &RELEASES) {
                return Mobility::CreateOnce;
            }
            Mobility::Forbidden
        }
    }
}

/// The mobility of `name` under the rules covering it.
pub fn mobility_of(rules: &Vec<(Vec<u8>, Rule)>, name: &Vec<u8>) -> (Rule, Mobility) {
    let (rule, at) = governing(rules, name);
    let m = match at {
        Some(j) => mobility(rule, &rules[j].0, name),
        None => mobility(rule, &Vec::new(), name),
    };
    (rule, m)
}

/// Compares byte strings lexicographically: -1, 0 or 1 (the order of
/// `BTreeMap<Name, _>`, since `Name` orders by its bytes).
pub fn cmp(a: &Vec<u8>, b: &Vec<u8>) -> i8 {
    let n = if a.len() < b.len() { a.len() } else { b.len() };
    let mut i: usize = 0;
    while i < n {
        if a[i] < b[i] {
            return -1;
        }
        if a[i] > b[i] {
            return 1;
        }
        i += 1;
    }
    if a.len() < b.len() {
        -1
    } else if a.len() > b.len() {
        1
    } else {
        0
    }
}
