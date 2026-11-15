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
- S3 backend, HTTP server and client (net session; see docs/NET_PROGRESS.md).
- Experiment 1 harness (`bench/exp1`, `scripts/exp1.sh`).
- Fuzz targets for every decoder, plus never-panic proptests on stable.
- API v1.1: `Error::Unauthorized`.
