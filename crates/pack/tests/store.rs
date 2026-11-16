use o5s_core::backend::{Backend, LocalDisk};
use o5s_core::fault::Faults;
use o5s_core::{Error, Hash, MemStore, ObjectStore};
use o5s_pack::{PackConfig, PackStore};
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
    let mut f = o5s_pack::format::PACK_MAGIC_V1.to_vec();
    f.extend_from_slice(&o5s_pack::format::encode_record(
        o5s_pack::format::Kind::Blob,
        o5s_pack::format::Codec::Raw,
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
    unsafe { std::env::set_var("O5S_MMAP_INDEX_BYTES", "0") };
    f();
    unsafe { std::env::remove_var("O5S_MMAP_INDEX_BYTES") };
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
    let mut es = o5s_pack::format::decode_index(&std::fs::read(&idx).unwrap()).unwrap();
    for e in es.iter_mut() {
        e.raw_len += 1000;
    }
    std::fs::write(&idx, o5s_pack::format::encode_index(&mut es)).unwrap();
    let s = open(d.path(), cfg);
    assert!(matches!(s.get(&h, Some(0..1010)), Err(Error::Corrupt(_))));
    assert!(matches!(s.get(&h, None), Err(Error::Corrupt(_))));
}

/// Review findings 10 and 6b: a CRC-valid but undecodable orphan is quarantined
/// (the store still opens); an index whose pack is gone is skipped, not fatal.
#[test]
fn bad_orphans_are_quarantined_and_missing_packs_skipped() {
    use o5s_pack::format::*;
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

/// A reader that yields `data` and then fails.
struct FailAfter {
    data: Vec<u8>,
    pos: usize,
}

impl Read for FailAfter {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos == self.data.len() {
            return Err(std::io::Error::other("source died"));
        }
        let n = buf.len().min(self.data.len() - self.pos).min(777);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The put pipeline: a source that fails mid-stream fails the put (never a
/// truncated object), and the store stays usable; a full multi-level stream
/// still hashes and reads back exactly.
#[test]
fn put_stream_pipeline_errors_and_large_trees() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::tiny());
    let data = pseudo(300_000, 9);
    let mut src = FailAfter { data: data.clone(), pos: 0 };
    let err = s.put_stream(&mut src).unwrap_err();
    assert!(matches!(err, Error::Io(_)), "{err:?}");
    // The prefix that was read must not have become an object.
    assert!(!s.contains(&Hash::of(&data)).unwrap());
    // Several manifest levels (fanout 4, ~1000 chunks), interleaved with other writers.
    let h = std::thread::scope(|sc| {
        let t = sc.spawn(|| {
            for i in 0..50u64 {
                s.put(&pseudo(3000, 100 + i)).unwrap();
            }
        });
        let h = s.put_stream(&mut &data[..]).unwrap();
        t.join().unwrap();
        h
    });
    assert_eq!(h, Hash::of(&data));
    assert_eq!(s.get(&h, None).unwrap(), data);
    assert_eq!(s.get(&h, Some(1234..250_001)).unwrap(), data[1234..250_001]);
    s.flush().unwrap();
    let s2 = open(d.path(), PackConfig::tiny());
    assert_eq!(s2.get(&h, None).unwrap(), data);
}

/// The compressibility probe: incompressible chunks are stored raw without
/// trying, compressible ones are still compressed, and the result is
/// byte-identical in content either way.
#[test]
fn compressibility_probe_picks_raw_or_zstd() {
    let cfg = |probe| PackConfig {
        chunk_threshold: 1 << 20,
        cdc_min: 16 << 10,
        cdc_avg: 64 << 10,
        cdc_max: 256 << 10,
        pack_cap: 64 << 20,
        zstd_level: 3,
        manifest_fanout: 64,
        compress_probe: probe,
        grouped_zstd_level: 3,
    };
    let text: Vec<u8> = (0..4_000_000u32).map(|i| b"fn main() { let x = 42; }\n"[(i % 26) as usize]).collect();
    // Random head, zeros after: the probe sees only the head and stores raw;
    // that's the documented trade-off of sampling.
    let mut tricky = pseudo(4096, 3);
    tricky.resize(20_000, 0);
    let rand = pseudo(4_000_000, 7);
    let mut sizes = Vec::new();
    for probe in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path(), cfg(probe));
        let ht = s.put_stream(&mut &text[..]).unwrap();
        let hr = s.put_stream(&mut &rand[..]).unwrap();
        let hk = s.put(&tricky).unwrap();
        s.flush().unwrap();
        for (h, v) in [(ht, &text), (hr, &rand), (hk, &tricky)] {
            assert_eq!(&s.get(&h, None).unwrap(), v);
        }
        sizes.push(dir_bytes(&d.path().join("objects")));
    }
    // Text compresses either way; random data is ~raw either way; only the
    // tricky 20 KB object differs (raw with the probe).
    assert!(sizes[0] < 4_000_000 + 4_000_000 / 10, "{sizes:?}");
    assert!(sizes[0] > sizes[1] && sizes[0] - sizes[1] < 20_000, "{sizes:?}");
}

/// Xet chunking: the store cuts exactly where the reference Xet chunker does
/// (every chunk is stored as its own object), identity and reads are the same
/// as with FastCDC, and a too-small chunk threshold is refused.
#[test]
fn put_stream_with_xet_chunker() {
    use o5s_pack::ChunkerKind;
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() };
    let s = open(d.path(), cfg);
    let data = pseudo(5_000_000, 11);
    let h = s.put_stream_with(ChunkerKind::Xet, &mut &data[..]).unwrap();
    assert_eq!(h, Hash::of(&data));
    assert_eq!(s.get(&h, None).unwrap(), data);
    assert_eq!(s.get(&h, Some(70_000..4_100_000)).unwrap(), data[70_000..4_100_000]);
    let lens = o5s_chunk::chunk_lengths(&mut o5s_chunk::XetChunker::new(), &data);
    assert!(lens.len() > 20 && lens.iter().all(|&l| l <= o5s_chunk::XET_MAX));
    let mut off = 0;
    for l in lens {
        assert!(s.contains(&Hash::of(&data[off..off + l])).unwrap(), "Xet chunk at {off}+{l} not stored");
        off += l;
    }
    // The same bytes through FastCDC: same object id, (mostly) different chunks.
    let d2 = tempfile::tempdir().unwrap();
    let s2 = open(d2.path(), PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() });
    assert_eq!(s2.put_stream(&mut &data[..]).unwrap(), h);
    // A chunk threshold not above the Xet maximum is refused, not silently wrong.
    let d3 = tempfile::tempdir().unwrap();
    let s3 = open(d3.path(), PackConfig::tiny());
    assert!(matches!(s3.put_stream_with(ChunkerKind::Xet, &mut &data[..]), Err(Error::Parse(_))));
}

/// bf16-like values (small floats: similar sign/exponent bytes, noisy
/// mantissas), as in real checkpoints.
fn bf16_like(n_elems: usize, seed: u64) -> Vec<u8> {
    let noise = pseudo(n_elems, seed);
    let mut out = Vec::with_capacity(2 * n_elems);
    for (i, &m) in noise.iter().enumerate() {
        let hi = 0x3c + (m % 3) + if i % 5 == 0 { 0x80 } else { 0 };
        out.push(m.rotate_left(3));
        out.push(hi);
    }
    out
}

/// The grouped codec: put_stream_grouped and grouped deltas round-trip
/// (whole and ranged reads, across reopen), keep identity, and store bf16-like
/// data smaller than plain zstd does.
#[test]
fn byte_grouped_records_roundtrip_and_shrink() {
    let cfg = || PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() };
    let data = bf16_like(3_000_000, 5);
    let mut next = data.clone();
    for i in (0..next.len()).step_by(2) {
        next[i] ^= (i as u8) & 0x07; // fine-tune-like: low mantissa bits move
    }
    let mut sizes = Vec::new();
    for grouped in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let (h, hn) = {
            let s = open(d.path(), cfg());
            let h =
                if grouped { s.put_stream_grouped(&mut &data[..], 2) } else { s.put_stream(&mut &data[..]) }.unwrap();
            assert_eq!(h, Hash::of(&data));
            let segs = [(0, 0, data.len() as u64)];
            let hn = if grouped {
                s.put_delta_mapped_grouped(&h, &segs, &mut &next[..], 2)
            } else {
                s.put_delta_mapped(&h, &segs, &mut &next[..])
            }
            .unwrap();
            assert_eq!(hn, Hash::of(&next));
            s.flush().unwrap();
            (h, hn)
        };
        let s = open(d.path(), cfg());
        assert_eq!(s.get(&h, None).unwrap(), data);
        assert_eq!(s.get(&hn, None).unwrap(), next);
        assert_eq!(s.get(&h, Some(123_457..2_000_001)).unwrap(), data[123_457..2_000_001]);
        assert_eq!(s.get(&hn, Some(5..4_999_999)).unwrap(), next[5..4_999_999]);
        sizes.push(dir_bytes(&d.path().join("objects")));
    }
    assert!(sizes[1] < sizes[0], "grouped {} should be < plain {}", sizes[1], sizes[0]);
    // Width out of range is refused; 0/1 mean "off".
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), cfg());
    assert!(matches!(s.put_stream_grouped(&mut &data[..], 17), Err(Error::Parse(_))));
    assert_eq!(s.put_stream_grouped(&mut &data[..], 1).unwrap(), Hash::of(&data));
}

/// Read-ahead (reads ≥ 2 MiB): a corrupt chunk in the middle fails a whole
/// read, and fails a ranged read only if the range covers it. Dropping a
/// reader mid-object neither hangs nor leaks state, and results are
/// returned strictly in order.
#[test]
fn readahead_reads_detect_corruption_and_stay_ordered() {
    let d = tempfile::tempdir().unwrap();
    let cfg = || PackConfig { chunk_threshold: 1 << 20, zstd_level: 0, pack_cap: 1 << 30, ..PackConfig::default() };
    let data = pseudo(12_000_000, 21);
    let h = {
        let s = open(d.path(), cfg());
        let h = s.put_stream(&mut &data[..]).unwrap();
        s.flush().unwrap();
        // In order and complete, with small and odd-sized reads.
        let mut r = s.open(&h, None).unwrap();
        let mut out = Vec::new();
        let mut buf = vec![0u8; 7777];
        loop {
            let n = r.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        assert_eq!(out, data);
        // Early drop.
        let mut r = s.open(&h, None).unwrap();
        let mut one = vec![0u8; 1 << 20];
        r.read_exact(&mut one).unwrap();
        drop(r);
        h
    };
    // Corrupt one byte at the middle of the pack (inside a chunk record).
    let dir = d.path().join("objects/packs");
    let pack = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    let mut b = std::fs::read(&pack).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0x01;
    std::fs::write(&pack, b).unwrap();
    let s = open(d.path(), cfg());
    let mut out = Vec::new();
    let err = s.open(&h, None).unwrap().read_to_end(&mut out).unwrap_err();
    assert!(err.to_string().contains("mismatch") || err.to_string().contains("crc"), "{err}");
    // The first 2 MiB (well before the corrupt middle) still read fine.
    assert_eq!(s.get(&h, Some(0..2_500_000)).unwrap(), data[..2_500_000]);
    // A range over the corrupt middle fails.
    assert!(s.get(&h, Some(4_000_000..8_000_000)).is_err());
}

/// Small records: the entropy check stores random bytes raw (no zstd attempt)
/// but still compresses text; with the probe off, both go through zstd. Reads
/// are exact either way, also through the cached pack handles after reopen.
#[test]
fn small_record_entropy_check() {
    let text: Vec<u8> = b"fn main() { println!(\"hello\"); }\n".iter().copied().cycle().take(4000).collect();
    let rand = pseudo(4000, 77);
    for probe in [true, false] {
        let d = tempfile::tempdir().unwrap();
        let cfg = PackConfig { compress_probe: probe, pack_cap: 1 << 30, ..PackConfig::default() };
        let (ht, hr) = {
            let s = open(d.path(), cfg.clone());
            let ht = s.put(&text).unwrap();
            let hr = s.put(&rand).unwrap();
            s.flush().unwrap();
            (ht, hr)
        };
        let s = open(d.path(), cfg);
        for _ in 0..3 {
            assert_eq!(s.get(&ht, None).unwrap(), text);
            assert_eq!(s.get(&hr, None).unwrap(), rand);
        }
        // Text compresses (pack much smaller than 8000 raw bytes + overhead).
        let packs = dir_bytes(&d.path().join("objects/packs"));
        assert!(packs < 4000 + 2000 + 1000, "{packs}");
    }
}

/// Frame3 (frame with a reference prefix): members read back exactly; the
/// reference is reported as their child; a staging file holding both the
/// reference (as an earlier record) and the frame is recovered after a crash,
/// resolving the reference from the same file; bad references are refused;
/// and the reference really helps compression.
#[test]
fn frame3_reference_prefix_roundtrip_recovery_and_gc_child() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, chunk_threshold: 1 << 20, ..PackConfig::default() };
    let base = pseudo(200_000, 3);
    let members: Vec<Vec<u8>> = (0..5)
        .map(|k| {
            let mut m = base.clone();
            m[k * 1000] ^= 1;
            m
        })
        .collect();
    let refs: Vec<&[u8]> = members.iter().map(|m| m.as_slice()).collect();
    let (href, hashes);
    {
        // Crash after: open.create, the reference's append, the frame write.
        let f = Faults::crash_after(3);
        let s = PackStore::open_with_faults(d.path().join("staging"), backend(d.path()), cfg.clone(), f).unwrap();
        href = s.put(&base).unwrap();
        hashes = s.put_frame_ref(&refs, 19, Some(&href)).unwrap();
        for (h, m) in hashes.iter().zip(&members) {
            assert_eq!(&s.get(h, None).unwrap(), m);
            assert_eq!(s.chunks(h).unwrap(), Some(vec![href]), "the reference is a child");
        }
        assert!(s.put(b"next write crashes").is_err());
    }
    let s = open(d.path(), cfg.clone());
    for (h, m) in hashes.iter().zip(&members) {
        assert_eq!(&s.get(h, None).unwrap(), m, "recovered from the orphaned staging file");
        assert_eq!(s.get(h, Some(150_000..150_010)).unwrap(), m[150_000..150_010]);
        assert!(s.ensure_live(h).unwrap());
    }
    // Bad references are refused: missing, and too large / not a leaf.
    let missing = Hash::of(b"never stored");
    assert!(s.put_frame_ref(&refs, 3, Some(&missing)).is_err());
    let big = s.put(&pseudo(3 << 20, 9)).unwrap(); // chunked: a manifest
    assert!(s.put_frame_ref(&refs, 3, Some(&big)).is_err());
    // Compression: with the reference, five near-copies of `base` cost little.
    let d2 = tempfile::tempdir().unwrap();
    let s2 = open(d2.path(), cfg);
    let r2 = s2.put(&base).unwrap();
    s2.flush().unwrap();
    let before = dir_bytes(&d2.path().join("objects"));
    s2.put_frame_ref(&refs[..1], 19, Some(&r2)).unwrap();
    s2.flush().unwrap();
    let with_ref = dir_bytes(&d2.path().join("objects")) - before;
    assert!(with_ref < 20_000, "one near-copy against its reference: {with_ref} bytes");
}

/// Large frames are decoded only as far as a read needs (prefix decoding),
/// with members still verified: random, sequential and reverse reads all
/// return exact bytes (Frame2 and Frame3); a corrupt byte in a frame is
/// detected (the record CRC covers the whole record, and every member is also
/// checked against its content hash, so a partial decode never returns
/// unverified bytes).
#[test]
fn frame_prefix_decoding_reads_exact_and_verified() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 30, chunk_threshold: 1 << 20, ..PackConfig::default() };
    let members: Vec<Vec<u8>> = (0..48).map(|i| pseudo(64 << 10, 500 + i)).collect();
    let refs: Vec<&[u8]> = members.iter().map(|m| m.as_slice()).collect();
    let (h2, h3) = {
        let s = open(d.path(), cfg.clone());
        let h2 = s.put_frame(&refs, 3).unwrap();
        let base = s.put(&members[0]).unwrap();
        let h3 = s.put_frame_ref(&refs[1..], 3, Some(&base)).unwrap();
        s.flush().unwrap();
        (h2, h3)
    };
    let s = open(d.path(), cfg.clone());
    let order: Vec<usize> = (0..48).map(|i| (i * 29) % 48).chain(0..48).chain((0..48).rev()).collect();
    for &i in &order {
        assert_eq!(s.get(&h2[i], None).unwrap(), members[i], "frame2 member {i}");
        if i > 0 {
            assert_eq!(s.get(&h3[i - 1], None).unwrap(), members[i], "frame3 member {i}");
        }
    }
    // Corrupt one byte near the end of the first frame's compressed body.
    let dir = d.path().join("objects/packs");
    let pack = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    let mut b = std::fs::read(&pack).unwrap();
    // The first record after the 24-byte pack header is the Frame2: its payload
    // ends well before 3 MiB; flip a byte at ~95% of it.
    let at = 24 + (members.len() * (64 << 10)) * 95 / 100;
    if at < b.len() {
        b[at] ^= 0x10;
    }
    std::fs::write(&pack, b).unwrap();
    let s = open(d.path(), cfg);
    assert!(s.get(&h2[47], None).is_err(), "corruption is detected");
    assert!(s.get(&h2[0], None).is_err(), "the record CRC covers every member");
}

/// Chunk-anchored XOR deltas: scattered in-place edits, an insertion and a
/// deletion all read back exactly (whole and ranged, after reopen), and cost
/// a small fraction of a full copy; a small object falls back cleanly.
#[test]
fn anchored_xor_deltas_handle_edits_and_shifts() {
    use o5s_core::{Derive, Encoding, PatchFormat};
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { chunk_threshold: 1 << 20, pack_cap: 1 << 30, ..PackConfig::default() };
    let base = pseudo(8 << 20, 41);
    let mut scattered = base.clone();
    for i in (0..scattered.len()).step_by(997) {
        scattered[i] = scattered[i].wrapping_add(1);
    }
    let mut inserted = base.clone();
    inserted.splice(3_000_000..3_000_000, pseudo(1 << 20, 42));
    let mut deleted = base.clone();
    deleted.drain(5_000_000..5_500_000);
    let small = b"tiny new version".to_vec();
    let enc = |b: &Hash| Encoding {
        derive: Some(Derive { base: *b, patch: PatchFormat::XorAnchored, segments: Vec::new() }),
        ..Default::default()
    };
    let mut cost = Vec::new();
    let hashes: Vec<(Hash, &Vec<u8>)>;
    {
        let s = open(d.path(), cfg.clone());
        let hb = s.put_stream(&mut &base[..]).unwrap();
        s.flush().unwrap();
        let mut hs = Vec::new();
        for v in [&scattered, &inserted, &deleted, &small] {
            let before = dir_bytes(&d.path().join("objects"));
            let h = s.put_encoded(&mut &v[..], &enc(&hb)).unwrap();
            s.flush().unwrap();
            cost.push(dir_bytes(&d.path().join("objects")) - before);
            assert_eq!(h, Hash::of(v));
            hs.push((h, v));
        }
        hashes = hs;
    }
    let s = open(d.path(), cfg);
    for (h, v) in &hashes {
        assert_eq!(&&s.get(h, None).unwrap(), v);
        if v.len() > 4_000_000 {
            assert_eq!(s.get(h, Some(2_999_000..3_001_000)).unwrap(), v[2_999_000..3_001_000]);
            assert_eq!(s.get(h, Some(4_000_000..6_000_000)).unwrap(), v[4_000_000..6_000_000]);
        }
    }
    // 8 MiB incompressible base: the deltas cost far less than a copy.
    assert!(cost[0] < 200_000, "scattered edits: {cost:?}"); // 0.1 % edits, offset-aligned
    assert!(cost[1] < (1 << 20) + 300_000, "insertion: {cost:?}"); // ~ the inserted MiB
    assert!(cost[2] < 300_000, "deletion: {cost:?}");
    // A chain: each version derived from the previous (a derived base has no
    // stored chunks; anchors come from chunking its content).
    let d2 = tempfile::tempdir().unwrap();
    let s2 = open(d2.path(), PackConfig { chunk_threshold: 1 << 20, pack_cap: 1 << 30, ..PackConfig::default() });
    let mut cur = base.clone();
    let mut prev = s2.put_stream(&mut &cur[..]).unwrap();
    s2.flush().unwrap();
    let one = dir_bytes(&d2.path().join("objects"));
    for v in 0..5u64 {
        let at = (1_000_000 + v * 1_234_567) as usize;
        cur.splice(at..at, pseudo(1 << 18, 60 + v));
        prev = s2.put_encoded(&mut &cur[..], &enc(&prev)).unwrap();
        assert_eq!(s2.get(&prev, None).unwrap(), cur);
    }
    s2.flush().unwrap();
    let chain = dir_bytes(&d2.path().join("objects")) - one;
    assert!(chain < 5 * (1 << 18) + 1_500_000, "5 insertions of 256 KiB along a chain cost {chain}");
}

/// Read verification modes. A chunk of a large object is corrupted in a
/// CRC-valid way (payload byte flipped, record CRC recomputed: only BLAKE3 can
/// tell). Strict never returns a wrong byte: the read stops with an error
/// before the bad chunk. Eof returns bytes up to and including the bad chunk
/// (that is its documented trade-off) and fails at EOF. Plain (CRC-invalid)
/// corruption fails in both modes. Ranged reads are strict even in eof mode.
#[test]
fn read_verify_strict_vs_eof() {
    use o5s_core::ReadVerify;
    use o5s_pack::format::{REC_HEADER, REC_TRAILER};
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { chunk_threshold: 1 << 20, zstd_level: 0, pack_cap: 1 << 30, ..PackConfig::default() };
    let data = pseudo(6 << 20, 88);
    let (h, victim) = {
        let s = open(d.path(), cfg.clone());
        let h = s.put_stream(&mut &data[..]).unwrap();
        s.flush().unwrap();
        // A chunk in the middle of the object.
        let chunks = s.chunks(&h).unwrap().unwrap();
        (h, chunks[chunks.len() / 2])
    };
    let pack = std::fs::read_dir(d.path().join("objects/packs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "pack"))
        .unwrap();
    let name = pack.file_stem().unwrap().to_str().unwrap().to_string();
    let entry = {
        let s = open(d.path(), cfg.clone());
        s.pack_entries(&name).unwrap().into_iter().find(|e| e.hash == victim).unwrap()
    };
    let original = std::fs::read(&pack).unwrap();
    let rec_start = entry.offset as usize;
    let payload_len = entry.stored_len as usize;
    let crafted = {
        let mut b = original.clone();
        b[rec_start + REC_HEADER + payload_len / 2] ^= 0x01;
        let body_end = rec_start + REC_HEADER + payload_len;
        let crc = crc32fast::hash(&b[rec_start..body_end]);
        b[body_end..body_end + REC_TRAILER].copy_from_slice(&crc.to_le_bytes());
        b
    };
    // The object offset where the bad byte lands, to check what strict returns.
    let read_all = |s: &PackStore, mode: ReadVerify| -> (Vec<u8>, bool) {
        let mut r = s.open_verified(&h, None, mode).unwrap();
        let mut out = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match r.read(&mut buf) {
                Ok(0) => return (out, false),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(_) => return (out, true),
            }
        }
    };
    std::fs::write(&pack, &crafted).unwrap();
    let s = open(d.path(), cfg.clone());
    // Strict: an error, and every byte returned before it is correct.
    let (got, err) = read_all(&s, ReadVerify::Strict);
    assert!(err, "strict detects CRC-valid corruption");
    assert_eq!(got[..], data[..got.len()], "strict never returns a wrong byte");
    // Eof: the bad bytes are returned, then the EOF hash check fails.
    let (got, err) = read_all(&s, ReadVerify::Eof);
    assert!(err, "eof detects it at EOF");
    assert!(got.len() > data.len() / 2 && got[..] != data[..got.len()], "eof returned the crafted bytes first");
    // A ranged read over the bad chunk is strict in either mode.
    let mid = data.len() as u64 / 2;
    assert!(s.open_verified(&h, Some(0..data.len() as u64), ReadVerify::Eof).is_ok());
    let ranged = (|| -> std::io::Result<Vec<u8>> {
        let mut v = Vec::new();
        s.open_verified(&h, Some(mid - (1 << 20)..mid + (1 << 20)), ReadVerify::Eof).unwrap().read_to_end(&mut v)?;
        Ok(v)
    })();
    assert!(ranged.is_err() || ranged.unwrap() == data[(mid - (1 << 20)) as usize..(mid + (1 << 20)) as usize]);
    // Plain corruption (CRC no longer matches): both modes fail.
    let mut plain = original.clone();
    plain[rec_start + REC_HEADER + payload_len / 2] ^= 0x01;
    std::fs::write(&pack, &plain).unwrap();
    let s = open(d.path(), cfg.clone());
    for mode in [ReadVerify::Strict, ReadVerify::Eof] {
        let (got, err) = read_all(&s, mode);
        assert!(err, "{mode:?} detects CRC-invalid corruption");
        assert_eq!(got[..], data[..got.len()], "{mode:?}: the CRC stops bad bytes");
    }
    // Intact: both modes read exactly.
    std::fs::write(&pack, &original).unwrap();
    let s = open(d.path(), cfg);
    for mode in [ReadVerify::Strict, ReadVerify::Eof] {
        assert_eq!(read_all(&s, mode), (data.clone(), false));
    }
}

#[test]
fn put_many_matches_put_with_duplicates_known_and_large_items() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 1 << 20, ..PackConfig::default() };
    let s = open(d.path(), cfg.clone());
    let known = pseudo(5_000, 99);
    let hk = s.put(&known).unwrap();
    // Enough bytes for the parallel path (≥ 256 KiB), mixed sizes, compressible and not,
    // a duplicate inside the batch, an already-stored item, an empty item, and two items
    // over the chunking threshold.
    let mut items: Vec<Vec<u8>> = (0..400u64)
        .map(|i| {
            if i % 3 == 0 {
                vec![b'a' + (i % 26) as u8; 100 + i as usize * 37]
            } else {
                pseudo(64 + (i as usize * 997) % 20_000, i)
            }
        })
        .collect();
    items.push(items[7].clone());
    items.push(known.clone());
    items.push(Vec::new());
    items.push(pseudo(cfg.chunk_threshold + 12_345, 7));
    items.push(pseudo(cfg.chunk_threshold, 8));
    let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let before = s.stats().unwrap().objects;
    let hs = s.put_many(&refs).unwrap();
    assert_eq!(hs.len(), items.len());
    for (h, v) in hs.iter().zip(&items) {
        assert_eq!(*h, Hash::of(v));
        assert_eq!(s.get(h, None).unwrap(), *v);
    }
    assert_eq!(hs[items.len() - 4], hk);
    let distinct: std::collections::HashSet<Hash> = hs.iter().copied().collect();
    let added = s.stats().unwrap().objects - before;
    // Large items add chunks and manifest nodes too, so count only what is certain:
    // every distinct small hash appears once, the known one is not added again.
    assert!(added >= distinct.len() - 1, "added {added}, distinct {}", distinct.len());
    let again = s.stats().unwrap().objects;
    assert_eq!(s.put_many(&refs).unwrap(), hs, "a second put_many is pure dedup");
    assert_eq!(s.stats().unwrap().objects, again);
    s.flush().unwrap();
    drop(s);
    let s = open(d.path(), cfg);
    for (h, v) in hs.iter().zip(&items) {
        assert_eq!(s.get(h, None).unwrap(), *v, "after reopen");
    }
    assert!(s.put_many(&[]).unwrap().is_empty());
}

#[test]
fn whole_and_ranged_reads_of_small_blobs_agree() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::default());
    for v in [Vec::new(), b"x".to_vec(), vec![b'a'; 5000], pseudo(70_000, 3)] {
        let h = s.put(&v).unwrap();
        for st in [false, true] {
            if st {
                s.flush().unwrap();
            }
            assert_eq!(s.get(&h, None).unwrap(), v);
            let mut all = Vec::new();
            s.open(&h, None).unwrap().read_to_end(&mut all).unwrap();
            assert_eq!(all, v);
            let n = v.len() as u64;
            for r in [0..0, 0..n, n / 3..n, 0..n / 2, n / 4..n / 4 + n / 3, n..n] {
                assert_eq!(s.get(&h, Some(r.clone())).unwrap(), &v[r.start as usize..r.end as usize]);
                let mut part = Vec::new();
                s.open(&h, Some(r.clone())).unwrap().read_to_end(&mut part).unwrap();
                assert_eq!(part, &v[r.start as usize..r.end as usize]);
            }
            assert!(s.get(&h, Some(0..n + 1)).is_err());
        }
    }
}

#[test]
fn mapped_index_cache_is_hard_linked_on_local_disk() {
    use std::os::unix::fs::MetadataExt;
    let d = tempfile::tempdir().unwrap();
    let hs: Vec<Hash>;
    {
        let s = open(d.path(), PackConfig::default());
        hs = (0..2000u64).map(|i| s.put(&pseudo(80, i)).unwrap()).collect();
        s.flush().unwrap();
    }
    unsafe_env_threshold_zero(|| {
        let s = open(d.path(), PackConfig::default()); // maps every sealed index
        for (i, h) in hs.iter().enumerate() {
            assert_eq!(s.get(h, None).unwrap(), pseudo(80, i as u64));
        }
    });
    let cache: Vec<_> = std::fs::read_dir(d.path().join("idx-cache"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "idx3"))
        .collect();
    assert!(!cache.is_empty(), "an index was mapped through the cache");
    for e in cache {
        assert_eq!(e.metadata().unwrap().nlink(), 2, "{:?} is a hard link to the pack index", e.path());
    }
}

/// Leaf chunk hashes of `h` (a manifest tree), in order.
fn leaves(s: &PackStore, h: &Hash) -> Vec<Hash> {
    match s.chunks(h).unwrap() {
        None => vec![*h],
        Some(kids) => kids.iter().flat_map(|k| leaves(s, k)).collect(),
    }
}

#[test]
fn put_concat_reuses_source_chunks_and_observes_every_byte() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::default());
    let a = pseudo(5 << 20, 11); // chunked
    let small = pseudo(3000, 12); // a blob
    let ha = s.put(&a).unwrap();
    let hs = s.put(&small).unwrap();
    let la = leaves(&s, &ha);
    assert!(la.len() > 4);
    // Chunk boundaries of `a` (for aligned ranges).
    let mut bounds = vec![0u64];
    for c in &la {
        bounds.push(bounds.last().unwrap() + s.size(c).unwrap());
    }
    let (b1, b3) = (bounds[1], bounds[3]);
    let n = a.len() as u64;
    let cases: Vec<Vec<(Hash, std::ops::Range<u64>)>> = vec![
        vec![(ha, 0..n)],                               // the whole object
        vec![(ha, b1..b3), (hs, 0..3000), (ha, 0..b1)], // aligned ranges plus a blob
        vec![(ha, 12_345..n - 999), (hs, 100..200)],    // unaligned ends
        vec![(hs, 0..10), (hs, 10..3000)],              // small result: one blob
        vec![],                                         // empty
    ];
    for parts in cases {
        let want: Vec<u8> = parts
            .iter()
            .flat_map(|(h, r)| {
                let src = if *h == ha { &a } else { &small };
                src[r.start as usize..r.end as usize].to_vec()
            })
            .collect();
        let mut seen = Vec::new();
        let h = s.put_concat(&parts, &mut |b| seen.extend_from_slice(b)).unwrap();
        assert_eq!(seen, want, "observe sees every byte in order");
        assert_eq!(h, Hash::of(&want));
        assert_eq!(s.get(&h, None).unwrap(), want);
        if parts.len() == 1 && parts[0].1 == (0..n) {
            assert_eq!(h, ha);
        }
    }
    // Aligned ranges (above the chunking threshold) reuse the source's chunk records exactly.
    let k = bounds.iter().position(|&b| b >= b1 + (2 << 20)).unwrap();
    let h = s.put_concat(&[(ha, b1..bounds[k])], &mut |_| {}).unwrap();
    assert_eq!(leaves(&s, &h), la[1..k].to_vec());
    // A bad range is refused.
    assert!(s.put_concat(&[(hs, 0..3001)], &mut |_| {}).is_err());
}

#[test]
fn put_chunked_keeps_the_given_boundaries() {
    let d = tempfile::tempdir().unwrap();
    let s = open(d.path(), PackConfig::default());
    let parts: Vec<Vec<u8>> = (0..40u64).map(|i| pseudo(20_000 + (i as usize * 7919) % 90_000, i)).collect();
    let refs: Vec<&[u8]> = parts.iter().map(|p| p.as_slice()).collect();
    let all = parts.concat();
    let h = s.put_chunked(&refs).unwrap();
    assert_eq!(h, Hash::of(&all));
    assert_eq!(s.get(&h, None).unwrap(), all);
    let want: Vec<Hash> = parts.iter().map(|p| Hash::of(p)).collect();
    assert_eq!(leaves(&s, &h), want);
    // Same object as a plain put of the concatenation (identity is content).
    assert_eq!(s.put(&all).unwrap(), h);
    // Small totals become one blob.
    let tiny = s.put_chunked(&[b"ab", b"cd"]).unwrap();
    assert_eq!(s.get(&tiny, None).unwrap(), b"abcd");
}

/// Parallel Xet chunking across 32 MiB windows (the last chunk of each window is carried into the
/// next): the cuts are still exactly the serial chunker's, through readers that return short reads,
/// at sizes around one window, a window plus a carried tail, and several windows, with a constant
/// run (max-size cuts) across a window boundary.
#[test]
fn put_stream_xet_parallel_windows_cut_exactly_like_serial() {
    use o5s_pack::ChunkerKind;
    struct Jagged<'a>(&'a [u8], u64);
    impl std::io::Read for Jagged<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 ^= self.1 << 13;
            self.1 ^= self.1 >> 7;
            self.1 ^= self.1 << 17;
            let n = (1 + (self.1 % 300_000) as usize).min(buf.len()).min(self.0.len());
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    const W: usize = 32 << 20;
    for (i, len) in [W - 5, W, W + 1, W + 100_000, 2 * W + 77_777, 3 * W + 1].into_iter().enumerate() {
        let mut data = pseudo(len, 40 + i as u64);
        // Constant bytes across the first window boundary: forced max-size cuts there.
        let (a, b) = (W.saturating_sub(300_000), (W + 300_000).min(len));
        data[a..b].fill(7);
        let d = tempfile::tempdir().unwrap();
        let s = open(d.path(), PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() });
        let h = s.put_stream_with(ChunkerKind::Xet, &mut Jagged(&data, 0x9e37 + i as u64)).unwrap();
        assert_eq!(h, Hash::of(&data));
        let lens = o5s_chunk::chunk_lengths(&mut o5s_chunk::XetChunker::new(), &data);
        let mut off = 0;
        for l in lens {
            assert!(
                s.contains(&Hash::of(&data[off..off + l])).unwrap(),
                "len {len}: serial chunk {off}+{l} not stored"
            );
            off += l;
        }
        assert_eq!(s.get(&h, None).unwrap(), data, "len {len}");
    }
}
