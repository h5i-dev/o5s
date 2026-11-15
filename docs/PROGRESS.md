# Progress log

## Status (2026-11-15)
- **M0 Plan: done.** See `docs/PLAN.md` (§4 Exp1 thresholds frozen; the plan was revised for the production bar and core/net split).
- **M1 Core API v1: done.** `crates/core` has the traits and LocalDisk, which passes `backend::conformance`. There's also a fault-injection helper, `core::fault`.
- **M2 Pack store: done.** `crates/pack` provides:
  - per-writer flocked staging, content-addressed create-only sealed packs, orphan adoption, and torn-tail truncation;
  - streaming `put_stream`/`open`, FastCDC chunks with manifests, and zstd records;
  - verified reads (CRC + BLAKE3 on every read).
  Tests: crash at every fault point, concurrent handles and threads, and proptest vs MemStore.
- **M3 Ref store: done.** `crates/refs` is a journal with an exclusive-flock commit point, catch-up replay, torn-tail truncation, and namespace rules whose immutability can't be retracted. Tests: crash sweep, a 6-handle contention test with no lost updates, and a model-based proptest.
- **M4 Local repo + CLI: done.** `LocalRepo: Repository` (with `open_with_backend` for S3) and the `pico2peta` CLI (init/put/get/size/resolve/replace/refs/ns/flush/stat; exit codes 0/1/2/3/4). The end-to-end tests include **multi-process OCC contention through the real binary**.
- **M5 Experiment 1: running.** The harness is `bench/exp1`, the driver is `scripts/exp1.sh`, and results go to `$BENCH_DATA/exp1/results.jsonl`. ripgrep and jq are done; redis and git are running (hours, because level-19 compression is slow).
- **M6 GC/repack: done.** Phased plan → sweep (repack live records verbatim, then condemn) → delete (re-mark, rescue, delete idx then pack), each phase at least the grace period apart. Writers ignore condemned copies when deduplicating and re-list at least every 60 s. Readers follow relocations. The CLI has `pico2peta gc [--grace 1h] [--dry-run]`. Tests: full cycle, resurrection after condemnation, dry run, and concurrent writers under time-scaled real parameters (8/8 stress runs green).
  - Fixed along the way: a race between sealing and GC condemnation could leave a writer's index pointing at its retired open pack. Seal now re-points its records directly.
- `net` merged at 1ad72a0 (S3 backend) and c3a3406 (server and client). The CLI has `--remote`/`--token`, smoke-tested end to end against `pico2peta-server`.
- **M7 Hardening: in progress.**
  - Done: decoder bounds checks; `deny(unwrap_used, expect_used, panic)` in the core crates; never-panic proptests; `fuzz/` targets for every decoder and for recovery; README and CHANGELOG.
  - Done: **ref journal compaction**, i.e. snapshot + generation rotation with a tombstone commit point. Lagging handles in other processes follow it. Tested with a crash sweep and contention across rotations.
  - Pending: running the fuzzers (nightly toolchain installing on scratch), and recording micro-benchmark numbers (`bench/micro`), which waits for Exp1 to finish so its timings stay clean.

## API changes
- **v1 (2026-11-15):** first stable shapes. `Backend` lives in `pico2peta_core::backend`. It is create-only (`put_if_absent` with `PutSource::{Bytes, File}`) and adds `get_range(key, Range)`, `open` (streaming), `head`, `list -> Vec<BlobMeta>`, `delete` and `describe`. New error variants are `KeyNotFound(String)` and `Backend { transient, msg }`. `ObjectStore`/`Repository` take `&self` everywhere, and `open()` returns a `'static` streaming reader. Every backend must pass `backend::conformance::run_all`.
  These changes cover all three of the net session's "Requests for core" from 2026-11-15: streaming put, a backend error variant, and committing early.

- **v1.1 (2026-11-15), additive:** `Error::Unauthorized(String)` (code `"unauthorized"`) for authn/authz failures, requested by net. The CLI exits with 5. Exhaustive `match`es on `Error` need the new arm.

## Production gaps
- **Index in memory.** Every handle loads all `.idx` files into a HashMap, about 100 B/object, which is fine up to ~10^7–10^8 objects. Billions need a fan-out/on-disk index or a PostgreSQL index (an open question in the brief).
- **Flat manifests.** A 100 GB object at a 64 KiB average chunk size has a ~64 MB manifest. This needs hierarchical manifests.
- **GC assumes an operation lease.** A `put` → `replace` must finish within the grace period (default 1 h). There's no enforcement yet; an epoch/lease token on `replace` would make it checkable.
- **GC tracer.** GC treats objects as opaque, so only ref targets and their chunks are roots. Structured objects (directories, snapshots) need a `Tracer` once those formats exist.
- **No ref deletion.** `Repository` has no `delete(name)` yet, so the only way to drop a root is to move a ref.
- **Small packs.** `replace` and `flush` seal the writer's open pack, so chatty writers make many small packs. Repack (M6) is the planned fix.
- **Ambiguous outcome on I/O error.** If an fsync fails during `replace`, the record may still be durable. The caller must re-`resolve` to find out (documented, as in most databases).
- **Remote mode.** The CLI is local-only until `crates/client` lands from net.
- **Fuzz targets** for the pack record, index, manifest and journal decoders aren't written yet (M7).

## Environment notes
- `source scripts/env.sh` puts the cargo target, `CARGO_HOME`, TMPDIR and bench data on `/mnt/scratch/pico2peta`, and adds the shared Lean 4.31 to PATH.

## Questions for human
- (none yet)
