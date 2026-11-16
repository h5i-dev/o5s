# Rebuilding pico2peta on h5i-app (from the human, 2026-11-16). This overrides the verification approach.

## Correction
The intent was always the following. **pico2peta is implemented with `h5i-app`. Its decision logic is written as h5i-app kernels: pure Rust functions that Charon/Aeneas extract to Lean 4. The theorems are proved about the extracted code.**

A hand-written Lean model plus a differential test against the Rust is **worth nothing** for this project, and it will be removed. That approach was never part of the brief; our original task file failed to state the intent.

## Phase H0: freeze the baseline first (do this now)
1. net finishes the in-flight final batch (window B, Maven).
2. Before any h5i code, write `docs/BASELINE.md`. It records every number we currently know, each with the commit, host load, and the source file of the raw rows:
   - Exp3: sizes, build, reads, RSS;
   - BENCH.md core numbers (put/get, put_many, read path, replace and group commit, verified read strict and eof, scale windows 1–2);
   - net's §6.5 final batch: Git, OCI, HF, PyPI, npm, Maven.
3. Tag that commit `baseline-pre-h5i` on `demo`, and push the tag. **Don't tag `v0.1.0-rc.1`.** The RC waits until the h5i rebuild passes the no-regression check.
4. **Pre-register the no-regression rule in `PLAN.md`:** after the rebuild, rerun the same benchmarks with the same protocol and corpus.
   - Every metric must be no worse than the larger of 3% or that metric's baseline run-to-run range.
   - No pass/fail tier may get worse.
   - Exp3 sizes must be byte-identical, unless the encoding is intentionally unchanged and the layout differs only by deterministic reordering.
   - A regression is a FAIL. Fix it before RC. Don't rationalize it.

## Phase H1: learn h5i-app (both sessions, read-only)
- Source: `/mnt/scratch/ec2-user/h5i-src` (h5i 0.5.5; `crates/h5i-app*`, `docs/design/design-app*.md`, especially `design-app-trust.md` and `design-app-authz.md`, and the examples in `examples/app/*`, especially `calculator/TUTORIAL.md`).
- Tools are already built:
  - `/mnt/scratch/ec2-user/bin/{h5i,charon,aeneas}`, with the env in `/mnt/scratch/ec2-user/env.sh`. Don't source it wholesale; it redirects `CARGO_TARGET_DIR`.
  - Lean packages are prebuilt for Lean 4.31 under `/mnt/scratch/ec2-user/lake/arith/packages/` (`h5i_app_lib`, `aeneas`, mathlib, ...). Use them by local path; don't download or rebuild.
- Workflow: `h5i app extract`, `h5i app prove`, `h5i app mutate` (proof sensitivity).
- Know the extraction limits: no interior mutability or unsafe in kernels, and Aeneas-supported std only. Design kernels for them.

## Phase H2: kernels (decision logic), extracted and proved
Restructure so that every **decision** is an h5i-app kernel (`transition(actor, snapshot, command) -> (writes, reply)` style, or a pure function the framework extracts). Trusted infrastructure (packs, fsync, S3, HTTP transport, zstd/BLAKE3) stays ordinary Rust behind the kernel boundary, as in h5i-app's trust model.

**Core session**
- **Name layer:** `RefState::decide_replace` / `decide_*_many` / delete / `setRule` / `mobility` (longest-prefix rules), group-commit ordering semantics, and the backend ref-log decision.
- **Fronts:** front ownership guard (owned prefixes), promotion (`attest::promote` decision and policy check), retention decision, and GC decisions (mark/sweep/delete verify-before-delete, `ensure_live`, protected children incl. Frame3 references).
- **Proofs on the extracted Lean:** prove P1–P7 (VERIFICATION/lean README list) about the extracted functions. Use `h5i_app_lib` obligations where they fit (panic-freedom, read-only soundness).
  - Run `h5i app mutate` and record its sensitivity.
  - **Delete** the hand-written `lean/Pico2peta/*` model and `crates/refs/tests/lean_diff.rs` once the extracted proofs cover them.
- **Extractable parsers/decoders where feasible:** pack record header, index entries, manifest/delta/Frame3 directories, ref-journal records. Prove panic-freedom and round-trip/spec equality, as h5i does for h1/token/json.

**Net session**
- **Server on `h5i-app-http`** (it's Axum-based, so keep the existing routes and streaming): `Actor`, `H5iApp`.
- **Authorization:** the per-namespace read/write/admin policy becomes an h5i-app authorization kernel with `h5i_app_std::authz::guard` (proved permitted-writes and noninterference). Bearer tokens use `h5i-app-token` (extracted, proven).
- **Front decisions as kernels:** Git ref-update/push acceptance (`--atomic`, ng rules), OCI tag/manifest rules, PyPI/npm/Maven publish/yank/unpublish/dist-tag rules, and the HF commit acceptance.
- **Extractable parsers:** pkt-line lengths, oid/digest hex (the strict decoders added on 2026-11-16), LFS pointers, and the Xet shard/xorb headers. Prove that they are panic-free and agree with their specs.

## Phase H3: no-regression check, then RC
Rerun every benchmark in BASELINE.md, same protocol, in announced bench windows. Report against the pre-registered rule. Then do release prep and tag `v0.1.0-rc.1`. The human merges to `main`.

## Rules
- Performance-critical hot paths (chunking, hashing, compression, streaming I/O) stay outside kernels, unless extraction is free.
- Kernel calls must not add measurable overhead; measure with short dev checks.
- Keep the gate, push and disk rules from ROADMAP.md. No Claude attribution.
- Coordinate kernel boundaries between the sessions with messages: who owns which kernel crate, and the shared types.
