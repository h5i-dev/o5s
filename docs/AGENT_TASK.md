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
