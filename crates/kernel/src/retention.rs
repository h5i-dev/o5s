//! Retention decisions (`o5s_prov::retention`): which old versions of a
//! series may be deleted.
//!
//! A series is the names under one parent (`pypi/team/pkg/1.0`, `.../1.1`,
//! ...), sorted oldest first by [`version_cmp`] (the shell sorts; the kernel
//! checks). The newest `keep` are always kept. An older one is deleted only
//! if, when the profile requires it, provenance can regenerate it, and the
//! namespace rules allow deleting it.

use crate::Mobility;
use crate::Rule;
use crate::names::mobility_of;

/// A version-aware comparison of the last name segments: digit runs compare
/// numerically (ignoring leading zeros), other runs as bytes; then the run
/// count, then the bytes. -1, 0 or 1.
pub fn version_cmp(a: &Vec<u8>, b: &Vec<u8>) -> i8 {
    let ra = runs(a);
    let rb = runs(b);
    let n = if ra.len() < rb.len() { ra.len() } else { rb.len() };
    let mut i: usize = 0;
    while i < n {
        let c = run_cmp(&ra[i], &rb[i]);
        if c != 0 {
            return c;
        }
        i += 1;
    }
    if ra.len() < rb.len() {
        return -1;
    }
    if ra.len() > rb.len() {
        return 1;
    }
    crate::names::cmp(a, b)
}

/// Whether `c` is an ASCII digit.
pub fn is_digit(c: u8) -> bool {
    c >= b'0' && c <= b'9'
}

/// The maximal runs of digits and of non-digits, in order.
pub fn runs(s: &Vec<u8>) -> Vec<(bool, Vec<u8>)> {
    let mut out: Vec<(bool, Vec<u8>)> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_digit = false;
    let mut i: usize = 0;
    while i < s.len() {
        let d = is_digit(s[i]);
        let r = flush_run(out, cur, cur_digit, d);
        out = r.0;
        cur = r.1;
        cur_digit = d;
        cur.push(s[i]);
        i += 1;
    }
    if cur.len() > 0 {
        out.push((cur_digit, cur));
    }
    out
}

/// Ends the current run if the next byte's class differs.
pub fn flush_run(out: Vec<(bool, Vec<u8>)>, cur: Vec<u8>, cur_digit: bool, d: bool) -> (Vec<(bool, Vec<u8>)>, Vec<u8>) {
    if cur.len() > 0 && d != cur_digit {
        let mut o = out;
        o.push((cur_digit, cur));
        (o, Vec::new())
    } else {
        (out, cur)
    }
}

/// Compares two runs: both digit runs numerically, otherwise as bytes.
fn run_cmp(p: &(bool, Vec<u8>), q: &(bool, Vec<u8>)) -> i8 {
    if p.0 && q.0 {
        let a = skip_zeros(&p.1);
        let b = skip_zeros(&q.1);
        let la = p.1.len() - a;
        let lb = q.1.len() - b;
        if la < lb {
            return -1;
        }
        if la > lb {
            return 1;
        }
        return cmp_from(&p.1, a, &q.1, b, la);
    }
    crate::names::cmp(&p.1, &q.1)
}

/// The number of leading `0` bytes of `s`.
fn skip_zeros(s: &Vec<u8>) -> usize {
    let mut i: usize = 0;
    while i < s.len() && s[i] == b'0' {
        i += 1;
    }
    i
}

/// Compares `a[ai..ai+n]` with `b[bi..bi+n]` (both in range).
fn cmp_from(a: &Vec<u8>, ai: usize, b: &Vec<u8>, bi: usize, n: usize) -> i8 {
    let mut k: usize = 0;
    while k < n && ai + k < a.len() && bi + k < b.len() {
        if a[ai + k] < b[bi + k] {
            return -1;
        }
        if a[ai + k] > b[bi + k] {
            return 1;
        }
        k += 1;
    }
    0
}

/// One version of a series.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The full name.
    pub name: Vec<u8>,
    /// Its last segment (the version).
    pub version: Vec<u8>,
    /// Provenance can regenerate the object it names.
    pub regenerable: bool,
    /// The rules covering the name.
    pub rules: Vec<(Vec<u8>, Rule)>,
}

/// What retention does with one entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Among the newest `keep`.
    Keep,
    /// Old, but provenance can't regenerate it.
    KeepNotRegenerable,
    /// Old, but a namespace rule forbids deleting it.
    KeepByRule,
    /// Delete it.
    Delete,
}

/// Retention's plan for a series sorted oldest first: one action per entry.
/// `Err(())`: the series is not sorted by [`version_cmp`] (a shell bug).
pub fn plan(keep: usize, only_if_regenerable: bool, entries: &Vec<Entry>) -> Result<Vec<Action>, ()> {
    let mut i: usize = 1;
    while i < entries.len() {
        if version_cmp(&entries[i - 1].version, &entries[i].version) > 0 {
            return Err(());
        }
        i += 1;
    }
    let old = if entries.len() > keep { entries.len() - keep } else { 0 };
    let mut out: Vec<Action> = Vec::new();
    let mut k: usize = 0;
    while k < entries.len() {
        out.push(action(k, old, only_if_regenerable, &entries[k]));
        k += 1;
    }
    Ok(out)
}

/// The action for entry `k` of a series whose first `old` entries are old.
pub fn action(k: usize, old: usize, only_if_regenerable: bool, e: &Entry) -> Action {
    if k >= old {
        return Action::Keep;
    }
    if only_if_regenerable && !e.regenerable {
        return Action::KeepNotRegenerable;
    }
    let (_, m) = mobility_of(&e.rules, &e.name);
    match m {
        Mobility::Mutable => Action::Delete,
        _ => Action::KeepByRule,
    }
}
