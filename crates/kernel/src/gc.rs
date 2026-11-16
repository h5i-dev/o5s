//! GC decisions (`pico2peta-repo`'s GC; docs/ARCHITECTURE.md).
//!
//! The shell marks (walks refs, manifests, deltas, Frame3 references and
//! front children: trusted I/O over up to 10^8 objects) and asks the kernel:
//! - which planned packs to condemn (the garbage-ratio rule), and
//! - whether a condemned set may be deleted: only if no live object is
//!   still served only by one of its packs (verify-before-delete, P5).
//!
//! Pack names are byte strings; ratios are in permille (no floats in the
//! extracted subset).

use crate::names::cmp;

/// Repack thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    /// Repack a pack whose garbage is at least this many permille of its bytes...
    pub min_garbage_permille: u64,
    /// ...or which is smaller than this many bytes.
    pub small_pack_bytes: u64,
    /// Repack every planned pack.
    pub repack_all: bool,
}

/// Whether a planned pack with `total` stored bytes, of which `live` are
/// live, and `size` bytes on disk becomes a repack victim.
pub fn is_victim(t: &Thresholds, total: u64, live: u64, size: u64) -> bool {
    if t.repack_all || size < t.small_pack_bytes {
        return true;
    }
    if total == 0 || live > total {
        // An empty pack is all garbage; `live > total` cannot happen for a
        // consistent pack and is repacked rather than trusted.
        return true;
    }
    // garbage / total >= min / 1000, without division or overflow:
    // (total - live) * 1000 >= min * total, both sides compared in u128.
    let garbage = (total - live) as u128;
    garbage * 1000 >= (t.min_garbage_permille as u128) * (total as u128)
}

/// Whether `p` is one of `doomed`.
pub fn is_doomed(doomed: &Vec<Vec<u8>>, p: &Vec<u8>) -> bool {
    let mut i: usize = 0;
    while i < doomed.len() {
        if cmp(&doomed[i], p) == 0 {
            return true;
        }
        i += 1;
    }
    false
}

/// Verify-before-delete: given, for every live object, the pack that serves
/// it now (`None`: served from staging or another store), the condemned
/// packs may be deleted only if none of them serves a live object.
pub fn delete_ok(doomed: &Vec<Vec<u8>>, serving: &Vec<Option<Vec<u8>>>) -> bool {
    let mut i: usize = 0;
    while i < serving.len() {
        match &serving[i] {
            Some(p) => {
                if is_doomed(doomed, p) {
                    return false;
                }
            }
            None => {}
        }
        i += 1;
    }
    true
}

/// Whether a single, fully live victim is the only one (then the sweep skips
/// it: consolidating it alone is churn).
pub fn skip_lone_live(victims: usize, lone_fully_live: bool, repack_all: bool) -> bool {
    !repack_all && victims == 1 && lone_fully_live
}
