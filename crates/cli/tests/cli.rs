//! End-to-end tests through the real binary, including multi-process contention.
use std::path::Path;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_o5s"))
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
    // A derived object (`--encoding derive-from=HASH`, both patch formats) round-trips.
    let mut v2 = data.clone();
    v2[500] ^= 1;
    let f2 = d.path().join("v2.bin");
    std::fs::write(&f2, &v2).unwrap();
    let hd = ok(&repo, &["put", "--encoding", &format!("derive-from={h}"), f2.to_str().unwrap()]).trim().to_string();
    let out2 = d.path().join("v2.out");
    ok(&repo, &["get", &hd, "-o", out2.to_str().unwrap()]);
    assert_eq!(std::fs::read(&out2).unwrap(), v2);
    let hd2 =
        ok(&repo, &["put", "--encoding", &format!("derive-from={h}"), "--encoding", "patch=xor", f2.to_str().unwrap()])
            .trim()
            .to_string();
    assert_eq!(hd2, hd, "same content, same identity regardless of storage");
    assert!(!run(&repo, &["put", "--encoding", "colour=blue", f2.to_str().unwrap()]).status.success());
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

#[test]
fn multi_process_contention_with_refs_on_backend() {
    contention(&["init", "--refs-on-backend"]);
}

#[test]
fn multi_process_contention() {
    contention(&["init"]);
}

/// Separate OS processes race on one counter ref with OCC. Every
/// acknowledged success must be reflected in the final version.
fn contention(init: &[&str]) {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, init);
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

/// `init --chunk PREFIX=CHUNKER` + `put --for-name`: the config is written,
/// objects round-trip, and bad specs are rejected at init.
#[test]
fn per_namespace_chunker_via_cli() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init", "--chunk", "models/=xet", "--chunk", "models/old/=fastcdc"]);
    let cfg = std::fs::read_to_string(repo.join("config.toml")).unwrap();
    assert!(cfg.contains("[chunking.prefixes]") && cfg.contains("\"models/\" = \"xet\""), "{cfg}");
    let f = d.path().join("w.bin");
    let mut x = 88172645463325252u64;
    let data: Vec<u8> = (0..3_000_000)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect();
    std::fs::write(&f, &data).unwrap();
    let h1 = ok(&repo, &["put", "--for-name", "models/llama/main", f.to_str().unwrap()]).trim().to_string();
    let h2 = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
    assert_eq!(h1, h2, "the chunker never changes object identity");
    let out = d.path().join("out.bin");
    ok(&repo, &["get", &h1, "-o", out.to_str().unwrap()]);
    assert_eq!(std::fs::read(&out).unwrap(), data);
    ok(&repo, &["fsck"]);
    // Bad specs fail at init and create nothing.
    for bad in [
        &["init", "--chunk", "models/=rabin"][..],
        &["init", "--chunk", "models"],
        &["init", "--chunk", "m/=xet", "--chunk-threshold", "65536"],
    ] {
        let r2 = d.path().join(format!("bad{}", bad.len() + bad[2].len()));
        assert!(!run(&r2, bad).status.success(), "{bad:?}");
        assert!(!r2.join("config.toml").exists(), "{bad:?}");
    }
}

/// Profiles through the CLI: `init --profile/--namespace`, then `put
/// --for-name` uses the profile's front (safetensors: derive from the
/// previous version). Bad profile specs fail at init.
#[test]
fn profiles_via_cli() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init", "--profile", "weights=front:safetensors,derive:previous", "--namespace", "models/=weights"]);
    let cfg = std::fs::read_to_string(repo.join("config.toml")).unwrap();
    assert!(cfg.contains("[profiles.weights]") && cfg.contains("front = \"safetensors\""), "{cfg}");
    let f = d.path().join("v.bin");
    let mut data: Vec<u8> = (0..2_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    for round in 0..2 {
        if round == 1 {
            for i in (0..data.len()).step_by(1000) {
                data[i] ^= 0x55;
            }
        }
        std::fs::write(&f, &data).unwrap();
        let h = ok(&repo, &["put", "--for-name", "models/m", f.to_str().unwrap()]).trim().to_string();
        let expect = if round == 0 { "absent".to_string() } else { "1".to_string() };
        ok(&repo, &["replace", "models/m", &h, "--expect", &expect]);
        let out = d.path().join("o.bin");
        ok(&repo, &["get", &h, "-o", out.to_str().unwrap()]);
        assert_eq!(std::fs::read(&out).unwrap(), data);
    }
    ok(&repo, &["fsck"]);
    for (i, bad) in [
        &["init", "--profile", "w=front:nope"][..],
        &["init", "--profile", "w=derive:sometimes"],
        &["init", "--profile", "w=colour:blue"],
        &["init", "--profile", "noequals"],
        &["init", "--namespace", "m/=missing"],
    ]
    .iter()
    .enumerate()
    {
        let r2 = d.path().join(format!("bad{i}"));
        assert!(!run(&r2, bad).status.success(), "{bad:?}");
        assert!(!r2.join("config.toml").exists(), "{bad:?}");
    }
}

/// `get --read-verify`: eof is refused on a strict repository and works when
/// `init --allow-eof-reads` enabled it; bad values are errors.
#[test]
fn read_verify_via_cli() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("d.bin");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
    std::fs::write(&f, &data).unwrap();
    for (dir, init, eof_ok) in [("s", &["init"][..], false), ("a", &["init", "--allow-eof-reads"][..], true)] {
        let repo = d.path().join(dir);
        ok(&repo, init);
        let h = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
        let out = d.path().join(format!("{dir}.out"));
        let r = run(&repo, &["get", &h, "--read-verify", "eof", "-o", out.to_str().unwrap()]);
        assert_eq!(r.status.success(), eof_ok, "{dir}: {}", String::from_utf8_lossy(&r.stderr));
        if eof_ok {
            assert_eq!(std::fs::read(&out).unwrap(), data);
        }
        ok(&repo, &["get", &h, "--read-verify", "strict", "-o", out.to_str().unwrap()]);
        assert!(!run(&repo, &["get", &h, "--read-verify", "fast"]).status.success());
    }
    assert!(!run(&d.path().join("x"), &["init", "--read-verify", "fast"]).status.success());
}

/// search / lineage / retention through the CLI.
#[test]
fn search_and_retention_via_cli() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init", "--profile", "pkgs=keep_versions:1", "--namespace", "pypi/=pkgs"]);
    for v in ["1.0", "1.1"] {
        let f = d.path().join(format!("w{v}"));
        std::fs::write(&f, format!("wheel {v}")).unwrap();
        let h = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
        ok(&repo, &["replace", &format!("pypi/torchlite/{v}"), &h, "--expect", "absent"]);
    }
    let out = ok(&repo, &["search", "--prefix", "pypi"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    let out = ok(&repo, &["search", "torchlite"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    assert!(ok(&repo, &["prov", "lineage", "pypi/torchlite/1.1"]).trim().is_empty(), "no provenance yet");
    let dry = ok(&repo, &["retention", "--dry-run"]);
    assert_eq!(dry.trim(), "would-delete pypi/torchlite/1.0");
    assert!(run(&repo, &["resolve", "pypi/torchlite/1.0"]).status.success());
    assert_eq!(ok(&repo, &["retention"]).trim(), "deleted pypi/torchlite/1.0");
    assert_eq!(run(&repo, &["resolve", "pypi/torchlite/1.0"]).status.code(), Some(2));
    assert!(!run(&d.path().join("x"), &["init", "--profile", "p=keep_versions:many"]).status.success());
}

/// The cleaned-up CLI: `prov` takes a HASH or a NAME; attestations (keygen,
/// create, list, verify); policy-gated promotion; multi-name `update`.
#[test]
fn prov_attest_promote_and_update_via_cli() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init"]);
    let f = d.path().join("w.whl");
    std::fs::write(&f, b"wheel v1").unwrap();
    let src = d.path().join("src");
    std::fs::write(&src, b"source tree").unwrap();
    let hw = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
    let hs = ok(&repo, &["put", src.to_str().unwrap()]).trim().to_string();
    // update: two names in one transaction; a stale read set conflicts (exit 3).
    assert_eq!(
        ok(
            &repo,
            &[
                "update",
                &format!("dev/app={hw}"),
                &format!("git/main={hs}"),
                "--expect",
                "dev/app=absent",
                "--expect",
                "git/main=absent"
            ]
        )
        .trim(),
        "ok 1,1"
    );
    assert_eq!(run(&repo, &["update", "dev/app=-", "--expect", "dev/app=absent"]).status.code(), Some(3));
    // prov: record, then query by NAME and by HASH.
    ok(&repo, &["prov", "record", "build", "--in", &format!("source={hs}"), "--out", &format!("wheel={hw}")]);
    assert!(ok(&repo, &["prov", "lineage", "dev/app"]).contains("git/main"));
    assert!(ok(&repo, &["prov", "impact", "git/main"]).contains("dev/app"));
    assert!(ok(&repo, &["prov", "why", &hw]).contains(&hs));
    // attest: keygen, create, list, verify (a wrong key verifies nothing).
    let key = d.path().join("ci.key");
    let kg = ok(&repo, &["attest", "keygen", key.to_str().unwrap()]);
    let pubhex = kg.split_whitespace().nth(1).unwrap().to_string();
    let other = d.path().join("other.key");
    let other_pub =
        ok(&repo, &["attest", "keygen", other.to_str().unwrap()]).split_whitespace().nth(1).unwrap().to_string();
    assert!(!run(&repo, &["attest", "keygen", key.to_str().unwrap()]).status.success(), "never overwrites a key");
    ok(&repo, &["attest", "create", "dev/app", "--key", key.to_str().unwrap()]);
    assert!(ok(&repo, &["attest", "list", "dev/app"]).contains("ed25519:"));
    assert!(ok(&repo, &["attest", "verify", "dev/app", "--trust", &pubhex]).contains("ed25519:"));
    assert!(!run(&repo, &["attest", "verify", "dev/app", "--trust", &other_pub]).status.success());
    // promote: refused without the required signer, promoted with it.
    let refused = run(&repo, &["promote", "dev/app", "prod/app", "--require-key", &other_pub]);
    assert_eq!(refused.status.code(), Some(4), "{}", String::from_utf8_lossy(&refused.stdout));
    assert!(String::from_utf8_lossy(&refused.stdout).contains("refused"));
    let p = ok(
        &repo,
        &["promote", "dev/app", "prod/app", "--require-key", &pubhex, "--require-provenance", "--actor", "alice"],
    );
    assert!(p.starts_with(&format!("promoted {hw}")), "{p}");
    assert!(ok(&repo, &["resolve", "prod/app"]).starts_with(&hw));
}

#[test]
fn put_many_files_shares_one_pack() {
    use std::io::Write as _;
    use std::process::Stdio;
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init"]);
    let src = d.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let mut paths = Vec::new();
    for i in 0..50 {
        // Odd names (space, newline-free) and one duplicate content.
        let p = src.join(format!("f {i}.txt"));
        std::fs::write(&p, format!("file {}\n", i % 49)).unwrap();
        paths.push(p.to_str().unwrap().to_string());
    }
    let args: Vec<&str> = std::iter::once("put").chain(paths.iter().map(|s| s.as_str())).collect();
    let out = ok(&repo, &args);
    let hashes: Vec<&str> = out.lines().collect();
    assert_eq!(hashes.len(), 50);
    assert_eq!(hashes[0], hashes[49]);
    for (p, h) in paths.iter().zip(&hashes) {
        assert_eq!(ok(&repo, &["get", h]).as_bytes(), std::fs::read(p).unwrap());
    }
    let packs = || {
        std::fs::read_dir(repo.join("objects/packs"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().unwrap() == "pack")
            .count()
    };
    assert_eq!(packs(), 1, "one process, one sealed pack");

    // --batch: same hashes in the same order, NUL- and newline-separated, after FILE args.
    for (sep, flag) in [("\n", None), ("\0", Some("-z"))] {
        let mut cmd = bin();
        cmd.arg("-C").arg(&repo).args(["put", &paths[0], "--batch"]);
        if let Some(f) = flag {
            cmd.arg(f);
        }
        let mut c = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        c.stdin.take().unwrap().write_all((paths[1..].join(sep) + sep).as_bytes()).unwrap();
        let o = c.wait_with_output().unwrap();
        assert!(o.status.success());
        assert_eq!(String::from_utf8(o.stdout).unwrap(), out);
    }
    assert_eq!(packs(), 1, "nothing new to seal");

    // A missing file fails the whole command, naming the file, and prints no hash.
    let o = run(&repo, &["put", &paths[0], "/nonexistent/x"]);
    assert!(!o.status.success());
    assert!(o.stdout.is_empty());
    assert!(String::from_utf8_lossy(&o.stderr).contains("/nonexistent/x"));
    assert!(!run(&repo, &["put", "--batch", "-"]).status.success());
    assert!(!run(&repo, &["put", "-", "-"]).status.success());
}

/// `o5s refs f/` lists what `o5s refs f` lists (a trailing `/` names the same prefix).
#[test]
fn refs_prefix_with_a_trailing_slash() {
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    ok(&repo, &["init"]);
    let f = d.path().join("x");
    std::fs::write(&f, b"x").unwrap();
    let h = ok(&repo, &["put", f.to_str().unwrap()]).trim().to_string();
    for name in ["f/a", "f/b", "foo/c"] {
        ok(&repo, &["replace", name, &h, "--expect", "absent"]);
    }
    let plain = ok(&repo, &["refs", "f"]);
    assert_eq!(plain.lines().count(), 2, "{plain}");
    assert_eq!(ok(&repo, &["refs", "f/"]), plain);
}
