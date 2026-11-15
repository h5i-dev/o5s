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
  - Pending: micro-benchmark numbers (`bench/micro`), which wait for Exp1 to finish so its timings stay clean.

- **M8 Verification: done.** There's a **Lean↔Rust differential test** (`crates/refs/tests/lean_diff.rs` against the `p2p-model` Lean executable), and it's sanity-checked to fail against a wrong model. Fuzzers ran 60 s per target (~24M executions total) with no crashes. `lean/` has a pure model plus machine-checked proofs, with no `sorry` and only standard axioms. Covered: P1 content immutability, P2 no undetected overwrite, P3 frozen names, P4 authorization, P5 GC safety. Rust already has model-based proptests for refs and packs.
- **M9 Later layers: provenance done (core side).** `crates/prov`: canonical content-addressed `Record`s (`P2PPROV1`), an OCC-appended log under `_prov/log`, and the queries `producers`/`consumers`/`lineage`/`impact`/`regenerable`. `ProvTracer` keeps inputs and params alive but not outputs, which gives regeneration-aware GC. The CLI has `prov record|why|impact`, and its `gc` uses `ProvTracer`. Git/OCI adapters are net's goal 6; the hash-mapping design is agreed (see NET_PROGRESS.md).

- **Similarity-grouped storage (the brief's "similar objects adjacent, compressed together"): done.**
  - Frame records (`Kind::Frame`) hold many small objects compressed together. The member directory sits uncompressed at the front, so recovery can index each member.
  - Pack index v2 (`P2PIDX02`) records each member's inner offset. Each handle keeps an 8-frame decompression cache.
  - GC grouped repack: `LocalRepo::gc_with(opts, tracer, Some(&dyn Grouper))`, plus `GcOptions::{repack_all, frame_level, frame_cap}`.
  - Net implements a path-based GitGrouper (agreed API). Test: 60 versions of one file shrink to under 1/3 after a grouped repack.
  - Defaulting it for the "source code" kind is still pending the Exp1 verdict (the pre-registered decision rule).
- `net` merged again at f0a6d9a: Git smart-HTTP and the OCI distribution adapter (the official conformance suite passes 64/64), with `ensure_live` adopted.

## API changes
- **v1 (2026-11-15):** first stable shapes. `Backend` lives in `pico2peta_core::backend`. It is create-only (`put_if_absent` with `PutSource::{Bytes, File}`) and adds `get_range(key, Range)`, `open` (streaming), `head`, `list -> Vec<BlobMeta>`, `delete` and `describe`. New error variants are `KeyNotFound(String)` and `Backend { transient, msg }`. `ObjectStore`/`Repository` take `&self` everywhere, and `open()` returns a `'static` streaming reader. Every backend must pass `backend::conformance::run_all`.
  These changes cover all three of the net session's "Requests for core" from 2026-11-15: streaming put, a backend error variant, and committing early.

- **v1.1 (2026-11-15), additive:** `Error::Unauthorized(String)` (code `"unauthorized"`) for authn/authz failures, requested by net. The CLI exits with 5. Exhaustive `match`es on `Error` need the new arm.
- **v1.2 (2026-11-15), additive:** `Repository::replace_many(updates: &[(Name, Hash)], read_set) -> ReplaceManyOutcome { Ok { versions } | Conflict { stale } }`. It's atomic (one journal record, one commit point) and checks rules per name. The default impl handles single updates and returns `Unsupported` otherwise; `LocalRepo` implements it fully. It's intended for git `push --atomic` and OCI manifest+tag.
- **v1.3 (2026-11-15), additive:** `ObjectStore::ensure_live(hash) -> bool`, the existence check to use before *referencing* an object without putting it again. If GC has condemned the only copy, it rewrites the object (recursively through its manifest tree) into live storage. The default impl is `contains`. `LocalRepo::replace`/`replace_many` use it, and OCI/Git adapters should too (requested by net).
- **Pack format v2 (`P2PPACK2`):** a 16-byte random nonce after the magic. This fixes a real aliasing bug: a pack holding exactly the same records as a condemned pack got the same content-derived name, so `put_if_absent` returned AlreadyExists and GC then deleted the "new" copy. v1 packs and staging files are still read.
- **v1.4 (2026-11-15), additive (repo crate):** `Grouper`/`GroupKey`, `LocalRepo::gc_with`, and new `GcOptions` fields (build it with `..Default::default()`).
- **v1.5 (2026-11-15), additive:** `Repository::delete_ref(name, read_set) -> ReplaceOutcome`. It's OCC and obeys rules: create-once and `releases/*` names can't be deleted. Tombstones keep the version sequence, so a re-create continues at v+1. The default impl returns `Unsupported`; `LocalRepo` implements it. The CLI has `pico2peta delete NAME --expect V`. It's covered by the Lean model (proofs re-checked) and by the Lean↔Rust difftest.
- **Hierarchical manifests:** `PackConfig::manifest_fanout` (default 4096; repo config key `pack.manifest_fanout`, optional). Every interior node is keyed by the content hash of its byte range, and reads expand the tree lazily.

## Production gaps
- **Index in memory.** Every handle loads all `.idx` files into a HashMap, about 100 B/object, which is fine up to ~10^7–10^8 objects. Billions need a fan-out/on-disk index or a PostgreSQL index (an open question in the brief).
- **GC assumes an operation lease.** A `put` → `replace` must finish within the grace period (default 1 h). There's no enforcement yet; an epoch/lease token on `replace` would make it checkable.
- **GC tracer.** GC treats objects as opaque, so only ref targets and their chunks are roots. Structured objects (directories, snapshots) need a `Tracer` once those formats exist.
- **Provenance index scale.** `Provenance::load` walks the whole log, O(records). Large deployments need persisted forward and backward index segments.
- **Ref store scale.** All refs live in memory (~150 B each), and compaction rewrites a full snapshot. That's fine to ~10^6 names, strained at 10^7. Scaling needs a sharded journal per prefix, or an LSM. Net was advised to keep per-blob OCI links in segment maps, not refs.
- **Small packs.** `replace` and `flush` seal the writer's open pack, so chatty writers make many small packs. Repack (M6) is the planned fix.
- **Ambiguous outcome on I/O error.** If an fsync fails during `replace`, the record may still be durable. The caller must re-`resolve` to find out (documented, as in most databases).

## Environment notes
- `source scripts/env.sh` puts the cargo target, `CARGO_HOME`, TMPDIR and bench data on `/mnt/scratch/pico2peta`, and adds the shared Lean 4.31 to PATH.

## Blockers
- (resolved 2026-11-15) `/mnt/scratch` hit 100%, and Exp1 died with ENOSPC at redis V0. I freed my own regenerable caches (`cargo-target/debug` 7.7 GB, `cargo-target-fuzz`) and made debug builds leaner in `scripts/env.sh` (no incremental cache, line-tables-only debug info). Exp1 was restarted for redis and git (`exp1-run2.log`).

## Questions for human
- **Disk budget:** `/mnt/scratch` has much less headroom than the ~31 GB in AGENT_TASK.md (24 GB → 13 GB → 0 during this session, partly from other directories). Could you free or extend space, or say which of these may be deleted: the Exp1 mirrors and spill (`bench-data`, ~1.5 GB), the fuzz build cache, the nightly toolchain (`rustup-nightly`)?
- **Disk headroom** is still tight: about 11 GB free after cleanup, and the net session's `cargo-target-net` alone is 13 GB.
