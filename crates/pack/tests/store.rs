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
        assert_eq!(s.stats().unwrap().manifests, 1);
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
