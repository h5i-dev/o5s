//! Line-range edits on one immutable base: validating a proposal's ranges,
//! merging concurrent versions, and rendering. A range `(start, end, bytes)`
//! replaces lines `start..end` of the base (an insertion when they are equal).
//!
//! The native adapter sorts the ranges of every version by `(start, end)` and
//! concatenates them; `merge` then needs one linear pass that compares each
//! range with the last one it kept. That suffices because, on sorted ranges,
//! "does not conflict with its predecessor" is transitive (proved in
//! `proofs/Lines.lean`). Results are indices into the input, so no content is
//! copied.
use crate::interval;

/// `(start, end, replacement)` in zero-based line coordinates of the base.
pub type Range = (u64, u64, Vec<u8>);

/// The same edit: equal coordinates and equal replacement bytes.
pub fn same(a: &Range, b: &Range) -> bool {
    a.0 == b.0 && a.1 == b.1 && h5i_app_std::bytes::eq(&a.2, &b.2)
}

/// `(a.start, a.end) <= (b.start, b.end)` lexicographically.
pub fn key_le(a: &Range, b: &Range) -> bool {
    a.0 < b.0 || (a.0 == b.0 && a.1 <= b.1)
}

/// Every adjacent pair is in key order.
pub fn sorted(rs: &Vec<Range>) -> bool {
    let mut ok = true;
    let mut i = 1;
    while i < rs.len() {
        ok = ok && key_le(&rs[i - 1], &rs[i]);
        i += 1;
    }
    ok
}

pub struct Merge {
    pub ok: bool,
    pub kept: Vec<usize>,
}

/// One range of the merge: refuse a reversed range or a conflict with the
/// last kept range, skip an identical copy of it, keep anything else.
fn merge_step(state: Merge, rs: &Vec<Range>, i: usize) -> Merge {
    if !state.ok {
        state
    } else if rs[i].0 > rs[i].1 {
        Merge {
            ok: false,
            kept: state.kept,
        }
    } else if state.kept.len() == 0 {
        let mut kept = state.kept;
        kept.push(i);
        Merge { ok: true, kept }
    } else {
        let last = &rs[state.kept[state.kept.len() - 1]];
        if same(last, &rs[i]) {
            state
        } else if interval::conflict(last.0, last.1, rs[i].0, rs[i].1) {
            Merge {
                ok: false,
                kept: state.kept,
            }
        } else {
            let mut kept = state.kept;
            kept.push(i);
            Merge { ok: true, kept }
        }
    }
}

/// Merge ranges sorted by key: the indices of the distinct ranges, or `None`
/// if a range is reversed or two different ranges conflict. Identical edits
/// from different versions count once.
pub fn merge(rs: &Vec<Range>) -> Option<Vec<usize>> {
    let mut state = Merge {
        ok: true,
        kept: Vec::new(),
    };
    let mut i = 0;
    while i < rs.len() {
        state = merge_step(state, rs, i);
        i += 1;
    }
    if state.ok { Some(state.kept) } else { None }
}

/// One proposal's ranges for one file: sorted, and pairwise free of
/// conflicts, including with an identical copy.
pub fn ranges_valid(rs: &Vec<Range>) -> bool {
    if !sorted(rs) {
        false
    } else {
        match merge(rs) {
            Some(kept) => kept.len() == rs.len(),
            None => false,
        }
    }
}

/// Byte intervals of the kept ranges on a base with line `boundaries`
/// (`coordinates::line_boundaries`), or `None` if a range ends past the last
/// boundary.
pub fn chunks(
    boundaries: &Vec<usize>,
    rs: &Vec<Range>,
    kept: &Vec<usize>,
) -> Option<Vec<(usize, usize, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut ok = true;
    let mut i = 0;
    while i < kept.len() {
        let r = &rs[kept[i]];
        if ok && r.0 <= r.1 && r.1 < boundaries.len() as u64 {
            let start = boundaries[r.0 as usize];
            let end = boundaries[r.1 as usize];
            out.push((start, end, h5i_app_std::bytes::slice(&r.2, 0, r.2.len())));
        } else {
            ok = false;
        }
        i += 1;
    }
    if ok { Some(out) } else { None }
}

/// The base with the kept ranges applied, or `None` if a range is out of
/// bounds or the result would not fit.
pub fn render(original: &Vec<u8>, rs: &Vec<Range>, kept: &Vec<usize>) -> Option<Vec<u8>> {
    let boundaries = crate::coordinates::line_boundaries(original);
    match chunks(&boundaries, rs, kept) {
        Some(c) => interval::checked_render(original, &c),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(s: u64, e: u64, c: &str) -> Range {
        (s, e, c.as_bytes().to_vec())
    }

    #[test]
    fn merges_disjoint_and_identical_and_refuses_conflicts() {
        let rs = vec![r(0, 1, "A"), r(0, 1, "A"), r(2, 2, "x"), r(3, 5, "")];
        assert_eq!(merge(&rs), Some(vec![0, 2, 3]));
        assert!(!ranges_valid(&rs));
        assert!(ranges_valid(&vec![r(0, 1, "A"), r(2, 2, "x"), r(3, 5, "")]));
        assert_eq!(merge(&vec![r(0, 1, "A"), r(0, 1, "B")]), None);
        // Insertion at a replacement boundary conflicts.
        assert_eq!(merge(&vec![r(1, 3, ""), r(3, 3, "x")]), None);
        assert_eq!(merge(&vec![r(1, 3, ""), r(3, 4, "y")]), Some(vec![0, 1]));
        assert_eq!(merge(&vec![r(2, 1, "")]), None);
        assert!(!sorted(&vec![r(2, 2, ""), r(1, 5, "")]));
    }

    #[test]
    fn renders_through_line_boundaries() {
        let base = b"one\ntwo\nthree\n".to_vec();
        let rs = vec![r(0, 0, "zero\n"), r(1, 2, "TWO\n"), r(3, 3, "four\n")];
        let kept = merge(&rs).unwrap();
        assert_eq!(
            render(&base, &rs, &kept).unwrap(),
            b"zero\none\nTWO\nthree\nfour\n"
        );
        assert_eq!(render(&base, &vec![r(3, 4, "")], &vec![0]), None);
    }

    /// Every sorted order of the same ranges merges to the same edits.
    #[test]
    fn merge_ignores_the_order_of_equal_keys() {
        let all = [
            r(0, 0, "a"),
            r(0, 0, "a"),
            r(1, 2, "b"),
            r(4, 4, "c"),
            r(4, 4, "c"),
        ];
        let base =
            merge(&all.to_vec()).map(|k| k.iter().map(|i| all[*i].clone()).collect::<Vec<_>>());
        let mut swapped = all.to_vec();
        swapped.swap(0, 1);
        swapped.swap(3, 4);
        let other =
            merge(&swapped).map(|k| k.iter().map(|i| swapped[*i].clone()).collect::<Vec<_>>());
        assert_eq!(base, other);
    }
}
