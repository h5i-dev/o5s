//! Decoders must reject arbitrary or mutated input with an error, never a
//! panic or an unbounded allocation. The fuzz/ targets run the same
//! properties under libFuzzer. These run on stable in CI.
use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::{Hash, ObjectStore};
use pico2peta_pack::format::*;
use pico2peta_pack::{PackConfig, PackStore};
use proptest::prelude::*;
use std::sync::Arc;

fn valid_record() -> Vec<u8> {
    encode_record(Kind::Blob, Codec::Raw, &Hash::of(b"hello"), 5, b"hello")
}

fn mutate(mut v: Vec<u8>, edits: &[(usize, u8)], cut: Option<usize>) -> Vec<u8> {
    for (i, b) in edits {
        if !v.is_empty() {
            let n = v.len();
            v[i % n] ^= b;
        }
    }
    if let Some(c) = cut {
        v.truncate(c % (v.len() + 1));
    }
    v
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]

    #[test]
    fn record_decoder_total(raw in proptest::collection::vec(any::<u8>(), 0..200),
                            edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..4),
                            cut in proptest::option::of(any::<usize>()),
                            use_valid in any::<bool>()) {
        let b = if use_valid { mutate(valid_record(), &edits, cut) } else { raw };
        if let Ok((h, p)) = decode_record(&b) {
            let _ = unpack_payload(&h, p);
        }
        let _ = parse_header(&b);
    }

    #[test]
    fn index_decoder_total(raw in proptest::collection::vec(any::<u8>(), 0..300),
                           edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..4),
                           cut in proptest::option::of(any::<usize>()),
                           use_valid in any::<bool>()) {
        let b = if use_valid {
            let mut es = vec![IdxEntry { hash: Hash::of(b"a"), offset: 8, stored_len: 5, raw_len: 5, kind: Kind::Blob, codec: Codec::Raw, frame: None }];
            mutate(encode_index(&mut es), &edits, cut)
        } else { raw };
        let _ = decode_index(&b);
    }

    #[test]
    fn manifest_decoder_total(raw in proptest::collection::vec(any::<u8>(), 0..300)) {
        let _ = decode_manifest(&raw);
    }
}

#[test]
fn hostile_lengths_rejected_without_allocation() {
    // raw_len = u64::MAX on a zstd blob record must not try to allocate it.
    let mut r = encode_record(Kind::Blob, Codec::Zstd, &Hash::of(b"x"), u64::MAX, b"junk");
    let n = r.len();
    let crc = crc32fast::hash(&r[..n - 4]);
    r[n - 4..].copy_from_slice(&crc.to_le_bytes());
    assert!(decode_record(&r).is_err());
    // Index claiming 2^62 entries.
    let mut idx = b"P2PIDX01".to_vec();
    idx.extend_from_slice(&(1u64 << 62).to_le_bytes());
    idx.extend_from_slice(blake3::hash(&idx).as_bytes());
    assert!(decode_index(&idx).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
    /// Recovery of an arbitrary (garbage or mutated) staging file never panics,
    /// and afterwards the store is usable.
    #[test]
    fn staging_recovery_total(garbage in proptest::collection::vec(any::<u8>(), 0..2000), prefix_valid in any::<bool>()) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("staging")).unwrap();
        let mut f = Vec::new();
        if prefix_valid {
            f.extend_from_slice(PACK_MAGIC);
            f.extend_from_slice(&[7u8; 16]); // v2 nonce
            f.extend_from_slice(&valid_record());
        }
        f.extend_from_slice(&garbage);
        std::fs::write(d.path().join("staging/x.pack"), &f).unwrap();
        let backend: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path().join("objects")).unwrap());
        let s = PackStore::open(d.path().join("staging"), backend, PackConfig::tiny()).unwrap();
        if prefix_valid {
            prop_assert_eq!(s.get(&Hash::of(b"hello"), None).unwrap(), b"hello");
        }
        let h = s.put(b"after").unwrap();
        prop_assert_eq!(s.get(&h, None).unwrap(), b"after");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]
    #[test]
    fn frame_decoder_total(raw in proptest::collection::vec(any::<u8>(), 0..400)) {
        if let Ok(h) = parse_header(&raw) {
            let _ = decode_frame_bodies(&h, &raw);
            let _ = frame_members(&h, &raw);
        }
    }
}

#[test]
fn index_v3_is_compact_for_frames() {
    // 1000 frame members in 10 frames, plus 10 plain blobs.
    let mut es = Vec::new();
    for f in 0..10u64 {
        for m in 0..100u32 {
            es.push(IdxEntry {
                hash: Hash::of(&[f as u8, m as u8, (m >> 8) as u8]),
                offset: 24 + f * 10_000,
                stored_len: 9_000,
                raw_len: 50,
                kind: Kind::Blob,
                codec: Codec::Zstd,
                frame: Some((m * 50, 5_000)),
            });
        }
        es.push(IdxEntry {
            hash: Hash::of(&[f as u8, 255]),
            offset: 200_000 + f * 100,
            stored_len: 40,
            raw_len: 60,
            kind: Kind::Blob,
            codec: Codec::Zstd,
            frame: None,
        });
    }
    let b = encode_index(&mut es);
    let per = b.len() as f64 / es.len() as f64;
    assert!(per < 47.0, "index bytes per entry: {per:.1}");
    let mut back = decode_index(&b).unwrap();
    back.sort_by_key(|e| e.hash);
    assert_eq!(back, es);
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]
    #[test]
    fn midx_and_idx3_views_total(raw in proptest::collection::vec(any::<u8>(), 0..400), as_midx in any::<bool>()) {
        let mut b = if as_midx { MIDX_MAGIC.to_vec() } else { IDX_MAGIC_V3.to_vec() };
        b.extend_from_slice(&raw);
        if let Ok(v) = MidxView::new(b.clone()) {
            let _ = v.find_all(&Hash::of(b"x"));
        }
        if let Ok(v) = IdxV3View::new(b) {
            let _ = v.find(&Hash::of(b"x"));
        }
    }
}

#[test]
fn midx_roundtrip_merges_sorted_packs() {
    let mk = |seed: u8, n: u8| {
        let mut es: Vec<IdxEntry> = (0..n)
            .map(|i| IdxEntry {
                hash: Hash::of(&[seed, i]),
                offset: 24 + i as u64 * 100,
                stored_len: 10,
                raw_len: 10,
                kind: Kind::Blob,
                codec: Codec::Raw,
                frame: None,
            })
            .collect();
        IdxV3View::new(encode_index(&mut es)).unwrap()
    };
    let (a, b) = (mk(1, 50), mk(2, 30));
    let na = "a".repeat(64);
    let nb = "b".repeat(64);
    let mut out = Vec::new();
    write_midx(&[(na.as_str(), &a), (nb.as_str(), &b)], &mut out).unwrap();
    let m = MidxView::new(out).unwrap();
    for i in 0..50u8 {
        let hits = m.find_all(&Hash::of(&[1, i]));
        assert_eq!(hits.len(), 1);
        assert_eq!(a.entry(hits[0].1 as usize).unwrap().hash, Hash::of(&[1, i]));
    }
    assert!(m.find_all(&Hash::of(b"absent")).is_empty());
}
