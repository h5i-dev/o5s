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

**§6.8 no-regression: PASS** (2026-11-17, after the merge as PR #10). These are the core-suite rows that open
stores, since the change touches every store open (`LocalBackend::list`, `refresh`, `recover_orphans`): small
(put/get), ingest (redis dump, 1024 MiB) and scale objects 1e7. Each ran 3 reps, interleaved, under the bench
lock: old = `ab55b21` (main just before the merge), new = `54574a5` (current main; same code as `2d77407`).
The window was 21:14–21:23 UTC, with start load 0.09. The foreign-load rule is the one the core windows use: no
foreign process above 20 % CPU in the 10 s `top` samples. The only process above 20 % was the script's own redis
dump extraction, before the first timed run. Load reached 1.6 during the run from the bench processes
themselves. Script: /mnt/scratch/pico2peta/tmp/store-bench/regress.sh; raw outputs and the process log are in
regress/{main,new}/ and regress/proclog.txt there.
`scripts/baseline-summary.py bench/baseline/core-40b6ed6 regress/new` (rows not rerun omitted):

| metric | baseline | new median | change | allowed | verdict |
|---|---|---|---|---|---|
| small put 1t / 4t k/s | 132.2 / 93.9 | 136.5 / 95.2 | +3.3 / +1.4 % | 3.0 % | ok |
| small get 1t / 4t k/s | 268.1 / 753.8 | 265.0 / 748.3 | −1.2 / −0.7 % | 3.0 / 8.5 % | ok |
| small re-put / ensure_live k/s | 715.2 / 3,672 | 718.3 / 3,694 | +0.4 / +0.6 % | 3.0 / 8.1 % | ok |
| ingest put / put_many MB/s | 187 / 406 | 192 / 417 | +2.7 / +2.7 % | 3.0 / 12.3 % | ok |
| ingest zstd-1/1/2/3 MB/s | 296/283/263/225 | 327/303/275/227 | +10.5/+7.1/+4.6/+0.9 % | ≥ 12.5 % | ok (ratios identical) |
| scale 1e7 open_s | 3.84 | 3.51 | +8.6 % | 3.0 % | ok |
| scale 1e7 get_p99 ms | 0.0129 | 0.0131 | −1.6 % | 3.0 % | ok |
| scale 1e7 put_rate /s | 187,716 | 196,138 | +4.5 % | 3.0 % | ok |
| scale 1e7 rss / anon MB, disk bytes | 1,048 / 32 / 2,260,014,682 | identical | 0 | 3.0 % | ok |

Old vs new in the same window. The scale 1e7 reps for old are open_s 3.53/3.19/3.22 and get_p99 0.0054/0.0133/0.0134;
for new they are open_s 3.56/3.51/3.48 and get_p99 0.0131/0.0131/0.0132. The old median get_p99 (0.0133) is itself −3.1 %
against the baseline, so that metric is at noise level on this host; new is within the allowance. A 1e7 store
is a single pack, so the O(packs) open fix does not show here; its gain is in the many-pack measurements above.

**Follow-ups**
- Store open is still O(packs): one listing plus one index read per pack not yet loaded. A local merged-index
  cache for small packs (like the MIDX for big ones) would remove the per-index reads. For now GC consolidation
  bounds the pack count.
- `bin/ws` / `harness/prepare.py` in /mnt/scratch/swe-o5s can switch to `o5s put --batch` (not our tree).

## 2. Tree object (branch `tree-object`)

**Done**
- Design: the "Trees" section in docs/ARCHITECTURE.md. The format is `"O5STREE1" | (mode u8 | name_len u16 LE | name | hash)*`,
  canonical (sorted unique raw-byte names), with modes regular/executable/symlink/subtree, at most 2 GiB.
- Kernel `crates/kernel/src/tree.rs` (`decode`, `encode`, `check`, `children`). The proofs are in
  `verify/core/proofs/Tree.lean`, on the extracted code, with only the standard axioms:
  - `decode_spec`: the decoder is total and accepts exactly `encodeL es` for canonical `es`, returning `es`;
  - `encode_spec` / `check_spec`: the encoder produces exactly that and refuses exactly the non-canonical lists;
  - `decode_encode`, `encode_decode`, `encodeL_inj`: the round trips, and that the encoding is unique;
  - `children_spec`: the tracer returns exactly the entry hashes.
- Mutants: `tree_duplicate_names`, `tree_mode_4_accepted`, `tree_dot_name_accepted`, `tree_tracer_drops_first`.
  All four are caught (`h5i app mutate --only tree`).
- Fuzz target `tree` (local only): 1.62M runs in 121 s, no failures.
- `crates/tree` (o5s-tree): `Tree`, `TreeFront` (GC children; claims no names), `put_tree` (`ensure_live` on
  every child), and `put_dir` / `checkout` / `diff`. The CLI has `o5s tree put DIR [--exclude NAME]`,
  `tree get TREE DIR`, `tree diff A B [-z]` and `tree ls TREE [-r] [-z]`, and registers `TreeFront`.
  - Tests: canonical form, proptests (round trip; arbitrary bytes never panic and accepted bytes re-encode
    identically), a filesystem round trip (exec bit, symlinks, dangling symlinks, empty dirs, raw-byte names,
    excludes), diff, and GC keeping tree-only blobs. A control test shows GC without the front collects them.

**Requests for net**
- Register `o5s_tree::TreeFront` in the server's front registry. Any process that runs GC on a repository with
  trees must have it; without it, blobs referenced only by a tree are collected. **Done by net** on branch
  `net-tree-front` (`dcb0350`, cut from `tree-object` `3990e76`, gate green). Its test,
  `tree_entries_keep_blobs_alive_and_readable`, fails without the registration. Merge it right after `tree-object`.

**Follow-ups**
- `put_dir` reads files one at a time. Parallel hashing and compression across files would help big trees.
- Checkout fan-out is not limited (a crafted DAG can repeat a subtree many times); depth is limited to 4,096.
