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
| P6 a name owned by a front moves only through that front's handle | `non_owner_cannot_move`, `non_owner_cannot_move_run` (`Fronts.lean`, mirroring `LocalRepo::guard` / `FrontHandle`) |
| P7 promotion moves only its destination, and never from a stale source | `promote_only_moves_dst`, `updateMany_stale_noop` (`Fronts.lean`, mirroring `pico2peta_attest::promote`); immutability of the destination follows from P3 (`updateMany_frozen`) |

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

## How later features map onto the model (2026-11-15 catch-up)

| feature | how it is covered |
|---|---|
| backend ref log (`[refs] store = "backend"`) | Same pure decision functions (`RefState::decide_*`) as the journal, which the model and the difftest cover. Linearizing them by conditional create is an assumption, like the journal's flock. Tested: multi-host contention, crash sweep, model proptest on the log engine (`use_log` in the difftest). |
| group commit (journal) | Batches commits under the one exclusive flock, decided in order against the batch's state, so the committed history is still a sequential run of `step`. The model's linearization assumption is unchanged. Durability before acknowledgement is tested in Rust (fault injection mid-batch). |
| `update_many`, `replace_many` | `updateMany` in the model; P3 through `updateMany_frozen`; in the difftest. The 2026-11-15 linear-time rewrite of `decide_*_many` is checked by the difftest. |
| Frame3 references and GC | `Gc.lean`'s `children` relation is abstract. Rust's `chunks()` returns a Frame3 member's reference, and GC follows it (mark, `protected_children`, `ensure_live`), so `gc_preserves_live` applies with references as children. |
| fronts' owned prefixes | P6. Rust compares claims as string prefixes of names (claims end with `/`); the model compares segment lists. They agree for claims that end at a segment boundary, which is how fronts claim them. |
| promotion | P7, plus the difftest's `M src dst` operation against `crates/attest::promote`'s transaction. The policy checks (signatures, provenance) are ordinary Rust preconditions, tested in `crates/attest`. |

**Assumptions (not proved):** the hash is collision-free; backend `put_if_absent` is atomic and durable; fsync is
honest; the journal append is atomic at record granularity (CRC plus torn-tail truncation); commits are linearized by
the flock (journal, including group commit) or by conditional create (backend log); front code holds its handle's
capability honestly (a front is trusted code in the server process).

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
