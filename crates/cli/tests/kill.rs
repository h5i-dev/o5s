//! Real crash test: SIGKILL `pico2peta` processes at random moments while they
//! stream objects in, move refs, and run GC. Afterwards `fsck` must pass, every
//! acknowledged ref must be readable, and the repository must stay writable.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pico2peta"))
}

fn ok(repo: &Path, args: &[&str]) -> String {
    let o = bin().arg("-C").arg(repo).args(args).output().unwrap();
    assert!(
        o.status.success(),
        "{args:?} failed\n--- stdout:\n{}\n--- stderr:\n{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn sigkill_at_random_points_keeps_repo_consistent() {
    // Deterministic inputs (fixed xorshift seed); kill *timing* varies run to run.
    let d = tempfile::tempdir().unwrap();
    let repo = d.path().join("r");
    if let Ok(keep) = std::env::var("P2P_KILL_KEEP") {
        // Debug aid: keep the repository for inspection.
        let _ = std::fs::remove_dir_all(&keep);
        std::fs::create_dir_all(&keep).unwrap();
        std::mem::forget(d);
        return run_kill(Path::new(&keep).join("r"));
    }
    run_kill(repo)
}

fn run_kill(repo: std::path::PathBuf) {
    ok(&repo, &["init", "--chunk-threshold", "65536", "--pack-cap", "262144"]);
    let mut acked: Vec<(String, String)> = Vec::new(); // (name, hash) whose replace returned ok
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let mut rnd = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    for round in 0..40 {
        // 1. A streaming put of ~3 MiB, killed after a random delay.
        let mut child = bin()
            .arg("-C")
            .arg(&repo)
            .args(["put", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let writer = std::thread::spawn(move || {
            let mut v = vec![0u8; 64 << 10];
            for i in 0..48u32 {
                for (j, b) in v.iter_mut().enumerate() {
                    *b = (j as u32 ^ i.wrapping_mul(2654435761) ^ round) as u8;
                }
                if stdin.write_all(&v).is_err() {
                    return;
                }
            }
        });
        std::thread::sleep(Duration::from_millis(rnd(60)));
        let _ = child.kill();
        let _ = child.wait();
        let _ = writer.join();

        // 2. A small put + replace that completes (so there is acknowledged state).
        let mut put =
            bin().arg("-C").arg(&repo).args(["put", "-"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
        put.stdin.take().unwrap().write_all(format!("round {round}").as_bytes()).unwrap();
        let o = put.wait_with_output().unwrap();
        assert!(o.status.success());
        let h = String::from_utf8(o.stdout).unwrap().trim().to_string();
        let name = format!("r/{}", round % 7);
        let cur = bin().arg("-C").arg(&repo).args(["resolve", &name]).output().unwrap();
        let expect = if cur.status.code() == Some(2) {
            "absent".to_string()
        } else {
            String::from_utf8(cur.stdout).unwrap().split_whitespace().nth(1).unwrap().to_string()
        };
        // 3. The replace itself is sometimes killed too.
        let mut rep = bin()
            .arg("-C")
            .arg(&repo)
            .args(["replace", &name, &h, "--expect", &expect])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        if rnd(3) == 0 {
            std::thread::sleep(Duration::from_millis(rnd(5)));
            let _ = rep.kill();
        }
        let out = rep.wait_with_output().unwrap();
        if out.status.success() {
            acked.retain(|(n, _)| *n != name);
            acked.push((name.clone(), h));
        } else if out.status.code().is_none() {
            // Killed: outcome unknown. Learn the truth from the repo.
            let cur = bin().arg("-C").arg(&repo).args(["resolve", &name]).output().unwrap();
            if cur.status.success() {
                let got = String::from_utf8(cur.stdout).unwrap();
                let got = got.split_whitespace().next().unwrap().to_string();
                acked.retain(|(n, _)| *n != name);
                acked.push((name.clone(), got));
            }
        }
        // 4. Occasionally a GC run (zero grace; nothing else is writing), sometimes killed.
        if round % 5 == 4 {
            let mut gc = bin()
                .arg("-C")
                .arg(&repo)
                .args(["gc", "--grace", "0s", "--unsafe-short-grace"])
                .stdout(Stdio::null())
                .spawn()
                .unwrap();
            if rnd(2) == 0 {
                std::thread::sleep(Duration::from_millis(rnd(20)));
                let _ = gc.kill();
            }
            let _ = gc.wait();
        }
    }
    // Recovery: any process may adopt orphans; fsck must be clean.
    let f = ok(&repo, &["fsck"]);
    assert!(f.trim_end().ends_with("ok"), "{f}");
    for (name, h) in &acked {
        let r = ok(&repo, &["resolve", name]);
        assert!(r.starts_with(h.as_str()), "{name}: expected {h}, got {r}");
        ok(&repo, &["get", h]);
    }
    // Still fully writable, and GC still converges.
    for _ in 0..3 {
        ok(&repo, &["gc", "--grace", "0s", "--unsafe-short-grace"]);
    }
    let f = ok(&repo, &["fsck"]);
    assert!(f.trim_end().ends_with("ok"), "{f}");
}
