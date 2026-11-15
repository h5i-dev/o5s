//! Chunker throughput: `chunk-bench [MiB per thread]` (release build; run under the bench lock:
//! `flock /mnt/scratch/pico2peta/bench.lock target/release/chunk-bench`).
//! Reports GB/s for 1 thread and 4 threads (each thread chunks its own buffer).

use pico2peta_chunk::{Chunker, FastCdcChunker, XetChunker};
use std::time::Instant;

fn data(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

fn run(mk: fn() -> Box<dyn Chunker>, bufs: &[Vec<u8>], threads: usize) -> (f64, usize) {
    let t = Instant::now();
    let chunks: usize = std::thread::scope(|s| {
        let hs: Vec<_> = bufs[..threads]
            .iter()
            .map(|b| {
                s.spawn(move || {
                    let mut c = mk();
                    let mut out = Vec::new();
                    for piece in b.chunks(1 << 20) {
                        c.update(piece, &mut out);
                    }
                    c.finish(&mut out);
                    out.len()
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap_or(0)).sum()
    });
    let bytes: usize = bufs[..threads].iter().map(|b| b.len()).sum();
    (bytes as f64 / t.elapsed().as_secs_f64() / 1e9, bytes / chunks.max(1))
}

fn main() {
    let mib: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(512);
    let bufs: Vec<Vec<u8>> = (0..4).map(|i| data(mib << 20, i + 1)).collect();
    fn xet() -> Box<dyn Chunker> {
        Box::new(XetChunker::new())
    }
    fn fcdc() -> Box<dyn Chunker> {
        Box::new(FastCdcChunker::pack_default())
    }
    for (name, mk) in [("xet (8/64/128K)", xet as fn() -> Box<dyn Chunker>), ("fastcdc v2020 (16/64/256K)", fcdc)] {
        // Warm-up, then best of 3.
        let _ = run(mk, &bufs, 1);
        let best = |th| (0..3).map(|_| run(mk, &bufs, th)).fold((0.0f64, 0), |a, b| if b.0 > a.0 { b } else { a });
        let (g1, avg1) = best(1);
        let (g4, _) = best(4);
        println!("{name:<28} 1 thread {g1:5.2} GB/s   4 threads {g4:5.2} GB/s   mean chunk {avg1} B");
    }
}
