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
