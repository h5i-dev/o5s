# Benchmarks

`bench/micro` (`p2p-bench <scratch-dir> [large-MiB]`) measures a local repository with deterministic inputs:

```sh
source scripts/env.sh
cargo build --release -p pico2peta-micro
$CARGO_TARGET_DIR/release/p2p-bench $SCRATCH/bench-micro 1024
```

Comparisons against Git storage size are in `docs/EXP1.md` (Experiment 1).

## 2026-11-15: quiet host (final)

- **Host:** AMD EPYC 7R32, 4 vCPU, 15 GB RAM, NVMe scratch, Linux 6.12. Load average below 1.4; the other session's builds were paused.
- **Runs:** three back-to-back runs at the current code (parallel chunk pipeline, hard-link publish, manifest cache, MIDX and v3 indexes), reported as the median plus the range.

| benchmark | median | range (3 runs) |
|---|---|---|
| put 1 KiB objects (incl. flush; all distinct) | 83.4k obj/s (85 MB/s) | 82.9–83.7k |
| get 1 KiB random (warm) | 106k obj/s, p50 9 µs, p99 18 µs | 105–107k |
| put_stream 1 GiB (incompressible, CDC + BLAKE3 + zstd-3) | 172 MB/s | 165–180 |
| open + read 1 GiB, hash-verified | 927 MB/s | 924–927 |
| 4 KiB range read in a 1 GiB object | 80 µs | 80–80 |
| re-put identical 1 GiB (dedup) | 532 MB/s | 530–534 |
| replace, uncontended (durable: fsync per commit) | 8.1k commits/s | 7.4–8.2k |
| replace, 4 contending handles | 5.3k commits/s (58–60% of attempts conflict and retry; no lost updates) | 5.3–5.4k |
| peak RSS (whole run, incl. 1 GiB stream in and out) | 170 MB | |

The model-weights results (below) are unchanged on the quiet host: they measure sizes, not speed.

## 2026-11-15: provisional, on a loaded host (history)

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

**Correction:** runs 1–3 used a generator that mapped seeds `2k` and `2k+1` to the same stream (`seed | 1`), so half of the "1 KiB objects" were duplicates (dedup hits). Their put/get small-object rates are inflated. It was fixed for run 4.

What changed:
- Run 2: LocalDisk publishes sealed packs by **hard link** instead of copying (the 256 MiB per seal is no longer copied), and decoded **manifest nodes are cached**. Range reads got 3× faster.
- Run 3: per-chunk BLAKE3 and zstd run in parallel batches. Run-to-run variance on the loaded host (±30%) dominates, so the effect will be judged on an idle host.

Memory stays bounded: streaming 1 GiB in and out peaks at about 210 MB RSS for the whole benchmark process, and the 100k small objects are held in memory by the benchmark itself.

### Run 4 (fixed generator, parallel chunk pipeline; load average ~5.3)

| benchmark | result |
|---|---|
| put 1 KiB objects (incl. flush), all distinct | 76k obj/s, 78 MB/s |
| get 1 KiB random (warm) | 93k obj/s, p50 10 µs, p99 23 µs |
| put_stream 1 GiB (incompressible) | 153 MB/s (peak RSS 152 MB) |
| open+read 1 GiB (verified) | 785 MB/s |
| 4 KiB range read in 1 GiB object | 90 µs |
| re-put identical 1 GiB (dedup) | 400 MB/s |
| replace, uncontended (durable, fsync) | 6.2k/s |
| replace, 4 contending handles | 4.6k commits/s, 40% conflict rate (OCC retries) |

## Model-weights versioning (Xet-style workload): `p2p-bench weights <dir> 256`

A synthetic 256 MiB safetensors checkpoint (a real safetensors header) (64 random f16 tensors, so incompressible)
stored in five successive versions. The baseline is "full copies", which is what plain object storage
or Git LFS keeps. Xet itself isn't available offline here, so this measures our chunk dedup against the
no-dedup baseline and identifies where tensor-aware dedup (ZipLLM, Xet) is needed.

| version | change | full copies | pico2peta | ratio |
|---|---|---|---|---|
| 1 | initial | 256 MiB | 257 MiB | 1.00 |
| 2 | 4 of 64 tensors retrained | 512 MiB | 273 MiB | 0.53 |
| 3 | one tensor resized (+1 KiB, shifts every later byte) | 768 MiB | 273 MiB | 0.36 |
| 4 | header/metadata edit only | 1024 MiB | 273 MiB | 0.27 |
| 5 | full fine-tune (pseudo-random noise in the low 4 mantissa bits of every f16) | 1280 MiB | 530 MiB | 0.41 |
| 5′ | same v5, stored with `put_delta` against v4 (XOR delta, API v1.7) | n/a | **+90 MiB** (vs +257 MiB) | n/a |
| 6 | the same tensors re-exported in a different order, with an edited header: offset-aligned `put_delta` | n/a | +256.5 MiB | n/a |
| 6′ | same, tensor-aligned (`put --delta-*`, pico2peta-weights, API v1.8) | n/a | **+0.0 MiB** | n/a |

- Content-defined chunking stores only what changed for partial retrains (v2 adds 16 MiB = 4 tensors)
  and resynchronizes after insertions that shift offsets (v3 adds ~0).
- A full fine-tune (v5) defeats byte-level chunk dedup: it adds a whole copy. That's exactly the
  ZipLLM result that motivates per-kind configuration.
- Stored as an **XOR delta** against the previous version (`put_delta`, or `pico2peta put --delta-base`),
  the same fine-tune costs 90 MiB instead of 257 MiB (−65%). The noise floor is 64 MiB (4 random bits per 16),
  so zstd gets within 1.4× of it. ZipLLM reports −54% on real models. The noise model is synthetic, so real
  checkpoints will differ.
- **Alignment matters:** a byte-offset XOR is useless once tensors move (v6, a whole copy). Aligning each
  tensor with its same-named counterpart makes the identical tensors' XOR all zeros, which deduplicates
  to nothing.
