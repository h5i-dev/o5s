import Checked
import Coordinates
import LinesModel
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Specifications of the extracted `lines` kernel (to prove)

Each theorem relates an extracted function in `crates/kernel/src/lines.rs` to
the model in `proofs/LinesModel.lean`, whose order-independence and
conflict theorems are already proved. When this file is proved, move it to
`proofs/` and add its theorems to `h5i-app.toml`.
-/
namespace o5s_kernel.Lines
open LinesModel Rendering

abbrev Rg := U64 × U64 × alloc.vec.Vec U8

/-- A range as the model sees it. -/
def ab (r : Rg) : R (List U8) := (r.1.val, r.2.1.val, r.2.2.val)

/-- The model input of an extracted range vector. -/
def L (rs : alloc.vec.Vec Rg) : List (R (List U8)) := rs.val.map ab

theorem ab_inj {a b : Rg} : ab a = ab b ↔ a = b := by
  constructor
  · intro h
    obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
    simp only [ab, Prod.mk.injEq] at h
    obtain ⟨h1, h2, h3⟩ := h
    have e1 : a1 = b1 := UScalar.eq_of_val_eq h1
    have e2 : a2 = b2 := UScalar.eq_of_val_eq h2
    have e3 : a3 = b3 := Subtype.ext h3
    subst e1 e2 e3; rfl
  · rintro rfl; rfl

theorem conflict_iff (a z b w : U64) :
    Interval.Conflict a z b w ↔ NConf a.val z.val b.val w.val := by
  have ez : a = z ↔ a.val = z.val := ⟨fun h => h ▸ rfl, UScalar.eq_of_val_eq⟩
  have ew : b = w ↔ b.val = w.val := ⟨fun h => h ▸ rfl, UScalar.eq_of_val_eq⟩
  unfold Interval.Conflict NConf
  rw [ez, ew]
  split_ifs <;> simp only [UScalar.le_equiv, UScalar.lt_equiv] <;> rfl

@[step] theorem same_spec (a b : Rg) : lines.same a b ⦃ r => r = decide (a = b) ⦄ := by
  obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
  unfold lines.same
  by_cases h1 : a1 = b1
  · by_cases h2 : a2 = b2
    · subst h1 h2
      simp only [↓reduceIte]
      apply WP.spec_mono (h5i_app_std.bytes.eq_spec a3 b3)
      intro r hr
      rw [hr]
      simp [Subtype.ext_iff]
    · simp [h1, h2]
  · simp [h1]

@[step] theorem key_le_spec (a b : Rg) :
    lines.key_le a b ⦃ r => r = decide (KeyLe (ab a) (ab b)) ⦄ := by
  obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
  unfold lines.key_le
  simp only [KeyLe, ab]
  by_cases h1 : a1 < b1
  · have : a1.val < b1.val := h1
    simp [h1, this]
  · have h1' : ¬ a1.val < b1.val := h1
    by_cases h2 : a1 = b1
    · subst h2
      simp [h1]
    · have : a1.val ≠ b1.val := fun e => h2 (UScalar.eq_of_val_eq e)
      simp [h1, h1', h2, this]

/-- `sorted` checks adjacent pairs, which is the pairwise order because
`KeyLe` is transitive. -/
@[step] theorem sorted_spec (rs : alloc.vec.Vec Rg) :
    lines.sorted rs ⦃ b => b = true ↔ Sorted (L rs) ⦄ := by sorry

/-- The merge state as the model sees it. -/
def absM (st : lines.Merge) : St := (st.ok, st.kept.val.map (·.val))

theorem at_L (rs : alloc.vec.Vec Rg) (j : Nat) (h : j < rs.val.length) :
    at_ (L rs) j = ab rs.val[j] := by
  simp [at_, L, getElem!_pos, h]

/-- One step of the extracted merge is one step of the model. -/
/-- What the merge loop keeps true after `i` steps. -/
def MInv (st : lines.Merge) (i : Nat) : Prop :=
  (∀ j ∈ st.kept.val, j.val < i) ∧ st.kept.val.length ≤ i

theorem merge_step_spec (st : lines.Merge) (rs : alloc.vec.Vec Rg) (i : Usize)
    (hi : i.val < rs.val.length) (inv : MInv st i.val) :
    lines.merge_step st rs i ⦃ out => absM out = step (L rs) (absM st) i.val ∧
      MInv out (i.val + 1) ⦄ := by
  sorry

@[step] theorem merge_loop_spec (rs : alloc.vec.Vec Rg) :
    lines.merge_loop rs { ok := true, kept := alloc.vec.Vec.new Usize } 0#usize ⦃ st =>
      absM st = merge (L rs) ⦄ := by
  unfold lines.merge_loop
  have hl : (List.range rs.val.length).length = rs.val.length := List.length_range
  apply WP.spec_mono (loop_fold (List.range rs.val.length) absM (step (L rs))
    (fun st i => MInv st i)
    (fun x => lines.merge_loop.body rs x.1 x.2) ?_ _ 0#usize (by simp) ⟨by simp, by simp⟩)
  · intro st h
    rw [h]
    simp [absM, merge, L]
  · intro st i hi hinv
    unfold lines.merge_loop.body
    by_cases hlt : i.val < rs.val.length
    · have hlt' : i < alloc.vec.Vec.len rs := hlt
      simp only [hlt', ↓reduceIte]
      apply WP.bind_spec (merge_step_spec st rs i hlt hinv)
      intro out ⟨habs, hout⟩
      apply WP.bind_spec (UScalar.add_spec (by scalar_tac))
      intro j hj
      simp only [FoldStep, WP.ok_spec]
      refine ⟨by simpa [hl] using hlt, ?_, by scalar_tac, ?_⟩
      · rw [habs]; simp [List.getElem_range]
      · simpa [show j.val = i.val + 1 by scalar_tac] using hout
    · have : ¬ i < alloc.vec.Vec.len rs := hlt
      simp only [this, ↓reduceIte, FoldStep, WP.ok_spec]
      exact ⟨by simp [hl]; omega, rfl⟩

/-- The extracted merge computes the model merge exactly, on any input. -/
@[step] theorem merge_spec (rs : alloc.vec.Vec Rg) :
    lines.merge rs ⦃ out => match out with
      | none => (merge (L rs)).1 = false
      | some k => (merge (L rs)).1 = true ∧ k.val.map (·.val) = (merge (L rs)).2 ⦄ := by
  unfold lines.merge
  apply WP.bind_spec (merge_loop_spec rs)
  intro st hst
  simp only [absM, Prod.ext_iff] at hst
  split <;> simp_all

/-- What a merge of sorted ranges means, from the model theorems: success iff
no range is reversed and no two different ranges conflict; the kept ranges
are the distinct inputs, pairwise separated and in strict key order. -/
theorem merge_correct (rs : alloc.vec.Vec Rg) (s : Sorted (L rs)) :
    lines.merge rs ⦃ out => match out with
      | none => Bad (L rs)
      | some k => ¬ Bad (L rs) ∧ (∀ x, x ∈ kept (L rs) (k.val.map (·.val)) ↔ x ∈ L rs) ∧
          (kept (L rs) (k.val.map (·.val))).Pairwise Sep ⦄ := by sorry

/-- Two sorted arrangements of the same ranges (for example the versions of
concurrent heads concatenated in either order, then sorted) merge to the same
edits. -/
theorem merge_order_independent (rs₁ rs₂ : alloc.vec.Vec Rg)
    (s₁ : Sorted (L rs₁)) (s₂ : Sorted (L rs₂)) (same : ∀ x, x ∈ rs₁.val ↔ x ∈ rs₂.val) :
    lines.merge rs₁ ⦃ o₁ => lines.merge rs₂ ⦃ o₂ =>
      (o₁.isSome ↔ o₂.isSome) ∧
      ∀ k₁ k₂, o₁ = some k₁ → o₂ = some k₂ →
        k₁.val.map (fun j => rs₁.val[j.val]!) = k₂.val.map (fun j => rs₂.val[j.val]!) ⦄ ⦄ := by
  sorry

/-- One proposal's ranges for one file: accepted exactly when sorted, no
range reversed, and no two ranges conflict (an identical copy included). This
is the check `prepare_lines` makes. -/
@[step] theorem ranges_valid_spec (rs : alloc.vec.Vec Rg) :
    lines.ranges_valid rs ⦃ b => b = true ↔
      Sorted (L rs) ∧ (∀ x ∈ L rs, Valid x) ∧
      ∀ i j (hi : i < (L rs).length) (hj : j < (L rs).length), i < j →
        ¬ Conf (L rs)[i] (L rs)[j] ⦄ := by sorry

/-- The byte chunks of kept line ranges on `boundaries`. -/
def chunkOf (bs : List Usize) (r : Rg) : Option Chunk :=
  if h : r.1.val ≤ r.2.1.val ∧ r.2.1.val < bs.length then
    some (bs[r.1.val]'(by omega), bs[r.2.1.val]'h.2, r.2.2)
  else none

def chunksOf (bs : List Usize) (rs : List Rg) : Option (List Chunk) :=
  rs.mapM (chunkOf bs)

@[step] theorem chunks_spec (bs : alloc.vec.Vec Usize) (rs : alloc.vec.Vec Rg)
    (kept : alloc.vec.Vec Usize) (hk : ∀ j ∈ kept.val, j.val < rs.val.length)
    (room : kept.val.length < Usize.max) :
    lines.chunks bs rs kept ⦃ out =>
      out.map (·.val) = chunksOf bs.val (kept.val.map fun j => rs.val[j.val]!) ⦄ := by sorry

/-- Rendering is the checked renderer on the line coordinates of the base. -/
@[step] theorem render_spec (original : alloc.vec.Vec U8) (rs : alloc.vec.Vec Rg)
    (kept : alloc.vec.Vec Usize) (hk : ∀ j ∈ kept.val, j.val < rs.val.length)
    (room : original.val.length < Usize.max) :
    lines.render original rs kept ⦃ out =>
      ∃ bs : List Usize, bs.map (·.val) = Coordinates.model original.val ∧
        match chunksOf bs (kept.val.map fun j => rs.val[j.val]!) with
        | none => out = none
        | some cs => match out with
          | none => ¬ Checked.Good original.val.length 0 original.val.length cs
          | some v => Checked.Good original.val.length 0 original.val.length cs ∧
              v.val = Rendering.model original.val cs ⦄ := by sorry

/-- A successful merge of sorted ranges, all inside the base, renders: the
chunks are in order because separated ranges map to ordered byte intervals
(`Coordinates.line_boundaries_ordered`), so only output capacity can fail. -/
theorem merged_renders (original : alloc.vec.Vec U8) (rs : alloc.vec.Vec Rg)
    (s : Sorted (L rs)) (room : original.val.length < Usize.max)
    (inside : ∀ x ∈ L rs, x.2.1 < (Coordinates.model original.val).length)
    (cap : original.val.length + (rs.val.map fun r => r.2.2.val.length).sum ≤ Usize.max) :
    lines.merge rs ⦃ o => ∀ k, o = some k → lines.render original rs k ⦃ out => out.isSome ⦄ ⦄ := by
  sorry

end o5s_kernel.Lines
