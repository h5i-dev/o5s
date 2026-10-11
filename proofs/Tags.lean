import Constructor

open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Tags

def restoredTag (restored : List Bool) (tag : Usize) : Bool :=
  restored[tag.val]?.getD false

def advance (restored : List Bool) (visible : Bool) (tag : Usize) : Bool :=
  visible && restoredTag restored tag

theorem fold_visible (restored : List Bool) (covering : List Usize) (initial : Bool) :
    covering.foldl (advance restored) initial = true ↔
      initial = true ∧ ∀ tag ∈ covering, restoredTag restored tag = true := by
  induction covering generalizing initial with
  | nil => simp
  | cons tag covering ih =>
    simp only [List.foldl_cons, ih, advance, Bool.and_eq_true, List.mem_cons]
    aesop

@[step] theorem visible_spec (covering : alloc.vec.Vec Usize)
    (restored : alloc.vec.Vec Bool)
    (bounds : ∀ tag ∈ covering.val, tag.val < restored.val.length) :
    tags.visible covering restored ⦃ visible =>
      visible = true ↔ ∀ tag ∈ covering.val, restoredTag restored.val tag = true ⦄ := by
  unfold tags.visible tags.visible_loop
  apply WP.spec_mono (loop_fold covering.val (fun visible : Bool => visible)
    (advance restored.val) (fun _ _ => True)
    (fun st => tags.visible_loop.body covering restored st.1 st.2)
    ?_ true 0#usize (by simp) trivial)
  · intro visible h
    rw [h]
    simpa using fold_visible restored.val covering.val true
  · intro visible i hi hinv
    unfold tags.visible_loop.body
    h5i_step
    all_goals have idx : i.val < covering.val.length := by scalar_tac
    all_goals have hb : covering.val[i.val].val < restored.val.length := bounds _ (List.getElem_mem _)
    all_goals simp_all [advance, restoredTag]

theorem visible_iff_no_active_tag (covering : alloc.vec.Vec Usize)
    (restored : alloc.vec.Vec Bool)
    (bounds : ∀ tag ∈ covering.val, tag.val < restored.val.length) :
    tags.visible covering restored ⦃ visible => visible = true ↔
      ¬ ∃ tag ∈ covering.val, restoredTag restored.val tag = false ⦄ := by
  apply WP.spec_mono (visible_spec covering restored bounds)
  intro visible h
  rw [h]
  constructor
  · intro allRestored ⟨tag, covers, active⟩
    have impossible := allRestored tag covers
    simp [active] at impossible
  · intro noActive tag covers
    cases flag : restoredTag restored.val tag
    · exact False.elim (noActive ⟨tag, covers, flag⟩)
    · rfl

/-- A still-active covering tag prevents output, regardless of other tags. -/
theorem active_tag_hides (covering : alloc.vec.Vec Usize)
    (restored : alloc.vec.Vec Bool)
    (bounds : ∀ tag ∈ covering.val, tag.val < restored.val.length)
    (tag : Usize) (covers : tag ∈ covering.val)
    (active : restoredTag restored.val tag = false) :
    tags.visible covering restored ⦃ visible => visible = false ⦄ := by
  apply WP.spec_mono (visible_spec covering restored bounds)
  intro visible h
  cases visible
  · rfl
  · have impossible := h.mp rfl tag covers
    simp [active] at impossible

/-- Restoring a different tag leaves the un-restored covering tag effective.
The updated flag table is the one supplied to the actual Rust scanner. -/
theorem restore_other_cannot_revive (covering : alloc.vec.Vec Usize)
    (before after : alloc.vec.Vec Bool)
    (bounds : ∀ tag ∈ covering.val, tag.val < after.val.length)
    (changed other : Usize) (distinct : other ≠ changed)
    (covers : other ∈ covering.val)
    (active : restoredTag before.val other = false)
    (only_changed : ∀ tag, tag ≠ changed →
      restoredTag after.val tag = restoredTag before.val tag) :
    tags.visible covering after ⦃ visible => visible = false ⦄ := by
  apply active_tag_hides covering after bounds other covers
  rw [only_changed other distinct, active]

end o5s_kernel.Tags
