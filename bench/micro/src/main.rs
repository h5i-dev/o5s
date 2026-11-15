//! `p2p-bench <scratch-dir>`: local-repository benchmarks. Prints a Markdown
//! table. Deterministic inputs (seeded xorshift), so runs are reproducible.
//! Run with `--release`. Record results in docs/BENCH.md with the host description.

use pico2peta_core::{Name, ObjectStore, Observed, RefValue, ReplaceOutcome, Repository};
use pico2peta_repo::{LocalRepo, PackConfig};
use std::io::Read;
use std::time::Instant;

struct Gen(u64);
impl Read for Gen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        for b in buf.iter_mut() {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *b = (self.0 >> 24) as u8;
        }
        Ok(buf.len())
    }
}

fn bytes(seed: u64, n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    let _ = Gen(seed | 1).read(&mut v);
    v
}

fn hwm_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).ok_or("usage: p2p-bench <scratch-dir> [large-MiB]")?;
    let large_mib: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1024);
    let root = std::path::Path::new(&dir).join(format!("bench-{}", std::process::id()));
    let repo = LocalRepo::init(&root, &PackConfig::default())?;
    let mut rows: Vec<(String, String)> = Vec::new();

    // 1. Small objects: 100k × 1 KiB (random, incompressible), then flush.
    let n = 100_000;
    let objs: Vec<Vec<u8>> = (0..n).map(|i| bytes(i as u64 + 1, 1024)).collect();
    let t = Instant::now();
    let hashes: Vec<_> = objs.iter().map(|o| repo.put(o)).collect::<Result<_, _>>()?;
    repo.flush()?;
    let s = t.elapsed().as_secs_f64();
    rows.push((
        "put 1 KiB objects (incl. flush)".into(),
        format!("{:.0} obj/s, {:.1} MB/s", n as f64 / s, n as f64 * 1024.0 / s / 1e6),
    ));

    // 2. Random small reads (warm).
    let t = Instant::now();
    let mut lat = Vec::with_capacity(20_000);
    for i in 0..20_000usize {
        let h = &hashes[(i * 7919) % n];
        let t1 = Instant::now();
        let d = repo.get(h, None)?;
        lat.push(t1.elapsed().as_secs_f64() * 1e6);
        assert_eq!(d.len(), 1024);
    }
    let s = t.elapsed().as_secs_f64();
    lat.sort_by(|a, b| a.total_cmp(b));
    rows.push((
        "get 1 KiB random (warm)".into(),
        format!("{:.0} obj/s, p50 {:.0} µs, p99 {:.0} µs", 20_000.0 / s, lat[10_000], lat[19_800]),
    ));

    // 3. Large streaming put: random data (incompressible, no dedup).
    let total = large_mib << 20;
    let t = Instant::now();
    let big = repo.put_stream(&mut Gen(42).take(total as u64))?;
    repo.flush()?;
    let s = t.elapsed().as_secs_f64();
    rows.push((
        format!("put_stream {large_mib} MiB"),
        format!("{:.0} MB/s (peak RSS so far {:.0} MB)", total as f64 / s / 1e6, hwm_mb()),
    ));

    // 4. Large streaming get with hash verification.
    let t = Instant::now();
    let mut r = repo.open(&big, None)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0usize;
    loop {
        let k = r.read(&mut buf)?;
        if k == 0 {
            break;
        }
        got += k;
    }
    let s = t.elapsed().as_secs_f64();
    assert_eq!(got, total);
    rows.push((format!("open+read {large_mib} MiB (verified)"), format!("{:.0} MB/s", total as f64 / s / 1e6)));

    // 5. Ranged read of 4 KiB from the large object.
    let t = Instant::now();
    for i in 0..2000u64 {
        let off = (i * 104_729 * 4096) % (total as u64 - 4096);
        repo.get(&big, Some(off..off + 4096))?;
    }
    rows.push((
        "4 KiB range read in large object".into(),
        format!("{:.0} µs avg", t.elapsed().as_secs_f64() / 2000.0 * 1e6),
    ));

    // 6. Dedup: re-put the same large stream.
    let t = Instant::now();
    repo.put_stream(&mut Gen(42).take(total as u64))?;
    let s = t.elapsed().as_secs_f64();
    rows.push((format!("re-put identical {large_mib} MiB (dedup)"), format!("{:.0} MB/s", total as f64 / s / 1e6)));

    // 7. Uncontended replace (each durable: fsync).
    let name = Name::new("bench/counter")?;
    let t = Instant::now();
    let k = 2000;
    for i in 0..k {
        let obs = RefValue::observed(repo.resolve(&name)?);
        match repo.replace(&name, &[(name.clone(), obs)], hashes[i % n])? {
            ReplaceOutcome::Ok { .. } => {}
            ReplaceOutcome::Conflict { .. } => return Err("unexpected conflict".into()),
        }
    }
    let s = t.elapsed().as_secs_f64();
    rows.push(("replace, uncontended (durable)".into(), format!("{:.0} commits/s", k as f64 / s)));

    // 8. Contended replace: 4 handles on one ref.
    let t = Instant::now();
    let per = 300;
    let (wins, conflicts) = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4)
            .map(|th| {
                let root = root.clone();
                let name = name.clone();
                let hs = &hashes;
                sc.spawn(move || -> (u64, u64) {
                    let Ok(r) = LocalRepo::open(&root) else { return (0, 0) };
                    let (mut w, mut c) = (0, 0);
                    while w < per {
                        let Ok(v) = r.resolve(&name) else { return (w, c) };
                        match r.replace(
                            &name,
                            &[(name.clone(), RefValue::observed(v))],
                            hs[(th * 1000 + w as usize) % n],
                        ) {
                            Ok(ReplaceOutcome::Ok { .. }) => w += 1,
                            Ok(ReplaceOutcome::Conflict { .. }) => c += 1,
                            Err(_) => return (w, c),
                        }
                    }
                    (w, c)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or((0, 0))).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
    });
    let s = t.elapsed().as_secs_f64();
    rows.push((
        "replace, 4 contending handles".into(),
        format!(
            "{:.0} commits/s, {:.1}% conflict rate",
            wins as f64 / s,
            100.0 * conflicts as f64 / (wins + conflicts) as f64
        ),
    ));
    let _ = Observed::Absent;

    println!("| benchmark | result |\n|---|---|");
    for (a, b) in rows {
        println!("| {a} | {b} |");
    }
    println!("\npeak RSS: {:.0} MB", hwm_mb());
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
