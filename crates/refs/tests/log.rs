//! The backend-log ref store: same semantics as the journal store, plus
//! multi-host safety via create-only commits.
use pico2peta_core::backend::{Backend, LocalDisk};
use pico2peta_core::fault::Faults;
use pico2peta_core::{Hash, Name, Observed, RefValue, ReplaceOutcome, Result, Rule};
use pico2peta_refs::RefStore;
use std::sync::Arc;

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}
fn h(i: u64) -> Hash {
    Hash::of(&i.to_le_bytes())
}
fn yes(_: &Hash) -> Result<bool> {
    Ok(true)
}

#[test]
fn log_store_semantics_and_snapshots() {
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
    let r = RefStore::open_backend(b.clone(), "repo1/refs/").unwrap();
    r.set_compact_threshold(5);
    r.set_rule("pkg", Rule::ImmutableAfterPublish).unwrap();
    let a = n("a");
    for i in 0..23u64 {
        let obs = RefValue::observed(r.resolve(&a).unwrap());
        assert!(matches!(r.replace(&a, &[(a.clone(), obs)], h(i), &yes).unwrap(), ReplaceOutcome::Ok { .. }));
    }
    let p = n("pkg/x");
    r.replace(&p, &[(p.clone(), Observed::Absent)], h(1), &yes).unwrap();
    assert!(r.replace(&p, &[(p.clone(), Observed::Version(1))], h(2), &yes).is_err());
    assert!(r.generation().unwrap() >= 20, "snapshots taken");
    // A fresh handle starts from the newest snapshot and replays the tail.
    let r2 = RefStore::open_backend(b.clone(), "repo1/refs/").unwrap();
    assert_eq!(r2.snapshot().unwrap(), r.snapshot().unwrap());
    assert_eq!(r2.resolve(&a).unwrap().unwrap().version, 23);
    // Stale writers conflict across handles.
    assert!(matches!(
        r2.replace(&a, &[(a.clone(), Observed::Version(22))], h(99), &yes).unwrap(),
        ReplaceOutcome::Conflict { .. }
    ));
    // Another prefix is an independent store.
    let other = RefStore::open_backend(b, "repo2/refs/").unwrap();
    assert_eq!(other.resolve(&a).unwrap(), None);
}

/// Many handles (standing in for hosts) contend on one counter: no lost updates.
#[test]
fn log_store_multi_handle_contention() {
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
    let c = n("counter");
    let wins: u64 = std::thread::scope(|sc| {
        (0..6u64)
            .map(|t| {
                let b = b.clone();
                let c = c.clone();
                sc.spawn(move || {
                    let r = RefStore::open_backend(b, "x/").unwrap();
                    r.set_compact_threshold(17);
                    let mut w = 0;
                    while w < 30 {
                        let obs = RefValue::observed(r.resolve(&c).unwrap());
                        if let ReplaceOutcome::Ok { .. } =
                            r.replace(&c, &[(c.clone(), obs)], h(t * 100 + w), &yes).unwrap()
                        {
                            w += 1;
                        }
                    }
                    w
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|j| j.join().unwrap())
            .sum()
    });
    let r = RefStore::open_backend(b, "x/").unwrap();
    assert_eq!(r.resolve(&c).unwrap().unwrap().version, wins);
}

/// Crash at every fault point: acknowledged commits survive; the store recovers.
#[test]
fn log_store_crash_sweep() {
    for budget in 0..30u64 {
        let d = tempfile::tempdir().unwrap();
        let b: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
        let mut acked = Vec::new();
        {
            let r = RefStore::open_backend_with_faults(b.clone(), "p/", Faults::crash_after(budget)).unwrap();
            r.set_compact_threshold(3);
            for i in 0..10u64 {
                let nm = n(&format!("k/{i}"));
                match r.replace(&nm, &[(nm.clone(), Observed::Absent)], h(i), &yes) {
                    Ok(ReplaceOutcome::Ok { .. }) => acked.push((nm, h(i))),
                    Ok(o) => panic!("{o:?}"),
                    Err(_) => break,
                }
            }
        }
        let r = RefStore::open_backend(b, "p/").unwrap();
        for (nm, hh) in &acked {
            assert_eq!(r.resolve(nm).unwrap().map(|v| v.hash), Some(*hh), "budget {budget}");
        }
        let z = n("z");
        r.replace(&z, &[(z.clone(), Observed::Absent)], h(77), &yes).unwrap();
    }
}

/// Review finding 5: a stale handle whose position was trimmed away (forced
/// here with zero grace) must never acknowledge a commit that others can't see.
#[test]
fn stale_handle_after_trimming_never_loses_acknowledged_commits() {
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
    let stale = RefStore::open_backend(b.clone(), "t/").unwrap();
    let busy = RefStore::open_backend(b.clone(), "t/").unwrap();
    busy.set_compact_threshold(3);
    busy.set_trim_grace(std::time::Duration::ZERO);
    let x = n("x");
    stale.replace(&x, &[(x.clone(), Observed::Absent)], h(1), &yes).unwrap();
    // The busy handle commits a lot and trims aggressively behind its snapshots.
    for i in 0..40u64 {
        let k = n(&format!("busy/{i}"));
        busy.replace(&k, &[(k.clone(), Observed::Absent)], h(i), &yes).unwrap();
    }
    // The stale handle now commits against what it believes is the end of the log.
    let y = n("y");
    match stale.replace(&y, &[(y.clone(), Observed::Absent)], h(7), &yes) {
        Ok(ReplaceOutcome::Ok { .. }) => {
            let fresh = RefStore::open_backend(b.clone(), "t/").unwrap();
            assert_eq!(fresh.resolve(&y).unwrap().map(|v| v.hash), Some(h(7)), "acknowledged commit must be visible");
        }
        Ok(o) => panic!("{o:?}"),
        Err(_) => {} // refusing is fine; acknowledging an invisible write is not
    }
    let fresh = RefStore::open_backend(b, "t/").unwrap();
    assert_eq!(fresh.list("busy").unwrap().len(), 40);
    assert_eq!(fresh.resolve(&x).unwrap().map(|v| v.hash), Some(h(1)));
}

#[test]
fn backend_without_atomic_create_is_refused_for_refs() {
    struct NonAtomic(LocalDisk);
    impl Backend for NonAtomic {
        fn put_if_absent(
            &self,
            k: &str,
            s: pico2peta_core::backend::PutSource<'_>,
        ) -> Result<pico2peta_core::backend::PutOutcome> {
            self.0.put_if_absent(k, s)
        }
        fn get_range(&self, k: &str, r: std::ops::Range<u64>) -> Result<Vec<u8>> {
            self.0.get_range(k, r)
        }
        fn open(&self, k: &str) -> Result<Box<dyn std::io::Read + Send>> {
            self.0.open(k)
        }
        fn head(&self, k: &str) -> Result<Option<pico2peta_core::backend::BlobMeta>> {
            self.0.head(k)
        }
        fn list(&self, p: &str) -> Result<Vec<pico2peta_core::backend::BlobMeta>> {
            self.0.list(p)
        }
        fn delete(&self, k: &str) -> Result<()> {
            self.0.delete(k)
        }
        fn describe(&self) -> String {
            "non-atomic".into()
        }
        fn atomic_create(&self) -> bool {
            false
        }
    }
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(NonAtomic(LocalDisk::new(d.path()).unwrap()));
    assert!(RefStore::open_backend(b, "r/").is_err());
}
