import Spec
/-!
# Committing a write set

The kernel's `apply` and the local engine's in-place `commit` both compute
`Spec.applyAll`. `schema!` generated the table operations and their lemmas
(`Schema.lean`); only the dispatch over `Write` is left.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema H5iAppLib

namespace o5s_registry.ApplyProofs

theorem write_clone (w : Write) : Write.Insts.CoreCloneClone.clone w = ok w := by
  cases w <;> simp [Write.Insts.CoreCloneClone.clone, Repository.clone_eq, PatchRow.clone_eq, Dependency.clone_eq,
    Approval.clone_eq, CheckReport.clone_eq, Channel.clone_eq, Member.clone_eq, PathKind.clone_eq, Event.clone_eq,
    Body.clone_eq, Listing.clone_eq, Role.clone_eq]

/-- One write does what `applyWrite` says, and adds at most one row. -/
theorem apply_write_spec (s : Snapshot) (w : Write) (h : Snapshot.size s < Usize.max) :
    apply_write s w ⦃ s' => Snapshot.toSt s' = applyWrite (Snapshot.toSt s) w ∧
      Snapshot.size s' ≤ Snapshot.size s + 1 ⦄ := by
  unfold Snapshot.size at *
  unfold apply_write
  cases w <;> step* <;> simp_all [Snapshot.toSt, applyWrite, Snapshot.size, alloc.vec.Vec.length] <;>
    grind [upsert_length]

/-- The kernel's `apply` computes `Spec.applyAll`, as long as no vector can
overflow (each write adds at most one row). -/
theorem apply_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Snapshot.size s + ws.length < Usize.max) :
    apply s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ :=
  apply_spec' Snapshot.toSt applyWrite apply_write_spec write_clone s ws h

/-- The in-place `commit` computes the same. -/
theorem commit_spec (s : Snapshot) (ws : alloc.vec.Vec Write) (h : Snapshot.size s + ws.length < Usize.max) :
    commit s ws ⦃ s' => Snapshot.toSt s' = applyAll (Snapshot.toSt s) ws.val ⦄ := by
  unfold commit commit_loop
  apply WP.spec_mono (loop_fold ws.val Snapshot.toSt applyWrite
    (fun t k => Snapshot.size t + (ws.length - k) < Usize.max)
    (fun x => commit_loop.body ws x.1 x.2) ?_ s 0#usize (by simp) (by simpa using h))
  · intro r hr; rw [hr]; simp [applyAll]
  · intro t j hj ht
    unfold commit_loop.body
    simp only [alloc.vec.Vec.length] at hj ht
    dsimp only
    split
    · rename_i hlt
      step as ⟨ w, hw1 ⟩
      rw [write_clone]
      simp only [bind_ok]
      step with apply_write_spec t w (by scalar_tac) as ⟨ t', ht1, ht2 ⟩
      step*
      simp only [FoldStep]
      have : j.val < ws.val.length := by scalar_tac
      refine ⟨by scalar_tac, by rw [ht1, hw1], by scalar_tac, by simp only [alloc.vec.Vec.length] at *; omega⟩
    · simp only [WP.spec_ok, FoldStep, and_true]
      scalar_tac

end o5s_registry.ApplyProofs
