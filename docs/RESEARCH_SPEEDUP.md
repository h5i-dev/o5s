# Speed-up research (2026-11-15, from the human's side)

## 0. Measurement first: the chunker is NOT the bottleneck

Single-core stage microbenchmark on this host (EPYC 7R32 Zen2, AVX2, no AVX-512). The harness is `/mnt/scratch/pico2peta/tmp/stagebench`.

| stage | random data | code-like data |
|---|---|---|
| FastCDC v2020 16/64/256K | 2.31 GB/s | 1.96 GB/s |
| BLAKE3 per chunk | 2.61 GB/s | 3.04 GB/s |
| zstd-1 per chunk | 3.41 GB/s | 0.84 GB/s (ratio 0.090) |
| zstd-3 per chunk | 2.24 GB/s | 0.57 GB/s (ratio 0.085) |
| zstd-1 on a 4 KiB sample (compressibility probe) | 12.2 GB/s, flags 100% as incompressible | 11.4 GB/s, flags 0% |

End-to-end `put_stream` does **172 MB/s** (BENCH.md), more than 10× slower than any single stage. The cause is the pipeline structure in `crates/pack/src/store.rs` `put_chunks`:
- **Bulk-synchronous batches.** Each 64-chunk batch runs read+chunk (serial) → scoped-thread hash+compress → serial `add_chunk`/append. The stages never overlap.
- **Serial per-byte work on the critical path:** CDC, the whole-object BLAKE3, the manifest-level hashers (the same bytes are hashed 2–3× serially), CRC, and `write_all_at`.
- **Wasted compression.** zstd runs to completion on incompressible chunks, and the output is then discarded.

## 1. Do now (no format change)

**1a. Streaming pipeline.**
- Chunker thread → bounded channel → N hash/compress workers → in-order committer.
- The whole-object hasher runs in its own thread, fed the same chunk buffers (Arc, no copies).
- Memory stays bounded by the channel depth.
- Re-measure `put_stream` 1 GiB on both incompressible and compressible inputs. The target is ≥ 800 MB/s incompressible on 4 cores.
- Profile with a sampling profiler (perf isn't installed; install it on scratch, or use `cargo flamegraph`) before and after.

**1b. Compressibility probe.**
- Run zstd-1 on the first 4 KiB of each chunk, or use an entropy estimate. If it saves < ~3%, store the chunk raw and skip full zstd.
- Make it configurable and deterministic: it affects only the codec, never identity.

**1c.** Check `maybe_seal`/fsync costs on the 1 GiB path. Batch small-object appends where possible.

## 2. Model weights: byte grouping (fills the "dtype-aware transforms" production gap)

- ZipNN groups bf16/fp32 bytes by position (exponent vs mantissa streams) before compression.
  - bf16 Llama-3.1 compresses to 66.4% of its size, vs 77.7% with plain zstd.
  - Xet ships the same idea as the `ByteGrouping4LZ4` codec.
- Apply it after the existing XOR delta. That's ZipLLM's BitX pipeline: XOR → zstd.
- Implement it as a record codec (a new codec id; the format stays versioned).
- Measure with the existing weights bench, plus a real bf16 safetensors file if one is reachable.

## 3. Experiment 1c (storage size): pre-register thresholds BEFORE running

Exp1b used only "previous version of same path" as the delta base. Git tries a window of 250 candidates.

**Candidate:**
- Pick the base by resemblance detection: Odess content-defined sampling super-features (ACM TOS 2023), or Palantir/Sylph.
- Encode with zstd `--patch-from` (the dictionary = base). The zstd wiki reports `--patch-from` produces patches about the same size as bsdiff while being 100–200× faster.

**Baseline update:**
- Git now has `pack-objects --name-hash-version=2` and `--path-walk`. One cover letter reports 440 MB → 161 MB for one repo (anecdotal).
- Report Exp1c against BOTH the original `gc --aggressive` baseline (the frozen thresholds) and `gc --aggressive --path-walk` / name-hash v2, for honesty.
- Also consider borrowing name-hash-v2 / path-walk ordering for our grouping key.

**Optional:** OpenZL (Meta, 2025, format-aware graph compression with a universal decoder) for structured objects such as trees and commits.

## 4. Chunking algorithm: later, decide on Xet compatibility

- **Xet chunker spec** (huggingface.co/docs/xet/chunking):
  - Gear hash, target 64 KiB, min 8 KiB, max 128 KiB.
  - `MASK = 0xFFFF000000000000`.
  - Min-size skip-ahead (the hash depends only on the last 64 bytes).
  - Reference chunk lists are published for conformance testing.
  - Adopting it byte-exactly gives boundary compatibility with Hugging Face Xet: dedup against HF-hosted models, and a possible Xet protocol adapter later. Our current FastCDC 16/64/256K is not boundary-compatible.
- **SeqCDC** (TPDS 2026): hashless, boundaries at monotonic byte runs, plus content-defined skipping.
  - 1.5× FastCDC at 8 KB and 2.8× at 16 KB; dedup is comparable.
  - The vector version reaches ~10× FastCDC, but with AVX-512, which this host lacks.
- **VectorCDC** (ACM TOS 2026): SIMD for RAM/AE/MAXP, 5.5–17.6×. RAM's dedup is the weakest.
- **CDC benchmark** (arXiv 2610.13704, ICDE 2027): AE is the most balanced algorithm. The commonly used AE parameterization is wrong: chunks come out 13–39% smaller than intended, which inflates dedup. Use the paper's corrected formulas if AE is adopted.
- Only worth doing after §1. Chunking at 2+ GB/s/core is not the limit today.
- If changing chunkers, make it a per-namespace config (it changes chunk identity, not object identity) and keep FastCDC readable.

## Sources

**Chunking**
- SeqCDC: https://arxiv.org/html/2505.21194v1 , https://cs.uwaterloo.ca/~alkiswan/papers/Vectorized_SeqCDC_TPDS_2026.pdf
- VectorCDC: https://arxiv.org/html/2508.05797v1 , https://dl.acm.org/doi/10.1145/3797270
- CDC benchmark (ICDE 2027): https://arxiv.org/abs/2610.13704
- DedupBench: https://wasl.uwaterloo.ca/projects/deduplication/

**Xet**
- Chunking spec: https://huggingface.co/docs/xet/chunking
- Xorb spec: https://huggingface.co/docs/xet/xorb

**Model weights**
- ZipNN: https://arxiv.org/pdf/2411.05239
- ZipLLM: https://arxiv.org/html/2505.06252v3

**Compression and delta**
- OpenZL: https://engineering.fb.com/2025/10/06/developer-tools/openzl-open-source-format-aware-compression-framework , https://arxiv.org/abs/2510.03203v2
- Odess (crossref): https://api.crossref.org/works/10.1145%2F3584663
- Finesse: https://www.usenix.org/conference/fast19/presentation/zhang
- zstd as a patching engine: https://github.com/facebook/zstd/wiki/Zstandard-as-a-patching-engine

**Git**
- `--path-walk`: https://lore.kernel.ime.usp.br/git/pull.1819.v2.git.1742829769.gitgitgadget@gmail.com/
- `git-pack-objects` docs: https://kernel.org/pub/software/scm/git/docs/git-pack-objects.html
