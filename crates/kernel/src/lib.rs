//! o5s's decision kernels (docs/H5I_PLAN.md, Phase H2).
//!
//! Every decision about names is made here: pure functions over a small
//! view of the state, written in the Rust subset Charon and Aeneas extract
//! to Lean, so the theorems in `verify/core` are about this code. The
//! shells (`o5s-refs`'s `RefState` and journal, `o5s-repo`'s
//! GC and fronts) build each view from their indexed state, call the
//! kernel, and apply what it returns. They make no decision of their own.
//!
//! Subset rules followed throughout: no `?`, no closures or iterator
//! adapters, `&Vec` rather than slices, `while` loops over indices, and
//! byte strings (`Vec<u8>`) instead of `String`.

pub mod codec;
pub mod fronts;
pub mod gc;
pub mod names;
pub mod objects;
pub mod pathname;
pub mod promote;
pub mod refs;
pub mod retention;
pub mod tree;

/// A BLAKE3-256 content address.
pub type Hash = [u8; 32];

/// Per-namespace name update rule (the on-disk codes are 0, 1 and 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Any validated replace succeeds (source code branches).
    Mutable,
    /// A name may be created once and never moved (published packages).
    Immutable,
    /// `<ns>/heads/...` mutable, `<ns>/releases/...` create-once, anything
    /// else under `<ns>` forbidden (model weights).
    HeadRelease,
}

/// Why a write is forbidden.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The name exists in a create-once namespace.
    Published,
    /// Deleting a create-once name.
    ImmutableDelete,
    /// Neither `heads/*` nor `releases/*` in a head-release namespace.
    HeadReleaseOnly,
}

/// What the governing rule says about one name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mobility {
    /// May move freely.
    Mutable,
    /// May only be created (from absent).
    CreateOnce,
    /// May never be written.
    Forbidden,
}

/// The state of one name as a reader observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observed {
    /// The name did not exist.
    Absent,
    /// Present at this version (versions start at 1, +1 per change).
    Version(u64),
}

/// The current value of a ref.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RefVal {
    /// Object the name points at.
    pub hash: Hash,
    /// Version (continues across deletes).
    pub version: u64,
}

/// What a reader observes for a current value.
pub fn observe(cur: &Option<RefVal>) -> Observed {
    match cur {
        Some(v) => Observed::Version(v.version),
        None => Observed::Absent,
    }
}
