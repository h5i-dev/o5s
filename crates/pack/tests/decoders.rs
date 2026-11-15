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

/// Golden vectors for record codec 2 (`GroupedZstd`): the byte-grouping
/// transform is part of the on-disk format, so its exact output is pinned
/// here (a change to `pico2peta_weights::bytegroup` that alters bytes must
/// fail this test, not silently break stored packs).
#[test]
fn grouped_codec_golden_and_malformed() {
    use pico2peta_weights::bytegroup::group;
    assert_eq!(group(&[0, 1, 2, 3, 4, 5, 6], 2).unwrap(), [0, 2, 4, 1, 3, 5, 6]);
    assert_eq!(group(&[0, 1, 2, 3, 4, 5, 6, 7, 8], 4).unwrap(), [0, 4, 1, 5, 2, 6, 3, 7, 8]);
    assert_eq!(group(&[9, 8, 7], 1).unwrap(), [9, 8, 7]);

    // A grouped record built by hand decodes to the original bytes.
    let raw: Vec<u8> = (0..10_001u32).map(|i| if i % 2 == 0 { (i / 7) as u8 } else { 0x3f }).collect();
    let mut payload = vec![2u8];
    payload.extend(zstd::bulk::compress(&group(&raw, 2).unwrap(), 3).unwrap());
    let rec = encode_record(Kind::Blob, Codec::GroupedZstd, &Hash::of(&raw), raw.len() as u64, &payload);
    assert_eq!(rec[5], 2, "codec byte on disk");
    let (h, p) = decode_record(&rec).unwrap();
    assert_eq!(unpack_payload(&h, p).unwrap(), raw);

    // Malformed: errors, never panics.
    let bad = |kind: Kind, payload: &[u8], raw_len: u64| {
        let rec = encode_record(kind, Codec::GroupedZstd, &Hash::of(b"x"), raw_len, payload);
        let (h, p) = decode_record(&rec).unwrap();
        assert!(unpack_payload(&h, p).is_err(), "{kind:?} {payload:?}");
    };
    bad(Kind::Blob, &[], 0); // no width byte
    let mut w0 = vec![0u8];
    w0.extend(zstd::bulk::compress(b"abcd", 3).unwrap());
    bad(Kind::Blob, &w0, 4); // width 0
    let mut w99 = vec![99u8];
    w99.extend(zstd::bulk::compress(b"abcd", 3).unwrap());
    bad(Kind::Blob, &w99, 4); // width out of range
    bad(Kind::Blob, &[2, 1, 2, 3], 4); // not zstd
    bad(Kind::Manifest, &payload, raw.len() as u64); // codec 2 is blob-only
    let mut short = vec![2u8];
    short.extend(zstd::bulk::compress(&raw, 3).unwrap());
    bad(Kind::Blob, &short, 10); // raw_len smaller than the content
}
