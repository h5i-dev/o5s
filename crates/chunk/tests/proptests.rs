use o5s_chunk::{Chunker, FastCdcChunker, XET_MAX, XET_MIN, XetChunker, chunk_lengths};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Lengths cover the input, respect min/max, and are split-invariant.
    #[test]
    fn chunk_invariants(seed in any::<u64>(), len in 0usize..600_000, piece in 1usize..100_000, runs in any::<bool>()) {
        let mut x = seed | 1;
        let d: Vec<u8> = (0..len).map(|i| {
            if runs && (i / 4096) % 3 == 0 { 0 } else {
                x ^= x << 13; x ^= x >> 7; x ^= x << 17; (x >> 24) as u8
            }
        }).collect();
        let xet = chunk_lengths(&mut XetChunker::new(), &d);
        prop_assert_eq!(xet.iter().sum::<usize>(), d.len());
        if let Some((_, init)) = xet.split_last() {
            prop_assert!(init.iter().all(|&l| (XET_MIN..=XET_MAX).contains(&l)));
        }
        let fc = chunk_lengths(&mut FastCdcChunker::pack_default(), &d);
        prop_assert_eq!(fc.iter().sum::<usize>(), d.len());
        let want: Vec<usize> = fastcdc::v2020::FastCDC::new(&d, 16 << 10, 64 << 10, 256 << 10).map(|c| c.length).collect();
        prop_assert_eq!(&fc, &want);
        for mut c in [Box::new(XetChunker::new()) as Box<dyn Chunker>, Box::new(FastCdcChunker::pack_default())] {
            let whole = if c.name() == "xet" { &xet } else { &fc };
            let mut out = Vec::new();
            for p in d.chunks(piece) { c.update(p, &mut out); }
            c.finish(&mut out);
            prop_assert_eq!(&out, whole);
        }
    }
}
