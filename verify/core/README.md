# Proofs about pico2peta's extracted core kernels (docs/H5I_PLAN.md, H2)

`crates/kernel` holds the core's decisions as pure Rust in the Charon/Aeneas subset.
`h5i app extract` translates it to `proofs/generated/Pico2petaKernel.lean`, which is generated, never
edited, and checked in CI with `--check`. `proofs/Proofs.lean` proves theorems about that extracted code. The
shells (`pico2peta-refs`'s `RefState`, the journal, `pico2peta-repo`) build each decision's view, call the
kernel, and apply its result.

## What is proved (all on the extracted functions; gate: no `sorry`, only `propext`, `Classical.choice`, `Quot.sound`)

Every kernel function has a `*_spec` theorem (the `⦃ ⦄` triple), which also proves it total: no panic,
overflow or out-of-bounds index on any input. Where a function writes to a `Vec`, the bound comes from its
input sizes, not from an assumption. The properties:

| property | theorem | statement |
|---|---|---|
| P1 reads | `read_accepts_only_key` | a verified read accepts bytes only if their digest is the key (`objects::accept_read`) |
| P1 puts | `put_never_overwrites` | a put of a present object writes nothing; only absent objects are appended |
| P2 no undetected overwrite | `commit_validated` | a committed batch validated its whole read set: every observed state equals the current one |
| P2 versions | `commit_versions` | a committed write sets the version to the last version (current, else tombstone) + 1 |
| P3 published names never move | `commit_respects_rules` | in a committed batch, an existing name under a create-once rule is neither written nor deleted; under a forbidding rule nothing is written |
| governance | `governing_spec`, `mobility_of_spec`, `mobilityOf_unique` | the governing rule is the longest covering prefix (first on ties) and is unique; `mobility` follows the rule table |
| P5 GC | `delete_keeps_live` | the delete phase passes only if no live object is served by a pack it deletes |
| GC victims | `is_victim_spec` | the repack rule exactly: `repack_all`, small pack, empty, inconsistent, or garbage ≥ min (permille, no overflow) |
| P6 fronts | `guard_owner`, `may_set_rule_spec`, `ownerOf_unique` | an accepted write (or rule) is by the name's owner, the front with the longest claimed prefix, or the name is unowned |
| P7 promotion | `promote_plan`, `promote_src_fresh` | an accepted plan meets every policy requirement and targets `src`'s current object at its current version; committed, `src` was still at that version (with P2) |
| decoders | `parse_header_spec`, `le32_spec`, `le64_spec`, `hash_at_spec` | the pack record header decoder equals its little-endian spec on every input: the first failing check (short, magic, kind, codec, bounds) or every field decoded |
| journal framing | `journal_step_spec` | a CRC-invalid record followed by more data is corruption, never a torn tail: valid history is never truncated |
| retention | `plan_spec` | one action per entry; the newest `keep` are kept; a deletion is old, regenerable when required, and allowed by the rules |

P4, authorization, is net's kernel (`verify/net`, `h5i_app_std::authz::guard`).

## What is trusted

- **Trusted infrastructure:** BLAKE3 (collision resistance), signature verification (ed25519), the backend's
  create-only puts, fsync and flock. These sit behind the kernels, as in h5i-app's trust model.
- **The shell builds the view from its state.** It supplies each read-set name's current state and each
  target's value, tombstone and covering rules (`RefState::decide_kernel`). This is tested
  (`crates/kernel/tests/kernel.rs` against a direct model of the old semantics, plus every refs test); the
  frame theorem relating view and full state is the next step.
- **The shell applies the result.** Journal records, fsync and flock linearization are covered by Rust tests
  (crash sweeps, contention, group commit), not proofs.
- **The tools:** Charon, Aeneas, Lean and rustc.

## Proof sensitivity (`h5i app mutate`, 2026-11-16)

17/17 mutants caught, 0 survived:

| mutant | caught by |
|---|---|
| `stale_read_accepted` | stale_reads_spec |
| `conflict_needs_two_stale` | decide_spec |
| `published_name_moves` | decide_target_spec |
| `immutable_delete_allowed` | decide_target_spec |
| `version_skips` | decide_target_spec |
| `releases_mutable` | mobility_spec |
| `governing_last_of_ties` | pick_spec |
| `prefix_not_segment_aligned` | is_under_spec |
| `non_owner_writes` | may_write_spec |
| `delete_with_live_in_doomed_pack` | delete_ok_spec |
| `victim_ratio_off_by_10x` | is_victim_spec |
| `promote_without_provenance` | decide_promote_spec |
| `unsigned_attestation_counts` | counts_spec, signed_by_spec |
| `retention_deletes_newest` | action_spec |
| `read_checks_31_bytes` | accept_read_spec |
| `header_skips_kind_check` | parse_header_spec |
| `journal_truncates_history` | journal_step_spec |

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
