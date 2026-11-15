use pico2peta_core::{Hash, Name, ObjectStore, RefValue, ReplaceOutcome, Repository};
use pico2peta_repo::{GcOptions, LocalRepo, OpaqueObjects, PackConfig};
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
impl pico2peta_repo::Grouper for OneFile {
    fn assign(
        &self,
        _: &dyn Repository,
        live: &std::collections::HashSet<Hash>,
    ) -> pico2peta_core::Result<std::collections::HashMap<Hash, pico2peta_repo::GroupKey>> {
        Ok(self
            .0
            .iter()
            .enumerate()
            .filter(|(_, h)| live.contains(h))
            .map(|(i, h)| (*h, pico2peta_repo::GroupKey { group: "file.rs".into(), order: format!("{i:06}") }))
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
