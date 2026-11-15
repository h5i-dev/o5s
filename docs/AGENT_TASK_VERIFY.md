# Task for the verification & hardening session (`pico2peta-verify`)

You work autonomously in tmux session `pico2peta-verify`, in git worktree
`/home/ec2-user/Dev/pico2peta-verify` on branch `verify`. The human may be disconnected for long periods.

Read first: `docs/DESIGN_BRIEF.md`, `docs/AGENT_TASK.md` (esp. "Quality bar" and "Team"), `docs/PLAN.md`, `docs/PROGRESS.md`.

The goal is a **production-grade** system, not a demo. Your job is to make it *provably and testably correct*, and to hold the implementation lead (session `pico2peta`, branch `demo`) to that bar.

## You own
`lean/`, `crates/model/`, `crates/difftest/`, `fuzz/`, crash/fault-injection suites under `tests/`, and `docs/REVIEW.md`, `docs/VERIFICATION.md`. Do not edit other paths except trivially to expose test hooks — request API changes via `docs/REVIEW.md` instead.

## Goals
1. **Reference model (`crates/model`)**: pure, executable model of objects / packs-as-maps / refs / namespace rules / `replace` OCC — structurally identical to the Lean model.
2. **Lean 4 (`lean/`)**: model + proofs of P1–P5 from `docs/PLAN.md §5` (content immutability; no undetected overwrite; immutable names can't move; authorization; GC/repack reachability safety). No `sorry` in the final state. State every assumption (hash injectivity, backend atomicity) as an explicit hypothesis/axiom in one file, documented in `docs/VERIFICATION.md`.
3. **Differential testing (Cedar style, `crates/difftest`)**: proptest generators of random op sequences (incl. concurrent interleavings, reopen/recovery) run against the model and the real implementation; compare every observable. Wire to CI.
4. **Crash consistency & fault injection**: a fault-injecting `Backend`/filesystem shim that can fail or crash at every write/fsync/rename point; tests that recovery always yields a state the model allows (ShardStore style).
5. **Fuzzing (`fuzz/`, cargo-fuzz)**: every on-disk/wire decoder (pack records, index, manifests, ref journal, protocol messages).
6. **Review**: after each new commit on `demo` (`git log verify..demo`), review for correctness, durability, concurrency, security, and panics; file findings in `docs/REVIEW.md` with severity, file:line, and a failing test where possible.

## Environment
- `CARGO_TARGET_DIR=/mnt/scratch/pico2peta/cargo-target-verify` and `TMPDIR=/mnt/scratch/pico2peta/tmp` are set. Root disk is nearly full — keep build/data on `/mnt/scratch`.
- `scripts/env.sh` (on demo) shows the scratch layout; don't let it override your CARGO_TARGET_DIR.
- Lean 4.31.0: `export ELAN_HOME=/mnt/scratch/ec2-user/elan PATH=/mnt/scratch/ec2-user/elan/bin:$PATH`. Prebuilt Mathlib at `/mnt/scratch/ec2-user/lake/arith/packages/mathlib` (rev fabf563a7c) — require by local path; do not download/rebuild Mathlib. Aeneas lib at `/mnt/scratch/ec2-user/lake/arith/packages/aeneas`.

## Working rules
- `git merge demo` into `verify` frequently (at least every hour and before each review). Commit often; don't push. End commit messages with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- The lead merges `verify` into `demo` at milestones — keep `verify` always building and `cargo test` green (mark tests that demonstrate open bugs with `#[ignore = "REVIEW-<n>"]` so they don't block, and list them in REVIEW.md).
- Keep `docs/VERIFICATION.md` current: what is proven, what is tested, what is assumed, and gaps.
- Human-only decisions go under "Questions for human" in `docs/REVIEW.md`; continue with a sensible default.
