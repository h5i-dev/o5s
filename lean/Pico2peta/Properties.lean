import Pico2peta.Model

/-!
# Properties of the pico2peta core

* **P1. Stored content never changes.** `objects_stable` and `objects_stable_run`.
  In addition, `wf_run` proves stored data always hashes to its key.
* **P2. No undetected overwrite.** `replace_ok_validated` says a successful replace
  saw exactly the current state for its whole read set.
  `no_lost_update` says two writers that observed the same version of a name
  can't both succeed.
* **P3. Immutable names can't move.** `frozen_step` and `frozen_run` say that once a
  name exists and its effective rule is create-once or forbidden, it keeps
  its value and its rule forever.
-/

set_option linter.unusedSectionVars false

namespace Pico2peta

variable {H B : Type} [DecidableEq H]

/-! ## P1: content immutability -/

theorem put_objects_stable (hashOf : B → H) (s : State H B) (d : B) (h : H) (x : B)
    (hx : s.objects h = some x) : (put hashOf s d).objects h = some x := by
  unfold put
  split
  · exact hx
  · rename_i hnone
    simp only [update]
    split
    · rename_i heq; subst heq; rw [hnone] at hx; cases hx
    · exact hx

theorem replace_objects (s : State H B) n rs new : (replace s n rs new).2.objects = s.objects := by
  unfold replace
  split
  · rfl
  split
  · rfl
  split
  · rfl
  split <;> rfl

theorem setRule_objects (s : State H B) p r : (setRule s p r).2.objects = s.objects := by
  unfold setRule; split
  · rfl
  · split <;> rfl

theorem objects_stable (hashOf : B → H) (s : State H B) (op : Op H B) (h : H) (x : B)
    (hx : s.objects h = some x) : (step hashOf s op).2.objects h = some x := by
  cases op with
  | put d => exact put_objects_stable hashOf s d h x hx
  | replace n rs new => simp only [step, replace_objects]; exact hx
  | setRule p r => simp only [step, setRule_objects]; exact hx

theorem objects_stable_run (hashOf : B → H) (s : State H B) (ops : List (Op H B)) (h : H) (x : B)
    (hx : s.objects h = some x) : (run hashOf s ops).objects h = some x := by
  induction ops generalizing s with
  | nil => exact hx
  | cons op ops ih => exact ih _ (objects_stable hashOf s op h x hx)

/-- Well-formedness: every stored object hashes to its key. -/
def WF (hashOf : B → H) (s : State H B) : Prop := ∀ h x, s.objects h = some x → hashOf x = h

theorem wf_step (hashOf : B → H) (s : State H B) (op : Op H B) (hw : WF hashOf s) :
    WF hashOf (step hashOf s op).2 := by
  intro h x hx
  cases op with
  | put d =>
    simp only [step, put] at hx
    split at hx
    · exact hw h x hx
    · simp only [update] at hx
      split at hx
      · rename_i heq; cases hx; exact heq.symm
      · exact hw h x hx
  | replace n rs new => simp only [step, replace_objects] at hx; exact hw h x hx
  | setRule p r => simp only [step, setRule_objects] at hx; exact hw h x hx

theorem wf_run (hashOf : B → H) (s : State H B) (ops : List (Op H B)) (hw : WF hashOf s) :
    WF hashOf (run hashOf s ops) := by
  induction ops generalizing s with
  | nil => exact hw
  | cons op ops ih => exact ih _ (wf_step hashOf s op hw)

/-! ## P2: no undetected overwrite -/

/-- Anatomy of a successful replace. -/
theorem replace_ok_spec (s : State H B) n rs new v s'
    (h : replace s n rs new = (.ok v, s')) :
    (rs.any fun x => decide (x.1 = n)) = true ∧
    (rs.any fun x => decide (observe s x.1 ≠ x.2)) = false ∧
    v = nextVersion (s.refs n) ∧
    s' = { s with refs := update s.refs n (some ⟨new, nextVersion (s.refs n)⟩) } := by
  unfold replace at h
  cases h1 : (rs.any fun x => decide (x.1 = n)) <;> simp only [h1, Bool.not_false, Bool.not_true] at h
  · cases h
  cases h2 : (s.objects new).isNone <;> simp only [h2] at h
  · cases h3 : (rs.any fun x => decide (observe s x.1 ≠ x.2)) <;> simp only [h3] at h
    · refine ⟨rfl, rfl, ?_⟩
      revert h
      cases hm : mobilityOf s.rules n <;> cases hr : s.refs n <;> simp
      all_goals (intro h1 h2; exact ⟨h1.symm, h2.symm⟩)
    · cases h
  · cases h

/-- A successful replace validated its whole read set against the current state. -/
theorem replace_ok_validated (s : State H B) n rs new v s'
    (h : replace s n rs new = (.ok v, s')) : ∀ x ∈ rs, observe s x.1 = x.2 := by
  obtain ⟨_, h2, _, _⟩ := replace_ok_spec s n rs new v s' h
  intro x hx
  have := List.any_eq_false.mp h2 x hx
  simpa using this

theorem replace_ok_observe (s : State H B) n rs new v s'
    (h : replace s n rs new = (.ok v, s')) : observe s' n = .version (nextVersion (s.refs n)) := by
  obtain ⟨_, _, _, rfl⟩ := replace_ok_spec s n rs new v s' h
  simp [observe, update]

/-- **No lost update.** Two writers that both observed `o` for name `n`, where `o`
was the true state: once the first succeeds, the second can't succeed. It
gets a conflict, or an input error. So no write is silently overwritten. -/
theorem no_lost_update (s : State H B) n rs₁ new₁ v s₁ rs₂ new₂ (o : Observed)
    (h₁ : replace s n rs₁ new₁ = (.ok v, s₁))
    (ho : observe s n = o) (hmem : (n, o) ∈ rs₂) :
    (replace s₁ n rs₂ new₂).1.isOk = false := by
  have hobs := replace_ok_observe s n rs₁ new₁ v s₁ h₁
  have hne : observe s₁ n ≠ o := by
    rw [hobs, ← ho]
    unfold observe nextVersion
    cases s.refs n <;> simp
  unfold replace
  split
  · rfl
  split
  · rfl
  have : (rs₂.any fun x => decide (observe s₁ x.1 ≠ x.2)) = true :=
    List.any_eq_true.mpr ⟨(n, o), hmem, by simpa using hne⟩
  rw [if_pos this]
  rfl

/-! ## P3: immutable names can't move -/

/-- A name is frozen if it exists and its effective rule forbids moving it. -/
def Frozen (s : State H B) (n : Name) (v : RefVal H) : Prop :=
  s.refs n = some v ∧ (mobilityOf s.rules n = .createOnce ∨ mobilityOf s.rules n = .forbidden)

theorem filter_setRule (rules : List (Name × Rule)) (p n : Name) (r : Rule)
    (hp : p.isPrefixOf n = false) :
    ((p, r) :: rules.filter (fun x => !(decide (x.1 = p)))).filter (fun x => x.1.isPrefixOf n)
      = rules.filter (fun x => x.1.isPrefixOf n) := by
  rw [List.filter_cons, if_neg (by simp [hp]), List.filter_filter]
  apply List.filter_congr
  intro x _
  by_cases hx : x.1.isPrefixOf n = true
  · have : x.1 ≠ p := by intro e; rw [e] at hx; rw [hx] at hp; cases hp
    simp [hx, this]
  · simp [hx]

theorem mobility_setRule (rules : List (Name × Rule)) (p n : Name) (r : Rule)
    (hp : p.isPrefixOf n = false) :
    mobilityOf ((p, r) :: rules.filter (fun x => !(decide (x.1 = p)))) n = mobilityOf rules n := by
  unfold mobilityOf governing
  rw [filter_setRule rules p n r hp]

theorem frozen_step (hashOf : B → H) (s : State H B) (op : Op H B) n v
    (hf : Frozen s n v) : Frozen (step hashOf s op).2 n v := by
  obtain ⟨hv, hm⟩ := hf
  cases op with
  | put d =>
    simp only [step, put]
    split <;> exact ⟨hv, hm⟩
  | setRule p r =>
    simp only [step, setRule]
    split
    · exact ⟨hv, hm⟩
    split
    · exact ⟨hv, hm⟩
    · rename_i hno
      have hp : p.isPrefixOf n = false := by
        cases h : p.isPrefixOf n
        · rfl
        · exact absurd ⟨n, h, by simp [hv]⟩ hno
      refine ⟨hv, ?_⟩
      simp only
      rw [mobility_setRule s.rules p n r hp]
      exact hm
  | replace m rs new =>
    simp only [step]
    by_cases hmn : m = n
    · subst hmn
      unfold replace
      split
      · exact ⟨hv, hm⟩
      split
      · exact ⟨hv, hm⟩
      split
      · exact ⟨hv, hm⟩
      rcases hm with hm | hm
      · rw [hm, hv]; exact ⟨hv, Or.inl hm⟩
      · rw [hm]; exact ⟨hv, Or.inr hm⟩
    · unfold replace
      split
      · exact ⟨hv, hm⟩
      split
      · exact ⟨hv, hm⟩
      split
      · exact ⟨hv, hm⟩
      split
      · exact ⟨hv, hm⟩
      · exact ⟨hv, hm⟩
      · refine ⟨?_, hm⟩
        simp only [update, if_neg (Ne.symm hmn)]
        exact hv

/-- **P3.** Once frozen, a name keeps its value forever. -/
theorem frozen_run (hashOf : B → H) (s : State H B) (ops : List (Op H B)) n v
    (hf : Frozen s n v) : (run hashOf s ops).refs n = some v := by
  induction ops generalizing s with
  | nil => exact hf.1
  | cons op ops ih => exact ih _ (frozen_step hashOf s op n v hf)

/-- Corollary: a published name in an `immutable` namespace never moves. -/
theorem published_never_moves (hashOf : B → H) (s : State H B) (ops : List (Op H B)) n v
    (hv : s.refs n = some v) (hrule : mobilityOf s.rules n = .createOnce) :
    (run hashOf s ops).refs n = some v :=
  frozen_run hashOf s ops n v ⟨hv, Or.inl hrule⟩

end Pico2peta
