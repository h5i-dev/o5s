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

**Assumptions** (not modeled): `replace`/`setRule` are linearized by the
journal's exclusive flock commit point; fsync is honest; the backend's
`put_if_absent` is atomic and create-only. These are tested in Rust by fault
injection and multi-process tests, not proved.

Build (toolchain is pinned; no Mathlib needed):

```sh
export ELAN_HOME=/mnt/scratch/ec2-user/elan PATH=$ELAN_HOME/bin:$PATH
cd lean && lake build
```

`.lake` is a symlink to scratch on the dev host.
