# Experiment 1: similarity-grouped compression vs `git gc --aggressive`

**Question (PLAN.md §4):** on real source repositories, how close does *similarity-grouped zstd
compression of whole objects* (no explicit deltas) get to Git's most compact packing?

The protocol and pass thresholds were pre-registered in `docs/PLAN.md` §4 (the commit "Add pre-registered plan (milestones, crate layout, Exp1 thresholds) and env script", `0451262` at the time of writing; branch history was rewritten on 2026-11-15 with identical trees, so find it by subject with `git log --format="%h %s" -- docs/PLAN.md` if the SHA has changed)
before any experiment code existed. They haven't changed since. Results are judged by
`scripts/exp1-report.py`, which applies §4.5 mechanically to the raw measurements.

## How it was run

```sh
source scripts/env.sh
scripts/exp1.sh                       # all four repos; appends to $BENCH_DATA/exp1/results.jsonl
scripts/exp1-report.py $BENCH_DATA/exp1/results.jsonl
```

- **Corpus:** `git clone --mirror` of BurntSushi/ripgrep, jqlang/jq, redis/redis and git/git, cloned
  2026-11-15. `results.jsonl` records each repo's ref-snapshot digest (sha256 of
  `for-each-ref`) and object count.
- **Object set:** `git rev-list --objects --all`, i.e. every reachable blob, tree, commit and annotated tag.
  Payloads are Git's canonical form `"<type> <size>\0<body>"`, identical for both systems. Every
  sampled read is checked: the SHA-1 of the bytes read back must equal the Git object id.
- **Baseline B:** `git -c pack.threads=4 gc --aggressive --prune=now` on a fresh copy of the mirror
  (Git 2.50.1: window 250, depth 50). B = `.pack` + `.idx`. `.rev`/`.bitmap` are reported but excluded.
  Plain `git gc` is reported for context.
- **Variants** (`bench/exp1`), all at zstd level 19. Partitions are `(type, extension)` (blobs
  without an extension are grouped by basename). Within a partition, objects are ordered by
  `(path, history position oldest first)`.
  - V0: every object compressed alone (strawman).
  - V1: per-object compression with one trained dictionary per partition (≤ 112 KiB).
  - V2: consecutive objects of a partition concatenated into frames of ≤ 4 MiB.
  - V3: one long-window (`--long=27`) stream per partition. This is a ceiling, not random access, and not eligible.
- **Size S(V):** payload + dictionaries + a modeled index of 48 B/object.
- **Secondary metrics** (V\* only): random single-object reads over 10,000 uniformly random
  objects, with a warm page cache and dictionaries preloaded; build time with 4 threads; peak RSS.

### Operational choices and deviations (all disclosed)

1. **History order.** "First-seen commit time" is operationalized as position in the
   `git rev-list --objects --all` output, reversed (rev-list emits newest first). This was fixed in the
   harness before the first run.
2. **Dictionary sizing (V1).** Dictionary = min(112 KiB, 5% of the partition, at least 1 KiB),
   trained on a stride sample of ≤ 100× the dictionary size. Partitions with < 8 objects or < 16 KiB get no
   dictionary. Fixed before the first run.
3. **Spill file.** Redis is 9 GB of canonical objects and git.git is more, while scratch had 13–24 GB
   free. So `extract` stores the object stream per partition as one zstd level-3 long-window
   stream, and each variant streams it back. Decoding the spill is **included** in our build time,
   which counts against us.
4. **Restart after ENOSPC.** The shared scratch volume filled up (other workloads) during redis V0.
   Redis and git were re-run from scratch, including the git baselines. ripgrep and jq results are from
   the first run, which completed normally.
5. **Shared, loaded host.** 4 vCPU AMD EPYC 7R32 (15 GB). A second development session and my own
   builds ran concurrently, with load averages of 4–9 at times. This affects build times (ours and
   Git's, which were measured at different moments) but not sizes or byte-exact verification. Where
   a build-time comparison is close, both are re-timed back-to-back on a quiet host and both numbers
   are reported (see Results).

6. **Independent harness review (2026-11-15, during the git.git run).** No silent-corruption bug: offsets,
   object coverage, chain encode/decode symmetry, and SHA-1 verification were all checked. The review found
   deviations, each disclosed here with its direction:
   - *Partition key detail:* extensions are lowercased, and extension-less files are grouped by basename.
     This was fixed before the first run but only partly stated above. It affects V1/V2 only, probably slightly in our favour.
   - *Order:* "history position" is the rev-list first-encounter position, i.e. the newest commit containing
     each version, reversed. On linear history this is time order; after reverts or merges it can differ. Direction mixed.
   - *Read latency* is measured with one reader thread. §4.5's "4 threads" was applied to the build, so the latency
     numbers are optimistic relative to a 4-concurrent-reader reading.
   - *Build time* for Exp1 V0–V3 stops before the output's final fsync, while Git's wall time includes its own
     fsyncs and housekeeping. That's slightly in our favour. **Fixed for Exp1b** (the timer now includes the fsync).
   - *Annotated tags* were ordered by tag name (rev-list prints the tag name as a path). **Fixed for Exp1b:**
     tags are path-less, as §4.4/§4.8 specify. Tags are a negligible share of bytes.
   - *Exp1b V4/V5 zstd usage:* the first harness version used streaming frames without a declared size
     (8 MiB+ buffers per object and per chain link) and read the target record twice, which biased V4/V5 build
     time and read latency *against* them. **Fixed before the official 1b run:** the content size is pledged and
     the target is read once. The sanity numbers above came from the old version.

## Results (final, generated by `scripts/exp1-report.py`)

| repo | objects | git --aggressive (B, bytes) | plain git gc | V0 | V1 | V2 | V3 (ceiling) |
|---|---|---|---|---|---|---|---|
| ripgrep | 23,245 | 6,227,408 | 10,386,303 | 8.262 | 4.850 | **0.850** | 0.708 |
| jq | 24,771 | 16,161,931 | 23,894,246 | 5.914 | 2.703 | **0.928** | 0.754 |
| redis | 274,683 | 268,625,488 | 522,791,116 | 8.487 | 6.709 | **1.336** | 0.840 |
| git | 861,069 | 319,033,230 | 635,170,415 | 19.383 | 13.689 | **1.160** | 0.709 |

Cells are R = S(V)/B (payload + dictionaries + 48 B/object modeled index). Lower is better.

| repo | V2 build s | git --aggressive s | ratio | V2 read p50 / p99 (ms) | V2 peak RSS |
|---|---|---|---|---|---|
| ripgrep | 6.4 | 6.4 | 1.01 | 0.38 / 3.8 | 269 MB |
| jq | 11.2 | 12.2 | 0.92 | 0.43 / 3.3 | 376 MB |
| redis | 328.9 | 303.8 | 1.08 | 0.38 / 5.2 | 689 MB |
| git | 325.3 | 531.7 | 0.61 | 0.32 / 4.1 | 619 MB |

### Verdict: **FAIL** (pre-registered rules, §4.5)

V\* = V2 (V1's geomean is 6.6×). Geomean R = **1.051**, which would satisfy the ≤ 1.15 condition, but max R =
**1.336** (redis) exceeds the 1.30 per-repo limit. The build-time threshold (≤ 1.0×) is also missed on
ripgrep (1.01) and redis (1.08). Read latency and memory are well within their thresholds everywhere.

*Correction:* an interim version of this section gave redis's build ratio as 1.30. That used the
`git gc --aggressive` time from the first redis run (252 s), which was discarded when redis was re-run after the
disk-full crash (deviation 4). The re-run baseline is 303.8 s. The same baseline measured twice on this shared
host differed by 20%, so the close build-time comparisons (ripgrep 1.01, redis 1.08) are within noise. Per
deviation 5, they would need back-to-back re-timing on a quiet host to be conclusive. The size result alone
decides the verdict.

What the numbers say:
- **Grouping beats Git on small histories and loses on large ones.** On ripgrep and jq, 4 MiB frames of
  path-sorted versions beat `git --aggressive` (0.85×, 0.93×). On redis and git.git, big files have thousands of
  versions and a 4 MiB frame spans only a few of them, so cross-version redundancy falls outside the window
  (1.34×, 1.16×).
- **The ceiling is well below Git.** V3 (one long-window stream per partition, not random access) is
  0.71–0.84× on every repo. Similarity grouping *can* beat delta packing on size. The 4 MiB cap is the
  price of random access.
- **Per-object compression is hopeless for source history** (V0 6–19×, V1 2.7–14×, even with dictionaries).
- **Consequences, as pre-registered:**
  - Grouped packing does *not* become the default repack strategy for source code. The net session made
    `[gc] grouped` opt-in.
  - **Experiment 1b** runs: explicit delta against the previous version of the same path, then zstd (PLAN.md
    §4.8, registered before any 1b code). A sanity run of the 1b harness on ripgrep (V4 1.166×, V5 1.667×)
    was made to check correctness before the official run. The official 1b numbers come from a fresh full run.

## Experiment 1b (PLAN.md §4.8)

Final (generated with `EXP1_ELIGIBLE=v4,v5 scripts/exp1-report.py results.jsonl,results-1b.jsonl`):

| repo | V4 (depth 50) | V5 (depth 10) | V4 build ratio | V4 read p50 / p99 (ms) | V4 mean / max chain depth |
|---|---|---|---|---|---|
| ripgrep | 1.164 | 1.665 | 2.44 | 0.10 / 0.38 | 21.9 / 50 |
| jq | 1.089 | 1.427 | 1.41 | 0.10 / 0.59 | |
| redis | **1.347** | 1.860 | 2.74 | 0.13 / 1.52 | 23.1 / 50 |
| git | **2.160** | 3.411 | 1.23 | 0.14 / 0.75 | 21.7 / 50 |

### Verdict: **FAIL** (same thresholds as §4.5)
V\* = V4: geomean R = 1.386, max R = 2.160. Both size conditions fail, and V4's build time is 1.2–2.7× Git's
(threshold ≤ 1.0) on every repo. Reads are fast: the whole chain is decoded, with p99 ≤ 1.5 ms. All 10,000
sampled reads per run were SHA-1-verified.

git.git is the outlier. Its 861k objects are dominated by small objects: plain per-object compression (V0)
is 19× there. Every chain root (one in 51) is compressed alone, and each small delta pays zstd frame and
entropy-table overhead per object. That's a hypothesis consistent with the V0/V2 numbers, not something
tested separately.

**Explicit delta against the previous version of the same path loses to Git** even before the index
model is counted: redis's V4 payload alone is 1.33× Git's pack, and git.git's is 2.2×. A plausible reason (an analysis, not
something tested here) is that Git's delta search tries 250 candidate bases chosen by size and name
similarity, so it finds better bases than "the immediately previous version". Level-19 patch-from per
object is also slow.

**Pre-registered consequence:** keep Git-compatible storage as the recommended configuration for source
code (the net session's Git adapter, plus opt-in grouped repack, which on a real ripgrep import reaches 1.02×
git-aggressive on payload). No new variants are added after seeing these results.

## Production cross-check (not part of the pre-registered verdict)

The net session's Git adapter imports a real repository object by object and then regroups it with
the production pack format (frames + index), using a path-based `Grouper` that follows the same
partitioning. Its test (`crates/server/tests/git.rs::grouped_repack_vs_git`, run with `--ignored`)
compares against `git gc --aggressive` on **the same refs** (ripgrep heads + tags, 14,192 objects):

| pack format | total vs git-aggressive | payload vs git pack | repack time |
|---|---|---|---|
| frame v1 + idx v2 (36 B/member directory, 62 B/entry index) | 1.264× | 1.154× | 52 s |
| frame v2 + idx v3 (lengths-only directory, 44 B/entry index) | **1.078×** | **1.021×** | 34.8 s (loaded host) |

The remaining gap is mostly the index: 44 B/object vs Git's ~28 B. 12 B of that is inherent to
32-byte BLAKE3 ids vs 20-byte SHA-1. The harness's 48 B/object index model was therefore
slightly pessimistic for the shipped format.

## Experiment 1c (PLAN.md §4.9; run by the net session)

Pre-registered before any 1c code (commit "PLAN §4.9: pre-register Experiment 1c …"). It is a separate
experiment commissioned by the human (RESEARCH_SPEEDUP.md §3): **the Exp1 and 1b verdicts are unchanged.**

**Candidates.** V6 picks each object's delta base among the previous 250 objects of its partition by
Odess-style super-feature resemblance (12 content-defined features, 3 super-features), with a +0.5 bonus
for the same path. Encoding is §4.8's zstd-19 patch-from, with chain depth ≤ 50. V7 is the same, except
the top 3 candidates (and "no base") are trial-encoded at zstd level 3 and the smallest is kept. Harness:
`bench/exp1c` (Exp1's extract/run/verification code, plus V6/V7). Driver: `scripts/exp1c.sh`. Every timed
step, including extraction, ran under `/mnt/scratch/pico2peta/bench.lock`. Extra baselines (reported, not
judged) are Git 2.56.1 `repack -adf --window=250 --depth=50` with `--path-walk` or
`--name-hash-version=2`, on fresh mirror copies.

Results (generated by `scripts/exp1c-report.py`; B is Exp1's frozen git-aggressive size):

| repo | objects | B (git --aggressive) | V6 R | V7 R | B' path-walk | B' name-hash v2 | V\* R' (path-walk / nh2) |
|---|---|---|---|---|---|---|---|
| ripgrep | 23,245 | 6,227,408 | 1.814 | 1.751 | 6,181,009 | 6,229,190 | 1.764 / 1.750 |
| jq | 24,771 | 16,161,931 | 1.480 | 1.414 | 15,999,198 | 16,164,176 | 1.428 / 1.414 |
| redis | 274,683 | 268,625,488 | 1.604 | 1.629 | 268,106,355 | 268,627,591 | 1.632 / 1.629 |
| git | 861,069 | 319,033,230 | 2.256 | 2.272 | not run (stopped by the human to prioritise implementation) | not run (stopped by the human to prioritise implementation) | not run / not run |

| repo | V\* build s | git --aggressive s | ratio | V\* read p50 / p99 (ms) | mean / max depth | peak RSS | verified reads |
|---|---|---|---|---|---|---|---|
| ripgrep | 21.4 | 6.4 | 3.35 | 0.18 / 0.55 | 34.5 / 50 | 179 MB | 10000 |
| jq | 25.7 | 12.2 | 2.10 | 0.17 / 0.86 | 35.3 / 50 | 526 MB | 10000 |
| redis | 1116.7 | 303.8 | 3.68 | 0.18 / 1.48 | 38.7 / 50 | 1003 MB | 10000 |
| git | 1177.7 | 531.7 | 2.21 | 0.22 / 0.66 | 36.0 / 50 | 977 MB | 10000 |

V* = v7 (geomean V6 1.765, V7 1.740); max R = 2.272; repos complete: 4/4
size verdict: FAIL

### Verdict: **FAIL** (§4.5 thresholds)
V\* = V7 (geomean R 1.740; V6 1.765). Every repo is over the per-repo PASS maximum of 1.30: the best is jq
at 1.41× and the worst is git.git at 2.27×. Build time is 2.1–3.7× Git's (threshold ≤ 1.0×). Read latency and
memory are within their thresholds: p99 ≤ 1.5 ms, peak RSS ≤ 1 GB, and every one of 10,000 sampled reads per
run was SHA-1-verified.

**Why resemblance lost to 1b's "previous version of the same path"** (diagnosis with the env-gated
`EXP1C_DIAG` counter on ripgrep, run after the official numbers; it does not change them):
- When the same-path predecessor is eligible, the resemblance pick *beats* it: 2,276 such objects cost
  424 KB with the chosen base vs 725 KB with the predecessor (level-3 trial sizes). Resemblance
  detection itself works.
- The loss comes from the pre-registered depth rule interacting with base choice. For 11,240 objects the
  predecessor was already at chain depth 50, so it was ineligible. 1b's V4 then compresses the object
  alone, starting a fresh chain. V6/V7 instead pick an *older* same-path version that is still under depth
  50 (score 0.5 > 0), which yields long-distance deltas: 7.8 MB for those objects. This also keeps chains
  near the limit (mean depth 34–39 vs 22 for V4).
- So the dominant effect is "a bounded chain depth plus always-take-the-best-eligible-base", not the
  feature quality. A variant that resets the chain instead would be a new experiment and is **not**
  added post hoc (§4.9).

**Baselines, honestly.** On ripgrep, jq and redis, Git 2.56's `--path-walk` and `--name-hash-version=2` barely
change Git's size (within ±1% of the frozen `gc --aggressive` B), so R' ≈ R and the conclusion is the same
against the newer Git. On git.git these two baselines were **not run: the human stopped them to prioritise
implementation**, and they are not re-run.

**Consequence (pre-registered):** FAIL. Git-compatible storage stays the recommendation for source code,
and no further 1c variants are added.
