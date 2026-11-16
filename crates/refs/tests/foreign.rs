//! Changes made by other writers (another process on a local journal, another host on a backend
//! log) are reported to a store's foreign observers as the store applies them; its own commits
//! are not.

use o5s_core::backend::{Backend, LocalDisk};
use o5s_core::{Hash, Name, RefValue, Result};
use o5s_refs::{Foreign, RefStore};
use std::sync::{Arc, Mutex};

fn n(s: &str) -> Name {
    Name::new(s).unwrap()
}
fn h(i: u64) -> Hash {
    Hash::of(&i.to_le_bytes())
}
fn yes(_: &Hash) -> Result<bool> {
    Ok(true)
}

fn watch(r: &RefStore) -> Arc<Mutex<Vec<Foreign>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = seen.clone();
    r.add_foreign_observer(Arc::new(move |f: &Foreign| s.lock().unwrap().push(f.clone())));
    seen
}

fn take(seen: &Arc<Mutex<Vec<Foreign>>>) -> Vec<Foreign> {
    std::mem::take(&mut *seen.lock().unwrap())
}

fn set(r: &RefStore, name: &str, t: Hash) {
    let k = n(name);
    let obs = RefValue::observed(r.resolve(&k).unwrap());
    r.replace(&k, &[(k.clone(), obs)], t, &yes).unwrap();
}

fn del(r: &RefStore, name: &str) {
    let k = n(name);
    let obs = RefValue::observed(r.resolve(&k).unwrap());
    r.delete(&k, &[(k.clone(), obs)]).unwrap();
}

/// Creates, moves and deletes by the other writer arrive in order with their versions; the
/// observer's own store reports none of its own commits.
fn changes_from_other_writers(a: RefStore, b: RefStore) {
    let at_a = watch(&a);
    let at_b = watch(&b);
    set(&b, "x", h(1));
    a.catch_up().unwrap();
    assert_eq!(take(&at_a), vec![Foreign::Changes(vec![(n("x"), Some(h(1)), 1)])]);
    set(&b, "x", h(2));
    del(&b, "x");
    a.catch_up().unwrap();
    let got: Vec<_> = take(&at_a)
        .into_iter()
        .flat_map(|f| match f {
            Foreign::Changes(c) => c,
            Foreign::Reset => panic!("unexpected reset"),
        })
        .collect();
    assert_eq!(got, vec![(n("x"), Some(h(2)), 2), (n("x"), None, 2)]);
    // A's own commit is not foreign to A, but is to B.
    set(&a, "y", h(3));
    assert!(take(&at_a).is_empty(), "own commits are not reported");
    b.catch_up().unwrap();
    assert_eq!(take(&at_b), vec![Foreign::Changes(vec![(n("y"), Some(h(3)), 1)])]);
}

#[test]
fn journal_reports_other_processes_changes() {
    let d = tempfile::tempdir().unwrap();
    changes_from_other_writers(RefStore::open(d.path()).unwrap(), RefStore::open(d.path()).unwrap());
}

#[test]
fn backend_log_reports_other_hosts_changes() {
    let d = tempfile::tempdir().unwrap();
    let b: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
    changes_from_other_writers(
        RefStore::open_backend(b.clone(), "t/").unwrap(),
        RefStore::open_backend(b, "t/").unwrap(),
    );
}

/// A rotation by another process (local journal compaction) replaces the whole state: the
/// observer gets `Reset`, and changes after it still arrive.
#[test]
fn journal_rotation_by_another_process_is_a_reset() {
    let d = tempfile::tempdir().unwrap();
    let a = RefStore::open(d.path()).unwrap();
    let b = RefStore::open(d.path()).unwrap();
    let at_a = watch(&a);
    set(&b, "x", h(1));
    b.compact().unwrap();
    a.catch_up().unwrap();
    assert!(take(&at_a).contains(&Foreign::Reset), "rotation reported as a reset");
    set(&b, "z", h(5));
    a.catch_up().unwrap();
    assert_eq!(take(&at_a), vec![Foreign::Changes(vec![(n("z"), Some(h(5)), 1)])]);
}

/// A handle whose position was trimmed behind a newer snapshot rebases on it: `Reset`.
#[test]
fn backend_log_rebase_on_a_snapshot_is_a_reset() {
    let d = tempfile::tempdir().unwrap();
    let be: Arc<dyn Backend> = Arc::new(LocalDisk::new(d.path()).unwrap());
    let stale = RefStore::open_backend(be.clone(), "t/").unwrap();
    let busy = RefStore::open_backend(be, "t/").unwrap();
    busy.set_compact_threshold(3);
    busy.set_trim_grace(std::time::Duration::ZERO);
    let at_stale = watch(&stale);
    set(&stale, "x", h(1));
    for i in 0..40u64 {
        set(&busy, &format!("busy/{i}"), h(i));
    }
    stale.catch_up().unwrap();
    assert!(take(&at_stale).contains(&Foreign::Reset), "rebase reported as a reset");
    assert_eq!(stale.list("busy").unwrap().len(), 40);
}
