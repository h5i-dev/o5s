# Lean 4 model of the pico2peta core

`Pico2peta/Model.lean` is a pure model of the object layer (`put`), the name
layer (`replace`, `deleteRef` and the atomic transaction `updateMany` with OCC read sets and tombstoned versions, `setRule`) and namespace rules
(longest-prefix governance, `mobilityOf`). It mirrors `crates/core::mobility`
and `crates/refs::RefState::{decide_replace, decide_set_rule}`.

`Pico2peta/Properties.lean` proves these properties, with no `sorry`. The only
axioms are `propext`, `Classical.choice` and `Quot.sound`:

| property | theorem |
|---|---|
| P1 stored content never changes | `objects_stable`, `objects_stable_run` |
| P1' stored data hashes to its key | `wf_step`, `wf_run` |
| P2 a successful replace validated its read set | `replace_ok_validated` |
| P2 no undetected overwrite | `no_lost_update` |
| P3 immutable names can't move (including through transactions) | `frozen_step`, `frozen_run`, `published_never_moves`, `updateMany_frozen` |
| P4 unauthorized principals can't move or read names | `unauthorized_cannot_move`, `unauthorized_cannot_move_run`, `resolve_denied` (`Auth.lean`, mirroring the server's `Authorizer`) |
| P5 GC/repack never lose live objects | `gc_preserves_live`, `gc_preserves_refs` (`Gc.lean`, covering the delete phase's verify-before-delete) |

**Assumptions** (not modeled): for P5, the live set is computed from the refs that are current at delete time, which is the put→replace lease documented in `crates/repo/src/gc.rs`. Also: `replace`/`setRule` are linearized by the
journal's exclusive flock commit point; fsync is honest; the backend's
`put_if_absent` is atomic and create-only. These are tested in Rust by fault
injection and multi-process tests, not proved.

Build (toolchain is pinned; no Mathlib needed):

```sh
export ELAN_HOME=/mnt/scratch/ec2-user/elan PATH=$ELAN_HOME/bin:$PATH
cd lean && lake build
```

`.lake` is a symlink to scratch on the dev host.

## Differential testing (Cedar-style)

`Pico2peta/Exec.lean` is an executable form of the model (`lake build p2p-model`).
Its only non-spec definition, `setRuleC`, is proved equal to the spec's
`setRule` (`setRuleC_eq`). `crates/refs/tests/lean_diff.rs` runs 300 random
operation sequences through both the Rust `RefStore` (with compaction forced
every few records) and the Lean binary, and requires identical outcomes.
Set `P2P_REQUIRE_LEAN=1` to fail rather than skip when the binary is missing.

The difftest already paid off once. Writing it exposed that the Lean
`setRule` lacked the Rust idempotency rule (re-setting the same rule on a
non-empty prefix is `Ok`). The model was fixed and the proofs re-checked.
