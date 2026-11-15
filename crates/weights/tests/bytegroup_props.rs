use pico2peta_weights::bytegroup::{GroupError, MAX_WIDTH, group, ungroup};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn roundtrip(b in proptest::collection::vec(any::<u8>(), 0..2000), w in 1usize..=MAX_WIDTH) {
        let g = group(&b, w).unwrap();
        prop_assert_eq!(g.len(), b.len());
        prop_assert_eq!(ungroup(&g, w).unwrap(), b.clone());
        // Grouping is a permutation: same multiset of bytes.
        let (mut x, mut y) = (b.clone(), g.clone());
        x.sort_unstable();
        y.sort_unstable();
        prop_assert_eq!(x, y);
    }

    #[test]
    fn any_input_any_width_never_panics(b in proptest::collection::vec(any::<u8>(), 0..300), w in any::<usize>()) {
        match (group(&b, w), ungroup(&b, w)) {
            (Ok(g), Ok(u)) => {
                prop_assert!((1..=MAX_WIDTH).contains(&w));
                prop_assert_eq!(g.len(), b.len());
                prop_assert_eq!(u.len(), b.len());
            }
            (Err(GroupError::BadWidth(x)), Err(GroupError::BadWidth(y))) => {
                prop_assert!(x == w && y == w && !(1..=MAX_WIDTH).contains(&w));
            }
            _ => prop_assert!(false, "group/ungroup disagree on validity"),
        }
    }
}
