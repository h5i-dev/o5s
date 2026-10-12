//! Push through `git receive-pack --stateless-rpc`, the program a Git HTTP
//! server runs, and check the result with Git itself.
use o5s_gitpush::*;
use std::{
    collections::BTreeMap,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn git(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .arg("--git-dir")
        .arg(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// `git receive-pack OPTIONS DIR`, which takes the repository as an argument.
fn receive(dir: &Path, opts: &[&str], input: &[u8]) -> Vec<u8> {
    let mut args = vec!["receive-pack"];
    args.extend_from_slice(opts);
    let path = dir.to_str().unwrap();
    args.push(path);
    let mut child = Command::new("git")
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "receive-pack: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

fn bare() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--bare", "-b", "main"], b"");
    dir
}

/// What an HTTP server answers to `GET info/refs?service=git-receive-pack`.
fn advertise(dir: &Path) -> Vec<u8> {
    let mut body = b"001f# service=git-receive-pack\n0000".to_vec();
    body.extend(receive(dir, &["--stateless-rpc", "--advertise-refs"], b""));
    body
}

fn push(dir: &Path, files: &BTreeMap<String, File>, message: &str) -> Result<Oid, String> {
    let refs = advertised_refs(&advertise(dir))?;
    let old = refs.get("refs/heads/main").copied();
    let (tree_id, mut objects) = tree(files)?;
    let c = commit(
        &tree_id,
        old.as_ref(),
        "o5s <o5s@localhost>",
        1_791_741_000,
        message,
    );
    let new = c.id();
    objects.push(c);
    let body = push_request(
        "refs/heads/main",
        &old.unwrap_or(ZERO),
        &new,
        &pack(&objects),
    );
    push_result(
        &receive(dir, &["--stateless-rpc"], &body),
        "refs/heads/main",
    )?;
    Ok(new)
}

fn files(entries: &[(&str, &[u8], bool)]) -> BTreeMap<String, File> {
    entries
        .iter()
        .map(|(p, b, x)| {
            let mode = if *x { Mode::Executable } else { Mode::Regular };
            (p.to_string(), (b.to_vec(), mode))
        })
        .collect()
}

#[test]
fn objects_match_git() {
    let dir = bare();
    let blob = Object {
        kind: Kind::Blob,
        data: b"hello\n".to_vec(),
    };
    assert_eq!(
        hex(&blob.id()),
        String::from_utf8(git(dir.path(), &["hash-object", "--stdin"], b"hello\n"))
            .unwrap()
            .trim()
    );
}

#[test]
fn pushes_create_update_and_round_trip_bytes_and_modes() {
    let dir = bare();
    let first = files(&[
        ("README.md", b"hi\n", false),
        ("bin/run.sh", b"#!/bin/sh\n", true),
        ("bin/blob", &[0, 255, 7], false),
        ("a-b/c", b"x", false),
        ("a/b", b"y", false),
        ("empty", b"", false),
    ]);
    let c1 = push(dir.path(), &first, "first\n\nGeneration: 1").unwrap();
    git(dir.path(), &["fsck", "--strict"], b"");
    assert_eq!(
        git(dir.path(), &["show", "main:bin/blob"], b""),
        vec![0, 255, 7]
    );
    let tree = String::from_utf8(git(dir.path(), &["ls-tree", "-r", "main"], b"")).unwrap();
    assert!(
        tree.contains("100755 blob") && tree.contains("bin/run.sh"),
        "{tree}"
    );
    // A fast-forward with a removed file and an edit.
    let mut second = first.clone();
    second.remove("empty");
    second.insert("README.md".into(), (b"hello\n".to_vec(), Mode::Regular));
    let c2 = push(dir.path(), &second, "second").unwrap();
    git(dir.path(), &["fsck", "--strict"], b"");
    assert_eq!(
        git(dir.path(), &["rev-parse", "main^"], b""),
        format!("{}\n", hex(&c1)).into_bytes()
    );
    assert_eq!(
        git(dir.path(), &["rev-parse", "main"], b""),
        format!("{}\n", hex(&c2)).into_bytes()
    );
    assert_eq!(
        git(dir.path(), &["show", "main:README.md"], b""),
        b"hello\n"
    );
}

#[test]
fn symlinks_and_submodules_round_trip() {
    let dir = bare();
    let commit_id = [7u8; 20];
    let mut entries = files(&[("target.txt", b"t", false)]);
    entries.insert("link".into(), (b"target.txt".to_vec(), Mode::Symlink));
    entries.insert("vendor/lib".into(), (commit_id.to_vec(), Mode::Gitlink));
    push(dir.path(), &entries, "special").unwrap();
    let listing = String::from_utf8(git(dir.path(), &["ls-tree", "-r", "main"], b"")).unwrap();
    assert!(
        listing.contains("120000 blob") && listing.contains("\tlink"),
        "{listing}"
    );
    assert!(
        listing.contains(&format!("160000 commit {}\tvendor/lib", hex(&commit_id))),
        "{listing}"
    );
    assert_eq!(
        git(dir.path(), &["cat-file", "blob", "main:link"], b""),
        b"target.txt"
    );
    let mut bad = BTreeMap::new();
    bad.insert("m".into(), (vec![1, 2], Mode::Gitlink));
    assert!(tree(&bad).is_err());
}

#[test]
fn a_stale_old_value_is_refused() {
    let dir = bare();
    let c1 = push(dir.path(), &files(&[("f", b"1", false)]), "one").unwrap();
    push(dir.path(), &files(&[("f", b"2", false)]), "two").unwrap();
    // A writer that still believes main is at c1.
    let (t, mut objects) = tree(&files(&[("f", b"3", false)])).unwrap();
    let c = commit(&t, Some(&c1), "o5s <o5s@localhost>", 0, "stale");
    let new = c.id();
    objects.push(c);
    let body = push_request("refs/heads/main", &c1, &new, &pack(&objects));
    let err = push_result(
        &receive(dir.path(), &["--stateless-rpc"], &body),
        "refs/heads/main",
    )
    .unwrap_err();
    assert!(err.contains("refused"), "{err}");
    assert_eq!(git(dir.path(), &["show", "main:f"], b""), b"2");
}

#[test]
fn invalid_trees_are_refused() {
    assert!(tree(&files(&[("a", b"", false), ("a/b", b"", false)])).is_err());
    assert!(tree(&files(&[("a//b", b"", false)])).is_err());
    assert!(tree(&files(&[("../x", b"", false)])).is_err());
}
