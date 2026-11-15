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
- Commit often, and don't push. End commit messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Keep `docs/NET_PROGRESS.md` current: what's done, what's next, production gaps, requests for core, and questions for the human. If a decision belongs to the human, write it down there and carry on with a sensible default.
