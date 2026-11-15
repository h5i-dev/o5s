# Experiment 1: similarity-grouped compression vs `git gc --aggressive`

**Question (PLAN.md §4):** on real source repositories, how close does *similarity-grouped zstd
compression of whole objects* (no explicit deltas) get to Git's most compact packing?

The protocol and pass thresholds were pre-registered in `docs/PLAN.md` §4 (commit `1c26989`)
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

## Results

*(git.git is still running. The table is regenerated from `results.jsonl` at the end. The verdict below is
already determined by the redis result and can't change.)*

Interim (ripgrep, jq, redis):

| repo | objects | git --aggressive (B) | V0 | V1 | V2 | V3 (ceiling) |
|---|---|---|---|---|---|---|
| ripgrep | 23,245 | 6,227,408 | 8.262 | 4.850 | 0.850 | 0.708 |
| jq | 24,771 | 16,161,931 | 5.914 | 2.703 | 0.928 | 0.754 |
| redis | 274,683 | 268,625,488 | 8.487 | 6.709 | **1.336** | 0.840 |

Secondary metrics for V2: p50 reads 0.38–0.43 ms and p99 3.3–5.2 ms (all well within 5 / 25 ms). Peak
RSS ≤ 0.7 GB. Build time ratio vs `git gc --aggressive`: ripgrep 1.01, jq 0.92, **redis 1.30**.

### Verdict: **FAIL** (pre-registered rules, §4.5)

V\* = V2. V1 is far worse everywhere: per-object compression, even with dictionaries, is 2.7–6.7×. On
redis, V2's R = 1.336 exceeds the PASS limit of 1.30 for every individual repo, so the verdict is FAIL
whatever git.git's result. V2 also misses the build-time threshold on redis (1.30 > 1.0) and,
marginally, on ripgrep (1.01).

What the numbers say:
- **Grouping works, but 4 MiB frames are too small for big histories.** On the small repos V2 beats
  `git --aggressive` (0.85×, 0.93×). On redis, whose large C files have thousands of versions, a 4 MiB frame
  holds only a few consecutive versions of a big file, so most redundancy across versions falls outside
  the window.
- **The ceiling is below Git.** V3 (one long-window stream per partition, so not random access) reaches
  0.71–0.84× of Git everywhere. Similarity grouping *can* beat delta packing on size. The cost is random
  access, which is what the frame cap buys.
- Per the pre-registered consequence, grouped packing does **not** become the default repack strategy
  for source code (the net session switched `[gc] grouped` to opt-in), and **Experiment 1b** runs:
  explicit delta against the previous version of the same path, then zstd (PLAN.md §4.8, registered
  before any 1b code). A sanity run of the 1b harness on ripgrep (V4 1.166×, V5 1.667×) was made to
  check correctness before the official run; the official 1b numbers come from a fresh full run.

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
