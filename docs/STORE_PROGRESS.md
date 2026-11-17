# Store session progress (pico2peta-store)

Third worker, next to core (`pico2peta`) and net (`pico2peta-net`). Task: /mnt/scratch/pico2peta/tmp/store-task.md
(2026-11-17), from the SWE-bench experiment in /mnt/scratch/swe-o5s.

## 1. Multi-file put in one process (branch `multi-put`)

**Done**
- `o5s put FILE...` and `o5s put --batch` (paths from stdin, one per line, or NUL-terminated with `-z`; after any
  FILEs) store every file through one writer and seal **once**. One hash per line, in input order, printed only
  after the seal, so every printed hash is durable. A failing file fails the command with its path in the message
  and prints nothing. The objects already written are unreferenced, so GC collects them.
- Cheaper store open with many packs. The cause of the 13m42s django import was not the seal alone: every
  `o5s put` opens the store, and opening cost O(packs). It went from 13 ms per put in an empty repo to 42 ms at 900
  packs. Fixes:
  - `LocalBackend::list` walks only the prefix's directory, so `list("gc/")` no longer stats every pack.
  - `refresh` takes index sizes from the listing instead of a `head` per index.
  - `recover_orphans` re-lists only if it sealed something.
  At 900 packs, one put went from 44 ms to 24 ms (dev check, not under the lock; the real numbers are below).
- **No lazy seal.** Reasons are in ARCHITECTURE.md, write path item 5. A staging file is local and only adopted
  when a store is opened, so a long-lived reader (the server, another host on a shared backend) would miss an
  object whose hash was already printed. Small packs are merged by GC (`small_pack_bytes`, 16 MiB).
- Test: `crates/cli/tests/cli.rs::put_many_files_shares_one_pack`.

**Measurement** (2026-11-17 15:23–16:04 UTC, under the bench lock; host load 3.2–10.8, the SWE-bench run was
still active; script and raw output in /mnt/scratch/pico2peta/tmp/store-bench/{run.sh,results.txt}). The input is
the 6,698-file django tree from the SWE-bench pilot (`git archive` of its base). main = `a479528`, new = `2c1fc62` (before the merge of origin/main, `50a6e92`).

| run | sealed packs | wall s | user s | sys s |
|---|---|---|---|---|
| main, one `o5s put` per file, sequential | 5,940 | 995.6 | 454.5 | 481.9 |
| new, one `o5s put` per file, sequential | 5,940 | 679.2 | 331.7 | 293.4 |
| main, one `o5s put` per file, 8 in parallel | 5,944 | 365.3 | 563.1 | 510.1 |
| new, one `o5s put` per file, 8 in parallel | 5,962 | 231.6 | 412.7 | 303.3 |
| new, `o5s put --batch` (cold page cache) | **1** | **1.57** | 0.47 | 0.27 |
| new, `o5s put --batch` (warm) | **1** | **0.38** | 0.29 | 0.08 |

- The hashes are identical across all runs: batch equals single puts, and main equals new. (There are 5,940
  packs for 6,698 files because duplicate contents add no record, so no pack.)
- Batch put vs main's per-file import: about 630× less wall time and 5,940 times fewer packs.
- The open-cost fixes alone make single-file puts 32 % faster (sequential) and 37 % faster (8 parallel), with
  39–41 % less sys time.

**§6.8 no-regression: pending.** The ingest reps taken in that window are invalid: start load was 4.3–10.1 (the
rule needs < 1.0) and the medians are noisy. Since the change touches every store open (`LocalBackend::list`,
`refresh`, `recover_orphans`), the check covers the core-suite rows that open stores: small (put/get),
ingest, and scale objects 1e7 (`open_s`, `get_p99`). They run interleaved main/new, 3 reps each, under the
lock in a quiet window (start load < 1.0, no foreign process, 10 s process log), using
/mnt/scratch/pico2peta/tmp/store-bench/regress.sh, and are judged against BASELINE.md. multi-put is not ready
to merge until this passes.

**Follow-ups**
- Store open is still O(packs): one listing plus one index read per pack not yet loaded. A local merged-index
  cache for small packs (like the MIDX for big ones) would remove the per-index reads. For now GC consolidation
  bounds the pack count.
- `bin/ws` / `harness/prepare.py` in /mnt/scratch/swe-o5s can switch to `o5s put --batch` (not our tree).

## 2. Tree object

Not started.
