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

**Process slip (2026-11-15, fixed):** commit `40e4194` (read-ahead) was pushed with a failing gate (one clippy `type_complexity` error). I had piped `scripts/check.sh` into `tail`, which hid its exit status. It was fixed and pushed in the next commit, with the gate's exit status checked directly. No test failed; it was a lint.

**Phase 3 (docs/VISION.md, PLAN §6; 2026-11-15). Implementation first; measurements wait for the batch phase.**

| item | state |
|---|---|
| P1 Front API (core) | **done**: API v1.14–v1.16. Profiles, fronts, nested prefix claims, front handles, the idmap, `put_named`, per-GC tracer snapshots. Net ported Git, OCI, HF (incl. Xet), PyPI, npm and Maven onto it. |
| Ordering building block (ORDERING.md, replaces step 3) | **built and evaluated.** Experiment 3 (§6.6, official run 2026-11-15) is a pre-registered **FAIL on jq's build time only** (re-timed 1.179 > 1.0). Size is a **STRONG PASS**: V\* = A3 geomean 0.842, max 0.947, and Git storage parity holds on every repo including the held-out redis (0.947) and git.git (0.796). Reads, RSS, and builds on the other three repos pass. A4 (no hints) is within 0.4 % of A3. Generalization: packages yes (0.806×), weights no (0.999×; that's the XOR-delta path's job). It ships as the repack layout (it beats A0 everywhere). Details: `docs/EXP1.md`. |
| Exp3 consequence (human decision, 2026-11-15) | The recorded verdict stays FAIL (jq build time). **The human overrode the consequence:** A3's similarity layout is the default repack layout for every domain (on by default, can be disabled). Future build-time conditions: geomean ≤ 1.0× and every repo ≤ 1.25× (PLAN general amendment). |
| Generic front (§6.4) | **run 2026-11-16, median of 3** (BENCH.md): durable write 1.002× dd+fsync (PASS), 4 KiB range p99 0.112 ms (PASS), 10 versions with scattered edits 1.057 GiB (PASS) and with inserts 1.017 GiB (PASS). **Verified read FAIL**: 0.184× (strict) and 0.198× (eof) of warm `cat`, since one verified stream reads 1.4–2.2 GB/s against a 7–12 GB/s page-cache copy. Follow-up (not started): parallel whole-object BLAKE3 over large buffers. |
| Perf pass (a)–(e) | (a) put is CPU-bound by host contention (schedstat), no change; (b) verified read 870 → 1,880 MB/s; (c) 1 KiB put 75k → 129k/s, get 97k → 286k/s; (d) group commit 1.76× at 4 threads, 2.04× at 8; (e) net (clone wire bytes 0.98× git-daemon in their smoke run). Plus: frame prefix decoding and a per-thread zstd decompression context (net's profile). |
| Chunk-anchored XOR deltas | **done** (v1.16): generic derived objects for edits and insertions. |
| P5 cross-domain (ROADMAP item 3) | **done** (core; net added the server endpoints, and the CLI's `--remote` uses them):
  - **attestations and promotion** (`crates/attest`: in-toto Statement v1 in DSSE envelopes, Ed25519 behind `Signer`/`Verifier` traits, stored as provenance records; `promote(src → dst, policy)` as one OCC transaction with required signatures and provenance, audited as a `promote` record);
  - **search** (`o5s_prov::search`: exact, prefix and full text over names and metadata, incremental from the append-only log);
  - **retention** (`keep_versions` / `only_if_regenerable` per profile, `o5s_prov::retention::apply`, rules respected, dry run exact);
  - **cross-domain dedup** test: the same 16 MiB stored raw, as a Git blob and inside a tar layer costs **+1.26 %** for the two extra framings (`crates/repo/tests/dedup.rs`; compressed containers aren't claimed).

  - **cross-domain provenance by name** (`lineage_named` / `impact_named`, tested on a git → pypi → oci → hf chain) and the CLI commands `search`, `lineage`, `impact` and `retention [--dry-run]` (`init --profile ...,keep_versions:N`).

  **Item 3 is done on the core side.** Server endpoints for promote, attest and search are net's ROADMAP item 3. |
| Put/get throughput for the fronts (human priority, 2026-11-15/16) | **done on the core side** (BENCH.md, put/get pass parts 1–3). `put_many`: 2.0× small-object ingest; with net's parallel index-pack, a redis first push is 1.08–1.15× git daemon (was ~2.7×). Reads of incompressible blobs are 1.2–1.5× faster: one CRC instead of two, none on hash-verified blobs, no second allocation. `put_concat`/`put_chunked` are for the Xet shard and xorb paths. Kept by decision: the strict whole-object hash and the GC-safety fstat. Fast ingest level: level 3 stays the default (human, 2026-11-16); `init --zstd-level 1` plus `gc --repack-all` is the documented opt-in. |
| **h5i rebuild (docs/H5I_PLAN.md, from the human 2026-11-16)** | **H0 done**: `baseline-pre-h5i` tagged (1b8f608); docs/BASELINE.md; PLAN §6.8 rule. **H1 done.** **H2, core side, done**: every core decision is a kernel in `crates/kernel`, extracted with Charon/Aeneas and called by its shell. That covers the name grammar and governance, the OCC batch decision, front ownership, promotion, retention, GC victims and verify-before-delete, the read digest check and put dedup, the pack record header decoder, and the journal framing. `verify/core` proves P1–P3 and P5–P7 plus totality of every kernel function: 182 theorems on the extracted code, standard axioms only. `h5i app mutate` caught 17/17. The hand model (`lean/`) and its difftest are deleted. Trusted, as documented in `verify/core/README.md`: BLAKE3, ed25519, the backend's create-only puts, fsync/flock, the shells' view building (tested), and the ref log's timing protocol (multi-host and fault tests). P4 is net's (`verify/net`). **H3, core side, run on `cbbbb8a`** (BENCH 2026-11-16): two real regressions found and fixed (name validation copy, commit-path allocations). Exp3 sizes 28/28 byte-identical. Three FAILs by the letter of §6.8, flagged to the human with A/B evidence: ingest put −5.9%, §6.4 durable-write tier 0.886× (dd faster), redis A3 peak RSS +14%. The human ruled on the three FAILs: one pre-declared re-measurement each, in the final batch (PLAN §6.8 amendment). **v0.5.5 switch done** (eb357b9): every h5i crate comes from crates.io at `=0.5.5`, the CLI is the official v0.5.5, and the extraction is byte-identical. The re-plan put H4 before any further H3. **H4, core side, done:**
- multi-core whole-object BLAKE3 for verified reads, exactly the serial hash, proptested (d6bfd5f, with the d00980b fix for compressible reads that net caught);
- reach and GC no longer trace chunk edges (9aba438);
- the reach worker drains additions only (eb357b9).

A parallel Xet upload cutter was tried and reverted (c155ab3): it lost the pipeline overlap. §6.4 strict read rose from 0.17× to 0.22× of `cat`, short of 0.50× (question for the human). **Next:** the human's answers on net's two HF questions, then net H4 done, then the one final batch (core windows, the three rechecks, then net's §6.5 A and B), then the RC. |
| Scale (ROADMAP item 4) | **done within the disk budget.** Windows 1–2 (BENCH.md): every §6.7 target met except plain RSS per object (110 B; heap is 3.4 B/object, the rest is mapped index pages). Three bottlenecks fixed: quadratic `replace_many`, idmap ranged lookups (net), and the duplicate index copy in `idx-cache` (now a hard link: 292 → 226 B/object on disk, cold open 6.6 → 3.6 s at 10⁷). 10⁸ objects **not run**: ≈ 22.6 GB doesn't fit the pre-registered disk rule. Limits are in ARCHITECTURE.md. |
| Security review, core (ROADMAP item 6) | **in progress.** Notes and fixed findings: `docs/SECURITY.md` (bounded manifest/delta decoding, envelope size checked before the read, fd-cache deleted-pack check, group-commit deadlock). New fuzz targets (`frames`, `attestations`, `provenance`, `delta_config`, `search_index`) ran clean for 120 s each under ASan (2026-11-15): search_index 26.6k runs, frames 12.6M, attestations 2.6M, provenance 10.4M, delta_config 456k. Fuzz smoke of all 20 targets, 60 s each under ASan (2026-11-16): 19 clean, from 13k runs (search_index) to 10M (manifest). `oci_parsers` (net's front) found a panic on a non-ASCII digest (SECURITY.md finding 6, fixed by net in c8f9942). Remaining: longer campaigns. |

**Speed-up work (`docs/RESEARCH_SPEEDUP.md`, from the human; split with net on 2026-11-15)**
- **§1 (core): done.** Streaming put pipeline, compressibility probe, incremental pack hash. On tmpfs (CPU-bound), incompressible `put_stream` went from 331 to **739 MB/s** (2.2×; median of 5 interleaved reps on a loaded host, load 2.9–5.2), text from 228 to 328 MB/s, dedup re-put from ~630 to ~900 MB/s. On NVMe, incompressible stays at the device's ~200 MB/s fsync ceiling. **§1a's ≥ 800 MB/s target was not met** (best run 791 MB/s); text is compression-bound in the workers. The probe gave no measurable gain (zstd already bails out fast on random data). Details, profiles and the ablation: `docs/BENCH.md`.
- **§2 byte grouping: done.** Net built the transform (`crates/weights/src/bytegroup.rs`) and measured it; the core added record codec 2 `GroupedZstd` (`width | zstd(group(chunk))`), the hint plumbing (API v1.12) and `pack.grouped_zstd_level` (default 6). Measured end to end through the CLI on SmolLM2-135M → -Instruct (real bf16, 269 MB each; byte-exact round trip, fsck clean, identical hashes in every mode):

  | stored bytes | plain zstd-3 | grouped, level 3 | grouped, level 6 (default) | grouped, level 15 |
  |---|---|---|---|---|
  | fine-tune as XOR delta (`put --delta-from-ref`) | 171,955,039 | 155,694,257 (−9.5%) | 153,780,813 (**−10.6%**) | 149,095,805 (−13.3%) |
  | base checkpoint (`put`) | 211,073,250 | 202,894,272 (−3.9%) | 198,508,585 (**−6.0%**) | 198,358,259 (−6.0%) |
  | put time, base + delta (unlocked, load ~6: indicative) | 1.4 + 3.2 s | | 3.8 + 3.7 s | 26.5 + 31.7 s |

  - **Why a separate level:** grouped data keeps shrinking with zstd level where plain zstd doesn't (net's finding, confirmed). Level 6 runs about 90 MB/s per core, so three workers keep up with this host's ~200 MB/s disk. Level 15 is about 8× slower for 2.7 more points on deltas and nothing on the base, so it's an opt-in (`init --grouped-zstd-level 15`) for write-once archives.
  - **Per-chunk vs per-tensor grouping (measured, `o5s-bench bytegroup-gran`):** net's first numbers grouped each whole tensor and then cut 64 KiB blocks, so a block holds a single byte plane. The record codec groups each chunk on its own, so every record stays independently decodable (ranged reads, CDC dedup). At zstd-3 on the same tensors: per-block −1.9% (bf16) / −0.6% (fp32 GPT-2), per-tensor −9.4% / −8.0%. Net has corrected NET_PROGRESS with both granularities.
  - *Correction:* a level sweep on only the first 64 MB of tensor data suggested that level 15 alone would reach the per-tensor gain on raw weights. The full-file measurement above doesn't bear that out (base −6.0% at both L6 and L15), so the sweep wasn't representative.
  - **Where net's −10% on raw weights comes from (measured, `o5s-bench group-align`, SmolLM2 base, whole file):**

    | grouped vs plain whole-file FastCDC | zstd-6 | zstd-15 | zstd-19 |
    |---|---|---|---|
    | whole-file FastCDC (the store today) | −6.0% | −6.2% | −5.8% |
    | FastCDC restarted inside each tensor | −5.9% | −6.3% | −6.0% |
    | tensor-aligned fixed 64 KiB blocks | −4.9% | −9.8% | −9.9% |

    The gain needs *both* fixed tensor-aligned blocks *and* a high level (≥ 15). Tensor-aligned CDC alone gains nothing, and so does a higher level alone. Level 15 runs about 12 MB/s per core, so this could only be an opt-in archival mode for weights. It is not implemented. I haven't established why fixed blocks benefit only at high levels.
  - **Possible follow-up (not done):** on raw checkpoints, an object-level transform (a record kind like `Delta`: object = ungroup(child) per tensor region, child chunked as usual; ranged reads gather from `width` planes) would get the per-tensor gain on raw checkpoints. It's a format addition, so it's left as a decision rather than added now.
- **§3 Experiment 1c: complete, FAIL** (net; PLAN §4.9, pre-registered). V\* = V7: R = ripgrep 1.75, jq 1.41, redis 1.63, git 2.27 (limit 1.30 per repo); build 2.1–3.7× Git; reads p99 ≤ 1.5 ms. Diagnosis (net): the depth-50 cap plus "always take the best eligible base" forced long-distance deltas once the nearest predecessor hit the cap. Phase 3's Experiment 2 (PLAN §6.3, V8) handles the cap the way Git does. Details: `docs/EXP1.md`.
- **§4 chunker:** net built `crates/chunk` (byte-exact Xet, conformance-tested on 796 reference boundaries). **The core wired it in as a per-namespace option** (`[chunking]`, API v1.11). FastCDC stays the default and every object stays readable whatever chunker wrote it. Trade-off below.

**§4 trade-off: Xet vs FastCDC chunking (measured 2026-11-15 with `o5s-bench chunkers` on SmolLM2-135M → -Instruct, real bf16 safetensors, 269 MB each; sizes only)**

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
| M1 Core API | done; v1.21 now, all additive since v1 (see API changes) | `crates/core` |
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
- **v1 (2026-11-15):** first stable shapes. `Backend` lives in `o5s_core::backend`. It is create-only (`put_if_absent` with `PutSource::{Bytes, File}`) and adds `get_range(key, Range)`, `open` (streaming), `head`, `list -> Vec<BlobMeta>`, `delete` and `describe`. New error variants are `KeyNotFound(String)` and `Backend { transient, msg }`. `ObjectStore`/`Repository` take `&self` everywhere, and `open()` returns a `'static` streaming reader. Every backend must pass `backend::conformance::run_all`.
  These changes cover all three of the net session's "Requests for core" from 2026-11-15: streaming put, a backend error variant, and committing early.

- **v1.1 (2026-11-15), additive:** `Error::Unauthorized(String)` (code `"unauthorized"`) for authn/authz failures, requested by net. The CLI exits with 5. Exhaustive `match`es on `Error` need the new arm.
- **v1.2 (2026-11-15), additive:** `Repository::replace_many(updates: &[(Name, Hash)], read_set) -> ReplaceManyOutcome { Ok { versions } | Conflict { stale } }`. It's atomic (one journal record, one commit point) and checks rules per name. The default impl handles single updates and returns `Unsupported` otherwise; `LocalRepo` implements it fully. It's intended for git `push --atomic` and OCI manifest+tag.
- **v1.3 (2026-11-15), additive:** `ObjectStore::ensure_live(hash) -> bool`, the existence check to use before *referencing* an object without putting it again. If GC has condemned the only copy, it rewrites the object (recursively through its manifest tree) into live storage. The default impl is `contains`. `LocalRepo::replace`/`replace_many` use it, and OCI/Git adapters should too (requested by net).
- **Pack format v2 (`O5SPACK2`):** a 16-byte random nonce after the magic. This fixes a real aliasing bug: a pack holding exactly the same records as a condemned pack got the same content-derived name, so `put_if_absent` returned AlreadyExists and GC then deleted the "new" copy. v1 packs and staging files are still read.
- **v1.4 (2026-11-15), additive (repo crate):** `Grouper`/`GroupKey`, `LocalRepo::gc_with`, and new `GcOptions` fields (build it with `..Default::default()`).
- **v1.5 (2026-11-15), additive:** `Repository::delete_ref(name, read_set) -> ReplaceOutcome`. It's OCC and obeys rules: create-once and `releases/*` names can't be deleted. Tombstones keep the version sequence, so a re-create continues at v+1. The default impl returns `Unsupported`; `LocalRepo` implements it. The CLI has `o5s delete NAME --expect V`. It's covered by the Lean model (proofs re-checked) and by the Lean↔Rust difftest.
- **v1.6 (2026-11-15), additive:** `Repository::update_many(updates: &[(Name, Option<Hash>)], read_set)`, where `None` = delete. It's atomic (one `Txn` journal record), and each entry follows the single-name rules. The default delegates to `replace_many` when there are no deletes. It's for `git push --atomic` with deletions (requested by net).
- **Frame v2 / index v3 (on-disk):** frame directories hold lengths only (hashes are recomputed on recovery), and the index uses a shared record table (~44 B/entry vs 62). Older formats are still read. On net's real ripgrep import, a grouped repack is now **1.078× git-aggressive total, 1.021× payload** (it was 1.264× / 1.154×).
- **v1.7 (2026-11-15), additive:** `ObjectStore::put_delta(base, src)` stores an object as `base XOR delta` (record kind `Delta`), keeping content-hash identity. Reads, ranges, GC (base kept alive) and `ensure_live` cover it. The default impl is a plain `put_stream`. The CLI has `put --delta-base HASH`.
- **v1.8 (2026-11-15), additive:** `ObjectStore::put_delta_mapped(base, segments, src)` is an XOR delta with an explicit `(dst, src, len)` alignment. The default delegates to `put_delta`. The new crate `o5s-weights` (`put_checkpoint`) plans the alignment for safetensors.
- **Refs on a backend (2026-11-15):** `RefStore::open_backend(backend, prefix)` and `LocalRepo` config `[refs] store = "backend"`, plus `o5s init --refs-on-backend`. The API is unchanged; this is a new engine.
- **v1.9 (2026-11-15), additive:** `Backend::atomic_create() -> bool` (default `true`). Backends that emulate create-only with check-then-put (S3 without conditional writes) must return `false`. The ref log engine refuses them.
- **Hierarchical manifests:** `PackConfig::manifest_fanout` (default 4096; repo config key `pack.manifest_fanout`, optional). Every interior node is keyed by the content hash of its byte range, and reads expand the tree lazily.
- **v1.10 (2026-11-15), additive:** `PackConfig::compress_probe` (default `true`; repo config key `pack.compress_probe`, optional, so existing config.toml files still load). Records of at least 8 KiB whose first 4 KiB shrink by less than 3% under zstd-1 are stored raw without a full compression attempt. It changes only the codec choice, never identity or the format. Build `PackConfig` with `..Default::default()`/`..PackConfig::tiny()` (all current callers already do).
- **v1.11 (2026-11-15), additive (pack + repo):** `ChunkerKind { FastCdc, Xet }` and `PackStore::put_stream_with(kind, src)`; repo config `[chunking]` (`default`, `prefixes` = name prefix → `"fastcdc"`/`"xet"`, longest prefix wins; optional) with `ChunkingSection`/`ChunkerChoice`, `LocalRepo::init_config(root, &RepoConfig)`, `LocalRepo::put_stream_for(name, src)` and `LocalRepo::chunking()`. CLI: `init --chunk PREFIX=CHUNKER` (repeatable), `--chunk-default`, `put --for-name NAME`. `RepoConfig` gained a field: build it with all fields (it has no `Default`). `ObjectStore::put_stream` on `LocalRepo` uses `[chunking] default`. The Xet chunker itself is net's `crates/chunk`. **For net:** the server can call `put_stream_for` for named uploads (OCI/Git adapters, `/v1` puts that carry a name).
- **v1.12 (2026-11-15), additive:** `ObjectStore::put_stream_grouped(src, width)` and `ObjectStore::put_delta_mapped_grouped(base, segs, src, width)`, a storage hint for fixed-width numeric data. Defaults ignore the hint, so other implementors (`RemoteRepo`, `MemStore`) are unaffected. Pack: `PutOpts { chunker, group_width }`, `PackStore::put_stream_opts`, `put_delta_opts`; repo: `LocalRepo::put_stream_for_grouped`. Weights: `group_width(head)`, `put_weights(store, src)`, `sniff(src)`, `put_checkpoint_opts(.., byte_group)`; `put_checkpoint` now passes the hint. CLI: `put` byte-groups safetensors automatically (`--no-byte-group` to opt out). **On-disk:** record codec 2 `GroupedZstd` (blobs only). Readers older than this reject it as an unknown codec (fail closed, never misread).
- **v1.13 (2026-11-15), additive:** `PackConfig::grouped_zstd_level` (default `DEFAULT_GROUPED_ZSTD_LEVEL` = 6; repo config key `pack.grouped_zstd_level`, optional; CLI `init --grouped-zstd-level`). It applies only to codec-2 (byte-grouped) records. Code that builds `PackConfig` with a full struct literal must add the field; `..Default::default()` callers are unaffected.
- **v1.14 (2026-11-15): the Front API (Phase 3 P1).** Additive except where noted.
  - **core:** `Encoding { chunker, group_width, derive: Option<Derive { base, patch: PatchFormat::Xor, segments }> }` and `ObjectStore::put_encoded(src, &Encoding)`. The default maps onto the older methods, so `RemoteRepo` and `MemStore` work unchanged.
  - **repo, `front` module:** `Front` (`id`, `prefixes`, `plan(ctx, src)`, `children`, `group`), `FrontRegistry` (nested prefix claims, longest wins; union tracer; merged grouper), `Profile { front, chunker, derive }`, `DerivePolicy`, `PlanCtx`.
  - **repo, `LocalRepo`:** `with_fronts`, `fronts`, `profile_for`, `put_named(name, src)`, `put_named_hinted`, `front(id) -> FrontHandle` (a `Repository` that may move that front's names), `gc_with_fronts(opts, extra, grouped)`. Config: `[profiles.<p>]` and `[namespaces]`, both optional, with the old `[chunking]` still honoured. CLI: `init --profile NAME=front:..,chunker:..,derive:none|previous` and `--namespace PREFIX=PROFILE`.
  - **Behaviour change:** names under a prefix claimed by a registered front can only be moved through that front's handle (`Error::Unauthorized` otherwise). A rule on a prefix that covers another front's claim is refused too. Nothing is claimed until fronts are registered, so existing callers are unaffected.
  - **`RepoConfig` gained `profiles` and `namespaces`:** full struct literals must add them; use the new `RepoConfig::new(&pack)` instead. I fixed the one literal in net's `crates/server/tests/server.rs`.
  - **repo, `idmap` module:** net's SegMap moved into the core as the generic external-id index (`IdMap` = `SegMap`, same on-disk format, sync API; the async wrappers stay in the server).
  - **pack:** `bytegroup` moved here from weights (it is on-disk format); `o5s_weights::bytegroup` re-exports it. `ChunkerKind::from_id`/`id`.
  - **Follow-up (net's requests):** `FrontHandle` owns a `LocalRepo` clone (no lifetime, so it fits in `Arc<dyn Repository>`). `Front::gc_tracer(repo)` is built once per GC run (e.g. over id-index roots snapshotted at GC start) and replaces `children` for that front. `FrontRegistry::tracer(repo)` now returns `Result`.
  - **weights:** the `safetensors` front (`SafetensorsFront`, `SAFETENSORS_FRONT`) and `plan_weights`. `put_checkpoint` and `put_weights` are now thin wrappers over the same plan.
- **v1.15 (2026-11-15): similarity ordering in the core (Phase 3, docs/ORDERING.md), additive except where noted.**
  - **New crate `o5s-order`:** `features`, `order(items, use_hints)` and `frames(items, order, cfg, boundary)`.
  - **pack:** `PackStore::put_frame_ref(members, level, Option<&Hash>)` and `is_leaf`. `chunks()` returns a Frame3 member's reference.
  - **On-disk:** record kind 5 `Frame3` and index rec kind 5 (older readers reject both).
  - **repo GC, behaviour change:** a grouped repack (`gc_with(.., Some(grouper))` / `gc_with_fronts(.., grouped = true)`) reorders **every** small live object by content similarity and writes Frame2/Frame3 frames. The `Grouper`'s `GroupKey`s are now tie-breaking hints only, and objects without a key are reordered too, where before they were copied verbatim.
  - **`GcOptions::frame_cap` default:** 4 → 6 MiB.
- **v1.16 (2026-11-15), additive:** `PatchFormat::XorAnchored` and `PackStore::put_delta_anchored(base, src, opts)`, a derived object whose XOR alignment is discovered while streaming. FastCDC chunks of the new content that also occur in the base anchor the alignment at their base offset; other chunks keep the last anchor's shift. When the base has no stored chunks (a delta, a frame member), its content is chunked first. It handles in-place edits and insertions or deletions alike, without knowing the format, and is stored as an ordinary `Delta` record with explicit segments (the read path is unchanged). **Behaviour change:** a profile with `derive = "previous"` now uses it (before: offset-aligned XOR); the `safetensors` front still passes explicit tensor segments. The `ObjectStore` default falls back to `put_delta`.
- **v1.17 (2026-11-15), additive:** `ReadVerify { Strict (default), Eof }` and `ObjectStore::open_verified(hash, range, mode)` (default: `open`); repo `[read] verify` / `allow_eof` (`ReadSection`), `LocalRepo::read_policy()`; `LocalRepo::open` follows `[read] verify`. `RepoConfig` gained a `read` field; it now implements `Default`, so prefer `RepoConfig { .., ..Default::default() }` to full literals. **For net:** the server can pass a per-request mode with `open_verified`. `LocalRepo` already refuses `eof` unless the repository enables it, so a server default of strict needs no extra logic.
- **v1.21 (2026-11-16), additive:** `o5s_repo::reach` (`ReachIndex::open(repo, cells)`, `cells_reaching_many(&[Hash], &[CellId]) -> Vec<Vec<CellId>>`, `readable_many`, `on_policy_reload`, `cell_of`; per-cell reachable sets for object-read authorization, eager on creations, exact recompute on moves and deletes) and `LocalRepo::add_ref_observer` (`reach::RefObserver`, notified of every committed name change on a handle and its clones).
- **v1.20 (2026-11-16), additive:** `ObjectStore::put_concat(&[(Hash, Range<u64>)], &mut dyn FnMut(&[u8])) -> Hash` (reuses stored chunks of chunked sources) and `ObjectStore::put_chunked(&[&[u8]]) -> Hash` (explicit chunk boundaries); defaults stream through `put_stream`.
- **v1.19 (2026-11-15), additive:** `Backend::local_path` (default `None`; LocalDisk returns the file, used to hard-link pack indexes into `idx-cache`); `ObjectStore::put_many(&[&[u8]]) -> Vec<Hash>` (default: one `put` per item; `PackStore`, `LocalRepo` and `FrontHandle` batch: parallel hash/compress, one writer-lock pass). CLI `gc --repack-all`.
- **v1.18 (2026-11-15), additive:** new crate `o5s-attest` (Statement, Envelope, Signer/Verifier, Ed25519, `attest`, `attestations`, `promote` with `Policy`/`PromoteOutcome`); `o5s_prov::search` (`SearchIndex`, `attach_meta`, `lineage_named`, `impact_named`) and `o5s_prov::retention` (`apply`, `version_cmp`); `Profile::{keep_versions, only_if_regenerable}`; `LocalRepo::namespace_profiles`; `RefState::rules_allow_delete`. CLI: `search`, `lineage`, `impact`, `retention`.

## Production gaps
- **Index scale (largely addressed).** Large pack indexes (≥ 4 MiB, ~95k entries) are memory-mapped from a verified local cache (`idx-cache/*.idx3`) and binary-searched. Small packs and the open pack stay in a HashMap. Per-object heap cost is now zero for big packs; the page cache holds the 44 B/entry. A **multi-pack index** (`O5SMIDX1`) is built locally, by a k-way merge with O(#packs) memory and outside the index lock, once ≥ 8 large packs exist and > 10% are uncovered. Lookups are then one binary search plus the few uncovered packs. That answers the brief's open question with "files alongside packs"; PostgreSQL isn't needed for lookups. Remaining: MIDX rebuilds are O(n) and triggered by the coverage rule; a production deployment at 10^9+ objects would want them scheduled with GC.
- **GC timing assumption (narrowed).** Since `replace` calls `ensure_live`, a slow put→replace is safe: the object is rewritten if condemned, or the replace fails with `MissingObject` if it's gone. What's left to assume is that ensure_live→commit (milliseconds) plus the 60 s dedup refresh interval are far shorter than the grace period (≥ 10 min, enforced). Adapters must `ensure_live` indirectly referenced children (Git and OCI do).
- **GC tracer.** GC treats objects as opaque, so only ref targets and their chunks are roots. Structured objects (directories, snapshots) need a `Tracer` once those formats exist.
- **Model weights: XOR delta exists, but isn't automatic.** `put_delta(base, …)` (API v1.7) stores a version as an XOR delta: −65% on a synthetic full fine-tune (see BENCH.md). The CLI can take the base from a ref (`put --delta-from-ref models/heads/main`). **Tensor-aware alignment is done** (API v1.8 `put_delta_mapped` plus the `o5s-weights` safetensors planner, used automatically by `put --delta-*`). Reordered, inserted or resized tensors and header edits keep the delta small. Remaining: dtype-aware transforms (e.g. bf16 exponent/mantissa splitting before compression, as ZipLLM does) aren't done.
- **Provenance scale (addressed).** Log nodes (`O5SPLOG2`) carry checkpoint indexes (`O5SPIDX1`, sorted forward and backward tables), rebuilt geometrically when recent records reach max(1024, total/8). `Provenance::load` reads only the recent tail. Older lookups are O(log n) ranged reads, and records are fetched on demand. Each rebuild is a full rewrite, but the geometric rule (rebuild once recent ≥ total/8) keeps the total amortized: about 9× write amplification over the life of the log, O(n) overall. A tiered LSM could lower that constant, but there's no known need.
- **Ref store scale.** All refs live in memory (~150 B each), and compaction rewrites a full snapshot. That's fine to ~10^6 names, strained at 10^7. Scaling needs a sharded journal per prefix, or an LSM. Net was advised to keep per-blob OCI links in segment maps, not refs.
- **Small packs.** `replace` and `flush` seal the writer's open pack, so chatty writers make many small packs. GC consolidates packs under 16 MiB (`small_pack_bytes`). Run GC regularly (cron, or the server's background GC).
- **Multi-host refs (addressed).** `[refs] store = "backend"` keeps refs as a create-only log on the pack backend (e.g. S3), with commits linearized by conditional create. It's tested against the model and the Lean difftest, plus multi-process contention and a crash sweep. Remaining: per-commit cost is one PUT plus catch-up GETs (fine for refs, slower than the local journal), and log trimming relies on a 1 h grace like GC.
- **Ambiguous outcome on I/O error.** If an fsync fails during `replace`, the record may still be durable. The caller must re-`resolve` to find out (documented, as in most databases).

## Environment notes
- `source scripts/env.sh` puts the cargo target, `CARGO_HOME`, TMPDIR and bench data on `/mnt/scratch/pico2peta`, and adds the shared Lean 4.31 to PATH.

## Blockers
- (resolved 2026-11-15) `/mnt/scratch` hit 100%, and Exp1 died with ENOSPC at redis V0. I freed my own regenerable caches (`cargo-target/debug` 7.7 GB, `cargo-target-fuzz`) and made debug builds leaner in `scripts/env.sh` (no incremental cache, line-tables-only debug info). Exp1 was restarted for redis and git (`exp1-run2.log`).

## Answered questions
- **Verified-read cost (answered by the human, 2026-11-15).** The question was whether a whole read must check every chunk's BLAKE3 before returning its bytes, given that it costs about half the read throughput against PLAN §6.4's "read ≥ 0.50× `cat`". **Decision:** keep `strict` (per-chunk BLAKE3 plus the whole-object hash; no unverified bytes ever returned) as the default, and add `eof` as an explicit opt-in: on whole reads, skip the per-chunk hash and rely on record CRC32 plus the whole-object hash at EOF. **Done** (API v1.17):
  - `ReadVerify { Strict, Eof }`, `ObjectStore::open_verified`;
  - the `[read] verify` / `allow_eof` config, under which `LocalRepo` refuses `eof` unless allowed, so the server is strict by default;
  - CLI `get --read-verify`, and `init --read-verify` / `--allow-eof-reads`;
  - the exact guarantees in ARCHITECTURE.md;
  - tests with CRC-valid crafted corruption (strict never yields a bad byte; eof errors at EOF) and plain corruption (both modes);
  - `o5s-bench generic` measures both modes for §6.4's read row (batch phase).
  - No per-namespace setting: reads are addressed by hash, not by name.

- **Fast ingest level (answered by the human, 2026-11-16).** **Decision:** zstd level 3 stays the default. No
  fast-ingest default and no pack-marker format change for now. The opt-in recipe stays documented: `init
  --zstd-level 1` plus a periodic `gc --repack-all` (BENCH.md, 2026-11-15 put/get pass; CLI.md).
- **The three core H3 FAILs on `cbbbb8a` (answered by the human, 2026-11-16).** Ingest put −5.9 %, the §6.4
  durable-write tier at 0.886×, and redis A3 peak RSS +14 % stay recorded as FAIL by the letter. Each is decided
  by one pre-declared, better-controlled re-measurement (PLAN §6.8, amendment 2026-11-16), and the result is
  acted on: anything still regressing is a real regression, fixed before H4.

- **Verified-read target (answered by the human, 2026-11-16).** The §6.4 target (verified read ≥ 0.50× warm
  `cat`) stays as written and is recorded as **missed** on this 4-core host. In the final batch, strict reached
  0.30× and eof 0.41×; the dev checks gave about 0.22× and 0.32×. `strict` stays the default. Re-measuring on a
  many-core host is a post-RC item.
- **HF front (answered by the human, 2026-11-16; net's PLAN amendment).** LFS upload keeps the Xet chunker
  (LFS↔Xet chunk dedup is worth more) and is recorded as a FAIL at 1.34× of `/v1`. Downloads are judged on
  server CPU against `/v1`, with busy time informational.

## Release candidate: checklist status (docs/RELEASE.md), 2026-11-16, demo `0951c7b`
Not tagged: the human said not to tag yet.
- **Done:**
  - **1, 2.** Gate green on `0951c7b`, with net merged.
  - **3.** CHANGELOG: the rename is listed as breaking; H2 and H4 are listed.
  - **4.** Docs current: CLI.md regenerated, no changes. SECURITY.md has the reachability section, ARCHITECTURE
    the verified-read limit, and net updated API.md.
  - **6.** Proofs: `h5i app prove` ok in both projects after the rename (core 182, net 167 theorems).
- **Dev pass only:**
  - **5.** Fuzz: all 20 targets 60 s clean under ASan, no artifacts, but on a lock that had two transitive h5i
    crates at 0.5.6 (fixed since). It must be rerun on the commit to tag.
- **Blocked on the human resuming measurements:**
  - the final batch's core suite (no usable run);
  - net's §6.5 windows A and B;
  - a measurement of 95f6a4a (the staging-file writeback, against the durable-write FAIL).
- **Then:** the fuzz smoke on the tag commit, the tag (7), and its PROGRESS record (8).

## Post-RC (agreed, not started)
- Re-measure the §6.4 verified read on a many-core host (the human, 2026-11-16).
- A double-buffered parallel Xet upload chunker: cut window i while window i+1 is read. The windowed version lost
  the pipeline's overlap and was reverted (BENCH.md, H4).
- Measure the staging-file writeback change (95f6a4a, `sync_file_range` every 8 MiB) against the durable-write
  tier. It was committed while measurements were stopped, so it is unmeasured.

## Questions for human
- **Disk budget:** `/mnt/scratch` has much less headroom than the ~31 GB in AGENT_TASK.md (24 GB → 13 GB → 0 during this session, partly from other directories). Could you free or extend space, or say which of these may be deleted: the Exp1 mirrors and spill (`bench-data`, ~1.5 GB), the fuzz build cache, the nightly toolchain (`rustup-nightly`)?
- **Disk headroom** is still tight: it has dipped below 8 GB twice. Both sessions now clean their build caches regularly.
- **Exp1 failed**, so grouped repack stays opt-in (`[gc] grouped = false` by default, per the pre-registered rule), and Exp1b is queued. Nothing is needed from you unless you want to override the pre-registered consequences.
