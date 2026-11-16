# Proofs about pico2peta's extracted core kernels (docs/H5I_PLAN.md, H2)

`crates/kernel` holds the core's decisions as pure Rust in the Charon/Aeneas subset.
`h5i app extract` translates it to `proofs/generated/Pico2petaKernel.lean`, which is generated, never
edited, and checked in CI with `--check`. `proofs/Proofs.lean` proves theorems about that extracted code. The
shells (`pico2peta-refs`'s `RefState`, the journal, `pico2peta-repo`) build each decision's view, call the
kernel, and apply its result.

## What is proved (all on the extracted functions; gate: no `sorry`, only `propext`, `Classical.choice`, `Quot.sound`)

| property | theorem | statement |
|---|---|---|
| totality | `decide_spec` (and every `*_spec`) | `refs::decide` never fails: no panic, overflow or out-of-bounds index, for every view |
| P2, no undetected overwrite | `commit_validated` | a committed batch validated its whole read set: every observed state equals the current one |
| P2, versions | `commit_versions` | a committed write sets the version to the last version (current, else tombstone) + 1 |
| P3, published names never move | `commit_respects_rules` | in a committed batch, an existing name under a create-once rule is neither written nor deleted; under a forbidding rule nothing is written |
| governance | `governing_spec`, `mobility_of_spec`, `mobilityOf_unique` | the governing rule is the longest covering prefix (first on ties) and is unique; `mobility` follows the rule table (`heads/*` mutable, `releases/*` create-once, otherwise forbidden) |
| name grammar helpers | `is_under_spec`, `starts_with_spec`, `cmp_spec` | segment-aligned prefixes; byte order equals `BTreeMap<Name>` order |

The remaining properties (P1, P4–P7) and the decoders follow as their kernels land (H2 order in
docs/PROGRESS.md). P4, authorization, is net's kernel (`verify/net`).

## What is trusted

- **The shell builds the view from its state.** It supplies each read-set name's current state and each
  target's value, tombstone and covering rules (`RefState::decide_kernel`). This is tested
  (`crates/kernel/tests/kernel.rs` against a direct model of the old semantics, plus every refs test); the
  frame theorem relating view and full state is the next step.
- **The shell applies the result.** Journal records, fsync and flock linearization are covered by Rust tests
  (crash sweeps, contention, group commit), not proofs.
- **The tools:** Charon, Aeneas, Lean and rustc.

## Proof sensitivity (`h5i app mutate`, 2026-11-16)

8/8 mutants caught, 0 survived:

| mutant | caught by |
|---|---|
| a stale read accepted (`Absent` observations skipped) | `stale_reads_spec` |
| conflict needs two stale entries | `decide_spec` |
| a published name moves (`is_some` → `is_none`) | `decide_target_spec` |
| deleting an immutable name allowed | `decide_target_spec` |
| versions skip (+2) | `decide_target_spec` |
| `releases/*` mutable | `mobility_spec` |
| the last of equally long rules wins | `pick_spec` |
| prefixes not segment-aligned | `is_under_spec` |

## Running

On the dev host, the tools and prebuilt Lean packages are under `/mnt/scratch/ec2-user`. Use that toolchain
env, but with your own target dir:

```sh
W=/mnt/scratch/ec2-user
export RUSTUP_HOME=$W/rustup CARGO_HOME=$W/cargo OPAMROOT=$W/opam ELAN_HOME=$W/elan \
  PATH=$W/bin:$W/cargo/bin:$W/elan/bin:$PATH CARGO_TARGET_DIR=/mnt/scratch/pico2peta/h5i-target \
  TMPDIR=/mnt/scratch/pico2peta/tmp H5I_LAKE_CACHE=off
mkdir -p verify/core/proofs/.lake && ln -sfn $W/lake/arith/packages verify/core/proofs/.lake/packages
cd verify/core && h5i app prove     # extract, build, gate (~2 min)
h5i app mutate                      # the mutants above (~5 min)
```
