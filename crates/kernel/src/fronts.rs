//! Front ownership: which front owns a name, and whether a writer may write it.
//!
//! A front claims name prefixes (byte prefixes such as `git/`). The owner of
//! a name is the front with the longest claimed prefix of it. A name with no
//! owner may be written by anyone (core callers included); an owned name only
//! through its owner's handle (P6). The shell maps front ids to indices.

use crate::names::starts_with;

/// Front `claims[k].1` claims prefix `claims[k].0`.
pub type Claims = Vec<(Vec<u8>, u32)>;

/// The front owning `name`: the one with the longest claimed prefix of it
/// (the first among equally long claims; a registry has distinct prefixes).
pub fn owner(claims: &Claims, name: &Vec<u8>) -> Option<u32> {
    let mut best: Option<usize> = None;
    let mut best_len: usize = 0;
    let mut i: usize = 0;
    while i < claims.len() {
        let covers = starts_with(name, &claims[i].0);
        let r = pick_owner(best, best_len, i, claims[i].0.len(), covers);
        best = r.0;
        best_len = r.1;
        i += 1;
    }
    match best {
        Some(j) => Some(claims[j].1),
        None => None,
    }
}

/// One step of [`owner`] (as `names::pick`).
pub fn pick_owner(best: Option<usize>, best_len: usize, i: usize, plen: usize, covers: bool) -> (Option<usize>, usize) {
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

/// Whether `who` (a front, or `None` for core callers) may write `name`.
pub fn may_write(claims: &Claims, name: &Vec<u8>, who: Option<u32>) -> bool {
    match owner(claims, name) {
        None => true,
        Some(o) => match who {
            Some(w) => o == w,
            None => false,
        },
    }
}

/// Whether `who` may set a namespace rule on `prefix`: it may write the
/// prefix itself, and no claimed prefix of another front that the rule
/// would cover (a claim that starts with `prefix` and is owned by its
/// claimant) belongs to someone else.
pub fn may_set_rule(claims: &Claims, prefix: &Vec<u8>, who: Option<u32>) -> bool {
    if !may_write(claims, prefix, who) {
        return false;
    }
    let mut i: usize = 0;
    while i < claims.len() {
        if !rule_spares(claims, prefix, who, i) {
            return false;
        }
        i += 1;
    }
    true
}

/// Claim `i` is not another front's prefix covered by a rule on `prefix`.
fn rule_spares(claims: &Claims, prefix: &Vec<u8>, who: Option<u32>, i: usize) -> bool {
    let p = &claims[i].0;
    let f = claims[i].1;
    if !starts_with(p, prefix) {
        return true;
    }
    if is_mine(who, f) {
        return true;
    }
    match owner(claims, p) {
        Some(o) => o != f,
        None => true,
    }
}

/// Whether front `f` is the writer `who`.
pub fn is_mine(who: Option<u32>, f: u32) -> bool {
    match who {
        Some(w) => w == f,
        None => false,
    }
}
