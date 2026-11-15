# Progress log

## Status (2026-11-15). Start here.

**Correction (2026-11-15).** An earlier "all milestones complete and verified" was wrong. The human's independent check found two problems:
1. **`scripts/check.sh` was a broken gate.** Under `set -e`, its final `! grep` line never aborted, and `cargo test` stopped at the first failing binary. It printed "check: OK" while a test failed. **Fixed:** it now uses `--no-fail-fast`, checks cargo's exit status explicitly, and greps for failures explicitly. Verified by a deliberately failing probe test, which the gate now rejects (exit 1).
2. **The SIGKILL test was flaky (about 4 of 6 runs failed), from a real engine bug.**
   - What happened: GC deleted chunks of an *unreferenced* large object (left by a killed streaming put) while that object's manifest survived in a pack outside the GC cycle. fsck then reported a manifest with a missing chunk, and the index served an object that couldn't be read.
   - **Root cause fixed in the engine:** GC now protects every child of a manifest or delta record in a surviving pack (sweep copies them, the delete phase rescues them). `ensure_live` reports a broken object as absent (so clients re-upload, which heals it) rather than erroring.
   - Deterministic regression test: `gc_never_deletes_children_of_surviving_records`. It fails on the old GC and passes now.
   - The kill test passes **50/50** (before: ~2/6). The full workspace suite passes **5/5** with `--no-fail-fast`, and no other flaky test was found.

Milestones M0–M9 are complete. `scripts/check.sh` passes.

**Speed-up work (`docs/RESEARCH_SPEEDUP.md`, from the human; split with net on 2026-11-15)**
- **§1 (core): done.** Streaming put pipeline, compressibility probe, incremental pack hash. On tmpfs (CPU-bound), incompressible `put_stream` went from 331 to **739 MB/s** (2.2×; median of 5 interleaved reps on a loaded host, load 2.9–5.2), text from 228 to 328 MB/s, dedup re-put from ~630 to ~900 MB/s. On NVMe, incompressible stays at the device's ~200 MB/s fsync ceiling. **§1a's ≥ 800 MB/s target was not met** (best run 791 MB/s); text is compression-bound in the workers. The probe gave no measurable gain (zstd already bails out fast on random data). Details, profiles and the ablation: `docs/BENCH.md`.
- **§2 byte grouping: done.** Net built the transform (`crates/weights/src/bytegroup.rs`) and measured it; the core added record codec 2 `GroupedZstd` (`width | zstd(group(chunk))`), the hint plumbing (API v1.12) and `pack.grouped_zstd_level` (default 6). Measured end to end through the CLI on SmolLM2-135M → -Instruct (real bf16, 269 MB each; byte-exact round trip, fsck clean, identical hashes in every mode):

  | stored bytes | plain zstd-3 | grouped, level 3 | grouped, level 6 (default) | grouped, level 15 |
  |---|---|---|---|---|
  | fine-tune as XOR delta (`put --delta-from-ref`) | 171,955,039 | 155,694,257 (−9.5%) | 153,780,813 (**−10.6%**) | 149,095,805 (−13.3%) |
  | base checkpoint (`put`) | 211,073,250 | 202,894,272 (−3.9%) | 198,508,585 (**−6.0%**) | 198,358,259 (−6.0%) |
  | put time, base + delta (unlocked, load ~6: indicative) | 1.4 + 3.2 s | | 3.8 + 3.7 s | 26.5 + 31.7 s |

  - **Why a separate level:** grouped data keeps shrinking with zstd level where plain zstd doesn't (net's finding, confirmed). Level 6 runs about 90 MB/s per core, so three workers keep up with this host's ~200 MB/s disk. Level 15 is about 8× slower for 2.7 more points on deltas and nothing on the base, so it's an opt-in (`init --grouped-zstd-level 15`) for write-once archives.
  - **Per-chunk vs per-tensor grouping (measured, `p2p-bench bytegroup-gran`):** net's first numbers grouped each whole tensor and then cut 64 KiB blocks, so a block holds a single byte plane. The record codec groups each chunk on its own, so every record stays independently decodable (ranged reads, CDC dedup). At zstd-3 on the same tensors: per-block −1.9% (bf16) / −0.6% (fp32 GPT-2), per-tensor −9.4% / −8.0%. Net has corrected NET_PROGRESS with both granularities.
  - *Correction:* a level sweep on only the first 64 MB of tensor data suggested that level 15 alone would reach the per-tensor gain on raw weights. The full-file measurement above doesn't bear that out (base −6.0% at both L6 and L15), so the sweep wasn't representative.
  - **Where net's −10% on raw weights comes from (measured, `p2p-bench group-align`, SmolLM2 base, whole file):**

    | grouped vs plain whole-file FastCDC | zstd-6 | zstd-15 | zstd-19 |
    |---|---|---|---|
    | whole-file FastCDC (the store today) | −6.0% | −6.2% | −5.8% |
    | FastCDC restarted inside each tensor | −5.9% | −6.3% | −6.0% |
    | tensor-aligned fixed 64 KiB blocks | −4.9% | −9.8% | −9.9% |

    The gain needs *both* fixed tensor-aligned blocks *and* a high level (≥ 15). Tensor-aligned CDC alone gains nothing, and so does a higher level alone. Level 15 runs about 12 MB/s per core, so this could only be an opt-in archival mode for weights. It is not implemented. I haven't established why fixed blocks benefit only at high levels.
  - **Possible follow-up (not done):** on raw checkpoints, an object-level transform (a record kind like `Delta`: object = ungroup(child) per tensor region, child chunked as usual; ranged reads gather from `width` planes) would get the per-tensor gain on raw checkpoints. It's a format addition, so it's left as a decision rather than added now.
- **§3 Experiment 1c:** net's (PLAN §4.9, pre-registered before any 1c code). ripgrep and jq already FAIL; redis and git.git are running.
- **§4 chunker:** net built `crates/chunk` (byte-exact Xet, conformance-tested on 796 reference boundaries). **The core wired it in as a per-namespace option** (`[chunking]`, API v1.11). FastCDC stays the default and every object stays readable whatever chunker wrote it. Trade-off below.

**§4 trade-off: Xet vs FastCDC chunking (measured 2026-11-15 with `p2p-bench chunkers` on SmolLM2-135M → -Instruct, real bf16 safetensors, 269 MB each; sizes only)**

| chunker | chunks | mean / max chunk | fine-tune deduped vs base | base + 777 B inserted at 1 MiB, deduped | index + manifest overhead |
|---|---|---|---|---|---|
| FastCDC 16/64/256K (default) | 3,383 | 77.7 / 256 KiB | 0.01% | 99.98% | 0.106% |
| Xet 8/64/128K | 4,202 | 62.5 / 128 KiB | 0.00% | 99.98% | 0.131% |

- **For Xet:** chunk boundaries are byte-identical to Hugging Face Xet's, the prerequisite for dedup against HF-hosted content and for a future Xet protocol adapter. Boundaries alone don't make chunk *ids* match: Xet names chunks with its own hash (I believe a keyed BLAKE3, not checked against the spec in this session) and groups them into xorbs, so an adapter would still need a mapping, as the Git/OCI adapters have.
- **Against Xet:** about 24% more chunks for the same data, so slightly more index and manifest metadata (0.13% vs 0.11% of the bytes here) and more per-chunk work. Shift resistance is the same. Speed (net's `chunk-bench`, 512 MiB random per thread, best of 3 under the bench lock): Xet 1.41 GB/s on 1 thread / 3.22 GB/s on 4, FastCDC 1.96 / 4.50 GB/s, so Xet is about 0.72–0.80× FastCDC (the ratio was stable while absolute numbers swung with host load). Both are far above `put_stream`'s rate, so the chunker choice doesn't limit put throughput.
- **Neither** dedups a fine-tune at chunk level (every weight changes); that's what the XOR-delta path (`put --delta-from-ref`) is for, and byte grouping (§2) targets.
- **Content written with different chunkers doesn't share chunks.** So the choice is per namespace (`[chunking.prefixes]`) and should be made once per kind of data; changing it later is safe (only new writes are affected) but costs dedup against older versions.
- **Decision:** FastCDC stays the default. Use `--chunk models/=xet` for namespaces that mirror or exchange content with Hugging Face.

| milestone | state | where |
|---|---|---|
| M0 Plan | done; Exp1 thresholds frozen | `docs/PLAN.md` |
| M1 Core API | done; v1.13 now, all additive since v1 (see API changes) | `crates/core` |
| M2 Pack store | done | `crates/pack` |
| M3 Ref store | done: local journal engine plus a multi-host log engine on any Backend | `crates/refs` |
| M4 Local repo + CLI | done; local and `--remote` | `crates/repo`, `crates/cli` |
| M5 Experiment 1 | **complete: FAIL** by the pre-registered rules. V2 geomean is 1.051×, but redis is 1.336× > the 1.30 max (ripgrep 0.85, jq 0.93, git 1.16); the V3 ceiling is 0.71–0.84×. **Exp1b complete: FAIL** (V4 geomean 1.386, max 2.160 on git.git; build 1.2–2.7× Git's). Pre-registered consequence: Git-compatible storage stays the recommendation for source code | `docs/EXP1.md`, `scripts/exp1-report.py`, `scripts/exp1b.sh` |
| M6 GC / repack | done, including grouped repack | `crates/repo/src/gc.rs` |
| N1–N5 (net) | done: S3, server, client, CI, deploy, Git + OCI adapters | `docs/NET_PROGRESS.md`, `docs/API.md` |
| M7 Hardening | done: two independent code reviews (all findings fixed), and quiet-host benchmark numbers recorded | `fuzz/`, `docs/BENCH.md`, `scripts/check.sh` |
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

**Notable bugs found and fixed by the tests** (the most recent was found by the human: GC partially collecting unreferenced large objects; see the correction above)
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
- **v1.10 (2026-11-15), additive:** `PackConfig::compress_probe` (default `true`; repo config key `pack.compress_probe`, optional, so existing config.toml files still load). Records of at least 8 KiB whose first 4 KiB shrink by less than 3% under zstd-1 are stored raw without a full compression attempt. It changes only the codec choice, never identity or the format. Build `PackConfig` with `..Default::default()`/`..PackConfig::tiny()` (all current callers already do).
- **v1.11 (2026-11-15), additive (pack + repo):** `ChunkerKind { FastCdc, Xet }` and `PackStore::put_stream_with(kind, src)`; repo config `[chunking]` (`default`, `prefixes` = name prefix → `"fastcdc"`/`"xet"`, longest prefix wins; optional) with `ChunkingSection`/`ChunkerChoice`, `LocalRepo::init_config(root, &RepoConfig)`, `LocalRepo::put_stream_for(name, src)` and `LocalRepo::chunking()`. CLI: `init --chunk PREFIX=CHUNKER` (repeatable), `--chunk-default`, `put --for-name NAME`. `RepoConfig` gained a field: build it with all fields (it has no `Default`). `ObjectStore::put_stream` on `LocalRepo` uses `[chunking] default`. The Xet chunker itself is net's `crates/chunk`. **For net:** the server can call `put_stream_for` for named uploads (OCI/Git adapters, `/v1` puts that carry a name).
- **v1.12 (2026-11-15), additive:** `ObjectStore::put_stream_grouped(src, width)` and `ObjectStore::put_delta_mapped_grouped(base, segs, src, width)`, a storage hint for fixed-width numeric data. Defaults ignore the hint, so other implementors (`RemoteRepo`, `MemStore`) are unaffected. Pack: `PutOpts { chunker, group_width }`, `PackStore::put_stream_opts`, `put_delta_opts`; repo: `LocalRepo::put_stream_for_grouped`. Weights: `group_width(head)`, `put_weights(store, src)`, `sniff(src)`, `put_checkpoint_opts(.., byte_group)`; `put_checkpoint` now passes the hint. CLI: `put` byte-groups safetensors automatically (`--no-byte-group` to opt out). **On-disk:** record codec 2 `GroupedZstd` (blobs only). Readers older than this reject it as an unknown codec (fail closed, never misread).
- **v1.13 (2026-11-15), additive:** `PackConfig::grouped_zstd_level` (default `DEFAULT_GROUPED_ZSTD_LEVEL` = 6; repo config key `pack.grouped_zstd_level`, optional; CLI `init --grouped-zstd-level`). It applies only to codec-2 (byte-grouped) records. Code that builds `PackConfig` with a full struct literal must add the field; `..Default::default()` callers are unaffected.

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
