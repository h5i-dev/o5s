import Pico2petaNetKernel
import H5iAppLib
import Policy
/-!
# h5i's obligations for the access kernel

`h5i app check` states these for `crates/net-kernel/src/lib.rs`'s `transition` and
`read_only` (`[obligations]` in `verify/net/h5i-app.toml`):

* `panic_free`: `transition` returns for every principal, snapshot and command;
* `read_only_sound`: a command `read_only` calls read-only (`ReadObject`, `ReadRef`) writes
  nothing when accepted.
-/
open Aeneas Aeneas.Std Result H5iAppLib pico2peta_net_kernel

namespace pico2peta_net_kernel.Spec.Obligations

/-- The value of ref `n` in the snapshot (the first entry for `n`). -/
def refS (s : access.Snapshot) (n : alloc.vec.Vec U8) : Option (alloc.vec.Vec U8) :=
  (s.refs.val.find? (fun e => decide (e.1 = n))).map (·.2)

@[step]
theorem ref_of_spec (s : access.Snapshot) (n : alloc.vec.Vec U8) :
    access.ref_of s n ⦃ r => r = refS s n ⦄ := by
  unfold access.ref_of access.ref_of_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_search (alloc.vec.Vec.deref s.refs) (fun e => decide (e.1 = n)) id
    (fun _ x => some x.2) none _ ?_ 0 (by simp))
  · intro y hy; rw [id_eq] at hy; rw [hy, searchFrom_find]; simp [refS, vec_deref_val]
  · intro i hi
    unfold access.ref_of_loop.body
    rw [H5iAppLib.slice_iter_next]
    split
    · rename_i hlt
      simp only [bind_ok]
      rcases hx : (alloc.vec.Vec.deref s.refs).val[i] with ⟨a, v⟩
      show (match (some (a, v) : Option _) with
        | none => ok (ControlFlow.done none)
        | some e =>
          (match e with
          | (v0, v1) => do
            let b ← alloc.vec.partial_eq.PartialEqVec.eq core.cmp.PartialEqU8 v0 n
            if b = true then do
              let v2 ← alloc.vec.CloneVec.clone core.clone.CloneU8 v1
              ok (ControlFlow.done (some v2))
            else ok (ControlFlow.cont ({ slice := alloc.vec.Vec.deref s.refs, i := i + 1 } : SIter _)))) ⦃ _ ⦄
      dsimp only
      step*
      · unfold IterSearchStep; left
        refine ⟨hlt, ?_, ?_⟩
        · erw [hx]; simp_all
        · erw [hx]; simp_all
      · unfold IterSearchStep
        refine ⟨hlt, ?_, rfl⟩
        erw [hx]; simp_all
    · step*
      unfold IterSearchStep; right; simp_all

@[step]
theorem one_row_spec (r : access.Row) : one_row r ⦃ rep => rep.rows.val = [r] ⦄ := by
  unfold one_row; step*

theorem transition_total (a : access.Principal) (s : access.Snapshot) (c : access.Command) :
    transition a s c ⦃ _ => True ⦄ := by
  unfold transition
  cases c <;> step*

/-- **Panic freedom** (`[obligations] panic-free`). -/
theorem panic_free : H5iAppLib.Obligation.PanicFree transition :=
  fun a s c => transition_total a s c

/-- **Read-only soundness** (`[obligations] read-only`). -/
theorem read_only_sound :
    H5iAppLib.Obligation.ReadOnlySound read_only transition (fun ws => ws.val) := by
  apply H5iAppLib.ReadOnly.sound_of_spec
  intro a s c hro
  cases c
  all_goals (simp only [read_only,
    access.Access.Insts.H5i_app_stdAuthzViewPrincipalSnapshotCommand.read_only] at hro)
  all_goals (try (cases hro))
  all_goals (unfold transition; step*)
  all_goals (simp_all [OnOk])

end pico2peta_net_kernel.Spec.Obligations
