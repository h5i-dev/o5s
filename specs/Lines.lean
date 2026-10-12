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

theorem ab_inj {a b : Rg} : ab a = ab b ↔ a = b := by sorry

theorem conflict_iff (a z b w : U64) :
    Interval.Conflict a z b w ↔ NConf a.val z.val b.val w.val := by sorry

@[step] theorem same_spec (a b : Rg) : lines.same a b ⦃ r => r = decide (a = b) ⦄ := by sorry

@[step] theorem key_le_spec (a b : Rg) :
    lines.key_le a b ⦃ r => r = decide (KeyLe (ab a) (ab b)) ⦄ := by sorry

/-- `sorted` checks adjacent pairs, which is the pairwise order because
`KeyLe` is transitive. -/
@[step] theorem sorted_spec (rs : alloc.vec.Vec Rg) :
    lines.sorted rs ⦃ b => b = true ↔ Sorted (L rs) ⦄ := by sorry

/-- The extracted merge computes the model merge exactly, on any input. -/
@[step] theorem merge_spec (rs : alloc.vec.Vec Rg) :
    lines.merge rs ⦃ out => match out with
      | none => (merge (L rs)).1 = false
      | some k => (merge (L rs)).1 = true ∧ k.val.map (·.val) = (merge (L rs)).2 ⦄ := by sorry

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
