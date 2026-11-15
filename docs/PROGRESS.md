# Progress log

## Status (2026-11-15)
- **M0 Plan: done.** See `docs/PLAN.md`. The Experiment 1 thresholds (§4) are frozen.
- **Core API v1 committed** in `crates/core`: `Hash`, `Error` (+ `code()`), `ObjectStore`, `Repository`, `Name`/`Rule`/`Observed`/`RefValue`/`ReplaceOutcome`, and `backend::{Backend, PutSource, PutOutcome, BlobMeta, LocalDisk, conformance}`.
- In progress: `crates/pack` (packs, index, bundling, FastCDC) rewritten against API v1 for multi-process safety and streaming.

## API changes
- **v1 (2026-11-15):** first stable shapes. `Backend` lives in `pico2peta_core::backend`. It is create-only (`put_if_absent` with `PutSource::{Bytes, File}`) and adds `get_range(key, Range)`, `open` (streaming), `head`, `list -> Vec<BlobMeta>`, `delete` and `describe`. New error variants are `KeyNotFound(String)` and `Backend { transient, msg }`. `ObjectStore`/`Repository` take `&self` everywhere, and `open()` returns a `'static` streaming reader. Every backend must pass `backend::conformance::run_all`.
  These changes cover all three of the net session's "Requests for core" from 2026-11-15: streaming put, a backend error variant, and committing early.

## Production gaps
- (tracked once the core crates land)

## Environment notes
- `source scripts/env.sh` puts the cargo target, `CARGO_HOME`, TMPDIR and bench data on `/mnt/scratch/pico2peta`, and adds the shared Lean 4.31 to PATH.

## Questions for human
- (none yet)
