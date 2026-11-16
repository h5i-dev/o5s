use o5s_core::{Hash, Name, ObjectStore, RefValue, ReplaceOutcome, Repository};
use o5s_repo::{GcOptions, LocalRepo, OpaqueObjects, PackConfig};
use std::time::Duration;

fn opts() -> GcOptions {
    GcOptions { grace: Duration::ZERO, allow_short_grace: true, ..GcOptions::default() }
}

fn set(r: &LocalRepo, name: &str, h: Hash) {
    let n = Name::new(name).unwrap();
    loop {
        let obs = RefValue::observed(r.resolve(&n).unwrap());
        if let ReplaceOutcome::Ok { .. } = r.replace(&n, &[(n.clone(), obs)], h).unwrap() {
            return;
        }
    }
}

fn walk(p: &std::path::Path) -> u64 {
    std::fs::read_dir(p).map_or(0, |rd| {
        rd.map(|e| {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() { walk(&e.path()) } else { e.metadata().unwrap().len() }
        })
        .sum()
    })
}

fn blob(i: u32, n: usize) -> Vec<u8> {
    let mut x = (i as u64 + 1).wrapping_mul(0x9E3779B97F4A7C15);
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}

#[test]
fn full_cycle_collects_garbage_keeps_live() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let live_small = r.put(b"live small").unwrap();
    let live_big = r.put(&blob(1, 30_000)).unwrap(); // chunked
    let garbage: Vec<Hash> = (0..20).map(|i| r.put(&blob(100 + i, 3000)).unwrap()).collect();
    set(&r, "keep/a", live_small);
    set(&r, "keep/b", live_big);
    r.flush().unwrap();
    let reader = LocalRepo::open(d.path()).unwrap(); // a long-lived concurrent reader
    let before = walk(&d.path().join("objects"));

    let p = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert!(p.planned.is_some());
    let s = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert_eq!(s.swept.len(), 1);
    assert!(s.condemned_packs > 0);
    // Condemned packs stay readable.
    assert!(reader.contains(&garbage[0]).unwrap());
    let del = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert!(del.deleted_packs > 0, "{del:?}");
    let after = walk(&d.path().join("objects"));
    assert!(after < before / 2, "before {before} after {after}");

    for h in [live_small, live_big] {
        assert_eq!(Hash::of(&reader.get(&h, None).unwrap()), h, "stale reader follows relocation");
        let fresh = LocalRepo::open(d.path()).unwrap();
        assert_eq!(Hash::of(&fresh.get(&h, None).unwrap()), h);
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    for g in &garbage {
        assert!(!fresh.contains(g).unwrap(), "garbage survived");
    }
}

#[test]
fn object_referenced_after_condemnation_is_rescued() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let x = r.put(b"unreferenced at sweep time").unwrap();
    let y = r.put(b"also garbage at sweep time").unwrap();
    set(&r, "anchor", r.put(b"anchor").unwrap());
    r.flush().unwrap();
    r.gc(&opts(), &OpaqueObjects).unwrap(); // plan
    let s = r.gc(&opts(), &OpaqueObjects).unwrap(); // sweep: x and y condemned
    assert!(s.condemned_packs > 0);
    // A writer with a stale view references x without putting it again.
    let w = LocalRepo::open(d.path()).unwrap();
    set(&w, "late", x);
    // Another writer puts y again: dedup must ignore the condemned copy.
    let w2 = LocalRepo::open(d.path()).unwrap();
    assert_eq!(w2.put(b"also garbage at sweep time").unwrap(), y);
    w2.flush().unwrap();
    set(&w2, "late2", y);
    let del = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert!(del.deleted_packs > 0);
    // (replace calls ensure_live, so x is rewritten before the delete phase
    // and the rescue path may have nothing left to do.)
    let fresh = LocalRepo::open(d.path()).unwrap();
    assert_eq!(fresh.get(&x, None).unwrap(), b"unreferenced at sweep time");
    assert_eq!(fresh.get(&y, None).unwrap(), b"also garbage at sweep time");
}

#[test]
fn short_grace_rejected_and_dry_run_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    assert!(r.gc(&GcOptions { grace: Duration::from_secs(5), ..GcOptions::default() }, &OpaqueObjects).is_err());
    r.put(b"g").unwrap();
    r.flush().unwrap();
    let dry = GcOptions { dry_run: true, ..opts() };
    let rep = r.gc(&dry, &OpaqueObjects).unwrap();
    assert!(rep.planned.is_none());
    assert!(r.packs().backend().list("gc/").unwrap().is_empty());
}

/// Real protocol, time scaled down: writers re-list every 200 ms, grace is
/// 2 s, and each put→replace finishes well within the grace period (lease).
#[test]
fn concurrent_writers_during_gc_lose_nothing() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    for i in 0..10 {
        r.put(&blob(i, 2000)).unwrap();
    }
    r.flush().unwrap();
    let gc_opts = GcOptions { grace: Duration::from_secs(2), allow_short_grace: true, ..GcOptions::default() };
    let root = d.path().to_path_buf();
    let done = std::sync::atomic::AtomicBool::new(false);
    let mut deleted = 0;
    let written: Vec<(String, Hash)> = std::thread::scope(|sc| {
        let writer = sc.spawn(|| {
            let w = LocalRepo::open(&root).unwrap();
            w.packs().set_dedup_refresh_interval(Duration::from_millis(200));
            let mut v = Vec::new();
            for i in 0..120u32 {
                // A mix of re-puts of old (soon-to-be-garbage) data and new data.
                let h = w.put(&blob(i % 15, 2000)).unwrap();
                let name = format!("w/{}", i % 40); // names are overwritten, so old targets become garbage
                set(&w, &name, h);
                v.push((name, h));
                std::thread::sleep(Duration::from_millis(60));
            }
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            v
        });
        while !done.load(std::sync::atomic::Ordering::SeqCst) {
            deleted += r.gc(&gc_opts, &OpaqueObjects).unwrap().deleted_packs;
            std::thread::sleep(Duration::from_millis(250));
        }
        writer.join().unwrap()
    });
    // Let at least one full cycle complete after the writer stops.
    for _ in 0..12 {
        deleted += r.gc(&gc_opts, &OpaqueObjects).unwrap().deleted_packs;
        std::thread::sleep(Duration::from_millis(700));
    }
    assert!(deleted > 0, "GC should have collected something");
    let fresh = LocalRepo::open(d.path()).unwrap();
    let mut last: std::collections::BTreeMap<String, Hash> = Default::default();
    for (name, h) in written {
        last.insert(name, h);
    }
    for (name, h) in last {
        let v = fresh.resolve(&Name::new(&name).unwrap()).unwrap().unwrap();
        assert_eq!(v.hash, h);
        assert_eq!(Hash::of(&fresh.get(&h, None).unwrap()), h, "{name}");
    }
}

/// A reference made via `ensure_live` to an object whose only copy is
/// condemned must survive the delete phase, even when the reference is created
/// *after* the delete phase's mark (the net session's OCI scenario). Here we
/// emulate that by calling ensure_live, then deleting without any rescue.
#[test]
fn ensure_live_rewrites_condemned_only_objects() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let small = r.put(b"old layer, unreferenced").unwrap();
    let big_data = blob(7, 40_000);
    let big = r.put(&big_data).unwrap(); // a manifest tree
    set(&r, "anchor", r.put(b"anchor").unwrap());
    r.flush().unwrap();
    r.gc(&opts(), &OpaqueObjects).unwrap(); // plan
    let s = r.gc(&opts(), &OpaqueObjects).unwrap(); // sweep: small and big condemned
    assert!(s.condemned_packs > 0);
    // Another process accepts an upload referencing both without re-putting.
    let w = LocalRepo::open(d.path()).unwrap();
    assert!(w.ensure_live(&small).unwrap());
    assert!(w.ensure_live(&big).unwrap());
    assert!(!w.ensure_live(&Hash::of(b"absent")).unwrap());
    w.flush().unwrap();
    // Delete phase with a mark that does NOT include them (refs unchanged).
    let del = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert!(del.deleted_packs > 0);
    let fresh = LocalRepo::open(d.path()).unwrap();
    assert_eq!(fresh.get(&small, None).unwrap(), b"old layer, unreferenced");
    assert_eq!(fresh.get(&big, None).unwrap(), big_data);
}

struct OneFile(Vec<Hash>);
impl o5s_repo::Grouper for OneFile {
    fn assign(
        &self,
        _: &dyn Repository,
        live: &std::collections::HashSet<Hash>,
    ) -> o5s_core::Result<std::collections::HashMap<Hash, o5s_repo::GroupKey>> {
        Ok(self
            .0
            .iter()
            .enumerate()
            .filter(|(_, h)| live.contains(h))
            .map(|(i, h)| (*h, o5s_repo::GroupKey { group: "file.rs".into(), order: format!("{i:06}") }))
            .collect())
    }
}

/// Exp1-V2 in production: 60 versions of one file, each a small edit of the
/// last, stored individually and then regrouped into frames by GC.
#[test]
fn grouped_repack_shrinks_versioned_history() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig { pack_cap: 1 << 20, ..PackConfig::tiny() }).unwrap();
    let mut text = blob(1, 1500);
    let mut versions = Vec::new();
    for v in 0..60u32 {
        let at = (v as usize * 37) % 1400;
        text[at..at + 8].copy_from_slice(&v.to_le_bytes().repeat(2));
        let h = r.put(&text).unwrap();
        set(&r, &format!("v/{v}"), h);
        versions.push((h, text.clone()));
    }
    r.flush().unwrap();
    let before = walk(&d.path().join("objects"));
    let g = OneFile(versions.iter().map(|v| v.0).collect());
    let o = GcOptions { repack_all: true, ..opts() };
    for _ in 0..3 {
        r.gc_with(&o, &OpaqueObjects, Some(&g)).unwrap();
    }
    let after = walk(&d.path().join("objects"));
    assert!(after * 3 < before, "grouped frames should compress versions together: {before} -> {after}");
    let fresh = LocalRepo::open(d.path()).unwrap();
    for (h, data) in &versions {
        assert_eq!(&fresh.get(h, None).unwrap(), data);
        assert_eq!(fresh.get(h, Some(10..20)).unwrap(), &data[10..20]);
    }
    // Copying a framed member out again (another GC cycle) works too.
    for _ in 0..3 {
        r.gc(&GcOptions { repack_all: true, ..opts() }, &OpaqueObjects).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    for (h, data) in &versions {
        assert_eq!(&fresh.get(h, None).unwrap(), data);
    }
}

/// A slow writer: puts (dedup-hits an object that is about to be condemned),
/// then replaces only after the condemned pack was deleted. The replace must
/// fail cleanly (MissingObject), never leave a dangling ref.
#[test]
fn slow_writer_after_delete_fails_cleanly() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let x = r.put(b"soon to be collected").unwrap();
    set(&r, "anchor", r.put(b"anchor").unwrap());
    r.flush().unwrap();
    // The slow writer has x in its index from before the GC cycle.
    let w = LocalRepo::open(d.path()).unwrap();
    assert!(w.contains(&x).unwrap());
    for _ in 0..3 {
        r.gc(&opts(), &OpaqueObjects).unwrap();
    }
    assert!(!LocalRepo::open(d.path()).unwrap().contains(&x).unwrap(), "x was collected");
    // Force the stale handle to re-list (its refresh interval has not elapsed).
    w.packs().set_dedup_refresh_interval(Duration::ZERO);
    let n = Name::new("late").unwrap();
    let res = w.replace(&n, &[(n.clone(), o5s_core::Observed::Absent)], x);
    assert!(matches!(res, Err(o5s_core::Error::MissingObject(_))), "{res:?}");
    assert_eq!(w.resolve(&n).unwrap(), None);
}

/// Crash GC at every fault point (writes, seals, record writes, deletes) across
/// whole cycles. After each crash, clean GC runs must converge: live data stays
/// intact, garbage is eventually collected, and no index points at a missing pack.
#[test]
fn gc_crash_at_every_fault_point() {
    use o5s_core::backend::{Backend, LocalDisk};
    use o5s_core::fault::Faults;
    use std::sync::Arc;
    let setup = |dir: &std::path::Path| -> (Vec<(Hash, Vec<u8>)>, Vec<Hash>) {
        let r = LocalRepo::init(dir, &PackConfig { pack_cap: 4096, ..PackConfig::tiny() }).unwrap();
        let mut live = Vec::new();
        let mut garbage = Vec::new();
        for i in 0..12u32 {
            let data = blob(i, 400 + (i as usize % 3) * 900);
            let h = r.put(&data).unwrap();
            if i % 3 == 0 {
                garbage.push(h);
            } else {
                set(&r, &format!("k/{i}"), h);
                live.push((h, data));
            }
        }
        let big = blob(99, 9000);
        let hb = r.put(&big).unwrap();
        set(&r, "k/big", hb);
        live.push((hb, big));
        r.flush().unwrap();
        (live, garbage)
    };
    let backend =
        |dir: &std::path::Path| -> Arc<dyn Backend> { Arc::new(LocalDisk::new(dir.join("objects")).unwrap()) };
    // Size the sweep with a disarmed run.
    let total = {
        let d = tempfile::tempdir().unwrap();
        setup(d.path());
        let f = Faults::disarmed();
        let r = LocalRepo::open_with_faults(d.path(), backend(d.path()), f.clone()).unwrap();
        for _ in 0..3 {
            r.gc(&opts(), &OpaqueObjects).unwrap();
        }
        f.hits()
    };
    assert!(total > 20, "GC should pass many fault points ({total})");
    for budget in 0..=total {
        let d = tempfile::tempdir().unwrap();
        let (live, garbage) = setup(d.path());
        {
            let f = Faults::crash_after(budget);
            if let Ok(r) = LocalRepo::open_with_faults(d.path(), backend(d.path()), f) {
                for _ in 0..3 {
                    if r.gc(&opts(), &OpaqueObjects).is_err() {
                        break;
                    }
                }
            }
        }
        // Recovery: fresh handles, clean GC runs until a full cycle completes.
        let r = LocalRepo::open(d.path()).unwrap();
        for _ in 0..6 {
            r.gc(&opts(), &OpaqueObjects).unwrap_or_else(|e| panic!("budget {budget}: gc after crash: {e}"));
        }
        let fresh = LocalRepo::open(d.path()).unwrap();
        for (h, data) in &live {
            assert_eq!(&fresh.get(h, None).unwrap_or_else(|e| panic!("budget {budget}: lost {h}: {e}")), data);
        }
        for g in &garbage {
            assert!(!fresh.contains(g).unwrap(), "budget {budget}: garbage {g} never collected");
        }
    }
}

#[test]
fn gc_keeps_delta_bases_alive() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap();
    let base = blob(3, 20_000);
    let hb = r.put(&base).unwrap();
    let mut v2 = base.clone();
    v2[100] ^= 0xff;
    let h2 = r.put_delta(&hb, &mut &v2[..]).unwrap();
    set(&r, "model/v2", h2); // only the delta is referenced
    for _ in 0..3 {
        r.gc(&GcOptions { repack_all: true, ..opts() }, &OpaqueObjects).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    assert_eq!(fresh.get(&h2, None).unwrap(), v2);
    assert!(fresh.contains(&hb).unwrap(), "base kept alive through the delta");
}

#[test]
fn fsck_passes_on_healthy_repo_and_detects_corruption() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig { pack_cap: 1 << 30, zstd_level: 0, ..PackConfig::tiny() }).unwrap();
    let h = r.put(b"precious bytes, stored raw so a flipped bit is visible").unwrap();
    let big = r.put(&blob(5, 30_000)).unwrap();
    set(&r, "a", h);
    set(&r, "b", big);
    r.flush().unwrap();
    let rep = r.fsck().unwrap();
    assert!(rep.ok(), "{rep:?}");
    assert!(rep.objects_checked > 10 && rep.refs_checked == 2);
    // Flip a byte inside the small object's payload in its sealed pack.
    let packs = d.path().join("objects/packs");
    for e in std::fs::read_dir(&packs).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "pack") {
            let mut b = std::fs::read(&p).unwrap();
            if let Some(i) = b.windows(8).position(|w| w == b"precious") {
                b[i] ^= 0x20;
                std::fs::write(&p, b).unwrap();
            }
        }
    }
    let rep = LocalRepo::open(d.path()).unwrap().fsck().unwrap();
    assert!(!rep.ok());
    assert!(rep.errors.iter().any(|e| e.contains("corrupt")), "{:?}", rep.errors);
}

/// Root cause of the flaky SIGKILL test (deterministic reproduction): an
/// unreferenced large object whose chunks were sealed into packs that GC
/// planned, while its manifest landed in a later pack outside the cycle.
/// GC deleted the chunks and kept the manifest, so the index served an object
/// that couldn't be read. GC must keep every child of a surviving record.
#[test]
fn gc_never_deletes_children_of_surviving_records() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { pack_cap: 4096, ..PackConfig::tiny() };
    let r = LocalRepo::init(d.path(), &cfg).unwrap();
    set(&r, "anchor", r.put(b"anchor").unwrap());
    r.flush().unwrap();
    // The writer streams a large object: its chunks seal into several small packs as it goes.
    let w = LocalRepo::open(d.path()).unwrap();
    let big = blob(42, 60_000);
    let h = w.put_stream(&mut &big[..]).unwrap();
    // GC plans now: the chunk packs are in the plan, but the writer's open pack
    // (holding the last records, including the root manifest) isn't sealed yet.
    let p = r.gc(&opts(), &OpaqueObjects).unwrap();
    assert!(p.planned.is_some());
    w.flush().unwrap(); // the manifest's pack appears after the plan
    drop(w);
    // The object is never referenced. Run the rest of the cycle (sweep, then delete).
    for _ in 0..3 {
        r.gc(&opts(), &OpaqueObjects).unwrap();
    }
    let fresh = LocalRepo::open(d.path()).unwrap();
    let rep = fresh.fsck().unwrap();
    assert!(rep.ok(), "GC left a surviving record pointing at deleted children: {:?}", rep.errors);
    // The object is either fully readable or fully gone, never half there.
    if fresh.contains(&h).unwrap() {
        assert_eq!(fresh.get(&h, None).unwrap(), big);
    }
    // And putting it again always works (and heals it if it had been broken).
    assert_eq!(fresh.put(&big).unwrap(), h);
    fresh.flush().unwrap();
    assert_eq!(fresh.get(&h, None).unwrap(), big);
}

/// A frame with a reference prefix keeps its reference alive: the reference
/// object is unreferenced by any name, yet GC must not delete it while a
/// framed member that needs it is live.
#[test]
fn gc_keeps_frame_references_alive() {
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() };
    let r = LocalRepo::init(d.path(), &cfg).unwrap();
    let base: Vec<u8> = (0..100_000u32).map(|i| (i.wrapping_mul(2654435761) >> 21) as u8).collect();
    let mut m = base.clone();
    m[500] ^= 1;
    let href = r.put(&base).unwrap();
    let hs = r.packs().put_frame_ref(&[&m[..]], 19, Some(&href)).unwrap();
    set(&r, "keep", hs[0]);
    r.flush().unwrap();
    for _ in 0..4 {
        r.gc(&GcOptions { repack_all: true, ..opts() }, &OpaqueObjects).unwrap();
    }
    let r = LocalRepo::open(d.path()).unwrap();
    assert_eq!(r.get(&hs[0], None).unwrap(), m, "the framed member still reads");
    assert!(r.contains(&href).unwrap(), "its reference survived");
    assert!(r.ensure_live(&hs[0]).unwrap());
}

/// Grouped repack through the content-similarity orderer: with no grouper
/// hints at all, every object reads back after repeated full GC cycles, and
/// near-duplicate versions end up in frames with reference prefixes.
#[test]
fn grouped_repack_uses_similarity_order_without_hints() {
    struct NoHints;
    impl o5s_repo::gc::Grouper for NoHints {
        fn assign(
            &self,
            _: &dyn o5s_core::Repository,
            _: &std::collections::HashSet<Hash>,
        ) -> o5s_core::Result<std::collections::HashMap<Hash, o5s_repo::gc::GroupKey>> {
            Ok(Default::default())
        }
    }
    let d = tempfile::tempdir().unwrap();
    let cfg = PackConfig { chunk_threshold: 1 << 20, ..PackConfig::default() };
    let r = LocalRepo::init(d.path(), &cfg).unwrap();
    let mut x = 7u64;
    let mut objs = Vec::new();
    for fam in 0..4u64 {
        let base: Vec<u8> = (0..60_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> (fam * 8)) as u8
            })
            .collect();
        for v in 0..30 {
            let mut b = base.clone();
            let l = b.len();
            b[(v * 997) % l] ^= 0x5a;
            objs.push(b);
        }
    }
    let hashes: Vec<Hash> = objs.iter().map(|o| r.put(o).unwrap()).collect();
    for (i, h) in hashes.iter().enumerate() {
        set(&r, &format!("o/{i}"), *h);
    }
    let garbage = r.put(b"unreferenced garbage object, collected").unwrap();
    r.flush().unwrap();
    let small = GcOptions { repack_all: true, frame_cap: 1 << 20, ..opts() };
    for _ in 0..4 {
        r.gc_with(&small, &OpaqueObjects, Some(&NoHints)).unwrap();
    }
    let r = LocalRepo::open(d.path()).unwrap();
    for (h, o) in hashes.iter().zip(&objs) {
        assert_eq!(&r.get(h, None).unwrap(), o);
    }
    let with_ref = hashes.iter().filter(|h| r.packs().chunks(h).unwrap().is_some()).count();
    assert!(with_ref > 0, "some members are in frames with a reference");
    assert!(!r.contains(&garbage).unwrap());
}

/// Similarity repack is the default (no grouper needed) and can be disabled:
/// with `similarity_repack = false` a full repack writes no frames; by
/// default the same repository ends up with framed members.
#[test]
fn similarity_repack_is_default_and_can_be_disabled() {
    let framed = |r: &LocalRepo| -> usize {
        let mut n = 0;
        for p in r.packs().live_packs().unwrap() {
            n += r.packs().pack_entries(&p).unwrap().iter().filter(|e| e.frame.is_some()).count();
        }
        n
    };
    for (similarity, expect_frames) in [(false, false), (true, true)] {
        let d = tempfile::tempdir().unwrap();
        let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
        assert!(r.gc_options().similarity_repack, "on by default");
        for i in 0..40u32 {
            let mut b = blob(7, 20_000);
            b[(i as usize * 101) % 20_000] ^= 1;
            let h = r.put(&b).unwrap();
            set(&r, &format!("v/{i}"), h);
        }
        r.flush().unwrap();
        let opts = GcOptions { repack_all: true, similarity_repack: similarity, ..opts() };
        for _ in 0..4 {
            r.gc(&opts, &OpaqueObjects).unwrap();
        }
        let r = LocalRepo::open(d.path()).unwrap();
        assert_eq!(framed(&r) > 0, expect_frames, "similarity_repack = {similarity}");
        for i in 0..40u32 {
            assert!(r.resolve(&Name::new(format!("v/{i}")).unwrap()).unwrap().is_some());
        }
    }
}
