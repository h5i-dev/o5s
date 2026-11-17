# Changelog

All notable changes. Format: [Keep a Changelog](https://keepachangelog.com/). Versioning: SemVer; on-disk formats carry their own version magic (`O5SPACK1`, `O5SIDX01`, `O5SREFJ1`, `config.toml format = 1`).

## [Unreleased]

### Added
- **File paths as names: a canonical, reversible encoding** (ARCHITECTURE.md, "File paths as names").
  - **The rule:** per `/`-separated segment, every byte outside `[A-Za-z0-9._-]` becomes `+XX` (uppercase hex),
    and a `.`/`..` segment has its dots escaped. Decoding is strict, so the encoding is a bijection onto what
    decoding accepts.
  - **Where:** a kernel (`o5s_kernel::pathname`) with Lean proofs (`verify/core/proofs/PathName.lean`: the output
    is always a valid name, decoding inverts encoding, only canonical names decode, the encoding is injective);
    `Name::from_path` / `to_path` (and `_bytes`) in the core API; `o5s name encode PATH...` /
    `o5s name decode NAME...` in the CLI; and a fuzz target, `name_path`.

## [0.1.0-rc.1] - 2026-11-17

The first release candidate. Benchmarks against `docs/BASELINE.md` (PLAN §6.8): BENCH.md, 2026-11-17 final batch.
Fuzz smoke for rc.1, per the human's ruling: `ref_journal` only, ≥ 60 s clean under ASan on the tag commit,
since rc.1 changed ref-journal replay. The full suite (every target ≥ 60 s) runs before v0.1.0 final.

### Fixed (rc.1)
- **Reads by hash through another server, or after another process committed, returned 404.** The reach index
  saw only its own handle's commits. Both ref engines now report what they apply from other writers (other hosts
  on a backend log, other processes on a local journal), and every reachability query catches the refs up first.
  So multi-host deployments on S3 authorize reads exactly: foreign creations grant, and foreign moves and deletes
  revoke. SECURITY.md's single-writer limit is gone.
- **Two memory regressions** found by the rc.1 batch before tagging: the ref journal cloned every replayed record
  at open (scale names 1e6: RSS 148 → 238 MB), and GC's mark kept a second set of every traced object (scale GC
  1e7: peak RSS +25 %). Both are back to baseline.
- **Back-to-back git pushes were slower** (jq re-push 0.608 vs 0.476 s; tier STRONG PASS → PASS): the reach
  index walked each new history in a background thread while the next push ran. Pending additions are now settled
  by the next query of their cell only, so a workload that never reads by hash never pays.

### Changed (breaking): the project is renamed pico2peta → o5s
- **Code:** crates `o5s-*`; binaries `o5s`, `o5s-server`, `o5s-bench`; environment variables `O5S_*`;
  metrics `o5s_*`; Lean modules `O5sKernel` and `O5sNetKernel`.
- **The repository:** `github.com/h5i-dev/o5s`.
- **Every on-disk magic:** `P2P*` → `O5S*`, same lengths (pack `O5SPACK2`, index `O5SIDX0x`, record `O5SR`,
  ref journal `O5SREFJ1`, …; docs/RELEASE.md "Formats written by v0.1"). The kernel's record magic and its Lean
  spec changed with it. **Stores written before the rename are not readable**, and there is no migration: this
  is before v0.1.
- **Unchanged:** recorded raw benchmark results keep their old system label (`pico2peta`).

### Changed: verified decisions (h5i-app rebuild, docs/H5I_PLAN.md)
- **Every core decision is an extracted, proved kernel** (`crates/kernel`, proofs in `verify/core`):
  - the name grammar, rule governance and mobility, and the OCC batch decision (`refs::decide`);
  - front ownership (P6), promotion (P7), retention, GC victim selection and verify-before-delete (P5);
  - the verified-read digest check and put dedup (P1);
  - the pack record header decoder and the ref-journal framing decision.
  The shells call them. The hand-written Lean model (`lean/`) and its Rust↔Lean difftest are removed.
- When several targets of one batch are invalid, the error now names the first in name order (it was the first in
  argument order).
- **Object reads by hash are authorized by reachability** (with net's authz kernel): a principal reads an object
  only if a name it may read reaches it. An object that no name references is not readable through the server,
  so `put` then `get` by hash needs a name first. The core building block is `o5s_repo::reach`:
  - commits never traverse, and nothing traverses in the background; created names queue their targets, and the
    next query of their cell settles that cell first, so answers are exact;
  - deletes and moves still revoke at once (the cell is recomputed by its next query);
  - the walk skips the tracer on chunk edges and keeps a separate traced set, so a hash that is both a chunk and
    a front object is still traced; GC's mark uses the same rule.
- **h5i v0.5.5 everywhere:** every h5i crate comes from crates.io, pinned `=0.5.5`; the CLI is the official v0.5.5
  release; Lean `h5i_app_lib` stays at the v0.5.5 tag (`verify/core/README.md`, Toolchain).

### Performance (H4)
- **Verified reads hash the whole object on several cores** (`o5s_pack::phash`): 1 MiB segments are hashed
  at their offsets and combined by BLAKE3's tree rule, giving exactly the serial hash (proptested). `strict` is
  unchanged, and the extra threads engage only when hashing is the bottleneck. On a 4-core host, 64 MiB
  incompressible reads: strict `open` +24 %, eof `open` +50 %; compressible reads are unchanged.

### Changed (breaking before v0.1)
- **CLI cleanup (plumbing over a domain-agnostic core).**
  - `put`: removed `--delta-base`, `--delta-from-ref` and `--no-byte-group`, and the automatic safetensors detection. Storage choices come from namespace profiles: `put --for-name NAME` applies NAME's profile, also with `--remote` (`PUT /v1/objects?for_name=`). The low-level override is the generic `--encoding KEY=VALUE` (`chunker`, `group-width`, `derive-from`, `patch=xor|xor-anchored`).
  - Provenance is one group: `prov record | lineage | impact | why`, each taking a HASH or a NAME. The top-level `lineage` and `impact` are removed, and `prov why/impact` now also accept names.
  - New: `update` (atomic multi-name OCC), `attest keygen | create | list | verify`, and `promote SRC DST` (local policy flags; with `--remote` the server's `[[promotion]]` policy applies). With `--remote`, `attest create|list|verify`, `prov lineage|impact` and `search` are answered by the server's endpoints (no full provenance-log download).
  - The reference is generated in `docs/CLI.md` (`scripts/gen-cli-doc.sh`).
- **GC lays out repacked objects by content similarity by default, for every domain** (`[gc] similarity_repack`, default `true`; `gc --no-similarity-repack`). The human's decision after Experiment 3, whose pre-registered verdict, FAIL on jq's build time, is unchanged in EXP1.md. Before, grouping needed a `Grouper` and was opt-in.

### Added
- `ObjectStore::put_concat` and `put_chunked` (API v1.20): build an object from ranges of stored objects reusing their chunks, or with explicit chunk boundaries (the HF/Xet front's shard and xorb paths). Faster verified reads of incompressible blobs: one CRC per record instead of two, no CRC on hash-verified blob reads, and no second allocation for raw blobs (512 KiB `open` 794 → ~2,300 MB/s).
- `ObjectStore::put_many` (API v1.19): batched small-object puts. Items are hashed, dedup-checked and compressed on up to 4 threads, then appended in one writer-lock pass; duplicates in a batch are stored once, and items over the chunking threshold go through `put`. 2.0× over per-object `put` on 1 GiB of real redis git objects (131 → 260 MB/s). `gc --repack-all` in the CLI (the server already had it): it recompresses records written at a fast ingest level (`init --zstd-level 1`) into similarity frames.
- Streaming `put_stream` pipeline (chunker → parallel hash/dedup/compress/encode → in-order committer, with the whole-object hash on its own thread and bounded memory): about 2.3× faster on CPU-bound storage. Compressibility probe (`pack.compress_probe`, default on). Incremental pack hashing, so sealing no longer re-reads the pack.
- Byte-grouped record codec (codec 2) for model weights: `put` of safetensors and `put --delta-from-ref` group bf16/fp16/fp32 bytes into planes before zstd (fine-tune deltas −10.6%, base −6.0% on SmolLM2-135M at the default `pack.grouped_zstd_level = 6`; `init --grouped-zstd-level`). `--no-byte-group` opts out. Older readers reject codec 2 rather than misread it.
- Per-namespace chunking: `[chunking]` in config.toml (`init --chunk PREFIX=xet|fastcdc`, `put --for-name`), with a byte-exact Hugging Face Xet chunker (from `crates/chunk`). FastCDC remains the default; reads are unaffected.
- Core API v1: `ObjectStore`, `Repository` and `Backend` traits, plus `LocalDisk` and the backend conformance suite.
- Pack store:
  - per-writer flocked staging files;
  - content-addressed, create-only sealed packs and indexes;
  - orphan recovery that truncates a torn tail;
  - FastCDC chunking with manifests, streaming `put_stream`/`open`, and zstd records;
  - verified reads.
- Ref store: append-only checksummed journal, exclusive-flock commit point, OCC `replace` over read sets, namespace rules whose immutability can't be retracted.
- `LocalRepo` combining both stores, and `open_with_backend` for S3.
- GC/repack: plan → sweep → delete with grace periods; condemn-aware dedup; rescue of resurrected objects.
- CLI `o5s` with local and `--remote` modes.
- S3 backend, HTTP server and client (net session; see docs/NET_PROGRESS.md):
  - `o5s-backend-s3`: SigV4, `If-None-Match` create-only writes, multipart from memory or file, resumable streaming reads, jittered retries with a retry budget, bounded connections. Passes the backend conformance suite on MinIO, including fault injection.
  - `o5s-server`: HTTP API v1 with bearer and Basic authn, per-namespace authz, a hash-chained fsync'd audit log, Prometheus metrics, health and readiness endpoints, graceful drain, load shedding, bounded-memory streaming, and admin GC.
  - OCI distribution `/v2`: passes distribution-spec v1.1.1 conformance (64/64).
  - Git smart HTTP `/git`: v0/v1 fetch and push, thin packs, atomic pushes.
  - Segment maps (`O5SSEG01`/`O5SMAP01`) for scalable digest and oid maps.
  - `o5s-client`: `RemoteRepo` implements `Repository`, with end-to-end BLAKE3 verification.
  - CI workflow (fmt, clippy, test, docs, MinIO, OCI conformance) and `deploy/` (Dockerfile, systemd, configs).
- Experiment 1 harness (`bench/exp1`, `scripts/exp1.sh`).
- Fuzz targets for every decoder, plus never-panic proptests on stable.
- API v1.1: `Error::Unauthorized`.
- API v1.2: atomic `replace_many`. v1.3: `ensure_live`. v1.4: `Grouper`/`gc_with` (grouped repack). v1.5: `delete_ref` with tombstoned versions. v1.6: `update_many` (sets and deletes in one transaction). v1.7: `put_delta` (XOR-delta objects).
- Ref journal compaction (snapshot + generation rotation).
- Hierarchical manifest trees (`pack.manifest_fanout`), and lazy streaming reads with a manifest cache.
- Frame records for similarity-grouped compression, and grouped repack via `Grouper`.
- Memory-mapped v3 pack indexes for large packs, plus a multi-pack index.
- Provenance layer (`o5s-prov`) and CLI `prov`.
- Lean 4 model: P1–P5 proofs, an executable model, and a Rust↔Lean differential test.
- GC crash sweep at every fault point.
- `docs/EXP1.md`, `docs/BENCH.md`, and `scripts/exp1-report.py`.

### On-disk formats
- Pack `O5SPACK2`: header nonce (fixes pack-name aliasing). `O5SPACK1` is still read.
- Index `O5SIDX03` (shared record table, ~44 B/entry). `O5SIDX01`/`O5SIDX02` are still read.
- Record kinds `Frame2` (lengths-only directory) and `Delta`. `Frame` (v1) is still read.
- Ref journal records: `Txn`, `Delete`, `Rotated`. Snapshot `O5SREFS1`.
- Local caches: `idx-cache/*.idx3` and `midx-*.midx` (`O5SMIDX1`), derived and safe to delete.
- Record kind `Frame3` (5): a frame compressed against a reference object (similarity repack; the reference is a GC child).
- Record codec `GroupedZstd` (2): byte-grouped weights. Readers that predate it refuse it.
- `Delta` records with segment maps (chunk-anchored XOR deltas).
- Provenance: records `O5SPROV1`, log nodes `O5SPLOG2` (`O5SPLOG1` read) with checkpoint indexes `O5SPIDX1`. Attestations are provenance records (op `attest`).
- Front id maps: segment maps `O5SSEG01` / `O5SMAP01`.
- Compatibility and upgrade policy: docs/RELEASE.md.

### Fixed (second review and net session, 2026-11-15)
- Ref log engine: identical decisions by two writers could both report Ok (introduced by the lost-response fix, caught by the net session's multi-host test). Each log entry now carries a per-attempt nonce.
- Provenance: `record()` now enforces the decoder's limits (an oversized record could make the log unreadable) and uses `ensure_live` for inputs.
- Weights: short or malformed inputs fall back to a plain delta, and base segments are clamped to the base.
- LocalDisk: newly created parent directories are fsynced; `get_range` checks bounds before allocating; a directory is never treated as an existing blob; temp names are unique across PID namespaces.
- Exp1b harness: pledged-size patch-from frames, a single target read, path-less tags, and a build timer that includes the final fsync (all disclosed in EXP1.md).

### Fixed (independent code review, 2026-11-15)
- Ref journal: batches over 4 MiB were acknowledged, then truncated by other handles as "torn tails". Records are now limited to 16 MiB and checked at write time.
- Ref journal: a CRC error in the middle of the journal was silently truncated along with all later records. It's now reported as corruption; only a torn *last* record or a zero-filled tail is truncated.
- Ref journal: a race at the first compaction could recreate an empty gen-0 journal and split the store.
- Ref log engine: a stale handle could acknowledge a commit into a trimmed slot (reproduced). Gaps are now detected by HEAD probes, and commits that land in a compacted region fail.
- Ref log engine: it refuses backends without atomic conditional create (`Backend::atomic_create`).
- Ref log engine: a lost create response was reported as Conflict for a commit that had landed (raised by the net session).
- Pack store:
  - after a seal failed post-publish, later writes could append to the published (hard-linked) pack;
  - refresh failed on harmless concurrent deletes;
  - `stats()` had a lock-order inversion;
  - a corrupt index length could panic;
  - one corrupt orphan staging file blocked every open (it's now quarantined).

### Fixed
- A race between sealing and GC condemnation left a writer's index pointing at a retired open pack.
- A rewritten pack could alias a condemned pack's content-derived name and be deleted with it.
- Index rebuilds could invalidate pack ids held by concurrent readers (wrong-pack read or out-of-bounds panic).
- The benchmark generator produced duplicate inputs (small-object rates in early BENCH runs were inflated).
