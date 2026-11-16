//! Never-panic property tests for every decoder that sees untrusted input.
//! The same entry points are cargo-fuzz targets in fuzz/ (nightly).

use o5s_server::fuzzing as f;
use proptest::prelude::*;

fn bytes(max: usize) -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..max)
}

fn mutate(mut v: Vec<u8>, edits: &[(usize, u8)]) -> Vec<u8> {
    for (i, b) in edits {
        if !v.is_empty() {
            let i = i % v.len();
            v[i] = *b;
        }
    }
    v
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, ..ProptestConfig::default() })]

    #[test]
    fn git_parsers_total(d in bytes(400)) { f::git_parsers(&d); }

    #[test]
    fn git_delta_roundtrip(base in bytes(600), edits in proptest::collection::vec((any::<usize>(), any::<u8>(), 0u8..3), 0..20)) {
        // Target = base with random substitutions, insertions and deletions.
        let mut t = base.clone();
        for (i, b, op) in edits {
            let i = if t.is_empty() { 0 } else { i % (t.len() + 1) };
            match op {
                0 if i < t.len() => t[i] = b,
                1 => t.insert(i, b),
                _ if i < t.len() => { t.remove(i); }
                _ => {}
            }
        }
        f::git_delta_roundtrip(&base, &t);
        f::git_delta_roundtrip(&t, &base);
    }

    #[test]
    fn oci_parsers_total(d in bytes(400)) { f::oci_parsers(&d); }

    #[test]
    fn segmap_total(d in bytes(300)) { f::segmap(&d); }

    #[test]
    fn range_total(a in any::<u64>(), b in any::<u64>(), size in any::<u64>(), form in 0u8..6) {
        let h = match form {
            0 => format!("bytes={a}-{b}"),
            1 => format!("bytes={a}-"),
            2 => format!("bytes=-{b}"),
            3 => format!("bytes={a}-{b},{b}-{a}"),
            4 => format!("bytes= {a} - {b} "),
            _ => format!("bytes={a}"),
        };
        f::range(&h, size % 1_000_000);
    }

    #[test]
    fn config_total(d in "[\\[\\]a-z_=\"0-9. \\n]{0,200}") { f::config(d.as_bytes()); }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn audit_recovery_total(d in bytes(300), valid_prefix in any::<bool>()) {
        let mut data = Vec::new();
        if valid_prefix {
            data.extend_from_slice(b"{\"seq\":1,\"ts\":\"t\",\"prev\":\"0000000000000000000000000000000000000000000000000000000000000000\",\"request_id\":\"r\",\"principal\":\"p\",\"action\":\"a\",\"target\":\"t\",\"outcome\":\"ok\"}\n");
        }
        data.extend_from_slice(&d);
        f::audit(&data);
    }

    /// Random bytes and mutated valid pushes through the full receive-pack path.
    #[test]
    fn git_push_total(
        objs in proptest::collection::vec((1u8..=4, bytes(60)), 1..5),
        edits in proptest::collection::vec((any::<usize>(), any::<u8>()), 0..4),
        raw in bytes(200),
        use_raw in any::<bool>(),
    ) {
        let (dir, repo) = f::temp_repo();
        let req = if use_raw { raw } else { mutate(f::git_push_request(&objs, 0), &edits) };
        f::git_push(repo, dir.path(), &req);
    }
}

#[test]
fn valid_blob_push_is_accepted() {
    let (dir, repo) = f::temp_repo();
    // A lone blob cannot be a branch tip, so this exercises the "ng" path.
    f::git_push(repo.clone(), dir.path(), &f::git_push_request(&[(3, b"hi".to_vec())], 0));
    // A tree + commit is a valid branch.
    let tree = Vec::new(); // empty tree
    let tree_oid = {
        use sha1::Digest;
        let c = [b"tree 0\0".as_slice()].concat();
        let o: [u8; 20] = sha1::Sha1::digest(&c).into();
        o.iter().map(|x| format!("{x:02x}")).collect::<String>()
    };
    let commit = format!("tree {tree_oid}\nauthor a <a> 0 +0000\ncommitter a <a> 0 +0000\n\nm\n").into_bytes();
    f::git_push(repo.clone(), dir.path(), &f::git_push_request(&[(2, tree), (1, commit)], 1));
    use o5s_core::Repository;
    let n = o5s_core::Name::new("git/fuzz/repo/refs/heads/main").unwrap();
    assert!(repo.resolve(&n).unwrap().is_some(), "valid push must set the ref");
}
