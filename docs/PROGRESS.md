# Progress log

## Status (2026-11-15). Start here.

| milestone | state | where |
|---|---|---|
| M0 Plan | done; Exp1 thresholds frozen | `docs/PLAN.md` |
| M1 Core API | done; v1.9 now, all additive since v1 (see API changes) | `crates/core` |
| M2 Pack store | done | `crates/pack` |
| M3 Ref store | done: local journal engine plus a multi-host log engine on any Backend | `crates/refs` |
| M4 Local repo + CLI | done; local and `--remote` | `crates/repo`, `crates/cli` |
| M5 Experiment 1 | **complete: FAIL** by the pre-registered rules. V2 geomean is 1.051×, but redis is 1.336× > the 1.30 max (ripgrep 0.85, jq 0.93, git 1.16); the V3 ceiling is 0.71–0.84×. **Exp1b also FAILS** (V4 redis 1.347 > 1.30, build 1.4–2.7×); git.git row pending. Pre-registered consequence: Git-compatible storage stays the recommendation for source code | `docs/EXP1.md`, `scripts/exp1-report.py`, `scripts/exp1b.sh` |
| M6 GC / repack | done, including grouped repack | `crates/repo/src/gc.rs` |
| N1–N5 (net) | done: S3, server, client, CI, deploy, Git + OCI adapters | `docs/NET_PROGRESS.md`, `docs/API.md` |
| M7 Hardening | done (two independent code reviews, all findings fixed) except quiet-host benchmark numbers | `fuzz/`, `docs/BENCH.md`, `scripts/check.sh` |
| M8 Verification | done: Lean P1–P5 and a Lean↔Rust difftest | `lean/` |
| M9 Later layers | provenance (core) done; Git/OCI adapters done (net) | `crates/prov` |

**Storage engine**
- Packs are content-addressed and create-only. Each writer has its own flocked staging file, and orphans are adopted on open.
- Large objects use FastCDC chunks under a hierarchical manifest tree.
- Frames hold similarity-grouped small objects (frame v2), and XOR-delta objects store model checkpoints.
- Index v3 is ~44 B/entry. Big packs have memory-mapped indexes, plus a multi-pack index.
- Every read verifies its CRC and BLAKE3 hash.

**Names**
- An OCC journal with an exclusive-flock commit point, snapshot/rotation compaction, rules whose immutability can't be retracted, deletes with tombstoned versions, and atomic multi-name transactions.
- Optionally (`[refs] store = "backend"`), a create-only log on S3 with per-attempt nonces, so several servers can share one repository. The net session verified it with two servers on MinIO.

**GC**
- Phases: plan → sweep (optionally grouped repack) → delete. The phases are separated by grace periods, and copies are rescued before deletion.
- `ensure_live` makes slow writers safe.

**Provenance**
- An OCC log with checkpointed indexes, lineage and impact queries, and GC that keeps inputs but lets outputs be regenerated.

**Testing**
- Crash sweeps at every fault point: packs, refs, compaction and GC.
- Real SIGKILL tests through the CLI.
- Multi-process contention tests.
- Model-based proptests, never-panic decoder tests, cargo-fuzz targets for every decoder (all clean), and `fsck`.

**Independent code review (2026-11-15).** A fresh-eyes review of pack, refs and GC found 10 confirmed bugs. All are fixed with regression tests (see CHANGELOG). The most severe: acknowledged large batches truncated as torn tails; mid-journal corruption silently truncated; a first-compaction orphan-journal race; stale log-engine handles acknowledging invisible commits; appends to a published pack after a failed seal. The GC protocol itself came out clean.

**Notable bugs found and fixed by the tests**
- A seal/condemn index race.
- Pack-name aliasing with condemned packs.
- Pack ids invalidated by index rebuilds.
- The benchmark generator producing duplicate inputs.
- Lean `setRule` idempotency.

## API changes
- **v1 (2026-11-15):** first stable shapes. `Backend` lives in `pico2peta_core::backend`. It is create-only (`put_if_absent` with `PutSource::{Bytes, File}`) and adds `get_range(key, Range)`, `open` (streaming), `head`, `list -> Vec<BlobMeta>`, `delete` and `describe`. New error variants are `KeyNotFound(String)` and `Backend { transient, msg }`. `ObjectStore`/`Repository` take `&self` everywhere, and `open()` returns a `'static` streaming reader. Every backend must pass `backend::conformance::run_all`.
  These changes cover all three of the net session's "Requests for core" from 2026-11-15: streaming put, a backend error variant, and committing early.

- **v1.1 (2026-11-15), additive:** `Error::Unauthorized(String)` (code `"unauthorized"`) for authn/authz failures, requested by net. The CLI exits with 5. Exhaustive `match`es on `Error` need the new arm.
- **v1.2 (2026-11-15), additive:** `Repository::replace_many(updates: &[(Name, Hash)], read_set) -> ReplaceManyOutcome { Ok { versions } | Conflict { stale } }`. It's atomic (one journal record, one commit point) and checks rules per name. The default impl handles single updates and returns `Unsupported` otherwise; `LocalRepo` implements it fully. It's intended for git `push --atomic` and OCI manifest+tag.
- **v1.3 (2026-11-15), additive:** `ObjectStore::ensure_live(hash) -> bool`, the existence check to use before *referencing* an object without putting it again. If GC has condemned the only copy, it rewrites the object (recursively through its manifest tree) into live storage. The default impl is `contains`. `LocalRepo::replace`/`replace_many` use it, and OCI/Git adapters should too (requested by net).
- **Pack format v2 (`P2PPACK2`):** a 16-byte random nonce after the magic. This fixes a real aliasing bug: a pack holding exactly the same records as a condemned pack got the same content-derived name, so `put_if_absent` returned AlreadyExists and GC then deleted the "new" copy. v1 packs and staging files are still read.
- **v1.4 (2026-11-15), additive (repo crate):** `Grouper`/`GroupKey`, `LocalRepo::gc_with`, and new `GcOptions` fields (build it with `..Default::default()`).
- **v1.5 (2026-11-15), additive:** `Repository::delete_ref(name, read_set) -> ReplaceOutcome`. It's OCC and obeys rules: create-once and `releases/*` names can't be deleted. Tombstones keep the version sequence, so a re-create continues at v+1. The default impl returns `Unsupported`; `LocalRepo` implements it. The CLI has `pico2peta delete NAME --expect V`. It's covered by the Lean model (proofs re-checked) and by the Lean↔Rust difftest.
- **v1.6 (2026-11-15), additive:** `Repository::update_many(updates: &[(Name, Option<Hash>)], read_set)`, where `None` = delete. It's atomic (one `Txn` journal record), and each entry follows the single-name rules. The default delegates to `replace_many` when there are no deletes. It's for `git push --atomic` with deletions (requested by net).
- **Frame v2 / index v3 (on-disk):** frame directories hold lengths only (hashes are recomputed on recovery), and the index uses a shared record table (~44 B/entry vs 62). Older formats are still read. On net's real ripgrep import, a grouped repack is now **1.078× git-aggressive total, 1.021× payload** (it was 1.264× / 1.154×).
- **v1.7 (2026-11-15), additive:** `ObjectStore::put_delta(base, src)` stores an object as `base XOR delta` (record kind `Delta`), keeping content-hash identity. Reads, ranges, GC (base kept alive) and `ensure_live` cover it. The default impl is a plain `put_stream`. The CLI has `put --delta-base HASH`.
- **v1.8 (2026-11-15), additive:** `ObjectStore::put_delta_mapped(base, segments, src)` is an XOR delta with an explicit `(dst, src, len)` alignment. The default delegates to `put_delta`. The new crate `pico2peta-weights` (`put_checkpoint`) plans the alignment for safetensors.
- **Refs on a backend (2026-11-15):** `RefStore::open_backend(backend, prefix)` and `LocalRepo` config `[refs] store = "backend"`, plus `pico2peta init --refs-on-backend`. The API is unchanged; this is a new engine.
- **v1.9 (2026-11-15), additive:** `Backend::atomic_create() -> bool` (default `true`). Backends that emulate create-only with check-then-put (S3 without conditional writes) must return `false`. The ref log engine refuses them.
- **Hierarchical manifests:** `PackConfig::manifest_fanout` (default 4096; repo config key `pack.manifest_fanout`, optional). Every interior node is keyed by the content hash of its byte range, and reads expand the tree lazily.

## Production gaps
- **Index scale (largely addressed).** Large pack indexes (≥ 4 MiB, ~95k entries) are memory-mapped from a verified local cache (`idx-cache/*.idx3`) and binary-searched. Small packs and the open pack stay in a HashMap. Per-object heap cost is now zero for big packs; the page cache holds the 44 B/entry. A **multi-pack index** (`P2PMIDX1`) is built locally, by a k-way merge with O(#packs) memory and outside the index lock, once ≥ 8 large packs exist and > 10% are uncovered. Lookups are then one binary search plus the few uncovered packs. That answers the brief's open question with "files alongside packs"; PostgreSQL isn't needed for lookups. Remaining: MIDX rebuilds are O(n) and triggered by the coverage rule; a production deployment at 10^9+ objects would want them scheduled with GC.
- **GC timing assumption (narrowed).** Since `replace` calls `ensure_live`, a slow put→replace is safe: the object is rewritten if condemned, or the replace fails with `MissingObject` if it's gone. What's left to assume is that ensure_live→commit (milliseconds) plus the 60 s dedup refresh interval are far shorter than the grace period (≥ 10 min, enforced). Adapters must `ensure_live` indirectly referenced children (Git and OCI do).
- **GC tracer.** GC treats objects as opaque, so only ref targets and their chunks are roots. Structured objects (directories, snapshots) need a `Tracer` once those formats exist.
- **Model weights: XOR delta exists, but isn't automatic.** `put_delta(base, …)` (API v1.7) stores a version as an XOR delta: −65% on a synthetic full fine-tune (see BENCH.md). The CLI can take the base from a ref (`put --delta-from-ref models/heads/main`). **Tensor-aware alignment is done** (API v1.8 `put_delta_mapped` plus the `pico2peta-weights` safetensors planner, used automatically by `put --delta-*`). Reordered, inserted or resized tensors and header edits keep the delta small. Remaining: dtype-aware transforms (e.g. bf16 exponent/mantissa splitting before compression, as ZipLLM does) aren't done.
- **Provenance scale (addressed).** Log nodes (`P2PPLOG2`) carry checkpoint indexes (`P2PPIDX1`, sorted forward and backward tables), rebuilt geometrically when recent records reach max(1024, total/8). `Provenance::load` reads only the recent tail. Older lookups are O(log n) ranged reads, and records are fetched on demand. Each rebuild is a full rewrite, but the geometric rule (rebuild once recent ≥ total/8) keeps the total amortized: about 9× write amplification over the life of the log, O(n) overall. A tiered LSM could lower that constant, but there's no known need.
- **Ref store scale.** All refs live in memory (~150 B each), and compaction rewrites a full snapshot. That's fine to ~10^6 names, strained at 10^7. Scaling needs a sharded journal per prefix, or an LSM. Net was advised to keep per-blob OCI links in segment maps, not refs.
- **Small packs.** `replace` and `flush` seal the writer's open pack, so chatty writers make many small packs. GC consolidates packs under 16 MiB (`small_pack_bytes`). Run GC regularly (cron, or the server's background GC).
- **Multi-host refs (addressed).** `[refs] store = "backend"` keeps refs as a create-only log on the pack backend (e.g. S3), with commits linearized by conditional create. It's tested against the model and the Lean difftest, plus multi-process contention and a crash sweep. Remaining: per-commit cost is one PUT plus catch-up GETs (fine for refs, slower than the local journal), and log trimming relies on a 1 h grace like GC.
- **Ambiguous outcome on I/O error.** If an fsync fails during `replace`, the record may still be durable. The caller must re-`resolve` to find out (documented, as in most databases).

## Environment notes
- `source scripts/env.sh` puts the cargo target, `CARGO_HOME`, TMPDIR and bench data on `/mnt/scratch/pico2peta`, and adds the shared Lean 4.31 to PATH.

## Blockers
- (resolved 2026-11-15) `/mnt/scratch` hit 100%, and Exp1 died with ENOSPC at redis V0. I freed my own regenerable caches (`cargo-target/debug` 7.7 GB, `cargo-target-fuzz`) and made debug builds leaner in `scripts/env.sh` (no incremental cache, line-tables-only debug info). Exp1 was restarted for redis and git (`exp1-run2.log`).

## Questions for human
- **Disk budget:** `/mnt/scratch` has much less headroom than the ~31 GB in AGENT_TASK.md (24 GB → 13 GB → 0 during this session, partly from other directories). Could you free or extend space, or say which of these may be deleted: the Exp1 mirrors and spill (`bench-data`, ~1.5 GB), the fuzz build cache, the nightly toolchain (`rustup-nightly`)?
- **Disk headroom** is still tight: it has dipped below 8 GB twice. Both sessions now clean their build caches regularly.
- **Exp1 failed**, so grouped repack stays opt-in (`[gc] grouped = false` by default, per the pre-registered rule), and Exp1b is queued. Nothing is needed from you unless you want to override the pre-registered consequences.
