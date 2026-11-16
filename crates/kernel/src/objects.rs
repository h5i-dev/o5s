//! Object-layer decisions (P1): content-addressed objects are immutable.
//!
//! Hashing (BLAKE3) and the backend's create-only puts are trusted
//! infrastructure; the decisions on top are here. A put never overwrites: an
//! object already present is skipped, an absent one appended. A verified read
//! accepts bytes only if their digest is the key.

use crate::Hash;

/// What a put does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PutAction {
    /// Store a new record.
    Append,
    /// The object is already stored: nothing to write (dedup).
    Skip,
}

/// The put decision, given whether `hash` is already present in live storage.
pub fn put_action(present: bool) -> PutAction {
    if present { PutAction::Skip } else { PutAction::Append }
}

/// Whether a read of `key` may return bytes whose digest is `digest`.
pub fn accept_read(key: &Hash, digest: &Hash) -> bool {
    let mut i: usize = 0;
    while i < 32 {
        if key[i] != digest[i] {
            return false;
        }
        i += 1;
    }
    true
}
