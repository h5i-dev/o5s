# Changelog

All notable changes. Format: [Keep a Changelog](https://keepachangelog.com/). Versioning: SemVer; on-disk formats carry their own version magic (`P2PPACK1`, `P2PIDX01`, `P2PREFJ1`, `config.toml format = 1`).

## [Unreleased]

### Added
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
- CLI `pico2peta` with local and `--remote` modes.
- S3 backend, HTTP server and client (net session; see docs/NET_PROGRESS.md):
  - `pico2peta-backend-s3`: SigV4, `If-None-Match` create-only writes, multipart from memory or file, resumable streaming reads, jittered retries with a retry budget, bounded connections. Passes the backend conformance suite on MinIO, including fault injection.
  - `pico2peta-server`: HTTP API v1 with bearer and Basic authn, per-namespace authz, a hash-chained fsync'd audit log, Prometheus metrics, health and readiness endpoints, graceful drain, load shedding, bounded-memory streaming, and admin GC.
  - OCI distribution `/v2`: passes distribution-spec v1.1.1 conformance (64/64).
  - Git smart HTTP `/git`: v0/v1 fetch and push, thin packs, atomic pushes.
  - Segment maps (`P2PSEG01`/`P2PMAP01`) for scalable digest and oid maps.
  - `pico2peta-client`: `RemoteRepo` implements `Repository`, with end-to-end BLAKE3 verification.
  - CI workflow (fmt, clippy, test, docs, MinIO, OCI conformance) and `deploy/` (Dockerfile, systemd, configs).
- Experiment 1 harness (`bench/exp1`, `scripts/exp1.sh`).
- Fuzz targets for every decoder, plus never-panic proptests on stable.
- API v1.1: `Error::Unauthorized`.
- API v1.2: atomic `replace_many`. v1.3: `ensure_live`. v1.4: `Grouper`/`gc_with` (grouped repack). v1.5: `delete_ref` with tombstoned versions. v1.6: `update_many` (sets and deletes in one transaction). v1.7: `put_delta` (XOR-delta objects).
- Ref journal compaction (snapshot + generation rotation).
- Hierarchical manifest trees (`pack.manifest_fanout`), and lazy streaming reads with a manifest cache.
- Frame records for similarity-grouped compression, and grouped repack via `Grouper`.
- Memory-mapped v3 pack indexes for large packs, plus a multi-pack index.
- Provenance layer (`pico2peta-prov`) and CLI `prov`.
- Lean 4 model: P1–P5 proofs, an executable model, and a Rust↔Lean differential test.
- GC crash sweep at every fault point.
- `docs/EXP1.md`, `docs/BENCH.md`, and `scripts/exp1-report.py`.

### On-disk formats
- Pack `P2PPACK2`: header nonce (fixes pack-name aliasing). `P2PPACK1` is still read.
- Index `P2PIDX03` (shared record table, ~44 B/entry). `P2PIDX01`/`P2PIDX02` are still read.
- Record kinds `Frame2` (lengths-only directory) and `Delta`. `Frame` (v1) is still read.
- Ref journal records: `Txn`, `Delete`, `Rotated`. Snapshot `P2PREFS1`.
- Local caches: `idx-cache/*.idx3` and `midx-*.midx` (`P2PMIDX1`), derived and safe to delete.

### Fixed
- A race between sealing and GC condemnation left a writer's index pointing at a retired open pack.
- A rewritten pack could alias a condemned pack's content-derived name and be deleted with it.
- Index rebuilds could invalidate pack ids held by concurrent readers (wrong-pack read or out-of-bounds panic).
- The benchmark generator produced duplicate inputs (small-object rates in early BENCH runs were inflated).
