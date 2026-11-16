# Baseline before the h5i-app rebuild (docs/H5I_PLAN.md, Phase H0)

Frozen 2026-11-16 at the tag `baseline-pre-h5i` on `demo`. After the rebuild (H3), every number here is
measured again with the same protocol and judged by the pre-registered no-regression rule (PLAN.md §6.8): a
metric may be worse by at most max(3 %, its baseline run-to-run range), no tier may get worse, and Exp3 sizes
must be byte-identical. Host: AWS EC2, AMD EPYC 7R32, 4 vCPUs, 15 GiB, local NVMe; shared with other users'
sessions, so every number carries the 1-minute load at the time.

## 1. Core suite (`scripts/baseline-core.sh micro|scale`)

- **Measured code:** commit `40b6ed6` (release build made from the clean tree before the runs). The raw files'
  headers record `dirty 0` for the first runs and `dirty 1–6` later: those were edits to new, unbuilt files
  (`crates/kernel`) made during the window; the binary measured is the one built at `40b6ed6`.
- **When:** 2026-11-16 02:08–02:19 UTC, under the bench lock; load 1.25–2.91.
- **Raw data:** `bench/baseline/core-40b6ed6/*.txt` (one file per run). **Summary:**
  `scripts/baseline-summary.py bench/baseline/core-40b6ed6`; the H3 comparison is
  `scripts/baseline-summary.py bench/baseline/core-40b6ed6 <new dir>`, which prints the §6.8 verdict per metric.
- Directions: `+` higher is better, `-` lower is better. Range % = (max − min) / median over the reps (micro:
  3 reps; scale: 1 rep, so §6.8's 3 % applies).

| metric | dir | n | median | min | max | range % | load |
|---|---|---|---|---|---|---|---|
| commits 1 threads /s | + | 3 | 8,004 | 7,998 | 8,063 | 0.8 | 1.38–1.47 |
| commits 4 threads /s | + | 3 | 12,706 | 12,454 | 12,729 | 2.2 | 1.38–1.47 |
| commits 8 threads /s | + | 3 | 14,355 | 14,220 | 14,363 | 1.0 | 1.38–1.47 |
| ingest put MB/s | + | 3 | 187 | 182 | 187 | 2.7 | 1.51–2.07 |
| ingest put_many MB/s | + | 3 | 406 | 363 | 413 | 12.3 | 1.51–2.07 |
| ingest zstd-1 MB/s | + | 3 | 296 | 263 | 319 | 18.9 | 1.51–2.07 |
| ingest zstd-1 ratio | - | 3 | 0.3869 | 0.3869 | 0.3869 | 0.0 | 1.51–2.07 |
| ingest zstd1 MB/s | + | 3 | 283 | 260 | 305 | 15.9 | 1.51–2.07 |
| ingest zstd1 ratio | - | 3 | 0.3391 | 0.3391 | 0.3391 | 0.0 | 1.51–2.07 |
| ingest zstd2 MB/s | + | 3 | 263 | 239 | 272 | 12.5 | 1.51–2.07 |
| ingest zstd2 ratio | - | 3 | 0.3308 | 0.3308 | 0.3308 | 0.0 | 1.51–2.07 |
| ingest zstd3 MB/s | + | 3 | 225 | 172 | 226 | 24.0 | 1.51–2.07 |
| ingest zstd3 ratio | - | 3 | 0.322 | 0.322 | 0.322 | 0.0 | 1.51–2.07 |
| reads random 4096KiB get_mb_s | + | 3 | 1,730 | 1,644 | 1,748 | 6.0 | 1.25–2.04 |
| reads random 4096KiB open_eof_mb_s | + | 3 | 1,944 | 1,937 | 1,960 | 1.2 | 1.25–2.04 |
| reads random 4096KiB open_strict_mb_s | + | 3 | 1,736 | 1,726 | 1,747 | 1.2 | 1.25–2.04 |
| reads random 4KiB get_mb_s | + | 3 | 856 | 845 | 868 | 2.7 | 1.25–2.04 |
| reads random 4KiB open_eof_mb_s | + | 3 | 915 | 813 | 934 | 13.2 | 1.25–2.04 |
| reads random 4KiB open_strict_mb_s | + | 3 | 912 | 799 | 928 | 14.1 | 1.25–2.04 |
| reads random 512KiB get_mb_s | + | 3 | 2,533 | 2,525 | 2,558 | 1.3 | 1.25–2.04 |
| reads random 512KiB open_eof_mb_s | + | 3 | 2,389 | 2,366 | 2,407 | 1.7 | 1.25–2.04 |
| reads random 512KiB open_strict_mb_s | + | 3 | 2,358 | 2,324 | 2,369 | 1.9 | 1.25–2.04 |
| reads random 64KiB get_mb_s | + | 3 | 2,303 | 2,232 | 2,307 | 3.3 | 1.25–2.04 |
| reads random 64KiB open_eof_mb_s | + | 3 | 2,329 | 2,294 | 2,336 | 1.8 | 1.25–2.04 |
| reads random 64KiB open_strict_mb_s | + | 3 | 2,285 | 2,281 | 2,337 | 2.5 | 1.25–2.04 |
| reads random 65536KiB get_mb_s | + | 3 | 1,312 | 1,301 | 1,329 | 2.1 | 1.25–2.04 |
| reads random 65536KiB open_eof_mb_s | + | 3 | 2,141 | 2,139 | 2,144 | 0.2 | 1.25–2.04 |
| reads random 65536KiB open_strict_mb_s | + | 3 | 2,023 | 2,011 | 2,043 | 1.6 | 1.25–2.04 |
| reads text 4096KiB get_mb_s | + | 3 | 1,000 | 984 | 1,000 | 1.6 | 1.25–2.04 |
| reads text 4096KiB open_eof_mb_s | + | 3 | 1,141 | 1,132 | 1,157 | 2.2 | 1.25–2.04 |
| reads text 4096KiB open_strict_mb_s | + | 3 | 995 | 990 | 1,009 | 1.9 | 1.25–2.04 |
| reads text 4KiB get_mb_s | + | 3 | 291 | 268 | 295 | 9.3 | 1.25–2.04 |
| reads text 4KiB open_eof_mb_s | + | 3 | 292 | 259 | 296 | 12.7 | 1.25–2.04 |
| reads text 4KiB open_strict_mb_s | + | 3 | 294 | 283 | 295 | 4.1 | 1.25–2.04 |
| reads text 512KiB get_mb_s | + | 3 | 654 | 654 | 660 | 0.9 | 1.25–2.04 |
| reads text 512KiB open_eof_mb_s | + | 3 | 644 | 643 | 646 | 0.5 | 1.25–2.04 |
| reads text 512KiB open_strict_mb_s | + | 3 | 642 | 640 | 643 | 0.5 | 1.25–2.04 |
| reads text 64KiB get_mb_s | + | 3 | 585 | 585 | 591 | 1.0 | 1.25–2.04 |
| reads text 64KiB open_eof_mb_s | + | 3 | 578 | 567 | 589 | 3.8 | 1.25–2.04 |
| reads text 64KiB open_strict_mb_s | + | 3 | 581 | 568 | 581 | 2.2 | 1.25–2.04 |
| reads text 65536KiB get_mb_s | + | 3 | 902 | 887 | 908 | 2.3 | 1.25–2.04 |
| reads text 65536KiB open_eof_mb_s | + | 3 | 1,273 | 1,260 | 1,284 | 1.9 | 1.25–2.04 |
| reads text 65536KiB open_strict_mb_s | + | 3 | 1,118 | 1,064 | 1,123 | 5.3 | 1.25–2.04 |
| scale gc 1e+07 all_live_kept | + | 1 | 1 | 1 | 1 | 0.0 | 2.10–2.13 |
| scale gc 1e+07 gc_s | - | 1 | 107.2 | 107.2 | 107.2 | 0.0 | 2.10–2.13 |
| scale gc 1e+07 peak_rss_mb | - | 1 | 2,113 | 2,113 | 2,113 | 0.0 | 2.10–2.13 |
| scale names 1e+06 open_s | - | 1 | 0.55 | 0.55 | 0.55 | 0.0 | 2.13–2.31 |
| scale names 1e+06 replace_rate | + | 1 | 8,115 | 8,115 | 8,115 | 0.0 | 2.13–2.31 |
| scale names 1e+06 resolve_p99_ms | - | 1 | 0.0045 | 0.0045 | 0.0045 | 0.0 | 2.13–2.31 |
| scale names 1e+06 rss_mb | - | 1 | 148 | 148 | 148 | 0.0 | 2.13–2.31 |
| scale objects 1e+06 anon_mb | - | 1 | 32 | 32 | 32 | 0.0 | 2.07–2.23 |
| scale objects 1e+06 disk_bytes | - | 1 | 226,001,722 | 226,001,722 | 226,001,722 | 0.0 | 2.07–2.23 |
| scale objects 1e+06 get_p99_ms | - | 1 | 0.0041 | 0.0041 | 0.0041 | 0.0 | 2.07–2.23 |
| scale objects 1e+06 open_s | - | 1 | 0.22 | 0.22 | 0.22 | 0.0 | 2.07–2.23 |
| scale objects 1e+06 put_rate | + | 1 | 174,946 | 174,946 | 174,946 | 0.0 | 2.07–2.23 |
| scale objects 1e+06 rss_mb | - | 1 | 138 | 138 | 138 | 0.0 | 2.07–2.23 |
| scale objects 1e+07 anon_mb | - | 1 | 32 | 32 | 32 | 0.0 | 2.10–2.23 |
| scale objects 1e+07 disk_bytes | - | 1 | 2,260,014,682 | 2,260,014,682 | 2,260,014,682 | 0.0 | 2.10–2.23 |
| scale objects 1e+07 get_p99_ms | - | 1 | 0.0129 | 0.0129 | 0.0129 | 0.0 | 2.10–2.23 |
| scale objects 1e+07 open_s | - | 1 | 3.84 | 3.84 | 3.84 | 0.0 | 2.10–2.23 |
| scale objects 1e+07 put_rate | + | 1 | 187,716 | 187,716 | 187,716 | 0.0 | 2.10–2.23 |
| scale objects 1e+07 rss_mb | - | 1 | 1,048 | 1,048 | 1,048 | 0.0 | 2.10–2.23 |
| scale prov 1e+06 load_s | - | 1 | 0.44 | 0.44 | 0.44 | 0.0 | 2.31–2.91 |
| scale prov 1e+06 record_s | - | 1 | 32.5 | 32.5 | 32.5 | 0.0 | 2.31–2.91 |
| small ensure_live k/s | + | 3 | 3,672 | 3,495 | 3,793 | 8.1 | 1.31–1.41 |
| small get 1t k/s | + | 3 | 268.1 | 267.7 | 269.4 | 0.6 | 1.31–1.41 |
| small get 4t k/s | + | 3 | 753.8 | 697.9 | 761.8 | 8.5 | 1.31–1.41 |
| small put 1t k/s | + | 3 | 132.2 | 132 | 134.4 | 1.8 | 1.31–1.41 |
| small put 4t k/s | + | 3 | 93.9 | 93.5 | 95.1 | 1.7 | 1.31–1.41 |
| small re-put k/s | + | 3 | 715.2 | 714.6 | 715.5 | 0.1 | 1.31–1.41 |
| stream peak RSS MB | - | 3 | 21 | 21 | 21 | 0.0 | 1.47–2.04 |
| stream random put MB/s | + | 3 | 306 | 238 | 308 | 22.9 | 1.47–2.04 |
| stream random re-put MB/s | + | 3 | 1,026 | 1,024 | 1,033 | 0.9 | 1.47–2.04 |
| stream random read MB/s | + | 3 | 2,029 | 2,018 | 2,055 | 1.8 | 1.47–2.04 |
| stream text put MB/s | + | 3 | 314 | 307 | 315 | 2.5 | 1.47–2.04 |
| stream text re-put MB/s | + | 3 | 1,029 | 1,028 | 1,035 | 0.7 | 1.47–2.04 |
| stream text read MB/s | + | 3 | 1,088 | 1,078 | 1,100 | 2.0 | 1.47–2.04 |

## 2. Generic front (PLAN §6.4)

- **Measured code:** `edd8c8b` (the core is unchanged from there to `40b6ed6`; only the CLI's hex parser and
  docs changed). 2026-11-16, three runs under the bench lock, load 2.6–4.9.
- **Raw data:** `bench/baseline/generic/edd8c8b-3runs.txt`. Command: `p2p-bench generic <dir> 1024`.

| metric | runs | median | threshold | tier |
|---|---|---|---|---|
| durable write vs `dd conv=fsync` | 0.945, 1.233, 1.002 | 1.002× | ≥ 0.90× | PASS |
| verified read vs warm `cat`, strict | 0.184, 0.192, 0.175 | 0.184× | ≥ 0.50× | FAIL |
| verified read vs warm `cat`, eof | 0.198, 0.272, 0.185 | 0.198× | ≥ 0.50× | FAIL |
| 4 KiB range read p99 | 0.112, 0.155, 0.110 ms | 0.112 ms | ≤ 1 ms | PASS |
| 10 versions, scattered edits | 1,135,462,813 B ×3 | 1.057 GiB | ≤ 1.15 GiB | PASS |
| 10 versions, 1 MiB inserts | 1,091,744,331 B ×3 | 1.017 GiB | ≤ 1.15 GiB | PASS |

## 3. Experiment 3 (PLAN §6.6), official run

- **Measured code:** `6855dca` (= `55901dd` + docs; see EXP1.md for the dirty-file disclosure). 2026-11-15.
  Load: window 1 (ripgrep, jq, redis) 2.9–4.9; window 2 (git.git, generalization) 3.9–7.1.
- **Raw data:** `bench/baseline/exp3/results.jsonl` (28 rows, every variant), `bench/baseline/exp3/retime.jsonl`
  (build-time re-time). Commands: `scripts/exp3.sh repos`, `scripts/exp3.sh general`, `scripts/exp3-retime.sh`;
  verdict: `EXP1_ELIGIBLE=a3,a4 scripts/exp1-report.py $BENCH_DATA/exp1/results.jsonl,$BENCH_DATA/exp3/results.jsonl`.
- **§6.8:** sizes (`total_bytes` per repo and variant) must be byte-identical.

| repo | git --aggressive B | A3 (V*) ratio | A4 ratio | A3 build s | git s | A3 read p50 / p99 ms | A3 peak RSS |
|---|---|---|---|---|---|---|---|
| ripgrep | 6,227,408 | 0.841 | 0.847 | 6.0 | 6.4 | 2.86 / 19.0 | 432 MB |
| jq | 16,161,931 | 0.794 | 0.794 | 12.2 | 12.2 | 3.62 / 14.4 | 501 MB |
| redis | 268,625,488 | 0.947 | 0.948 | 190.4 | 303.8 | 2.83 / 12.2 | 2.3 GB |
| git | 319,033,230 | 0.796 | 0.796 | 401.0 | 531.7 | 3.28 / 14.2 | 4.9 GB |

- Size STRONG PASS (geomean 0.842, max 0.947); Git parity met; reads and RSS met.
- Build re-time: ripgrep 0.944, jq 1.068 then the decisive 1.179 (load 2.0–3.3) → **FAIL** (verdict kept; the
  human overrode the consequence, EXP1.md).
- Generalization: packages A4 0.806 (generalizes), weights A4 0.999 (does not).

## 4. Net fronts, final batch (PLAN §6.5)

- **Measured code:** `c8f9942` on `net` plus the bench-only `first_push_s` row (server code identical; net
  `5df5cb6`). 2026-11-16, windows A (git, OCI) and B (HF, PyPI, npm, Maven), under the bench lock, load
  1.2–6.2 (per row in the report).
- **Raw data:** `bench/results/fronts-baseline-pre-h5i.jsonl`. **Report:** `bench/results/fronts-baseline-pre-h5i-report.md`
  (every metric, ratio, tier and load). **Run-to-run ranges for §6.8:** `bench/results/fronts-baseline-pre-h5i-ranges.md`.
- **Tiers:** git PASS, OCI FAIL (pull 1.99×), HF FAIL (transfer times), PyPI PASS, npm PASS, Maven PASS;
  135/135 functional checks passed.

## 5. Recorded earlier, not re-measured for this baseline

These are in BENCH.md with their own dates, commits and loads; they are not part of the §6.8 comparison unless
rerun first:
- scale names at 10⁷ (window 1: replace 7.5k/s, resolve p99 7.6 µs, open 6.4 s, RSS 2.1 GB);
- model-weights versioning (`p2p-bench weights`), byte-grouping and level sweeps;
- the git push A/B of the put/get pass (39.5 → 23.5 s, measured with a local call-site change; superseded by net's
  index-pack rework, whose result is in §4 as `first_push_s`).
