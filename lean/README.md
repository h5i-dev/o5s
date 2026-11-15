# Lean 4 model of the pico2peta core

`Pico2peta/Model.lean` is a pure model of the object layer (`put`), the name
layer (`replace` with OCC read sets, `setRule`) and namespace rules
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
| P3 immutable names can't move | `frozen_step`, `frozen_run`, `published_never_moves` |
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
