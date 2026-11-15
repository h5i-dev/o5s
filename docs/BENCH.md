# Benchmarks

`bench/micro` (`p2p-bench <scratch-dir> [large-MiB]`) measures a local repository with deterministic inputs:

```sh
source scripts/env.sh
cargo build --release -p pico2peta-micro
$CARGO_TARGET_DIR/release/p2p-bench $SCRATCH/bench-micro 1024
```

Comparisons against Git storage size are in `docs/EXP1.md` (Experiment 1).

## 2026-11-15: provisional, on a loaded host

- **Host:** AMD EPYC 7R32, 4 vCPU, 15 GB RAM, NVMe scratch, Linux 6.12.
- **Load:** the load average was **9.2** during these runs. Experiment 1 was running a 4-thread level-19 compression and the net session was building, so these numbers understate an idle host. They'll be re-run in a quiet window.

| benchmark | run 1 (before fixes) | run 2 (+hard-link publish, manifest cache) | run 3 (+parallel chunk hash/compress) |
|---|---|---|---|
| put 1 KiB objects (incl. flush) | 66k obj/s | 96k obj/s | 85k obj/s |
| get 1 KiB random (warm) | 43k obj/s, p50 10 µs | 80k obj/s, p50 10 µs | 55k obj/s, p50 11 µs |
| put_stream 1 GiB (incompressible) | 59 MB/s | 101 MB/s | 88 MB/s |
| open+read 1 GiB (verified) | 533 MB/s | 534 MB/s | 499 MB/s |
| 4 KiB range read in 1 GiB object | 402 µs | 133 µs | 113 µs |
| re-put identical 1 GiB (dedup) | 160 MB/s | 175 MB/s | 130 MB/s |
| replace, uncontended (durable, fsync) | 6.4k/s | 6.4k/s | 6.5k/s |
| replace, 4 contending handles | 3.1k/s, 1.6% conflicts | 2.8k/s, 12% | 3.0k/s, 0.6% |
| peak RSS (whole run, incl. 1 GiB stream) | 208 MB | 212 MB | 210 MB |

What changed:
- Run 2: LocalDisk publishes sealed packs by **hard link** instead of copying (the 256 MiB per seal is no longer copied), and decoded **manifest nodes are cached**. Range reads got 3× faster.
- Run 3: per-chunk BLAKE3 and zstd run in parallel batches. Run-to-run variance on the loaded host (±30%) dominates, so the effect will be judged on an idle host.

Memory stays bounded: streaming 1 GiB in and out peaks at about 210 MB RSS for the whole benchmark process, and the 100k small objects are held in memory by the benchmark itself.
