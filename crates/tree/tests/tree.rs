//! Tree objects end to end: canonical form, the filesystem round trip, diff,
//! and GC keeping a tree's children alive.
use o5s_core::{Hash, Name, Observed, ObjectStore, ReplaceOutcome, Repository};
use o5s_repo::gc::GcOptions;
use o5s_repo::{FrontRegistry, LocalRepo, PackConfig};
use o5s_tree::{Change, Entry, Mode, PutDirOpts, Status, Tree, TreeFront};
use proptest::prelude::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn repo(d: &Path) -> LocalRepo {
    let reg = FrontRegistry::new().with(Arc::new(TreeFront)).unwrap();
    LocalRepo::init(d, &PackConfig::tiny()).unwrap().with_fronts(reg).unwrap()
}

fn e(name: &[u8], mode: Mode, h: Hash) -> Entry {
    Entry { name: name.to_vec(), mode, hash: h }
}

fn h(b: u8) -> Hash {
    Hash([b; 32])
}

#[test]
fn canonical_form() {
    // Entry order doesn't matter: `Tree::new` sorts, so equal directories encode equally.
    let a = Tree::new(vec![e(b"b", Mode::Regular, h(1)), e(b"a", Mode::Subtree, h(2))]).unwrap();
    let b = Tree::new(vec![e(b"a", Mode::Subtree, h(2)), e(b"b", Mode::Regular, h(1))]).unwrap();
    assert_eq!(a.encode().unwrap(), b.encode().unwrap());
    let bytes = a.encode().unwrap();
    assert_eq!(&bytes[..8], b"O5STREE1");
    assert_eq!(bytes.len(), 8 + 2 * 36);
    assert_eq!(Tree::decode(&bytes).unwrap(), a);
    // The empty tree is just the magic.
    assert_eq!(Tree::default().encode().unwrap(), b"O5STREE1");

    // Refused: duplicate names, invalid names.
    assert!(Tree::new(vec![e(b"x", Mode::Regular, h(1)), e(b"x", Mode::Executable, h(1))]).is_err());
    for bad in [&b""[..], b".", b"..", b"a/b", b"a\0b"] {
        assert!(Tree::new(vec![e(bad, Mode::Regular, h(1))]).is_err(), "{bad:?}");
    }
    assert!(Tree::new(vec![e(&vec![b'n'; 65536], Mode::Regular, h(1))]).is_err());
    assert!(Tree::new(vec![e(&vec![b'n'; 65535], Mode::Regular, h(1))]).is_ok());
    // "..." and ".x" are ordinary names.
    assert!(Tree::new(vec![e(b"...", Mode::Regular, h(1)), e(b".x", Mode::Regular, h(1))]).is_ok());

    // Not canonical: unsorted entries, a bad mode, trailing bytes, a short entry.
    let mut unsorted = b"O5STREE1".to_vec();
    for name in [b"b", b"a"] {
        unsorted.extend_from_slice(&[0, 1, 0]);
        unsorted.extend_from_slice(name);
        unsorted.extend_from_slice(&[7; 32]);
    }
    assert!(Tree::decode(&unsorted).is_err());
    let mut bad_mode = bytes.clone();
    bad_mode[8] = 4;
    assert!(Tree::decode(&bad_mode).is_err());
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Tree::decode(&trailing).is_err());
    assert!(Tree::decode(&bytes[..bytes.len() - 1]).is_err());
    assert!(Tree::decode(b"O5STREE").is_err());
    assert!(Tree::decode(b"O5STREE2").is_err());
}

fn arb_entries() -> impl Strategy<Value = Vec<Entry>> {
    let name = prop::collection::vec(any::<u8>(), 1..12);
    let mode = prop_oneof![Just(Mode::Regular), Just(Mode::Executable), Just(Mode::Symlink), Just(Mode::Subtree)];
    prop::collection::vec((name, mode, any::<[u8; 32]>()), 0..20)
        .prop_map(|v| v.into_iter().map(|(n, m, hh)| Entry { name: n, mode: m, hash: Hash(hh) }).collect())
}

proptest! {
    /// Valid entry sets round-trip; invalid ones are refused, never mangled.
    #[test]
    fn roundtrip(entries in arb_entries()) {
        match Tree::new(entries.clone()) {
            Ok(t) => {
                let bytes = t.encode().unwrap();
                let back = Tree::decode(&bytes).unwrap();
                prop_assert_eq!(&back, &t);
                prop_assert_eq!(back.encode().unwrap(), bytes);
            }
            Err(_) => {
                let mut names: Vec<&Vec<u8>> = entries.iter().map(|e| &e.name).collect();
                names.sort();
                let dup = names.windows(2).any(|w| w[0] == w[1]);
                let bad = entries.iter().any(|e| e.name.contains(&0) || e.name.contains(&b'/')
                    || e.name == b"." || e.name == b"..");
                prop_assert!(dup || bad);
            }
        }
    }

    /// Arbitrary bytes never panic the decoder, and whatever it accepts re-encodes to the same bytes.
    #[test]
    fn decode_any(mut tail in prop::collection::vec(any::<u8>(), 0..300), magic in any::<bool>()) {
        let mut b = if magic { b"O5STREE1".to_vec() } else { Vec::new() };
        b.append(&mut tail);
        if let Ok(t) = Tree::decode(&b) {
            prop_assert_eq!(t.encode().unwrap(), b);
        }
    }
}

#[test]
fn put_dir_checkout_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let r = repo(&d.path().join("r"));
    let src = d.path().join("src");
    std::fs::create_dir_all(src.join("sub/deep")).unwrap();
    std::fs::create_dir_all(src.join("empty")).unwrap();
    std::fs::create_dir_all(src.join(".git")).unwrap();
    std::fs::write(src.join("a.txt"), b"hello\n").unwrap();
    std::fs::write(src.join("run.sh"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(src.join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(src.join("sub/deep/big.bin"), (0..200_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>())
        .unwrap();
    std::os::unix::fs::symlink("a.txt", src.join("link")).unwrap();
    std::os::unix::fs::symlink("../../nowhere", src.join("sub/dangling")).unwrap();
    let raw = std::ffi::OsStr::from_bytes(b"\xff raw name");
    std::fs::write(src.join(raw), b"x").unwrap();
    std::fs::write(src.join(".git/HEAD"), b"ref").unwrap();

    let opts = PutDirOpts { exclude: [b".git".to_vec()].into() };
    let (root, stats) = o5s_tree::put_dir(&r, &src, &opts).unwrap();
    assert_eq!(stats.files, 4);
    assert_eq!(stats.symlinks, 2);
    assert_eq!(stats.dirs, 4);
    // Deterministic: the same directory gives the same hash.
    assert_eq!(o5s_tree::put_dir(&r, &src, &opts).unwrap().0, root);

    let t = o5s_tree::get_tree(&r, &root).unwrap();
    assert!(t.get(b".git").is_none());
    assert_eq!(t.get(b"run.sh").unwrap().mode, Mode::Executable);
    assert_eq!(t.get(b"a.txt").unwrap().mode, Mode::Regular);
    assert_eq!(t.get(b"link").unwrap().mode, Mode::Symlink);
    assert_eq!(r.get(&t.get(b"link").unwrap().hash, None).unwrap(), b"a.txt");
    assert_eq!(t.get(b"empty").unwrap().hash, r.put(b"O5STREE1").unwrap());

    let out = d.path().join("out");
    o5s_tree::checkout(&r, &root, &out).unwrap();
    for p in ["a.txt", "sub/deep/big.bin"] {
        assert_eq!(std::fs::read(out.join(p)).unwrap(), std::fs::read(src.join(p)).unwrap());
    }
    assert_eq!(std::fs::read(out.join(raw)).unwrap(), b"x");
    assert!(std::fs::metadata(out.join("run.sh")).unwrap().permissions().mode() & 0o100 != 0);
    assert!(std::fs::metadata(out.join("a.txt")).unwrap().permissions().mode() & 0o111 == 0);
    assert_eq!(std::fs::read_link(out.join("link")).unwrap(), Path::new("a.txt"));
    assert_eq!(std::fs::read_link(out.join("sub/dangling")).unwrap(), Path::new("../../nowhere"));
    assert!(std::fs::read_dir(out.join("empty")).unwrap().next().is_none());
    assert!(!out.join(".git").exists());
    // The checkout snapshots back to the same tree.
    assert_eq!(o5s_tree::put_dir(&r, &out, &PutDirOpts::default()).unwrap().0, root);

    // A non-empty target is refused; an empty one is fine.
    assert!(o5s_tree::checkout(&r, &root, &out).is_err());
    let empty = d.path().join("empty-target");
    std::fs::create_dir(&empty).unwrap();
    o5s_tree::checkout(&r, &root, &empty).unwrap();
    // A blob is not a tree.
    let blob = r.put(b"not a tree").unwrap();
    assert!(o5s_tree::checkout(&r, &blob, &d.path().join("x")).is_err());
}

#[test]
fn put_tree_needs_children() {
    let d = tempfile::tempdir().unwrap();
    let r = repo(d.path());
    let t = Tree::new(vec![e(b"missing", Mode::Regular, h(9))]).unwrap();
    assert!(o5s_tree::put_tree(&r, &t).is_err());
    let blob = r.put(b"present").unwrap();
    let t = Tree::new(vec![e(b"present", Mode::Regular, blob)]).unwrap();
    let th = o5s_tree::put_tree(&r, &t).unwrap();
    assert_eq!(o5s_tree::get_tree(&r, &th).unwrap(), t);
}

fn ch(path: &str, status: Status) -> (Vec<u8>, Status) {
    (path.as_bytes().to_vec(), status)
}

#[test]
fn diff_trees() {
    let d = tempfile::tempdir().unwrap();
    let r = repo(d.path());
    let b1 = r.put(b"one").unwrap();
    let b2 = r.put(b"two").unwrap();
    let put = |es: Vec<Entry>| o5s_tree::put_tree(&r, &Tree::new(es).unwrap()).unwrap();
    let deep_a = put(vec![e(b"f", Mode::Regular, b1)]);
    let deep_b = put(vec![e(b"f", Mode::Regular, b2), e(b"g", Mode::Regular, b1)]);
    let a = put(vec![
        e(b"same", Mode::Regular, b1),
        e(b"gone", Mode::Regular, b1),
        e(b"exec", Mode::Regular, b1),
        e(b"dir", Mode::Subtree, deep_a),
        e(b"was-dir", Mode::Subtree, deep_a),
        e(b"link", Mode::Symlink, b1),
    ]);
    let b = put(vec![
        e(b"same", Mode::Regular, b1),
        e(b"new", Mode::Regular, b2),
        e(b"exec", Mode::Executable, b1),
        e(b"dir", Mode::Subtree, deep_b),
        e(b"was-dir", Mode::Regular, b2),
        e(b"link", Mode::Symlink, b2),
    ]);
    let got: Vec<(Vec<u8>, Status)> = o5s_tree::diff(&r, &a, &b).unwrap().into_iter().map(|c| (c.path, c.status)).collect();
    assert_eq!(
        got,
        vec![
            ch("dir/f", Status::Modified),
            ch("dir/g", Status::Added),
            ch("exec", Status::TypeChanged),
            ch("gone", Status::Deleted),
            ch("link", Status::Modified),
            ch("new", Status::Added),
            ch("was-dir", Status::Added),
            ch("was-dir/f", Status::Deleted),
        ]
    );
    assert!(o5s_tree::diff(&r, &a, &a).unwrap().is_empty());
    let back: Vec<Change> = o5s_tree::diff(&r, &b, &a).unwrap();
    assert_eq!(back.len(), 8);
}

#[test]
fn gc_keeps_tree_children() {
    let d = tempfile::tempdir().unwrap();
    let r = repo(d.path());
    let leaf = r.put(b"only the tree references me").unwrap();
    let link = r.put(b"target").unwrap();
    let sub = o5s_tree::put_tree(&r, &Tree::new(vec![e(b"leaf", Mode::Executable, leaf)]).unwrap()).unwrap();
    let root =
        o5s_tree::put_tree(&r, &Tree::new(vec![e(b"sub", Mode::Subtree, sub), e(b"l", Mode::Symlink, link)]).unwrap())
            .unwrap();
    // A blob that starts with the magic but isn't a tree, referencing nothing.
    let fake = r.put(b"O5STREE1 but not canonical").unwrap();
    let garbage = r.put(b"unreferenced").unwrap();
    let n = |s: &str| Name::new(s).unwrap();
    assert!(matches!(r.replace(&n("snap"), &[(n("snap"), Observed::Absent)], root).unwrap(), ReplaceOutcome::Ok { .. }));
    assert!(matches!(r.replace(&n("fake"), &[(n("fake"), Observed::Absent)], fake).unwrap(), ReplaceOutcome::Ok { .. }));
    r.flush().unwrap();
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, repack_all: true, ..GcOptions::default() };
    for _ in 0..3 {
        r.gc_with_fronts(&opts, None, false).unwrap();
    }
    for x in [root, sub, leaf, link, fake] {
        assert!(r.contains(&x).unwrap(), "{x} must survive");
    }
    assert!(!LocalRepo::open(d.path()).unwrap().contains(&garbage).unwrap(), "garbage collected");
    // The tracer: exactly the entries, for trees only.
    assert_eq!(o5s_tree::tree_children(&root, &r).unwrap(), vec![link, sub]);
    assert!(o5s_tree::tree_children(&fake, &r).unwrap().is_empty());
}

/// Control: without the tree front, GC doesn't know trees, and a blob only a tree references is collected.
#[test]
fn gc_without_tree_front_drops_children() {
    let d = tempfile::tempdir().unwrap();
    let r = LocalRepo::init(d.path(), &PackConfig::tiny()).unwrap().with_fronts(FrontRegistry::new()).unwrap();
    let leaf = r.put(b"only the tree references me").unwrap();
    let root = o5s_tree::put_tree(&r, &Tree::new(vec![e(b"leaf", Mode::Regular, leaf)]).unwrap()).unwrap();
    let n = Name::new("snap").unwrap();
    assert!(matches!(r.replace(&n, &[(n.clone(), Observed::Absent)], root).unwrap(), ReplaceOutcome::Ok { .. }));
    r.flush().unwrap();
    let opts = GcOptions { grace: Duration::ZERO, allow_short_grace: true, repack_all: true, ..GcOptions::default() };
    for _ in 0..3 {
        r.gc_with_fronts(&opts, None, false).unwrap();
    }
    let r2 = LocalRepo::open(d.path()).unwrap();
    assert!(r2.contains(&root).unwrap());
    assert!(!r2.contains(&leaf).unwrap());
}
