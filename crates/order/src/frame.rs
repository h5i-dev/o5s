//! Similarity-aware framing (PLAN §6.6).
//! - *Natural cut:* before an object that shares no super-feature with any of
//!   the frame's last `lookback` objects, once the frame holds ≥ `min` bytes.
//! - *Forced cut:* when the next object would push the frame over `cap`.
//! - *Reference prefix:* a forced cut inside a similar run (the next object
//!   shares super-features with the previous frame) gets the most similar
//!   earlier object as a zstd reference prefix. The reference must sit in a
//!   frame of depth < `max_ref_depth`, so a read decodes at most
//!   `max_ref_depth + 1` frames.
//!
//! `fixed = true` gives fixed-size framing (cuts only at the cap, no
//! references), which is Exp1 V2's layout.

use crate::order::{Item, Lsh, sim};

/// Framing parameters.
#[derive(Clone, Debug)]
pub struct FrameConfig {
    /// Minimum raw frame size before a natural cut.
    pub min: u64,
    /// Maximum raw frame size (a single larger object gets its own frame).
    pub cap: u64,
    /// Objects at the frame's end compared for a natural cut.
    pub lookback: usize,
    /// Reference depth bound (0 = no references).
    pub max_ref_depth: u32,
    /// Fixed-size framing: cut only at the cap, no references.
    pub fixed: bool,
}

/// Frozen Experiment 3 cap (PLAN §6.6 amendment): 6 MiB raw.
pub const DEFAULT_CAP: u64 = 6 << 20;

impl Default for FrameConfig {
    /// The values frozen for Experiment 3 (PLAN §6.6 amendment, tuned on
    /// ripgrep and jq only): cap 6 MiB, natural cuts off (`min = cap`),
    /// lookback 8, reference depth ≤ 2.
    fn default() -> Self {
        FrameConfig { min: DEFAULT_CAP, cap: DEFAULT_CAP, lookback: 8, max_ref_depth: 2, fixed: false }
    }
}

/// One frame: its members (item indices, in order) and an optional reference
/// object (an item in an earlier frame) used as zstd reference prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Member items, in order.
    pub members: Vec<usize>,
    /// Reference-prefix item, if any.
    pub reference: Option<usize>,
    /// Reference depth (0 = none; a frame referencing a depth-d frame has d+1).
    pub depth: u32,
}

/// Cuts `order` into frames. `boundary(prev, next)` forces a plain cut
/// between two items (e.g. Exp1 V2's partition boundaries); pass `|_, _| false`
/// for none.
pub fn frames(
    items: &[Item],
    order: &[usize],
    cfg: &FrameConfig,
    boundary: &dyn Fn(usize, usize) -> bool,
) -> Vec<Frame> {
    let lsh = (!cfg.fixed && cfg.max_ref_depth > 0).then(|| Lsh::new(items));
    let mut frame_of: Vec<Option<u32>> = vec![None; items.len()];
    let mut out: Vec<Frame> = Vec::new();
    let mut cur = Frame { members: Vec::new(), reference: None, depth: 0 };
    let mut size = 0u64;
    let mut cand = Vec::new();
    let similar_to_tail = |members: &[usize], n: usize, lookback: usize| {
        members.iter().rev().take(lookback).any(|&m| sim(&items[m], &items[n]).0 > 0)
    };
    for &n in order {
        let len = items[n].len;
        if let Some(&prev) = cur.members.last() {
            let forced = size + len > cfg.cap;
            let natural = !cfg.fixed && size >= cfg.min && !similar_to_tail(&cur.members, n, cfg.lookback);
            if boundary(prev, n) || forced || natural {
                let in_run = forced && !natural && !boundary(prev, n) && similar_to_tail(&cur.members, n, cfg.lookback);
                let id = out.len() as u32;
                for &m in &cur.members {
                    frame_of[m] = Some(id);
                }
                out.push(std::mem::replace(&mut cur, Frame { members: Vec::new(), reference: None, depth: 0 }));
                size = 0;
                if in_run && let Some(lsh) = &lsh {
                    // Most similar earlier object in a shallow enough frame.
                    lsh.candidates(items, n, &mut cand);
                    let mut best: Option<(usize, (usize, usize))> = None;
                    for &j in &cand {
                        let Some(fj) = frame_of[j] else { continue };
                        if out[fj as usize].depth >= cfg.max_ref_depth {
                            continue;
                        }
                        let sc = sim(&items[n], &items[j]);
                        if sc.0 == 0 {
                            continue;
                        }
                        if best.is_none_or(|(b, bs)| sc > bs || (sc == bs && j > b)) {
                            best = Some((j, sc));
                        }
                    }
                    if let Some((j, _)) = best {
                        cur.reference = Some(j);
                        cur.depth = out[frame_of[j].unwrap_or(0) as usize].depth + 1;
                    }
                }
            }
        }
        cur.members.push(n);
        size += len;
    }
    if !cur.members.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::features;
    use crate::order::order;

    fn blob(seed: u64, n: usize) -> Vec<u8> {
        let mut x = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    fn versions(seed: u64, n: usize, v: usize) -> Vec<Vec<u8>> {
        let base = blob(seed, n);
        (0..v)
            .map(|k| {
                let mut b = base.clone();
                b[(k * 131) % n] ^= 1;
                b
            })
            .collect()
    }

    #[test]
    fn cuts_where_content_changes_and_references_at_forced_cuts() {
        // Two families; the first is longer than the cap, so it needs a forced cut.
        let mut data = versions(1, 100_000, 8);
        data.extend(versions(2, 100_000, 3));
        let items: Vec<Item> =
            data.iter().map(|b| Item { len: b.len() as u64, feats: features(b), hint: None }).collect();
        let o = order(&items, false);
        let cfg = FrameConfig { min: 1, cap: 450_000, ..FrameConfig::default() };
        let fr = frames(&items, &o, &cfg, &|_, _| false);
        // Every item exactly once, in order.
        let flat: Vec<usize> = fr.iter().flat_map(|f| f.members.clone()).collect();
        assert_eq!(flat, o);
        // No frame mixes the families (natural cut at the switch).
        for f in &fr {
            let fam: std::collections::HashSet<bool> = f.members.iter().map(|&m| m < 8).collect();
            assert_eq!(fam.len(), 1, "{fr:?}");
        }
        // The forced cut inside family 1 got a reference to an earlier family-1 object.
        let with_ref: Vec<&Frame> = fr.iter().filter(|f| f.reference.is_some()).collect();
        assert!(!with_ref.is_empty(), "{fr:?}");
        for f in &fr {
            if let Some(r) = f.reference {
                assert!(f.depth >= 1 && f.depth <= cfg.max_ref_depth);
                assert_eq!(r < 8, f.members[0] < 8);
                let pos_ref = flat.iter().position(|&x| x == r).unwrap();
                let pos_first = flat.iter().position(|&x| x == f.members[0]).unwrap();
                assert!(pos_ref < pos_first, "references point backwards");
            }
        }
    }

    #[test]
    fn fixed_mode_is_v2() {
        let items: Vec<Item> = (0..10).map(|i| Item { len: 1000 + i, feats: None, hint: None }).collect();
        let o: Vec<usize> = (0..10).collect();
        let cfg = FrameConfig { cap: 3000, fixed: true, ..FrameConfig::default() };
        let fr = frames(&items, &o, &cfg, &|a, b| a < 5 && b >= 5);
        let sizes: Vec<usize> = fr.iter().map(|f| f.members.len()).collect();
        assert_eq!(sizes, vec![2, 2, 1, 2, 2, 1]);
        assert!(fr.iter().all(|f| f.reference.is_none()));
    }
}
