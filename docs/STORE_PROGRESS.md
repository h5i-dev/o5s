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

**Measurement** (6,698-file django tree, `/mnt/scratch/pico2peta/tmp/store-bench/run.sh`, under the bench lock):
pending, queued behind the SWE-bench run's lock.

**Follow-ups**
- Store open is still O(packs): one listing plus one index read per pack not yet loaded. A local merged-index
  cache for small packs (like the MIDX for big ones) would remove the per-index reads. For now GC consolidation
  bounds the pack count.
- `bin/ws` / `harness/prepare.py` in /mnt/scratch/swe-o5s can switch to `o5s put --batch` (not our tree).

## 2. Tree object

Not started.
