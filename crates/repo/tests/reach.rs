//! The per-cell reachability index (object-read authorization).

use o5s_core::{Hash, Name, ObjectStore, Observed, RefValue, Repository};
use o5s_repo::reach::ReachIndex;
use o5s_repo::{LocalRepo, PackConfig};

fn set(r: &LocalRepo, name: &str, h: Hash) {
    let n = Name::new(name).unwrap();
    let cur = r.resolve(&n).unwrap();
    r.replace(&n, &[(n.clone(), RefValue::observed(cur))], h).unwrap();
}

fn del(r: &LocalRepo, name: &str) {
    let n = Name::new(name).unwrap();
    let cur = r.resolve(&n).unwrap();
    r.delete_ref(&n, &[(n.clone(), RefValue::observed(cur))]).unwrap();
}

fn blob(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(6364136223846793005) | 1;
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
fn readability_follows_names_and_cells() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    // Cells: 0 = "" (everything not under a deeper cell), 1 = team-a, 2 = team-b.
    let cells = vec![String::new(), "team-a".to_string(), "team-b".to_string()];
    let big = blob(1, 2 << 20); // chunked: its chunks must be members too
    let hb = r.put(&big).unwrap();
    let small = r.put(b"small").unwrap();
    let loose = r.put(b"never named").unwrap();
    set(&r, "team-a/model", hb);
    let idx = ReachIndex::open(&r, cells.clone()).unwrap();
    let chunks = r.packs().chunks(&hb).unwrap().unwrap();
    assert!(!chunks.is_empty());
    let mut probe = vec![hb, small, loose];
    probe.extend(&chunks);
    let a = idx.cells_reaching_many(&probe, &[0, 1, 2]).unwrap();
    assert_eq!(a[0], vec![1], "the object is reachable from team-a only");
    assert!(a[1].is_empty() && a[2].is_empty(), "unnamed objects are unreadable");
    assert!(a[3..].iter().all(|c| c == &vec![1]), "chunks are reachable with their object");

    // A new name (created after open, through a clone of the handle) is visible at once.
    let r2 = r.clone();
    set(&r2, "team-b/x", small);
    assert_eq!(idx.cells_reaching_many(&[small], &[0, 1, 2]).unwrap()[0], vec![2]);
    // A name outside every deeper cell belongs to "".
    set(&r, "other/y", small);
    assert_eq!(idx.cells_reaching_many(&[small], &[0, 1, 2]).unwrap()[0], vec![0, 2]);

    // Deleting the last readable name makes the object unreadable immediately.
    del(&r, "team-b/x");
    assert_eq!(idx.cells_reaching_many(&[small], &[0, 1, 2]).unwrap()[0], vec![0]);
    del(&r, "other/y");
    assert!(idx.readable_many(&[small], &[0, 1, 2]).unwrap() == vec![false]);

    // Moving a name: the old target goes, the new one comes.
    set(&r, "team-a/model", small);
    let m = idx.cells_reaching_many(&[hb, small, chunks[0]], &[1]).unwrap();
    assert!(m[0].is_empty() && m[2].is_empty(), "the old target and its chunks are unreadable");
    assert_eq!(m[1], vec![1]);

    // A policy reload regroups the cells.
    idx.on_policy_reload(vec!["team-a".to_string()]).unwrap();
    assert_eq!(idx.cells_reaching_many(&[small], &[0]).unwrap()[0], vec![0]);
    // Unknown cell ids are ignored.
    assert!(idx.cells_reaching_many(&[small], &[7]).unwrap()[0].is_empty());
}

#[test]
fn batched_updates_and_versions_after_delete() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let idx = ReachIndex::open(&r, vec![String::new()]).unwrap();
    let h1 = r.put(b"one").unwrap();
    let h2 = r.put(b"two").unwrap();
    let a = Name::new("a/1").unwrap();
    let b = Name::new("a/2").unwrap();
    r.update_many(
        &[(a.clone(), Some(h1)), (b.clone(), Some(h2))],
        &[(a.clone(), Observed::Absent), (b.clone(), Observed::Absent)],
    )
    .unwrap();
    assert_eq!(idx.readable_many(&[h1, h2], &[0]).unwrap(), vec![true, true]);
    // A delete inside a batch, then re-creating the name (version > 1): still exact.
    r.update_many(&[(a.clone(), None)], &[(a.clone(), Observed::Version(1))]).unwrap();
    assert_eq!(idx.readable_many(&[h1, h2], &[0]).unwrap(), vec![false, true]);
    set(&r, "a/1", h1);
    assert_eq!(idx.readable_many(&[h1], &[0]).unwrap(), vec![true]);
}

#[test]
fn pending_creates_settle_exactly_with_or_without_the_worker() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::default()).unwrap();
    let idx = ReachIndex::open(&r, vec![String::new(), "t".to_string()]).unwrap();
    let hs: Vec<Hash> = (0..50u64).map(|i| r.put(&blob(i + 10, 1000)).unwrap()).collect();
    // Creates only queue roots; a query right after them answers exactly (read-your-writes).
    for (i, h) in hs.iter().enumerate() {
        set(&r, &format!("t/n{i}"), *h);
    }
    // A delete while roots are still pending: the cell is recomputed, the target revoked at once.
    del(&r, "t/n0");
    let got = idx.readable_many(&hs, &[1]).unwrap();
    assert!(!got[0], "deleted name's target is unreadable immediately");
    assert!(got[1..].iter().all(|b| *b));
    // More creates, then let the background worker settle them before asking.
    let more: Vec<Hash> = (0..20u64).map(|i| r.put(&blob(i + 100, 1000)).unwrap()).collect();
    for (i, h) in more.iter().enumerate() {
        set(&r, &format!("t/m{i}"), *h);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(idx.readable_many(&more, &[1]).unwrap().iter().all(|b| *b));
    assert!(idx.readable_many(&more, &[0]).unwrap().iter().all(|b| !*b), "other cells are unaffected");
}

/// Commits by another writer, which this handle never sees as a local observer: another process on
/// a local journal, or another host on a backend log (here a second handle on the same repository,
/// with its own ref store). Reads stay exact: grants after a foreign create, revocations after a
/// foreign move or delete.
fn foreign_writer_case(store: o5s_repo::RefsStore) {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = o5s_repo::RepoConfig::default();
    cfg.refs.store = store;
    let r1 = LocalRepo::init_config(d.path(), &cfg).unwrap();
    let r2 = LocalRepo::open(d.path()).unwrap();
    let idx = ReachIndex::open(&r1, vec![String::new(), "team".to_string()]).unwrap();
    let big = blob(9, 2 << 20);
    let a = r2.put(&big).unwrap();
    let b = r2.put(b"other target").unwrap();
    r2.flush().unwrap();
    let chunks = r2.packs().chunks(&a).unwrap().unwrap();
    // A foreign create: readable through handle 1 at once, chunks included, in the right cell.
    set(&r2, "team/m", a);
    let got = idx.cells_reaching_many(&[a, chunks[0], b], &[0, 1]).unwrap();
    assert_eq!(got[0], vec![1], "{store:?}: foreign create grants");
    assert_eq!(got[1], vec![1], "{store:?}: with its chunks");
    assert!(got[2].is_empty());
    // A foreign move revokes the old target.
    set(&r2, "team/m", b);
    assert_eq!(idx.readable_many(&[a, b], &[1]).unwrap(), vec![false, true], "{store:?}: foreign move revokes");
    // A foreign delete revokes.
    del(&r2, "team/m");
    assert_eq!(idx.readable_many(&[a, b], &[0, 1]).unwrap(), vec![false, false], "{store:?}: foreign delete revokes");
    // And a re-create (version > 1) grants again.
    set(&r2, "team/m", a);
    assert_eq!(idx.readable_many(&[a], &[1]).unwrap(), vec![true]);
}

#[test]
fn foreign_writers_on_a_local_journal_are_seen() {
    foreign_writer_case(o5s_repo::RefsStore::Local);
}

#[test]
fn foreign_hosts_on_a_backend_log_are_seen() {
    foreign_writer_case(o5s_repo::RefsStore::Backend);
}
