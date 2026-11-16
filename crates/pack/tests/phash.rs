//! The multi-core whole-object hash equals the serial BLAKE3 on every input.

use pico2peta_core::Hash;
use pico2peta_pack::phash::{self, SEGMENT};
use proptest::prelude::*;
use std::sync::Arc;

fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005) | 1;
    let mut v = Vec::with_capacity(n);
    while v.len() < n {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes()[..(n - v.len()).min(8)]);
    }
    v
}

/// Lengths around the cases that break tree-hash splitting: segment and chunk
/// boundaries, a tail of one byte or one chunk, and one to three segments.
fn length() -> impl Strategy<Value = usize> {
    prop_oneof![
        0usize..(4 * SEGMENT),
        (0usize..12, -2048i64..=2048).prop_map(|(k, d)| (k as i64 * SEGMENT as i64 + d).max(0) as usize),
        (1usize..12, 0usize..4).prop_map(|(k, c)| k * SEGMENT + c * 1024),
        (1usize..12).prop_map(|k| k * SEGMENT + 1),
    ]
}

/// Splits `data` into buffers at random points (the stream's chunking).
fn split(data: &[u8], cuts: &[usize]) -> Vec<Arc<Vec<u8>>> {
    let mut at: Vec<usize> = cuts.iter().map(|c| if data.is_empty() { 0 } else { c % (data.len() + 1) }).collect();
    at.push(0);
    at.push(data.len());
    at.sort_unstable();
    at.dedup();
    at.windows(2).map(|w| Arc::new(data[w[0]..w[1]].to_vec())).collect()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 96, .. ProptestConfig::default() })]

    #[test]
    fn equals_serial(len in length(), seed in any::<u64>(), threads in 1usize..5, cuts in prop::collection::vec(any::<usize>(), 0..24)) {
        let data = bytes(seed, len);
        let want = Hash(*blake3::hash(&data).as_bytes());
        prop_assert_eq!(phash::hash(&data, threads), want);
        prop_assert_eq!(phash::hash_stream(split(&data, &cuts), len as u64, threads), Some(want));
    }

    #[test]
    fn wrong_length_is_refused(len in (2 * SEGMENT + 1)..(5 * SEGMENT), seed in any::<u64>(), short in 1usize..4096) {
        let data = bytes(seed, len);
        // Fewer or more bytes than declared never yields a hash.
        prop_assert_eq!(phash::hash_stream(split(&data, &[]), len as u64 + short as u64, 3), None);
        prop_assert_eq!(phash::hash_stream(split(&data, &[]), len as u64 - short as u64, 3), None);
    }
}

#[test]
fn exact_segment_multiples_and_one_byte_tails() {
    for k in [2usize, 3, 4, 5, 7, 8, 9, 16, 17] {
        for extra in [0usize, 1, 1023, 1024, 1025] {
            let data = bytes(k as u64 * 31 + extra as u64, k * SEGMENT + extra);
            let want = Hash(*blake3::hash(&data).as_bytes());
            assert_eq!(phash::hash(&data, 4), want, "k={k} extra={extra}");
            assert_eq!(phash::hash_stream([Arc::new(data.clone())], data.len() as u64, 4), Some(want));
        }
    }
}
