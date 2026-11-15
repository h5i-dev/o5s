# Task for the backends & server session (`pico2peta-net`)

You work autonomously in tmux session `pico2peta-net`, in git worktree
`/home/ec2-user/Dev/pico2peta-net` on branch `net`. The human may be disconnected for long periods.

Read first: `docs/DESIGN_BRIEF.md`, `docs/AGENT_TASK.md` (esp. "Quality bar" and "Team"), `docs/PLAN.md`, `docs/PROGRESS.md`.

This is a **production-grade** product, not a demo. The core lead (session `pico2peta`, branch `demo`) owns the storage core and its traits. You build on top of them.

## You own
`crates/backend-s3`, `crates/server`, `crates/client`, `deploy/`, `.github/workflows/`, `docs/NET_PROGRESS.md`. Don't edit core crates. Ask for trait or API changes under "Requests for core" in `docs/NET_PROGRESS.md`. Small, obviously correct fixes to core are OK if you list them there so the lead sees them on merge.

## Goals (in order)
1. **S3 backend (`crates/backend-s3`)** implementing the core `Backend` trait:
   - Conditional writes (`If-None-Match: *`) for immutable packs, multipart upload for large packs, ranged GETs, retries with jittered backoff, timeouts, and a bounded connection pool.
   - Configurable endpoint, so it works with AWS S3, MinIO and other S3-compatible stores.
   - Integration tests against a local MinIO (or another S3-compatible server) on `/mnt/scratch`, skipped cleanly when it's absent. Include failure injection (5xx, throttling, dropped connections).
2. **Server (`crates/server`)**, an HTTP API (axum or similar) exposing `put` (streaming upload), `get` (streaming, with Range), `resolve` and `replace`, plus namespace admin:
   - Authn: bearer tokens, with a pluggable trait for OIDC later.
   - Authz: per-namespace read/write/admin policies.
   - An append-only audit log.
   - Prometheus metrics, `tracing` structured logs, health and readiness endpoints, graceful shutdown, and a TOML config file.
   - Bounded memory under large uploads, and backpressure.
3. **Client (`crates/client`)**: a Rust client library plus a remote mode for the `pico2peta` CLI. Coordinate with the lead for the CLI wiring.
4. **CI (`.github/workflows/`)**: fmt, clippy `-D warnings`, test, and the MinIO integration job.
5. **Deploy (`deploy/`)**: Dockerfile, a sample config, and a systemd unit.
6. Later: the Git smart-HTTP and OCI distribution adapters (compatibility layer). Agree the hash-mapping design with the lead first, and write it down in `docs/NET_PROGRESS.md`.

Until the core traits land on `demo`, design against the trait shapes in `docs/PLAN.md` and the current `crates/core`. Merge `demo` often and adapt.

## Environment
- `CARGO_TARGET_DIR=/mnt/scratch/pico2peta/cargo-target-net` and `TMPDIR=/mnt/scratch/pico2peta/tmp` are set. The root disk is nearly full, so keep builds, MinIO data and binaries under `/mnt/scratch/pico2peta/`.
- 4 cores and 15 GB RAM, shared with the lead's builds. Use `cargo build -j2` to avoid starving them.

## Working rules
- `git merge demo` into `net` at least every hour. Keep `net` building against the latest `demo`, because the lead merges `net` into `demo` at milestones.
- Commit often. **Do not add any Claude / Claude Code attribution** (no `Co-Authored-By: Claude…` trailer, no "Generated with Claude Code"). A commit-msg hook strips them anyway. **Push `net` to `origin` yourself** after each goal or meaningful batch, once fmt, clippy and tests pass (`git push origin net`; never force-push, never push `main`).
- Keep `docs/NET_PROGRESS.md` current: what's done, what's next, production gaps, requests for core, and questions for the human. If a decision belongs to the human, write it down there and carry on with a sensible default.

## Phase 2 (2026-11-15): share the core's speed-up backlog (from the human)

The core session keeps `docs/RESEARCH_SPEEDUP.md` §1, the streaming put pipeline in `crates/pack/src/store.rs`. You take the parts below, which don't touch `store.rs`, and you work in parallel with it. Order: A, B, C.

**A. Experiment 1c (§3).** You temporarily own `bench/exp1c/`, `scripts/exp1c*`, and an "Experiment 1c" section appended to `docs/EXP1.md`.
- Pre-register the protocol and thresholds in a new section of `docs/PLAN.md` and **commit that before writing any measurement code**. Reuse Exp1's corpus and thresholds unless there's a stated reason not to.
- Candidate: resemblance-detected base selection (Odess-style content-defined sampling super-features), then zstd patch-from (dictionary = base) delta. Keep a bounded candidate set, like Git's window.
- Baselines: the original `git gc --aggressive` (the frozen thresholds), plus `git repack -adf --path-walk` and `--name-hash-version=2`. Report against both, honestly.
- The mirrors are already in `/mnt/scratch/pico2peta/bench-data/mirrors`. Watch the disk: `/mnt/scratch` has about 19 GB free.

**B. Xet-compatible chunker (§4) as a standalone crate `crates/chunk`.**
- Implement Gear CDC exactly per the Xet chunking spec (https://huggingface.co/docs/xet/chunking): 8K/64K/128K, `MASK=0xFFFF000000000000`, min-size skip-ahead.
- Add a conformance test against the published reference chunk list (`xet-team/xet-spec-reference-files`). Download it to `/mnt/scratch/pico2peta/bench-data/xet-ref`, and skip the test cleanly if it's absent.
- Expose a streaming `Chunker` trait with FastCDC and Xet implementations, and benchmark both (GB/s, 1 core and 4 cores).
- Do not wire it into `store.rs`. The core lead integrates it later as a per-namespace option.

**C. Byte-grouping transform (§2) as pure functions in `crates/weights`.** You own a new module, `crates/weights/src/bytegroup.rs`.
- Implement `group(bytes, width) -> Vec<u8>` / `ungroup`, with width 2 (bf16/fp16) and 4 (fp32), plus proptest round-trips and no-panic tests on bad input.
- Measure zstd on the XOR delta with and without grouping, using the weights bench data or a real bf16 safetensors file.
- Do not add a pack codec. Hand the core a "Requests for core" entry with the measured gain, and the core adds the codec id in `store.rs`.

**Shared-machine rule.** Any timing measurement, and any long CPU-heavy run (Exp1c), must hold an exclusive lock, so that one session's benchmarks aren't distorted by the other's load:
`flock /mnt/scratch/pico2peta/bench.lock <command>`
Builds and tests don't need the lock; use `-j2`.
