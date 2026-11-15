//! Opt-in load benchmark (release build recommended):
//!   cargo test --release -p pico2peta-server --test load -- --ignored --nocapture
//! Reports throughput and latency percentiles for the native API over loopback.

mod common;
use common::*;
use std::time::{Duration, Instant};

fn pct(v: &mut [Duration], p: f64) -> f64 {
    v.sort();
    v[((v.len() as f64 - 1.0) * p) as usize].as_secs_f64() * 1e3
}

fn report(name: &str, mut lat: Vec<Duration>, wall: Duration) {
    let n = lat.len();
    eprintln!(
        "LOAD {name:<28} ops={n:<6} {:>8.0} ops/s  p50={:.2}ms p99={:.2}ms",
        n as f64 / wall.as_secs_f64(),
        pct(&mut lat, 0.50),
        pct(&mut lat, 0.99)
    );
}

#[test]
#[ignore]
fn load() {
    let s = TestServer::start(|c| c.server.max_concurrent_requests = 1024);
    let threads = 16;
    let per = 300;

    // 1. Small puts (4 KiB, distinct).
    let t = Instant::now();
    let lat: Vec<Duration> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|i| {
                let c = s.client(ALICE);
                sc.spawn(move || {
                    (0..per)
                        .map(|j| {
                            let d = data(4096, (i * 100_000 + j) as u64);
                            let t0 = Instant::now();
                            c.put(&d).unwrap();
                            t0.elapsed()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    report("put 4KiB x16 threads", lat, t.elapsed());

    // 2. Uncontended replace (each thread its own name).
    let h = s.client(ALICE).put(b"target").unwrap();
    let t = Instant::now();
    let lat: Vec<Duration> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|i| {
                let c = s.client(ALICE);
                sc.spawn(move || {
                    let name = n(&format!("alice/load/{i}"));
                    let mut obs = Observed::Absent;
                    (0..per)
                        .map(|_| {
                            let t0 = Instant::now();
                            match c.replace(&name, &[(name.clone(), obs)], h).unwrap() {
                                ReplaceOutcome::Ok { version } => obs = Observed::Version(version),
                                o => panic!("{o:?}"),
                            }
                            t0.elapsed()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    report("replace own name x16", lat, t.elapsed());

    // 3. Contended replace: 16 threads incrementing one counter name via OCC retry loops.
    let hot = n("alice/hot");
    let t = Instant::now();
    let (lat, conflicts): (Vec<Duration>, usize) = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let c = s.client(ALICE);
                let hot = hot.clone();
                sc.spawn(move || {
                    let mut conflicts = 0;
                    let lat: Vec<Duration> = (0..per / 5)
                        .map(|_| {
                            let t0 = Instant::now();
                            loop {
                                let obs = pico2peta_core::RefValue::observed(c.resolve(&hot).unwrap());
                                match c.replace(&hot, &[(hot.clone(), obs)], h).unwrap() {
                                    ReplaceOutcome::Ok { .. } => break,
                                    ReplaceOutcome::Conflict { .. } => conflicts += 1,
                                }
                            }
                            t0.elapsed()
                        })
                        .collect();
                    (lat, conflicts)
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).fold((Vec::new(), 0), |mut a, (l, c)| {
            a.0.extend(l);
            a.1 += c;
            a
        })
    });
    let ok = lat.len();
    report("contended replace (1 name)", lat, t.elapsed());
    eprintln!("LOAD   committed={ok} conflicts={conflicts} (every commit is a distinct version)");
    let v = s.client(ALICE).resolve(&hot).unwrap().unwrap().version;
    assert_eq!(v as usize, ok, "no lost updates");

    // 4. Streaming throughput: 4 x 256 MiB up (distinct data), then down.
    let len = 256u64 << 20;
    let t = Instant::now();
    let hashes: Vec<Hash> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..4u64)
            .map(|i| {
                let c = s.client(ALICE);
                sc.spawn(move || c.put_stream(&mut Gen { left: len, x: 0x9E37_79B9_7F4A_7C15 ^ (i + 1) }).unwrap())
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let up = t.elapsed();
    let t = Instant::now();
    std::thread::scope(|sc| {
        for h in &hashes {
            let c = s.client(ALICE);
            sc.spawn(move || {
                let mut r = c.open(h, None).unwrap();
                std::io::copy(&mut r, &mut std::io::sink()).unwrap();
            });
        }
    });
    let down = t.elapsed();
    let mib = (4 * len) as f64 / (1 << 20) as f64;
    eprintln!(
        "LOAD stream 4x256MiB          up {:.0} MiB/s  down {:.0} MiB/s (BLAKE3-verified)",
        mib / up.as_secs_f64(),
        mib / down.as_secs_f64()
    );
}

/// `left` pseudo-random bytes, generated on the fly.
struct Gen {
    left: u64,
    x: u64,
}

impl std::io::Read for Gen {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.left) as usize;
        for b in &mut buf[..n] {
            self.x ^= self.x << 13;
            self.x ^= self.x >> 7;
            self.x ^= self.x << 17;
            *b = (self.x >> 24) as u8;
        }
        self.left -= n as u64;
        Ok(n)
    }
}
