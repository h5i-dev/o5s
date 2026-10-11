import Helpers
/-!
# Exact results of the validators

`Helpers.lean` gives one-directional facts about the validators; the model in
`Model.lean` needs their exact results.
-/
open Aeneas Aeneas.Std Result o5s_registry H5iAppLib

namespace o5s_registry.Exact

/-- `name_byte`, branch for branch. -/
def nameByte (c : U8) : Bool :=
  if c ≥ 97#u8 then (if c ≤ 122#u8 then true else if c ≥ 65#u8 then (if c ≤ 90#u8 then true else
    if c ≥ 48#u8 then (if c ≤ 57#u8 then true else if c = 45#u8 then true else decide (c = 95#u8))
    else if c = 45#u8 then true else decide (c = 95#u8))
    else if c ≥ 48#u8 then (if c ≤ 57#u8 then true else if c = 45#u8 then true else decide (c = 95#u8))
    else if c = 45#u8 then true else decide (c = 95#u8))
  else if c ≥ 65#u8 then (if c ≤ 90#u8 then true else
    if c ≥ 48#u8 then (if c ≤ 57#u8 then true else if c = 45#u8 then true else decide (c = 95#u8))
    else if c = 45#u8 then true else decide (c = 95#u8))
  else if c ≥ 48#u8 then (if c ≤ 57#u8 then true else if c = 45#u8 then true else decide (c = 95#u8))
  else if c = 45#u8 then true else decide (c = 95#u8)

@[step] theorem name_byte_exact (c : U8) : name_byte c ⦃ b => b = nameByte c ⦄ := by
  unfold name_byte nameByte
  split_ifs <;> simp

def validName (l : List U8) : Bool := l.length ≠ 0 && l.length ≤ 64 && l.all nameByte

theorem valid_name_exact (n : alloc.vec.Vec U8) : valid_name n ⦃ b => b = validName n.val ⦄ := by
  unfold valid_name valid_name_loop
  dsimp only
  split
  · rename_i h; rw [H5iAppLib.usize_ofNatCore_eq_zero] at h; simp [validName, h]
  · split
    · simp only [MAX_NAME] at *; simp [validName]; scalar_tac
    · rename_i h0 hmax
      apply WP.spec_mono (loop_search n.val (fun c => !nameByte c) (fun b : Bool => b)
        (fun _ _ => false) true _ ?_ 0#usize (by simp))
      · intro r hr; rw [search_all _ _ _ hr]; simp only [MAX_NAME] at hmax
        have : n.val.length ≠ 0 := by intro h; apply h0; scalar_tac
        simp [validName, this]; scalar_tac
      · intro j hj; unfold valid_name_loop.body; h5i_step

/-- The hexadecimal check of `valid_hash`, branch for branch. -/
def hexByte (c : U8) : Bool :=
  if c ≥ 48#u8 then (if c ≤ 57#u8 then true else if c ≥ 97#u8 then decide (c ≤ 102#u8) else false)
  else if c ≥ 97#u8 then decide (c ≤ 102#u8) else false

def validHash (l : List U8) : Bool := l.length = 64 && l.all hexByte

theorem valid_hash_exact (h : alloc.vec.Vec U8) : valid_hash h ⦃ b => b = validHash h.val ⦄ := by
  unfold valid_hash valid_hash_loop
  dsimp only
  split
  · simp only [HASH_LEN, bne_iff_ne, ne_eq] at *; simp [validHash]; scalar_tac
  · rename_i h64
    simp only [HASH_LEN, bne_iff_ne, ne_eq, Decidable.not_not] at h64
    apply WP.spec_mono (loop_search h.val (fun c => !hexByte c) (fun b : Bool => b)
      (fun _ _ => false) true _ ?_ 0#usize (by simp))
    · intro r hr; rw [search_all _ _ _ hr]
      have : h.val.length = 64 := by scalar_tac
      simp [validHash, this]
    · intro j hj; unfold valid_hash_loop.body; h5i_step [hexByte]

/-- Strictly ascending handles. -/
def ascending : List U64 → Bool
  | a :: b :: t => decide (a.val < b.val) && ascending (b :: t)
  | _ => true

theorem ascending_iff (l : List U64) :
    ascending l = true ↔ ∀ k (h : k + 1 < l.length), l[k].val < l[k + 1].val := by
  induction l with
  | nil => simp [ascending]
  | cons a t ih =>
    cases t with
    | nil => simp [ascending]
    | cons b t =>
      simp only [ascending, Bool.and_eq_true, decide_eq_true_eq, ih]
      constructor
      · rintro ⟨hab, hrest⟩ k hk
        cases k with
        | zero => simpa using hab
        | succ k =>
          have := hrest k (by simp at hk ⊢; omega)
          simp only [List.getElem_cons_succ] at this ⊢
          exact this
      · intro h
        refine ⟨by simpa using h 0 (by simp), fun k hk => ?_⟩
        have := h (k + 1) (by simp at hk ⊢; omega)
        simp only [List.getElem_cons_succ] at this ⊢
        exact this

def validBatch (l : List U64) : Bool := l.length ≠ 0 && l.length ≤ 10000 && ascending l

theorem valid_batch_exact (ids : alloc.vec.Vec U64) : valid_batch ids ⦃ b => b = validBatch ids.val ⦄ := by
  unfold valid_batch valid_batch_loop
  dsimp only
  split
  · rename_i h; rw [H5iAppLib.usize_ofNatCore_eq_zero] at h; simp [validBatch, h]
  · split
    · simp only [MAX_BATCH] at *; simp [validBatch]; scalar_tac
    · rename_i h0 hmax
      simp only [MAX_BATCH] at hmax
      have hlen : ids.val.length ≠ 0 ∧ ids.val.length ≤ 10000 := ⟨fun h => h0 (by scalar_tac), by scalar_tac⟩
      apply loop_idx_spec (fun i => valid_batch_loop.body ids i) (fun i => i) ids.val.length
        (fun i => 1 ≤ i.val ∧ ∀ k, k + 1 < i.val → ∀ hk : k + 1 < ids.val.length, ids.val[k].val < ids.val[k + 1].val)
      · intro i ⟨h1, hi⟩ hn
        unfold valid_batch_loop.body
        dsimp only
        split
        · rename_i hlt
          step*
          · -- A descent: not ascending.
            rename_i hge
            have : ¬ ascending ids.val = true := by
              rw [ascending_iff]; intro h
              have := h i2.val (by scalar_tac)
              have e : i2.val + 1 = i.val := by scalar_tac
              simp only [e] at this
              rw [← i3_post, ← i4_post] at this
              scalar_tac
            simp [validBatch, hlen, this]
          · refine ⟨by scalar_tac, ?_, by scalar_tac, by scalar_tac⟩
            intro k hk hk'
            by_cases hkk : k + 1 < i.val
            · exact hi k hkk hk'
            · have e1 : k = i2.val := by scalar_tac
              subst e1
              have e2 : i2.val + 1 = i.val := by scalar_tac
              simp only [e2, ← i3_post, ← i4_post]
              scalar_tac
        · simp only [WP.spec_ok]
          have : ascending ids.val = true := by
            rw [ascending_iff]; exact fun k hk => hi k (by scalar_tac) hk
          simp [validBatch, hlen, this]
      · exact ⟨by simp, fun k hk => by simp at hk⟩
      · have := hlen.1; simp; omega

end o5s_registry.Exact
