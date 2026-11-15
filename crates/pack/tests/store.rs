use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::fault::Faults;
use pico2peta_core::{Error, Hash, MemStore, ObjectStore};
use pico2peta_pack::{PackConfig, PackStore};
use proptest::prelude::*;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

fn backend(dir: &Path) -> Arc<dyn Backend> {
    Arc::new(LocalDisk::new(dir.join("objects")).unwrap())
}

fn open(dir: &Path, cfg: PackConfig) -> PackStore {
    PackStore::open(dir.join("staging"), backend(dir), cfg).unwrap()
}

fn pseudo(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

#[test]
fn small_and_large_roundtrip_with_reopen() {
    let d = tempfile::tempdir().unwrap();
    let big = pseudo(20_000, 1);
    let (h1, h2);
    {
        let s = open(d.path(), PackConfig::tiny());
        h1 = s.put(b"hello").unwrap();
        h2 = s.put(&big).unwrap();
        assert_eq!(h2, Hash::of(&big));
        assert_eq!(s.get(&h1, None).unwrap(), b"hello");
        assert_eq!(s.get(&h2, None).unwrap(), big);
        assert_eq!(s.get(&h2, Some(1000..15000)).unwrap(), &big[1000..15000]);
        assert!(s.stats().unwrap().manifests >= 1);
        s.flush().unwrap();
    }
    let s = open(d.path(), PackConfig::tiny());
    assert_eq!(s.get(&h1, None).unwrap(), b"hello");
    assert_eq!(s.get(&h2, Some(19_999..20_000)).unwrap(), &big[19_999..]);
    assert_eq!(s.size(&h2).unwrap(), 20_000);
    assert!(s.stats().unwrap().sealed_packs >= 2, "16 KiB cap should have sealed packs");
}

#[test]
fn put_stream_matches_put_and_is_chunked() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::tiny());
    for n in [0usize, 1, 2047, 2048, 2049, 100_000] {
        let data = pseudo(n, n as u64);
        let h = s.put_stream(&mut &data[..]).unwrap();
        assert_eq!(h, Hash::of(&data), "len {n}");
        assert_eq!(s.put(&data).unwrap(), h);
        let mut out = Vec::new();
        s.open(&h, None).unwrap().read_to_end(&mut out).unwrap();
        assert_eq!(out, data);
    }
}

#[test]
fn chunk_dedup_of_shifted_data() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::tiny());
    let a = pseudo(50_000, 7);
    let mut b = b"prefix-insert".to_vec();
    b.extend_from_slice(&a);
    s.put(&a).unwrap();
    let blobs_a = s.stats().unwrap().blobs;
    s.put(&b).unwrap();
    let new = s.stats().unwrap().blobs - blobs_a;
    assert!(new <= 3, "CDC should resync after an insert; got {new} new chunks");
}

#[test]
fn torn_tail_of_orphan_is_truncated_and_sealed() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    // Crash the handle on its 4th fault point: create(1), write(2), write(3), torn write(4).
    let faults = Faults::crash_after(3);
    let s = PackStore::open_with_faults(d.path().join("staging"), backend(d.path()), cfg.clone(), faults).unwrap();
    let h1 = s.put(b"first").unwrap();
    let h2 = s.put(b"second").unwrap();
    assert!(s.put(b"torn record payload that is long enough").is_err());
    drop(s); // dead process: staging file stays (unsealed, torn)
    let s = open(d.path(), cfg);
    assert_eq!(s.get(&h1, None).unwrap(), b"first");
    assert_eq!(s.get(&h2, None).unwrap(), b"second");
    assert!(!s.contains(&Hash::of(b"torn record payload that is long enough")).unwrap());
    assert_eq!(s.stats().unwrap().sealed_packs, 1);
    assert_eq!(std::fs::read_dir(d.path().join("staging")).unwrap().count(), 0);
}

#[test]
fn live_staging_file_is_not_adopted() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    let a = open(d.path(), cfg.clone());
    let h = a.put(b"in a's open pack").unwrap();
    let staged = a.staging_file().unwrap();
    let b = open(d.path(), cfg.clone());
    assert!(staged.exists(), "b must not steal a's locked staging file");
    // Not visible to b until a flushes.
    assert!(!b.contains(&h).unwrap());
    a.flush().unwrap();
    assert!(b.contains(&h).unwrap(), "visible after flush via re-list on miss");
    assert_eq!(b.get(&h, None).unwrap(), b"in a's open pack");
}

#[test]
fn concurrent_writers_threads_and_handles() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig::tiny();
    let stores: Vec<PackStore> = (0..3).map(|_| open(d.path(), cfg.clone())).collect();
    let hashes: Vec<Vec<(Hash, Vec<u8>)>> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..6)
            .map(|t| {
                let s = stores[t % 3].clone();
                sc.spawn(move || {
                    let mut v = Vec::new();
                    for i in 0..60u64 {
                        let n = if i % 10 == 0 { 9_000 } else { (i as usize * 37) % 1500 };
                        let data = pseudo(n, t as u64 * 1000 + i % 20); // overlap between threads
                        v.push((s.put(&data).unwrap(), data));
                    }
                    s.flush().unwrap();
                    v
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    drop(stores);
    let s = open(d.path(), cfg);
    for (h, data) in hashes.into_iter().flatten() {
        assert_eq!(s.get(&h, None).unwrap(), data);
    }
}

#[test]
fn corruption_is_detected() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig { pack_cap: 1 << 30, zstd_level: 0, ..PackConfig::tiny() });
    let h = s.put(b"some bytes to corrupt").unwrap();
    s.flush().unwrap();
    let dir = d.path().join("objects/packs");
    let pack = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    let mut b = std::fs::read(&pack).unwrap();
    let n = b.len();
    b[n - 10] ^= 0xff;
    std::fs::write(&pack, b).unwrap();
    assert!(matches!(s.get(&h, None), Err(Error::Corrupt(_))));
}

#[test]
fn range_errors() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::tiny());
    let h = s.put(&pseudo(10_000, 3)).unwrap();
    assert!(matches!(s.get(&h, Some(5..10_001)), Err(Error::InvalidRange { .. })));
    #[allow(clippy::reversed_empty_ranges)]
    let bad = 6..5;
    assert!(matches!(s.get(&h, Some(bad)), Err(Error::InvalidRange { .. })));
    assert_eq!(s.get(&h, Some(10_000..10_000)).unwrap(), b"");
    assert!(matches!(s.get(&Hash::of(b"nope"), None), Err(Error::NotFound(_))));
}

/// Crash at every fault point of a workload. After a clean reopen:
/// everything flushed before the crash is intact, every readable object is
/// correct, and the store accepts new writes.
#[test]
fn crash_at_every_fault_point() {
    let cfg = PackConfig { pack_cap: 4096, ..PackConfig::tiny() };
    let objs: Vec<Vec<u8>> = (0..12).map(|i| pseudo(if i % 4 == 0 { 6000 } else { 300 + i * 50 }, i as u64)).collect();
    // Size the sweep with a disarmed run.
    let total = {
        let d = tempfile::tempdir().unwrap();
        let f = Faults::disarmed();
        let s =
            PackStore::open_with_faults(d.path().join("staging"), backend(d.path()), cfg.clone(), f.clone()).unwrap();
        for (i, o) in objs.iter().enumerate() {
            s.put(o).unwrap();
            if i % 5 == 4 {
                s.flush().unwrap();
            }
        }
        s.flush().unwrap();
        f.hits()
    };
    assert!(total > 20, "workload should hit many fault points ({total})");
    for budget in 0..=total {
        let d = tempfile::tempdir().unwrap();
        let mut durable = Vec::new();
        {
            let f = Faults::crash_after(budget);
            let Ok(s) = PackStore::open_with_faults(d.path().join("staging"), backend(d.path()), cfg.clone(), f) else {
                continue;
            };
            let mut pending = Vec::new();
            for (i, o) in objs.iter().enumerate() {
                match s.put(o) {
                    Ok(h) => pending.push(h),
                    Err(_) => break,
                }
                if i % 5 == 4 || i == objs.len() - 1 {
                    if s.flush().is_err() {
                        break;
                    }
                    durable.append(&mut pending);
                }
            }
        }
        let s = open(d.path(), cfg.clone());
        for h in &durable {
            let got = s.get(h, None).unwrap_or_else(|e| panic!("budget {budget}: lost durable {h}: {e}"));
            assert_eq!(Hash::of(&got), *h);
        }
        for h in s.hashes().unwrap() {
            let got = s.get(&h, None).unwrap_or_else(|e| panic!("budget {budget}: unreadable {h}: {e}"));
            assert_eq!(Hash::of(&got), h, "budget {budget}");
        }
        let h = s.put(b"after recovery").unwrap();
        s.flush().unwrap();
        assert_eq!(s.get(&h, None).unwrap(), b"after recovery");
        assert_eq!(std::fs::read_dir(d.path().join("staging")).unwrap().count(), 0, "budget {budget}");
    }
}

#[derive(Debug, Clone)]
enum Op {
    Put(Vec<u8>),
    PutLarge(u64, usize),
    Get(usize, Option<(u16, u16)>),
    Flush,
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => proptest::collection::vec(any::<u8>(), 0..300).prop_map(Op::Put),
        1 => (0..8u64, 2048usize..12_000).prop_map(|(s, n)| Op::PutLarge(s, n)),
        4 => (any::<usize>(), proptest::option::of((any::<u16>(), any::<u16>()))).prop_map(|(i, r)| Op::Get(i, r)),
        1 => Just(Op::Flush),
        1 => Just(Op::Reopen),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]
    /// Reference model: MemStore. Every observable result must agree.
    #[test]
    fn matches_reference_model(ops in proptest::collection::vec(op(), 1..40)) {
        let d = tempfile::tempdir().unwrap();
        let cfg = PackConfig::tiny();
        let mut s = open(d.path(), cfg.clone());
        let model = MemStore::new();
        let mut known: Vec<Hash> = vec![Hash::of(b"never stored")];
        for o in ops {
            match o {
                Op::Put(v) => {
                    let h = s.put(&v).unwrap();
                    prop_assert_eq!(h, model.put(&v).unwrap());
                    known.push(h);
                }
                Op::PutLarge(seed, n) => {
                    let v = pseudo(n, seed);
                    let h = s.put_stream(&mut &v[..]).unwrap();
                    prop_assert_eq!(h, model.put(&v).unwrap());
                    known.push(h);
                }
                Op::Get(i, r) => {
                    let h = known[i % known.len()];
                    let range = r.map(|(a, b)| a as u64..b as u64);
                    let a = s.get(&h, range.clone()).map_err(|e| e.code());
                    let b = model.get(&h, range).map_err(|e| e.code());
                    prop_assert_eq!(a, b);
                }
                Op::Flush => s.flush().unwrap(),
                Op::Reopen => {
                    s.flush().unwrap();
                    drop(s);
                    s = open(d.path(), cfg.clone());
                }
            }
        }
    }
}

#[test]
fn manifest_tree_bounded_fanout_and_ranges() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig::tiny(); // fanout 4
    let s = open(d.path(), cfg.clone());
    let data = pseudo(300_000, 99); // ~1200 chunks → 5+ levels
    let h = s.put_stream(&mut &data[..]).unwrap();
    assert_eq!(h, Hash::of(&data));
    // Every manifest node has at most `fanout` entries; every key is a content hash.
    let mut stack = vec![h];
    let mut nodes = 0;
    while let Some(x) = stack.pop() {
        if let Some(kids) = s.chunks(&x).unwrap() {
            nodes += 1;
            assert!(kids.len() <= cfg.manifest_fanout, "node {x} has {} entries", kids.len());
            let bytes = s.get(&x, None).unwrap();
            assert_eq!(Hash::of(&bytes), x, "interior node keyed by content hash");
            stack.extend(kids);
        }
    }
    assert!(nodes > 50, "expected a deep tree, got {nodes} nodes");
    for (a, b) in [(0, 1), (12_345, 99_999), (299_000, 300_000), (0, 300_000), (150_000, 150_000)] {
        assert_eq!(s.get(&h, Some(a..b)).unwrap(), &data[a as usize..b as usize]);
    }
    // put (slice) and put_stream build the identical tree.
    assert_eq!(s.put(&data).unwrap(), h);
    s.flush().unwrap();
    let s2 = open(d.path(), cfg);
    assert_eq!(s2.get(&h, None).unwrap(), data);
}

#[test]
fn exact_fanout_boundary_objects() {
    // Objects whose chunk count is exactly fanout^k must also round-trip.
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { manifest_fanout: 2, ..PackConfig::tiny() };
    let s = open(d.path(), cfg);
    for n in [2048usize, 2500, 4096, 5000, 8192, 20_000] {
        let data = pseudo(n, n as u64 + 7);
        let h = s.put(&data).unwrap();
        assert_eq!(s.get(&h, None).unwrap(), data, "len {n}");
        assert_eq!(s.size(&h).unwrap(), n as u64);
    }
}

#[test]
fn identical_record_sets_never_alias_one_pack_name() {
    // Two handles write the same single record. Their packs must get distinct
    // names, or GC condemning one would silently delete the other's "copy".
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    for _ in 0..2 {
        let s = open(d.path(), cfg.clone());
        // Bypass dedup by using fresh handles that haven't seen each other's packs yet.
        s.put(b"same bytes").unwrap();
        s.flush().unwrap();
    }
    let packs = std::fs::read_dir(d.path().join("objects/packs"))
        .unwrap()
        .filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "pack"))
        .count();
    assert!(packs >= 1);
    let s = open(d.path(), cfg.clone());
    // Explicit copy of a record into a new pack: must be a new name.
    let h = Hash::of(b"same bytes");
    let before: std::collections::HashSet<_> = s.backend().list("packs/").unwrap().into_iter().map(|m| m.key).collect();
    s.copy_record(&h).unwrap();
    s.flush().unwrap();
    let after: std::collections::HashSet<_> = s.backend().list("packs/").unwrap().into_iter().map(|m| m.key).collect();
    assert_eq!(after.len(), before.len() + 2, "copy must produce a new .pack and .idx");
}

#[test]
fn v1_staging_files_are_still_recovered() {
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("staging")).unwrap();
    let mut f = pico2peta_pack::format::PACK_MAGIC_V1.to_vec();
    f.extend_from_slice(&pico2peta_pack::format::encode_record(
        pico2peta_pack::format::Kind::Blob,
        pico2peta_pack::format::Codec::Raw,
        &Hash::of(b"v1"),
        2,
        b"v1",
    ));
    std::fs::write(d.path().join("staging/old.pack"), f).unwrap();
    let s = open(d.path(), PackConfig::tiny());
    assert_eq!(s.get(&Hash::of(b"v1"), None).unwrap(), b"v1");
}

#[test]
fn frames_roundtrip_and_survive_crash_recovery() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    let members: Vec<Vec<u8>> = (0..20).map(|i| pseudo(100 + i * 30, i as u64)).collect();
    let refs: Vec<&[u8]> = members.iter().map(|m| m.as_slice()).collect();
    let hashes;
    {
        // Crash right after the frame write (no flush, no seal).
        let f = Faults::crash_after(2); // open.create, frame.write
        let s = PackStore::open_with_faults(d.path().join("staging"), backend(d.path()), cfg.clone(), f).unwrap();
        hashes = s.put_frame(&refs, 19).unwrap();
        for (h, m) in hashes.iter().zip(&members) {
            assert_eq!(&s.get(h, None).unwrap(), m);
        }
        assert!(s.put(b"next write crashes").is_err());
    }
    let s = open(d.path(), cfg);
    for (h, m) in hashes.iter().zip(&members) {
        assert_eq!(&s.get(h, None).unwrap(), m, "frame member recovered from orphaned staging");
        assert_eq!(s.size(h).unwrap(), m.len() as u64);
    }
    assert!(s.put_frame(&[&vec![0u8; 5000][..]], 3).is_err(), "members must be below the chunk threshold");
}

#[test]
fn multi_pack_index_covers_many_mapped_packs() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 4096, ..PackConfig::tiny() };
    let mut objs = Vec::new();
    {
        let s = open(d.path(), cfg.clone());
        for i in 0..400u64 {
            let o = pseudo(300 + (i as usize % 7) * 100, i);
            s.put(&o).unwrap();
            objs.push(o);
        }
        s.flush().unwrap();
    }
    let s = PackStore::open(d.path().join("staging"), backend(d.path()), cfg.clone()).unwrap();
    s.set_mmap_index_threshold(0);
    // Force a rebuild path: a fresh handle with the low threshold maps every index.
    drop(s);
    unsafe_env_threshold_zero(|| {
        let s = open(d.path(), cfg.clone());
        assert!(s.stats().unwrap().sealed_packs >= 8);
        let midx = std::fs::read_dir(d.path().join("idx-cache"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("midx-"))
            .count();
        assert_eq!(midx, 1, "one multi-pack index built");
        for o in &objs {
            assert_eq!(&s.get(&Hash::of(o), None).unwrap(), o);
        }
        assert!(!s.contains(&Hash::of(b"absent")).unwrap());
    });
}

/// Runs `f` with every index memory-mapped (threshold 0) for handles opened inside it.
fn unsafe_env_threshold_zero(f: impl FnOnce()) {
    // SAFETY: test-only; the variable is read at PackStore::open. Other tests
    // may observe it, which only switches them to the (equally correct) mmap path.
    unsafe { std::env::set_var("P2P_MMAP_INDEX_BYTES", "0") };
    f();
    unsafe { std::env::remove_var("P2P_MMAP_INDEX_BYTES") };
}

#[test]
fn xor_delta_objects_roundtrip_and_save_space() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig::tiny();
    let s = open(d.path(), cfg.clone());
    let base = pseudo(60_000, 5);
    let hb = s.put(&base).unwrap();
    // "Fine-tune": flip the low bit of every 97th byte, plus a longer tail.
    let mut v2 = base.clone();
    for b in v2.iter_mut().step_by(97) {
        *b ^= 1;
    }
    v2.extend_from_slice(b"appended tail beyond the base");
    s.flush().unwrap();
    let before = dir_bytes(&d.path().join("objects"));
    let h2 = s.put_delta(&hb, &mut &v2[..]).unwrap();
    s.flush().unwrap();
    let delta_cost = dir_bytes(&d.path().join("objects")) - before;
    assert_eq!(h2, Hash::of(&v2), "identity is the content hash");
    assert_eq!(s.stats().unwrap().deltas, 1);
    assert_eq!(s.get(&h2, None).unwrap(), v2);
    for (a, b) in [(0u64, 1u64), (59_990, 60_010), (1234, 40_000), (60_000, v2.len() as u64)] {
        assert_eq!(s.get(&h2, Some(a..b)).unwrap(), &v2[a as usize..b as usize], "range {a}..{b}");
    }
    // A shorter object than its base; and a delta chain.
    let v3: Vec<u8> = v2[..30_000].to_vec();
    let h3 = s.put_delta(&h2, &mut &v3[..]).unwrap();
    assert_eq!(s.get(&h3, None).unwrap(), v3);
    s.flush().unwrap();
    let s = open(d.path(), cfg);
    assert_eq!(s.get(&h2, None).unwrap(), v2);
    assert_eq!(s.get(&h3, Some(100..200)).unwrap(), &v3[100..200]);
    assert_eq!(s.chunks(&h2).unwrap().map(|c| c.len()), Some(2), "delta children: base + xor");
    // Compare with storing v2 plainly in a separate store: the mostly-zero XOR
    // stream compresses far better than v2 itself (incompressible random data).
    let d2 = tempfile::tempdir().unwrap();
    let plain = open(d2.path(), PackConfig::tiny());
    plain.put(&v2).unwrap();
    plain.flush().unwrap();
    let plain_cost = dir_bytes(&d2.path().join("objects"));
    assert!(delta_cost * 4 < plain_cost, "delta {delta_cost} B vs plain {plain_cost} B");
    assert!(matches!(s.put_delta(&Hash::of(b"no such base"), &mut &b"x"[..]), Err(Error::NotFound(_))));
}

fn dir_bytes(p: &Path) -> u64 {
    std::fs::read_dir(p).map_or(0, |rd| {
        rd.flatten()
            .map(|e| if e.path().is_dir() { dir_bytes(&e.path()) } else { e.metadata().map_or(0, |m| m.len()) })
            .sum()
    })
}

/// Review finding 7: a seal that fails after its pack was published must not
/// let later writes append to the published file. Every transient failure point
/// of a flush is swept.
#[test]
fn transient_seal_failures_never_corrupt_published_packs() {
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    for n in 0..12u64 {
        let d = tempfile::tempdir().unwrap();
        let s = PackStore::open_with_faults(
            d.path().join("staging"),
            backend(d.path()),
            cfg.clone(),
            Faults::fail_once_at(n),
        )
        .unwrap();
        let a: Vec<Vec<u8>> = (0..5).map(|i| pseudo(300, i)).collect();
        let b: Vec<Vec<u8>> = (0..5).map(|i| pseudo(300, 100 + i)).collect();
        // Any single operation may fail once (that's the injected fault); retry it like a client would.
        for o in &a {
            if s.put(o).is_err() {
                s.put(o).unwrap();
            }
        }
        let _ = s.flush();
        for o in &b {
            if s.put(o).is_err() {
                s.put(o).unwrap();
            }
        }
        s.flush().unwrap_or_else(|e| panic!("n={n}: second flush must succeed: {e}"));
        for o in a.iter().chain(&b) {
            assert_eq!(&s.get(&Hash::of(o), None).unwrap(), o, "n={n}");
        }
        drop(s);
        // Every published pack's bytes still hash to its name, and everything is readable.
        for e in std::fs::read_dir(d.path().join("objects/packs")).unwrap() {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "pack") {
                let name = p.file_stem().unwrap().to_string_lossy().to_string();
                assert_eq!(
                    blake3::hash(&std::fs::read(&p).unwrap()).to_hex().to_string(),
                    name,
                    "n={n}: published pack modified"
                );
            }
        }
        let s = open(d.path(), cfg.clone());
        for o in a.iter().chain(&b) {
            assert_eq!(&s.get(&Hash::of(o), None).unwrap(), o, "n={n} after reopen");
        }
    }
}

/// Review finding 8: stats() and writers take locks in the same order.
#[test]
fn stats_and_puts_concurrently_do_not_deadlock() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::tiny());
    std::thread::scope(|sc| {
        let s1 = s.clone();
        sc.spawn(move || {
            for i in 0..2000u64 {
                s1.put(&pseudo(200, i)).unwrap();
            }
        });
        for _ in 0..2000 {
            s.stats().unwrap();
        }
    });
}

/// Review finding 9: an index entry claiming a longer object than its record is
/// reported as corruption, not a panic.
#[test]
fn corrupt_index_length_is_an_error_not_a_panic() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, ..PackConfig::tiny() };
    let h;
    {
        let s = open(d.path(), cfg.clone());
        h = s.put(b"twenty bytes exactly").unwrap();
        s.flush().unwrap();
    }
    let packs = d.path().join("objects/packs");
    let idx = std::fs::read_dir(&packs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "idx"))
        .unwrap();
    let mut es = pico2peta_pack::format::decode_index(&std::fs::read(&idx).unwrap()).unwrap();
    for e in es.iter_mut() {
        e.raw_len += 1000;
    }
    std::fs::write(&idx, pico2peta_pack::format::encode_index(&mut es)).unwrap();
    let s = open(d.path(), cfg);
    assert!(matches!(s.get(&h, Some(0..1010)), Err(Error::Corrupt(_))));
    assert!(matches!(s.get(&h, None), Err(Error::Corrupt(_))));
}

/// Review findings 10 and 6b: a CRC-valid but undecodable orphan is quarantined
/// (the store still opens); an index whose pack is gone is skipped, not fatal.
#[test]
fn bad_orphans_are_quarantined_and_missing_packs_skipped() {
    use pico2peta_pack::format::*;
    let d = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(d.path().join("staging")).unwrap();
    // A frame record whose directory claims more bytes than the frame holds.
    let mut payload = 2u32.to_le_bytes().to_vec();
    payload.extend_from_slice(&100u32.to_le_bytes());
    payload.extend_from_slice(&100u32.to_le_bytes());
    payload.extend_from_slice(&[1u8; 10]);
    let rec = encode_record(Kind::Frame2, Codec::Raw, &Hash::of(&[1u8; 10]), 10, &payload);
    let mut f = PACK_MAGIC.to_vec();
    f.extend_from_slice(&[0u8; 16]);
    f.extend_from_slice(&rec);
    std::fs::write(d.path().join("staging/bad.pack"), &f).unwrap();
    let s = open(d.path(), PackConfig::tiny());
    assert_eq!(s.quarantined().unwrap().len(), 1);
    let h = s.put(b"still works").unwrap();
    s.flush().unwrap();
    // Remove a pack but keep its index (as a crashed GC delete would): refresh skips it.
    let packs = d.path().join("objects/packs");
    let pack = std::fs::read_dir(&packs)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    std::fs::remove_file(&pack).unwrap();
    let s2 = open(d.path(), PackConfig::tiny());
    assert!(!s2.contains(&h).unwrap());
    assert_eq!(s2.orphan_indexes().unwrap().len(), 1);
}
