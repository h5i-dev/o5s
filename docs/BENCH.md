# Benchmarks

`bench/micro` (`p2p-bench <scratch-dir> [large-MiB]`) measures a local repository with deterministic inputs:

```sh
source scripts/env.sh
cargo build --release -p pico2peta-micro
$CARGO_TARGET_DIR/release/p2p-bench $SCRATCH/bench-micro 1024
```

Other modes: `p2p-bench stream <dir> [MiB]` (put_stream only; `P2P_BENCH_NO_PROBE=1`, `P2P_BENCH_ONLY=random|text`),
`p2p-bench weights <dir> [MiB]`, `p2p-bench chunkers <A> <B>` (FastCDC vs Xet chunk counts and dedup, sizes only), and
`p2p-bench bytegroup-gran <safetensors>` (byte-grouping gain per block vs per tensor). Hold `bench.lock` for timing runs.

Comparisons against Git storage size are in `docs/EXP1.md` (Experiment 1).

## 2026-11-15: scale tests (PLAN §6.7), window 1

`p2p-bench scale`, local NVMe, under the bench lock, load 1.9–2.5, 64-byte distinct objects, `flush` every 10⁵.

| test | result | target | |
|---|---|---|---|
| put, N = 10⁶ | 196k obj/s | | |
| put, N = 10⁷ | 188k obj/s overall (201k for the first 10⁶) | ≥ 50 % of the first 10⁶ | met (94 %) |
| cold open, N = 10⁷ | 5.97 s | ≤ 30 s | met |
| warm random get p50 / p99, N = 10⁷ | 4.5 µs / 14.2 µs | p99 ≤ 1 ms | met |
| RSS after open + 10⁵ gets | 144 B/object at 10⁶, 110 B/object at 10⁷ (1.05 GB) | ≤ 64 B/object | **missed** |
| GC full cycle, N = 10⁷, all live | 110.7 s, peak RSS 2.1 GB, every live object kept | ≤ 30 min, ≤ 6 GB | met |
| names, M = 10⁶: replace / resolve p99 / open | 6.8k/s / 5.7 µs / 0.57 s | ≥ 1k/s / ≤ 1 ms / ≤ 10 s | met |
| names, M = 10⁷: replace / resolve p99 / open | 7.5k/s / 7.6 µs / 6.4 s (RSS 2.1 GB, ≈ 208 B per name) | | met |
| provenance load, R = 10⁶ | 0.35 s (recording took 31 s) | ≤ 10 s | met |

- **Bottlenecks found and fixed on the way:**
  - `replace_many` / `update_many` decisions were quadratic in the batch (10⁵ names: 250 s → 2.1 s).
  - The idmap's large-segment lookups decoded a whole chunk per probe (fixed by net with a page cache and a
    merge-join `get_many`).
- **The RSS miss is mostly mapped files.** Pack indexes over 4 MiB, and the multi-pack index, are memory-mapped, and
  `VmRSS` counts their touched pages: ≈ 440 MB of pack indexes plus ≈ 400 MB of MIDX at 10⁷ objects, all
  file-backed and reclaimable. The harness now also reports `RssAnon` (heap) and `RssFile` separately; the next
  window measures the split. As pre-registered, the plain-RSS target counts as missed.
- **Names live in memory:** ≈ 208 B per name, i.e. 2.1 GB at 10⁷ names. That is the ref store's design (the whole
  state in memory, journal on disk) and is recorded as a limit.

## 2026-11-15: performance pass (Phase 3, items a–e)

Short runs under `bench.lock`, on `/dev/shm` unless stated. The host is shared (load 1.3–2.9).

**(a) put_stream on code-like text (328 MB/s): the host is CPU-bound, not the pipeline.**
- *Ceiling for this input.* The benchmark's generated text is hard for zstd. Single-core `zstd -b3 -B64KiB` on
  it does **182–188 MB/s** (zstd-1: 210 MB/s), not the ~570 MB/s of RESEARCH_SPEEDUP §0's code-like sample. With 3
  workers the compression ceiling is therefore about 560 MB/s.
- *Where the time goes.* Per-thread `/proc/<tid>/schedstat` during a put shows each of the three workers
  **running 40–41 %** of its lifetime and **runnable but waiting for a CPU 51–52 %**. The whole-object hasher ran
  9 % and waited 42 %. The workers are almost never blocked on the pipeline (credits or ordering): the 4 vCPUs are
  oversubscribed by our threads plus other load.
- *Consequence.* The only lever is less CPU per byte, and zstd dominates. No change was made. Real source code
  compresses faster than this generator's output, so a realistic input is planned for the final batch.

**(b) Verified read: 870 → ~1,880 MB/s (2.2×), incompressible 512 MiB.**

| change | open + read, verified |
|---|---|
| before (serial: pread, CRC, decompress, chunk BLAKE3, whole BLAKE3, two copies) | 855–924 MB/s |
| read-ahead: up to 3 workers fetch + decompress + verify the next chunks (4 in flight each), in order | 1,777–1,843 MB/s |
| plus the whole-object hash on its own thread; full chunks moved, not copied | **1,866–1,900 MB/s** |

- Reads under 2 MiB stay serial (no thread setup).
- Errors are returned in order, as before.
- A new test covers read-ahead specifically: corruption in the middle of a 12 MB object, ranged reads on either
  side of it, an early drop, and odd-sized reads.

**(c) Small objects (`p2p-bench small <dir> 100000`, 1 KiB distinct objects, put + one flush, warm random gets).**

| 1 KiB objects | NVMe 1 thread | NVMe 4 threads | tmpfs 1 thread | tmpfs 4 threads |
|---|---|---|---|---|
| put, before | 75.1k/s | 64.5k/s | 108–111k/s | 70–73k/s |
| put, after | **128.7k/s** | **95.5k/s** | **136–148k/s** | **111–132k/s** |
| get, before | 97.2k/s | 233k/s | 110k/s | 268–343k/s |
| get, after | **286k/s** | **797k/s** | 232k/s | 537k/s |

Before, 4-thread puts were *slower* than 1-thread puts. The profile showed three causes:
- **zstd under the writer lock.** `put()` took the lock, then compressed. Now compression, record encoding and
  CRC happen before the lock, and only the append is serialized. `lock_contended` fell from 8.3 % to 3.9 %.
- **Hopeless zstd attempts on small incompressible records** (zstd match finder + histograms, ~24 % of samples).
  Records of 256 B–8 KiB now get an order-0 entropy check (≥ 7.5 bits/byte → raw) when the probe is on. It is
  table-driven: a first version with a `log2` per histogram bin cost 32 % of the run and made 1-thread puts
  slower, so it was replaced before commit.
- **Four syscalls per sealed-pack read** (`open`, `fstat`, `pread`, `close`). `LocalDisk` now caches open handles
  to `*.pack` files, which are immutable. A hit costs one `fstat` to check the link count, so a pack deleted by
  another process's GC still reads as `KeyNotFound`. Without that check, `ensure_live` could have trusted a deleted
  pack (net's HF test caught this before commit; a regression test was added).

The remaining small-put cost is BLAKE3 on 1 KiB inputs (single-block path, ~33 %): once for identity and once
for the incremental pack hash.

**(d) Group commit for the ref journal (`p2p-bench commits <dir> 20000`: durable `replace` of distinct names, N threads on one handle; one run each, load ~1.5).**

| threads | before (one fsync per commit) | after (group commit) |
|---|---|---|
| 1 | 7,566/s | 7,916/s |
| 4 | 7,081/s | **12,472/s** (1.76×) |
| 8 | 7,151/s | **14,604/s** (2.04×) |

- **How it works:** concurrent commits of one process share fsyncs. The leader holds the exclusive flock for the
  whole batch and repeats the fsync until it covers every appended record, so durability and visibility are
  unchanged: no ack, and no reader here or in another process sees a record, before an fsync covers it. A single
  thread is unchanged, since it is bound by the fsync.
- **Contention between processes:** separate handles still serialize on the flock, as before.
- **Bug found and fixed before shipping:** the crash-sweep tests hung, and the human spotted the flock wait. The
  fsync-failure path reloaded state via `load_generation`, which takes an exclusive flock on a *new* open file
  description while the batch still held ours. Those tests now run under a 120 s deadline.

**Re-push of stored content (net's report: a second redis push is ~6× slower than the first).** The core paths
are fast on a fresh store with 300k × 1 KiB objects: re-put of already-stored objects runs at 697k/s, and
`ensure_live` at 2.9M/s. The cause is being profiled on the server, with net.

## 2026-11-15: streaming put pipeline (RESEARCH_SPEEDUP §1), before/after

`p2p-bench stream <dir> 512` puts 512 MiB twice (the second put is all dedup hits) and reads it back with
verification, for incompressible and code-like input. Inputs are generated once into files in `/dev/shm` and
streamed from the page cache, so the source costs about a memcpy. "Before" is the previous `put_chunks`
(64-chunk batches with a barrier, serial hashing), built from the parent commit with the same bench source.
"After" is the current code. Both variants were interleaved within each of 5 reps, all under
`flock /mnt/scratch/pico2peta/bench.lock`. Medians, with the range in brackets.

- **Host:** the shared 4-vCPU EPYC 7R32, **not quiet.** Load average was 2.9–5.2 during the run (net's Exp1c steps
  ran between our reps, and a root-owned python process used about one core throughout). CPU-bound numbers are
  therefore conservative and noisy.
- **Disclosure:** the "after" binary was rebuilt once during the run, for unrelated bench modes. Its only change on
  the put path is a codec branch that isn't taken without a byte-grouping hint.

| put_stream 512 MiB | target | before | after | speed-up |
|---|---|---|---|---|
| incompressible | tmpfs (`/dev/shm`) | 331 MB/s [250–332] | **739 MB/s** [678–756] | 2.2× |
| code-like text | tmpfs | 228 MB/s [179–233] | 328 MB/s [296–332] | 1.4× |
| incompressible | NVMe (fsync'd) | 181 MB/s [172–254] | 206 MB/s [205–266] | (device-bound) |
| code-like text | NVMe | 221 MB/s [152–228] | 307 MB/s [257–316] | 1.4× |
| re-put, all dedup hits | tmpfs | 628 MB/s | 902 MB/s | 1.4× |
| open + read, verified | tmpfs | 915 MB/s | 855 MB/s | (unchanged path; noise) |
| peak RSS (whole run) | | 24 MB | 20 MB | |

**Against the §1a target (≥ 800 MB/s incompressible on 4 cores): not met.** The median was 739 MB/s, and the best of
all runs across the A/B series was 791 MB/s, on this loaded host.
- On NVMe, durable writes are capped by the device: `dd` with `conv=fsync` does about 200 MB/s on this volume, so
  no pipeline change can lift the NVMe incompressible row.
- The tmpfs number is the CPU-bound one.

**What changed (and what the profiles showed).** Profiles use `perf record -e cpu-clock:u` (no call graphs:
`perf_event_paranoid=2`).
- *Before:* zstd 29%, BLAKE3 20%, FastCDC 7%, and memmove; the stages never overlapped because of the batch barrier.
- *First pipeline version (incompressible):* the in-order committer was the bottleneck. It was busy about 1.35 s of
  a 2.0 s put, while each worker was about 4% busy. It ran three serial BLAKE3 passes (the whole object, the
  manifest top level, and the pack), plus record encoding and CRC.
- *Fixes:*
  - The whole-object hash runs on its own thread, publishing snapshots at fanout^k chunks, the only points the tree needs.
  - The manifest top level reuses the whole-object hash, so objects under 4096 chunks (256 MiB) are hashed twice, not three times.
  - Workers build the complete record, including its CRC.
  - zstd contexts are reused per thread.
- *Text after:* compression-bound in the workers, while the committer (2.5% of samples) and the hash thread (2%)
  are nearly idle.
  - The three workers were each busy about 55% of the put. The pipeline wasn't limiting; with this host's other
    load, they were runnable but not scheduled.
  - zstd-3 on this generated text runs at about 170 MB/s per core, slower than RESEARCH_SPEEDUP §0's code-like
    sample (570 MB/s).

**Compressibility probe (§1b): no measurable gain here** (ablation `P2P_BENCH_NO_PROBE=1`, 5 reps):

| tmpfs | probe on | probe off |
|---|---|---|
| incompressible | 655 MB/s [598–791] | 608 MB/s [598–775] |
| code-like text | 251 MB/s [217–329] | 263 MB/s [257–347] |

zstd already gives up quickly on random blocks, so skipping it saves little. On compressible data the probe adds a
4 KiB zstd-1 pass per chunk. The differences are within the noise. The probe stays on (default) because it is
deterministic and cheap. `pack.compress_probe = false` turns it off.

**Sealing (§1c):** the open pack keeps an incremental BLAKE3, so seal no longer reads the pack back. Before sealing,
the file is truncated to the hashed length, which covers torn half-records left by failed writes.

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

## Server over HTTP (quiet host; measured by the net session, see NET_PROGRESS.md)

Release build; client and server on the same box over loopback.

| benchmark | result |
|---|---|
| put 4 KiB, 16 concurrent clients | 9.5k ops/s, p99 2.5 ms |
| replace on own names | 4.8k/s |
| contended single-name OCC | 1.2k commits/s, no lost updates |
| streaming upload / download (BLAKE3-verified) | 181 MiB/s up, 1,269 MiB/s down |
