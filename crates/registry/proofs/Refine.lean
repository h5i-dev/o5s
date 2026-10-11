import Model
import Commands
/-!
# The extracted kernel computes the model

`transition_refines`: whenever the extracted `transition` returns, its result
is `Model.step` on the same rows, with write vectors read as lists.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema o5s_registry.Helpers
  o5s_registry.Exact H5iAppLib

namespace o5s_registry.Refine

/-- A result with its write vector read as a list. -/
def lift : core.result.Result (alloc.vec.Vec Write × Reply) Error → Model.Out
  | .Ok (ws, r) => .Ok (ws.val, r)
  | .Err e => .Err e

attribute [local simp] H5iAppLib.u64_val_eq

theorem inc_val {x : U64} (h : x ≠ Model.MAX) : (Model.inc x).val = x.val + 1 := by
  have hlt := Commands.lt_max (show ¬x = core.num.U64.MAX from h)
  have hw : UScalarTy.U64.numBits = 64 := rfl
  show (x.bv + 1).toNat = x.val + 1
  rw [BitVec.toNat_add, U64.bv_toNat]
  have h1 : (1 : BitVec UScalarTy.U64.numBits).toNat = 1 := by rw [hw]; rfl
  rw [h1]
  exact Nat.mod_eq_of_lt (by omega)

theorem inc_of_val {x y : U64} (hne : x ≠ Model.MAX) (h : y.val = x.val + 1) : y = Model.inc x :=
  (H5iAppLib.u64_val_eq _ _).1 (by rw [inc_val hne, h])

/-- `x + 1#u64` computes `inc x`. -/
theorem add_one_eq {x y : U64} (h : x + 1#u64 = ok y) : y = Model.inc x := by
  have hy := H5iAppLib.add_ok_val h
  have hb := H5iAppLib.add_ok_bound h
  have hne : x ≠ Model.MAX := by
    intro e; subst e; have := H5iAppLib.u64_max_val; simp [Model.MAX] at hb; scalar_tac
  apply (H5iAppLib.u64_val_eq _ _).1
  rw [inc_val hne]; simpa using hy

theorem register_refines (a : Principal) (ls : alloc.vec.Vec Listing) (n : alloc.vec.Vec U8) (repo : U64) :
    register a ls n repo ⦃ r => lift r = Model.register a ls.val n repo ⦄ := by
  unfold register Model.register
  by_cases h1 : a.scope = a.org
  · by_cases h2 : a.org.val ≥ REPOSITORY_SCOPE.val
    · have : a.org ≥ REPOSITORY_SCOPE := h2
      simp [h1, this, h2, lift]
    · by_cases h3 : repo.val < REPOSITORY_SCOPE.val
      · have : repo < REPOSITORY_SCOPE := h3
        have : ¬ a.org ≥ REPOSITORY_SCOPE := h2
        simp [h1, *, lift]
      · have : ¬ a.org ≥ REPOSITORY_SCOPE := h2
        have : ¬ repo < REPOSITORY_SCOPE := h3
        simp only [h1, bne_self_eq_false, Bool.false_eq_true, ↓reduceIte, this, h2, h3, ‹¬ a.org ≥ REPOSITORY_SCOPE›]
        step with valid_name_exact as ⟨ b, hb ⟩
        subst hb
        simp only [ne_eq, not_true_eq_false, ↓reduceIte]
        cases hv : validName n.val
        · simp [lift]
        · simp only [↓reduceIte, Bool.not_true, Bool.false_eq_true]
          step as ⟨ o, ho ⟩
          rw [← ho]
          cases o with
          | some l => simp only; split <;> simp [lift]
          | none =>
            simp only
            step as ⟨ u, hu ⟩
            rw [← hu]
            cases u <;> simp only [↓reduceIte, Bool.false_eq_true] <;> step* <;> simp_all [lift]
  · simp [h1, lift]

theorem create_refines (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) :
    create a r n ⦃ res => lift res = Model.create a r n ⦄ := by
  unfold create Model.create event
  by_cases h1 : a.scope.val < REPOSITORY_SCOPE.val
  · have : a.scope < REPOSITORY_SCOPE := h1
    simp [this, h1, lift]
  · have : ¬ a.scope < REPOSITORY_SCOPE := h1
    simp only [this, h1, ↓reduceIte]
    step with valid_name_exact as ⟨ b, hb ⟩
    subst hb
    cases hv : validName n.val
    · simp [lift]
    · simp only [↓reduceIte, Bool.not_true, Bool.false_eq_true]
      cases hc : r.created
      · simp only [Bool.false_eq_true, ↓reduceIte]
        step*
        all_goals (try (have := H5iAppLib.usize_max_ge; simp_all; omega))
        have hm : main3 = Model.mainLabel := alloc.vec.Vec.ext _ _ (by
          simp [main3_post, main2_post, main1_post, main_post, Model.mainLabel])
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]
      · simp only [↓reduceIte]
        by_cases ho : r.org = a.org
        · simp only [ho, ↓reduceIte, true_and]
          step as ⟨ e, he ⟩
          split <;> simp_all [lift]
        · simp [ho, lift]

theorem approve_refines (u : U64) (s : Snapshot) (patch : U64) :
    approve u s patch ⦃ res => lift res = Model.approve u (Snapshot.toSt s) patch ⦄ := by
  unfold approve Model.approve
  step as ⟨ o, ho ⟩
  simp only [Snapshot.toSt, ← ho]
  cases o with
  | none => simp [lift]
  | some p =>
    simp only
    by_cases ha : p.author = u
    · simp [ha, lift]
    · by_cases hm : s.repository.events = core.num.U64.MAX
      · simp [ha, hm, lift, Model.MAX]
      · have hm' : s.repository.events ≠ Model.MAX := hm
        simp only [ha, hm, hm', ↓reduceIte]
        unfold bump_events event
        step*
        all_goals (try (have := Commands.lt_max hm; scalar_tac))
        have hx := inc_of_val hm' x_post
        have hi := inc_of_val hm' i_post
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]

theorem check_refines (u : U64) (s : Snapshot) (patch : U64) (passed : Bool) :
    check u s patch passed ⦃ res => lift res = Model.check u (Snapshot.toSt s) patch passed ⦄ := by
  unfold check Model.check
  step as ⟨ o, ho ⟩
  simp only [Snapshot.toSt, ← ho]
  cases o with
  | none => simp [lift]
  | some p =>
    simp only
    by_cases ha : p.author = u
    · simp [ha, lift]
    · by_cases hm : s.repository.events = core.num.U64.MAX
      · simp [ha, hm, lift, Model.MAX]
      · have hm' : s.repository.events ≠ Model.MAX := hm
        simp only [ha, hm, hm', ↓reduceIte]
        unfold bump_events event
        step*
        all_goals (try (have := Commands.lt_max hm; scalar_tac))
        have hx := inc_of_val hm' x_post
        have hi := inc_of_val hm' i_post
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]

end o5s_registry.Refine
