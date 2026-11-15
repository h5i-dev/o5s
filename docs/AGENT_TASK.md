# Task for the autonomous worker session

You are working autonomously in a tmux session on `/home/ec2-user/Dev/pico2peta` (branch `demo`).
The human may be disconnected for long periods — keep making progress without waiting for input.

## Read first
- `docs/DESIGN_BRIEF.md` — the full design (layers, core API, per-kind config, Lean properties, open questions, related work).

## Environment constraints
- Root disk is ~99% full (≈2 GB free). **Do not build or store large data on `/`.**
- Use `/mnt/scratch/pico2peta/` (owned by you, ~31 GB free):
  - `CARGO_TARGET_DIR=/mnt/scratch/pico2peta/cargo-target` (already exported in this session)
  - `/mnt/scratch/pico2peta/bench-data` for cloned benchmark repos/datasets
  - `/mnt/scratch/pico2peta/tmp` for temp files (`TMPDIR` is set to it)
- Rust (cargo 1.100) is installed.
- **Lean 4 + Mathlib are already installed** on scratch — do not reinstall:
  - `export ELAN_HOME=/mnt/scratch/ec2-user/elan PATH=/mnt/scratch/ec2-user/elan/bin:$PATH` (toolchain `leanprover/lean4:v4.31.0`)
  - Prebuilt Mathlib (with oleans): `/mnt/scratch/ec2-user/lake/arith/packages/mathlib` (rev `fabf563a7c`). Aeneas Lean lib also at `/mnt/scratch/ec2-user/lake/arith/packages/aeneas`.
  - In `lean/`, pin `lean-toolchain` to `leanprover/lean4:v4.31.0` and require mathlib by **local path** to that checkout (or the same git rev) so nothing is rebuilt/downloaded. Put the lake `.lake` build dir somewhere sensible; avoid filling `/`.
  - `/mnt/scratch/ec2-user/env.sh` shows the full toolchain env used elsewhere (don't source it wholesale — it redirects CARGO_TARGET_DIR).
- 4 cores, 15 GB RAM.

## Quality bar — PRODUCTION GRADE, not a demo (from the human)
The goal is a production-grade storage system that teams can run for real data, **not a demo or research prototype**. Concretely:
- **Durability & crash consistency:** explicit fsync discipline (file + directory), atomic rename for index/ref updates, versioned on-disk formats with magic + checksums, recovery paths that are *tested* with fault injection (kill at every write/fsync point), not just argued.
- **Concurrency:** multiple processes/threads writing the same repo safely (locking or lock-free OCC with a single commit point); `replace` contention behaves well under load.
- **Real backends:** the S3 backend must be real (conditional writes / If-None-Match, multipart, retries with backoff), tested against MinIO or a local S3-compatible server — not a stub.
- **Server:** a network service (HTTP/gRPC) with authn/authz, audit log, metrics (Prometheus), structured logs/tracing, graceful shutdown, config file. The CLI talks to either a local repo or the server.
- **GC, retention, repack** that are safe against concurrent writers (mark/sweep with grace periods or epochs), with tests.
- **Engineering hygiene:** `cargo clippy -- -D warnings`, `cargo fmt`, no `unwrap()`/`expect()` on non-invariant paths, no panics on bad input, fuzz targets for every parser/decoder, documented public APIs, `CHANGELOG`, CI config (GitHub Actions), reproducible benchmarks.
- **Performance is a requirement:** streaming I/O (never load whole large objects into memory), bounded memory, parallelism where it pays, benchmarks with recorded numbers vs Git/OCI/Xet baselines.
- No shortcuts that would need a rewrite to productionize. If you must defer something, record it in `docs/PROGRESS.md` under "Production gaps" with what is missing.
- Branch name `demo` is historical; treat the code as the real product.

## Team (two Claude sessions)
- **You (session `pico2peta`, worktree `/home/ec2-user/Dev/pico2peta`, branch `demo`) = implementation lead.** You own everything except the paths below.
- **Session `pico2peta-verify` (worktree `/home/ec2-user/Dev/pico2peta-verify`, branch `verify`) = verification & hardening.** It owns `lean/`, `crates/model/`, `crates/difftest/`, `fuzz/`, and fault-injection / crash-consistency test suites under `tests/`. It also reviews your commits and files findings in `docs/REVIEW.md` on its branch.
- Protocol: at the end of every milestone (and at least every ~2 hours), run `git merge verify` into `demo`, address every open item in `docs/REVIEW.md` (fix, or reply inline why not), and commit. Do not edit the verify-owned paths except to resolve merge conflicts; ask for changes via a "Requests for verify" section in `docs/PROGRESS.md`. Keep public trait/API changes noted in `docs/PROGRESS.md` so the verifier can track them.
- M5 (reference model + differential tests) and M7 (Lean) are now owned by the verifier — drop them from your plan, but keep the code testable through public traits.

## Goals (in order)
1. **Plan:** Write `docs/PLAN.md` — milestones, crate layout (Cargo workspace), and the *pre-registered* numeric pass thresholds for experiment 1, before running it.
2. **Core (Rust workspace):**
   - Object layer: content hashing (BLAKE3), `put/get(range)`.
   - Physical layer: append-only packs + index, small-object bundling, FastCDC chunking for large objects, local-disk backend (S3 behind a trait, stub OK).
   - Name layer: refs with `replace(name, read_set, new_hash) → Ok | Conflict`, per-namespace update rules (mutable / immutable-after-publish / head-mutable-release-immutable).
   - A small CLI to exercise it.
   - Executable reference model (ShardStore/Cedar style) + property/differential tests (proptest) of the implementation against it.
3. **Experiment 1 (first open question):** similarity-grouped compression (e.g. group by path/type, zstd with trained dictionaries or long-window) vs `git gc --aggressive` pack size on a few real source repos. Record methodology + results in `docs/EXP1.md`. Be honest if thresholds are missed.
4. **Lean 4 core:** pure model of objects/refs/replace with proofs of: immutability of stored content, no undetected overwrite under conflicting replaces, immutable names can't move. (Auth + GC-safety if time permits.) Put in `lean/`.
5. Later layers (provenance, Git/OCI adapters, GC) only after the above are solid.

## Working rules
- Commit early and often on branch `demo` with clear messages (end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`). Do **not** push or open PRs unless asked.
- Keep `docs/PROGRESS.md` updated after each milestone (what's done, what's next, blockers) — this is how the human catches up after reconnecting.
- Run `cargo test` before each commit. Keep `cargo build` warnings at zero where practical.
- If you hit a genuine decision only the human can make, write it under "Questions for human" in `docs/PROGRESS.md` and continue with the most reasonable default.
