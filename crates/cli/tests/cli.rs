//! End-to-end tests through the real binary, including multi-process contention.
use std::path::Path;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pico2peta"))
}

fn run(repo: &Path, args: &[&str]) -> Output {
    bin().arg("-C").arg(repo).args(args).output().unwrap()
}

fn ok(repo: &Path, args: &[&str]) -> String {
    let o = run(repo, args);
    assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn put_get_replace_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init", "--chunk-threshold", "4096"]);
    let f = d.path().join("data.bin");
    let data: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&f, &data).unwrap();
    let h = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
    assert_eq!(ok(&repo, &["size", &h]).trim(), "100000");
    let out = d.path().join("out.bin");
    ok(&repo, &["get", &h, "-o", out.to_str().unwrap()]);
    assert_eq!(std::fs::read(&out).unwrap(), data);
    let o = run(&repo, &["get", &h, "--range", "10..20"]);
    assert_eq!(o.stdout, &data[10..20]);

    assert_eq!(run(&repo, &["resolve", "main"]).status.code(), Some(2));
    assert_eq!(ok(&repo, &["replace", "main", &h, "--expect", "absent"]).trim(), "ok 1");
    let o = run(&repo, &["replace", "main", &h, "--expect", "absent"]);
    assert_eq!(o.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), "conflict main 1");
    assert!(ok(&repo, &["resolve", "main"]).starts_with(&h));

    ok(&repo, &["replace", "tmp", &h, "--expect", "absent"]);
    assert_eq!(run(&repo, &["delete", "tmp", "--expect", "absent"]).status.code(), Some(3));
    assert_eq!(ok(&repo, &["delete", "tmp", "--expect", "1"]).trim(), "deleted 1");
    assert_eq!(run(&repo, &["resolve", "tmp"]).status.code(), Some(2));
    ok(&repo, &["ns", "set", "pkg", "immutable"]);
    ok(&repo, &["replace", "pkg/a", &h, "--expect", "absent"]);
    assert_eq!(run(&repo, &["replace", "pkg/a", &h, "--expect", "1"]).status.code(), Some(4));
    assert_eq!(run(&repo, &["delete", "pkg/a", "--expect", "1"]).status.code(), Some(4));
    assert!(ok(&repo, &["refs"]).contains("pkg/a"));
    assert!(ok(&repo, &["stat"]).contains("sealed_packs"));
    // XOR delta against an existing object round-trips through the CLI.
    let mut v2 = data.clone();
    v2[500] ^= 1;
    let f2 = d.path().join("v2.bin");
    std::fs::write(&f2, &v2).unwrap();
    let hd = ok(&repo, &["put", "--delta-base", &h, f2.to_str().unwrap()]).trim().to_string();
    let out2 = d.path().join("v2.out");
    ok(&repo, &["get", &hd, "-o", out2.to_str().unwrap()]);
    assert_eq!(std::fs::read(&out2).unwrap(), v2);
    // --delta-from-ref uses the ref's current object as the base.
    let hd2 = ok(&repo, &["put", "--delta-from-ref", "main", f2.to_str().unwrap()]).trim().to_string();
    assert_eq!(hd2, hd, "same content, same identity regardless of storage");
    // Provenance.
    let h2 = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
    let small = d.path().join("small.txt");
    std::fs::write(&small, b"derived").unwrap();
    let h3 = ok(&repo, &["put", small.to_str().unwrap()]).trim().to_string();
    ok(
        &repo,
        &["prov", "record", "derive", "--in", &format!("src={h2}"), "--out", &format!("dst={h3}"), "--meta", "by=test"],
    );
    assert!(ok(&repo, &["prov", "why", &h3]).contains(&h2));
    assert!(ok(&repo, &["prov", "impact", &h2]).contains(&h3));
    // GC: three invocations with zero grace run a full cycle; live data survives.
    assert!(!run(&repo, &["gc", "--grace", "0s"]).status.success(), "short grace needs the unsafe flag");
    for _ in 0..3 {
        ok(&repo, &["gc", "--grace", "0s", "--unsafe-short-grace"]);
    }
    assert_eq!(run(&repo, &["get", &h, "--range", "10..20"]).stdout, &data[10..20]);
    // Bad input never panics.
    for args in [&["get", "zz"][..], &["replace", "a//b", &h, "--expect", "absent"], &["get", &h, "--range", "5"]] {
        let o = run(&repo, args);
        assert!(!o.status.success());
        assert!(!String::from_utf8_lossy(&o.stderr).contains("panicked"), "{args:?}");
    }
}

/// Separate OS processes race on one counter ref with OCC. Every
/// acknowledged success must be reflected in the final version.
#[test]
fn multi_process_contention() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init"]);
    let procs = 4;
    let per = 8;
    let wins: usize = std::thread::scope(|sc| {
        (0..procs)
            .map(|p| {
                let repo = repo.clone();
                sc.spawn(move || {
                    let mut wins = 0;
                    let mut i = 0;
                    while i < per {
                        let mut put = bin()
                            .arg("-C")
                            .arg(&repo)
                            .arg("put")
                            .arg("-")
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .spawn()
                            .unwrap();
                        use std::io::Write;
                        put.stdin.take().unwrap().write_all(format!("proc {p} iter {i}").as_bytes()).unwrap();
                        let o = put.wait_with_output().unwrap();
                        assert!(o.status.success());
                        let h = String::from_utf8(o.stdout).unwrap().trim().to_string();
                        let res = run(&repo, &["resolve", "counter"]);
                        let expect = if res.status.code() == Some(2) {
                            "absent".to_string()
                        } else {
                            String::from_utf8(res.stdout).unwrap().split_whitespace().nth(1).unwrap().to_string()
                        };
                        let o = run(&repo, &["replace", "counter", &h, "--expect", &expect]);
                        match o.status.code() {
                            Some(0) => {
                                wins += 1;
                                i += 1;
                            }
                            Some(3) => {}
                            c => panic!("unexpected exit {c:?}: {}", String::from_utf8_lossy(&o.stderr)),
                        }
                    }
                    wins
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|j| j.join().unwrap())
            .sum()
    });
    assert_eq!(wins, procs * per);
    let r = ok(&repo, &["resolve", "counter"]);
    let mut it = r.split_whitespace();
    let h = it.next().unwrap().to_string();
    assert_eq!(it.next().unwrap(), (procs * per).to_string());
    // The object the final ref points at is visible to a fresh process.
    assert!(ok(&repo, &["get", &h]).starts_with("proc "));
}
