//! OCC decisions of the name layer: `replace`, `replace_many`,
//! `update_many`, `delete` and `set_rule`.
//!
//! The kernel decides over a [`View`]: the part of the ref state the
//! decision reads. The shell builds it from its indexed state:
//! - every read-set entry with the name's *current* state;
//! - every target with its current value, its tombstone, and the rules whose
//!   prefixes cover it (`names::is_under`).
//!
//! Both lists are sorted by name, which the kernel checks, so duplicate and
//! membership checks are linear merges. The decision on the view equals the
//! decision on the whole state (`verify/core`, frame theorem); that the
//! shell fills the view from its state is the shell's contract.
//!
//! Order of checks (unchanged from the original `RefState::decide_*`):
//! 1. batch size;
//! 2. per target, a duplicate, then membership in the read set;
//! 3. read-set validation (`Conflict`, with every stale entry);
//! 4. per target, the rule (`Forbidden`).
//!
//! Since targets are checked in name order, the error reported when several
//! targets are invalid is the first by name.

use crate::names::{cmp, mobility_of};
use crate::{Hash, Mobility, Observed, RefVal, Rule, Why, observe};

/// Maximum number of names in one atomic batch.
pub const MAX_BATCH: usize = 10_000;

/// A read-set entry: what the writer observed, and the name's current state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Read {
    pub name: Vec<u8>,
    pub observed: Observed,
    pub current: Observed,
}

/// A target of the batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: Vec<u8>,
    /// The new value, or `None` to delete.
    pub new: Option<Hash>,
    /// The name's current value.
    pub current: Option<RefVal>,
    /// The last version of the name if it was deleted (and not re-created).
    pub tombstone: Option<u64>,
    /// The rules whose prefixes cover the name (any order).
    pub rules: Vec<(Vec<u8>, Rule)>,
}

/// The slice of state one decision reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// Sorted by name (duplicates allowed: the same name observed twice).
    pub reads: Vec<Read>,
    /// Sorted by name.
    pub targets: Vec<Target>,
}

/// One effect of an accepted batch, per target, in target order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Point the name at a value.
    Set(RefVal),
    /// Remove the name; its last version becomes its tombstone.
    Delete(u64),
    /// Deleting an absent name: nothing to do.
    Noop,
}

/// A decision that passed input validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Commit these effects (one per target, in `View::targets` order).
    Commit(Vec<Effect>),
    /// The read set is stale: every stale entry with its current state,
    /// sorted by name, without duplicates.
    Conflict(Vec<(Vec<u8>, Observed)>),
}

/// An input refused before or regardless of the OCC check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// No targets, or more than [`MAX_BATCH`].
    BatchSize,
    /// Target `i` (index in `View::targets`) appears twice.
    Duplicate(usize),
    /// Target `i` is not in the read set.
    NotInReadSet(usize),
    /// Target `i` may not be written under `rule`.
    Forbidden(usize, Rule, Why),
    /// `View::reads` or `View::targets` is not sorted by name (a shell bug).
    Unsorted,
    /// Target `i` is at version `u64::MAX` (or so is its tombstone): no
    /// further version exists.
    VersionExhausted(usize),
}

/// True if `v`'s names are non-decreasing (`strict`: increasing).
fn reads_sorted(v: &Vec<Read>) -> bool {
    let mut i: usize = 1;
    while i < v.len() {
        if cmp(&v[i - 1].name, &v[i].name) > 0 {
            return false;
        }
        i += 1;
    }
    true
}

/// Index of the first target equal to its predecessor, or `Unsorted` if the
/// targets decrease somewhere.
fn first_duplicate(v: &Vec<Target>) -> Result<Option<usize>, Refusal> {
    let mut i: usize = 1;
    while i < v.len() {
        let c = cmp(&v[i - 1].name, &v[i].name);
        if c > 0 {
            return Err(Refusal::Unsorted);
        }
        if c == 0 {
            return Ok(Some(i));
        }
        i += 1;
    }
    Ok(None)
}

/// Index of the first target that no read-set entry names (a merge of the
/// two sorted lists).
fn first_unread(reads: &Vec<Read>, targets: &Vec<Target>) -> Option<usize> {
    let mut r: usize = 0;
    let mut t: usize = 0;
    while t < targets.len() {
        // Advance r past reads that sort before target t.
        while r < reads.len() && cmp(&reads[r].name, &targets[t].name) < 0 {
            r += 1;
        }
        if r == reads.len() || cmp(&reads[r].name, &targets[t].name) != 0 {
            return Some(t);
        }
        t += 1;
    }
    None
}

/// True if `out`'s last entry is `(name, cur)`.
fn same_as_last(out: &Vec<(Vec<u8>, Observed)>, name: &Vec<u8>, cur: Observed) -> bool {
    if out.len() == 0 {
        return false;
    }
    let last = &out[out.len() - 1];
    cmp(&last.0, name) == 0 && last.1 == cur
}

/// Every stale read-set entry with its current state, sorted by name and
/// deduplicated (reads are sorted, so equal entries are adjacent).
fn stale_reads(reads: &Vec<Read>) -> Vec<(Vec<u8>, Observed)> {
    let mut out: Vec<(Vec<u8>, Observed)> = Vec::new();
    for e in reads.iter() {
        if e.observed != e.current && !same_as_last(&out, &e.name, e.current) {
            out.push((e.name.clone(), e.current));
        }
    }
    out
}

/// The version a name was last at: its current version, else its
/// tombstone, else 0 (never existed).
pub fn last_version(current: &Option<RefVal>, tombstone: &Option<u64>) -> u64 {
    match current {
        Some(v) => v.version,
        None => match tombstone {
            Some(tv) => *tv,
            None => 0,
        },
    }
}

/// The effect of one target that passed every check, or why its rule
/// refuses it.
pub fn decide_target(t: &Target, i: usize) -> Result<Effect, Refusal> {
    let (rule, m) = mobility_of(&t.rules, &t.name);
    match t.new {
        Some(h) => {
            match m {
                Mobility::Mutable => {}
                Mobility::CreateOnce => {
                    if t.current.is_some() {
                        return Err(Refusal::Forbidden(i, rule, Why::Published));
                    }
                }
                Mobility::Forbidden => return Err(Refusal::Forbidden(i, rule, Why::HeadReleaseOnly)),
            }
            let last = last_version(&t.current, &t.tombstone);
            if last == u64::MAX {
                return Err(Refusal::VersionExhausted(i));
            }
            let version = last + 1;
            Ok(Effect::Set(RefVal { hash: h, version }))
        }
        None => match t.current {
            // Deleting an absent name is a no-op, under any rule.
            None => Ok(Effect::Noop),
            Some(cur) => match m {
                Mobility::Mutable => Ok(Effect::Delete(cur.version)),
                Mobility::CreateOnce => Err(Refusal::Forbidden(i, rule, Why::ImmutableDelete)),
                Mobility::Forbidden => Err(Refusal::Forbidden(i, rule, Why::HeadReleaseOnly)),
            },
        },
    }
}

/// Decides an atomic batch (`replace` is a batch of one, `replace_many` a
/// batch without deletes). Pure.
pub fn decide(view: &View) -> Result<Decision, Refusal> {
    let n = view.targets.len();
    if n == 0 || n > MAX_BATCH {
        return Err(Refusal::BatchSize);
    }
    if !reads_sorted(&view.reads) {
        return Err(Refusal::Unsorted);
    }
    match first_duplicate(&view.targets) {
        Err(e) => return Err(e),
        Ok(Some(i)) => {
            // A duplicate at i is reported unless an earlier target is unread.
            match first_unread(&view.reads, &view.targets) {
                Some(j) => {
                    if j < i {
                        return Err(Refusal::NotInReadSet(j));
                    }
                }
                None => {}
            }
            return Err(Refusal::Duplicate(i));
        }
        Ok(None) => {}
    }
    match first_unread(&view.reads, &view.targets) {
        Some(j) => return Err(Refusal::NotInReadSet(j)),
        None => {}
    }
    let stale = stale_reads(&view.reads);
    if stale.len() > 0 {
        return Ok(Decision::Conflict(stale));
    }
    let mut effects: Vec<Effect> = Vec::new();
    let mut i: usize = 0;
    while i < n {
        match decide_target(&view.targets[i], i) {
            Ok(e) => effects.push(e),
            Err(r) => return Err(r),
        }
        i += 1;
    }
    Ok(Decision::Commit(effects))
}

/// Decides `set_rule(prefix, rule)` given the prefix's current rule and
/// whether any name lies under it. `Ok(false)`: a no-op; `Ok(true)`: set it.
/// The prefix's grammar is checked by the caller (`names::validate`).
pub fn decide_set_rule(current: Option<Rule>, rule: Rule, any_under: bool) -> Result<bool, SetRuleRefusal> {
    match current {
        Some(r) => {
            if r == rule {
                return Ok(false);
            }
        }
        None => {}
    }
    if any_under {
        return Err(SetRuleRefusal::NonEmpty);
    }
    Ok(true)
}

/// Why `set_rule` is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetRuleRefusal {
    /// Names already exist under the prefix.
    NonEmpty,
}

/// The observed state a writer would record for a target's current value.
pub fn observed_of(t: &Target) -> Observed {
    observe(&t.current)
}
