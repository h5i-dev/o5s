//! Similarity order (PLAN §6.6):
//! - Candidates are objects sharing a super-feature (one LSH bucket per
//!   super-feature, each query limited to the [`BUCKET_NEAREST`] nearest in size).
//! - Similarity = shared super-features, then equal raw features.
//! - Greedy chaining: start at the largest unplaced object, then repeatedly
//!   append the most similar unplaced candidate of the last placed objects.
//! - Then 2-opt inside windows of [`TWO_OPT_WINDOW`].
//! - Featureless objects go last.
//!
//! Front hints only break ties between equally similar candidates, and only when
//! enabled. Without hints, ties go to the nearest size, then the lower index.

use crate::features::Features;
use std::collections::HashMap;

/// Per bucket, a query considers this many objects nearest in size.
pub const BUCKET_NEAREST: usize = 256;
/// When the current object has no unplaced neighbour, try the neighbours of
/// this many recently placed objects before starting a new chain.
pub const RECENT: usize = 8;
/// 2-opt window.
pub const TWO_OPT_WINDOW: usize = 64;

/// Optional front-supplied ordering hints (tie-breakers only).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hint {
    /// A name-like key (e.g. a path). Equal keys, then longer common prefixes, are "closer".
    pub key: String,
    /// A position (e.g. history order). Nearer is closer.
    pub pos: u64,
}

/// One object to order.
#[derive(Clone, Debug, Default)]
pub struct Item {
    /// Size in bytes.
    pub len: u64,
    /// Content features (`None` = featureless).
    pub feats: Option<Features>,
    /// Front hint, if any.
    pub hint: Option<Hint>,
}

/// LSH: super-feature → items, each bucket sorted by size.
pub(crate) struct Lsh {
    buckets: HashMap<u64, Vec<(u64, usize)>>,
}

impl Lsh {
    pub(crate) fn new(items: &[Item]) -> Lsh {
        let mut buckets: HashMap<u64, Vec<(u64, usize)>> = HashMap::new();
        for (i, it) in items.iter().enumerate() {
            if let Some(f) = &it.feats {
                for s in &f.sf {
                    buckets.entry(*s).or_default().push((it.len, i));
                }
            }
        }
        for b in buckets.values_mut() {
            b.sort_unstable();
        }
        Lsh { buckets }
    }

    /// Candidates of `i` (may contain duplicates and `i` itself).
    pub(crate) fn candidates(&self, items: &[Item], i: usize, out: &mut Vec<usize>) {
        out.clear();
        let Some(f) = &items[i].feats else { return };
        let len = items[i].len;
        for s in &f.sf {
            let Some(b) = self.buckets.get(s) else { continue };
            if b.len() <= BUCKET_NEAREST {
                out.extend(b.iter().map(|&(_, j)| j));
            } else {
                let p = b.partition_point(|&(l, _)| l < len);
                let lo = p.saturating_sub(BUCKET_NEAREST / 2);
                let hi = (lo + BUCKET_NEAREST).min(b.len());
                let lo = hi.saturating_sub(BUCKET_NEAREST);
                out.extend(b[lo..hi].iter().map(|&(_, j)| j));
            }
        }
    }
}

/// Similarity of two items: (shared super-features, equal raw features).
pub(crate) fn sim(a: &Item, b: &Item) -> (usize, usize) {
    match (&a.feats, &b.feats) {
        (Some(x), Some(y)) => (x.shared(y), x.equal(y)),
        _ => (0, 0),
    }
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// Tie-break rank of candidate `j` for the current object `i` (smaller is
/// better). Hints first when enabled, then size distance, then index.
fn tie(items: &[Item], i: usize, j: usize, hints: bool) -> (u8, usize, u64, u64, usize) {
    let size = items[i].len.abs_diff(items[j].len);
    match (hints, &items[i].hint, &items[j].hint) {
        (true, Some(a), Some(b)) => {
            let same = if a.key == b.key { 0 } else { 1 };
            (same, usize::MAX - common_prefix(&a.key, &b.key), a.pos.abs_diff(b.pos), size, j)
        }
        _ => (0, 0, 0, size, j),
    }
}

/// A similarity order of `items` (a permutation of `0..items.len()`).
pub fn order(items: &[Item], use_hints: bool) -> Vec<usize> {
    let n = items.len();
    let lsh = Lsh::new(items);
    let mut placed = vec![false; n];
    let mut out: Vec<usize> = Vec::with_capacity(n);
    // Chain starts: the first is the largest object; each later chain starts at
    // the unplaced object whose coarse sketch is nearest to the last placed
    // object's (searched among the COARSE_NEAREST numeric neighbours of its
    // SimHash, then by Hamming distance), so consecutive chains have similar content.
    let mut by_coarse: std::collections::BTreeSet<(u64, usize)> =
        (0..n).filter_map(|i| items[i].feats.as_ref().map(|f| (f.coarse, i))).collect();
    let first =
        (0..n).filter(|&i| items[i].feats.is_some()).max_by(|&a, &b| items[a].len.cmp(&items[b].len).then(b.cmp(&a)));
    let mut cand = Vec::new();
    let mut next_start = first;
    while let Some(s) = next_start.take() {
        if placed[s] {
            next_start = nearest_start(items, &by_coarse, out.last().copied().unwrap_or(s));
            continue;
        }
        placed[s] = true;
        by_coarse.remove(&(coarse_of(items, s), s));
        out.push(s);
        loop {
            // The best unplaced neighbour of the most recent object that has one.
            let mut next = None;
            let recent = out.len().saturating_sub(RECENT);
            for k in (recent..out.len()).rev() {
                let cur = out[k];
                lsh.candidates(items, cur, &mut cand);
                let mut best: Option<(usize, (usize, usize))> = None;
                for &j in &cand {
                    if placed[j] {
                        continue;
                    }
                    let sc = sim(&items[cur], &items[j]);
                    if sc.0 == 0 {
                        continue;
                    }
                    let better = match best {
                        None => true,
                        Some((b, bs)) => {
                            sc > bs || (sc == bs && tie(items, cur, j, use_hints) < tie(items, cur, b, use_hints))
                        }
                    };
                    if better {
                        best = Some((j, sc));
                    }
                }
                if let Some((j, _)) = best {
                    next = Some(j);
                    break;
                }
            }
            match next {
                Some(j) => {
                    placed[j] = true;
                    by_coarse.remove(&(coarse_of(items, j), j));
                    out.push(j);
                }
                None => break,
            }
        }
        next_start = nearest_start(items, &by_coarse, out.last().copied().unwrap_or(s));
    }
    two_opt(items, &mut out);
    // Featureless objects last: by hint, or by size.
    let mut rest: Vec<usize> = (0..n).filter(|&i| !placed[i]).collect();
    rest.sort_by(|&a, &b| {
        let ka = (use_hints).then(|| items[a].hint.as_ref()).flatten();
        let kb = (use_hints).then(|| items[b].hint.as_ref()).flatten();
        match (ka, kb) {
            (Some(x), Some(y)) => x.key.cmp(&y.key).then(x.pos.cmp(&y.pos)).then(a.cmp(&b)),
            _ => items[a].len.cmp(&items[b].len).then(a.cmp(&b)),
        }
    });
    out.extend(rest);
    out
}

/// Neighbours (by SimHash value, on each side) considered when choosing the
/// next chain start.
pub const COARSE_NEAREST: usize = 64;

fn coarse_of(items: &[Item], i: usize) -> u64 {
    items[i].feats.as_ref().map_or(0, |f| f.coarse)
}

/// The unplaced object nearest to `last` by coarse sketch: among the
/// [`COARSE_NEAREST`] numeric neighbours on each side, minimum Hamming
/// distance, then nearest size, then lower index.
fn nearest_start(items: &[Item], set: &std::collections::BTreeSet<(u64, usize)>, last: usize) -> Option<usize> {
    let c = coarse_of(items, last);
    let len = items[last].len;
    let after = set.range((c, 0)..).take(COARSE_NEAREST);
    let before = set.range(..(c, 0)).rev().take(COARSE_NEAREST);
    after.chain(before).min_by_key(|&&(h, i)| ((h ^ c).count_ones(), items[i].len.abs_diff(len), i)).map(|&(_, i)| i)
}

fn score(items: &[Item], a: usize, b: usize) -> i64 {
    let (s, e) = sim(&items[a], &items[b]);
    (s * 16 + e) as i64
}

/// One pass of 2-opt (segment reversal) inside overlapping windows, keeping
/// any reversal that increases the summed similarity of adjacent objects.
fn two_opt(items: &[Item], o: &mut [usize]) {
    let n = o.len();
    let mut start = 0;
    while start + 3 < n {
        let end = (start + TWO_OPT_WINDOW).min(n);
        for a in start + 1..end - 1 {
            for b in a + 1..end - 1 {
                let before = score(items, o[a - 1], o[a]) + score(items, o[b], o[b + 1]);
                let after = score(items, o[a - 1], o[b]) + score(items, o[a], o[b + 1]);
                if after > before {
                    o[a..=b].reverse();
                }
            }
        }
        start += TWO_OPT_WINDOW / 2;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::features;

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

    /// Versions of 3 unrelated "files", shuffled: similarity order puts each
    /// family together, with or without hints, and it's a permutation.
    #[test]
    fn groups_families_without_hints() {
        let mut items = Vec::new();
        let mut family = Vec::new();
        for f in 0..3u64 {
            let base = blob(f + 1, 20_000);
            for v in 0..6 {
                let mut b = base.clone();
                for k in 0..v * 3 {
                    let l = b.len();
                    b[(k * 997) % l] ^= 0x5a;
                }
                items.push(Item { len: b.len() as u64, feats: features(&b), hint: None });
                family.push(f);
            }
        }
        // Interleave the families.
        let perm: Vec<usize> = (0..6).flat_map(|v| (0..3).map(move |f| f * 6 + v)).collect();
        let items: Vec<Item> = perm.iter().map(|&i| items[i].clone()).collect();
        let family: Vec<u64> = perm.iter().map(|&i| family[i]).collect();
        let o = order(&items, false);
        let mut sorted = o.clone();
        sorted.sort();
        assert_eq!(sorted, (0..items.len()).collect::<Vec<_>>());
        let switches = o.windows(2).filter(|w| family[w[0]] != family[w[1]]).count();
        assert_eq!(switches, 2, "{:?}", o.iter().map(|&i| family[i]).collect::<Vec<_>>());
    }

    #[test]
    fn featureless_and_empty() {
        assert!(order(&[], true).is_empty());
        let items = vec![Item { len: 3, feats: None, hint: None }, Item { len: 1, feats: None, hint: None }];
        assert_eq!(order(&items, false), vec![1, 0]);
    }
}
