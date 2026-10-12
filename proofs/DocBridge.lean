import DocMatKernel
import DocMatModel
import DocView
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The kernel's boundary graph is the model's

On a document that encodes the history, the extracted liveness table agrees
with `Live` by anchor index (`liveL_iff`), and the boundary rows
`materialize_loop1` builds relate exactly the anchors `Sum` relates
(`bridge`), through `kedge_iff`, `liveM_iff` and the transport of segments.
So the model's depth and blocking satisfy the Kahn axioms for the graph the
extracted code builds.
-/
namespace o5s_kernel.DocBridge
open DocModel Document DocRep DocKernel DocKahn DocMatModel DocView

variable {d : document.Doc} {H : Hist}

theorem atoms_len (hr : Rep d H) : d.atoms.val.length = (anchorList H).length := by
  have := congrArg List.length (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms)
  simpa [Document.anchorList] using this

theorem nodup_anchors (hb : Built H) : (anchorList H).Nodup :=
  anchorList_nodup (ops_keys_nodup (built_nodup hb))

theorem aIdx_anc (hb : Built H) {a : Nat} (h : a < (anchorList H).length) : aIdx H (anc H a) = a := by
  rw [anc_get h]; exact idx_getElem (nodup_anchors hb) a h

theorem anc_aIdx {x : Anchor} (h : x ∈ anchorList H) : anc H (aIdx H x) = x := by
  unfold aIdx; rw [anc_get (idx_lt h)]; exact getElem_idx h

theorem anc_mem {a : Nat} (h : a < (anchorList H).length) : anc H a ∈ anchorList H := by
  rw [anc_get h]; exact List.getElem_mem h

/-- The kernel's liveness flag at an index is the model's `Live` of the anchor there. -/
theorem liveL_iff (hr : Rep d H) (hb : Built H) {a : Nat} (ha : a < d.atoms.val.length) :
    (liveL d)[a]?.getD false = true ↔ Live H (anc H a) := by
  have hl : a < (anchorList H).length := atoms_len hr ▸ ha
  rw [liveL, List.getElem?_map, List.getElem?_range ha]
  simp only [Option.map_some, Option.getD_some]
  by_cases h2 : 2 ≤ a
  · have hm := anc_mem hl
    have : ∃ n, anc H a = .node n := by
      rw [anc_get hl]
      unfold anchorList at hl ⊢
      have : a - 2 < ((DocModel.atoms H).map fun x => Anchor.node x.1).length := by simp at hl ⊢; omega
      rw [List.getElem_append_right (by simp; omega)]
      simp
    obtain ⟨n, hn⟩ := this
    have := liveM_iff d H hr hb n (hn ▸ hm)
    rw [← hn, aIdx_anc hb hl] at this
    exact this
  · have hlt : a < 2 := by omega
    have e : liveM (coverUpTo d.deletions.val d.atoms.val.length d.deletions.val.length) d.restored.val a = false := by
      unfold liveM; simp [h2]
    rw [e]
    have : anc H a = .start ∨ anc H a = .stop := by
      rw [anc_get hl]; unfold anchorList
      rcases (by omega : a = 0 ∨ a = 1) with rfl | rfl <;> simp
    rcases this with h | h <;> rw [h] <;> simp [Live]

theorem alive_eq (live : List Bool) (x : Usize) : Boundary.alive live x = live[x.val]?.getD false := rfl

theorem live_mem {x : Anchor} (h : Live H x) : x ∈ anchorList H := by
  cases x with
  | node n => exact (mem_anchorList_node H n).mpr h.1
  | _ => exact h.elim

theorem kedge_kE (u v : Usize) : KEdge d u.val v.val ↔ (u, v) ∈ kE d := by
  rw [mem_kE]
  unfold KEdge
  constructor
  · rintro ⟨i, hi, hw, h1, h2⟩; exact ⟨i, hi, hw, UScalar.eq_of_val_eq h1, UScalar.eq_of_val_eq h2⟩
  · rintro ⟨i, hi, hw, h1, h2⟩; exact ⟨i, hi, hw, by rw [h1], by rw [h2]⟩

/-- The kernel's boundary rows, read through the anchor table, are the model's `Sum`. -/
theorem bridge (hr : Rep d H) (hb : Built H) (wf : DocWF d) (bnd : List (alloc.vec.Vec Usize))
    (hrows : RowsOK (liveL d) (kE d) d.atoms.val.length bnd) :
    Bridge H (KG.mk d.atoms.val.length (liveL d) (rowN bnd) (DH H) (BH H)) := by
  have hat := atoms_len hr
  have hmax : d.atoms.val.length ≤ Usize.max := d.atoms.property
  have hlive : ∀ (a : Usize), a.val < d.atoms.val.length →
      (Live H (anc H a.val) ↔ Boundary.alive (liveL d) a = true) := fun a ha => by
    rw [alive_eq]; exact (liveL_iff hr hb ha).symm
  have hseg : ∀ uu vv : Usize, uu.val < d.atoms.val.length → vv.val < d.atoms.val.length →
      (Projection.Segment (DocModel.E H) (Live H) (anc H uu.val) (anc H vv.val) ↔
        Projection.Segment (kE d) (fun x => Boundary.alive (liveL d) x = true) uu vv) := by
    intro uu vv hu hv
    refine segment_map (fun x : Usize => anc H x.val) (fun x => x.val < d.atoms.val.length) (kE d)
      (DocModel.E H) _ (Live H) ?_ ?_ ?_ ?_ ?_ uu vv hu hv
    · intro a b ha hb' e
      apply UScalar.eq_of_val_eq
      have h1 : aIdx H (anc H a.val) = a.val := aIdx_anc hb (a := a.val) (hat ▸ ha)
      have h2 : aIdx H (anc H b.val) = b.val := aIdx_anc hb (a := b.val) (hat ▸ hb')
      try simp only at e
      rw [← h1, ← h2, e]
    · intro a b ha hb'
      try simp only
      have e1 : aIdx H (anc H a.val) = a.val := aIdx_anc hb (a := a.val) (hat ▸ ha)
      have e2 : aIdx H (anc H b.val) = b.val := aIdx_anc hb (a := b.val) (hat ▸ hb')
      rw [← kedge_kE]
      have := (kedge_iff d H hr hb _ _ (anc_mem (hat ▸ ha)) (anc_mem (hat ▸ hb'))).symm
      rw [e1, e2] at this
      exact this
    · intro x y hxy
      have hx : x ∈ anchorList H ∧ y ∈ anchorList H := by
        rcases (mem_E H x y).mp hxy with h | ⟨f, hf, -, h1, h2⟩
        · simp only [Prod.mk.injEq] at h; rw [h.1, h.2]; simp [anchorList]
        · have := facts_anchors H hb f hf; rw [← h1, ← h2]; exact this
      obtain ⟨a, ha⟩ := usize_exists (aIdx H x) (by have := idx_lt hx.1; unfold aIdx; omega)
      obtain ⟨b, hb'⟩ := usize_exists (aIdx H y) (by have := idx_lt hx.2; unfold aIdx; omega)
      refine ⟨a, b, by rw [ha]; exact anc_aIdx hx.1, by rw [hb']; exact anc_aIdx hx.2,
        by rw [ha, hat]; exact idx_lt hx.1, by rw [hb', hat]; exact idx_lt hx.2⟩
    · intro a b hab
      obtain ⟨i, hi, -, h1, h2⟩ := (mem_kE d a b).mp hab
      have := wf.edges _ (List.getElem_mem hi)
      rw [h1, h2] at this
      exact this
    · intro a ha; exact hlive a ha
  refine ⟨hat, rfl, rfl, fun a ha => liveL_iff hr hb ha, ?_, ?_, ?_, ?_⟩
  · -- sum
    intro u v hu hv
    change u < d.atoms.val.length at hu
    change v < d.atoms.val.length at hv
    show (u < d.atoms.val.length ∧ (liveL d)[u]?.getD false = true) ∧ v ∈ rowN bnd u ↔ _
    have hub : u < bnd.length := by rw [hrows.1]; exact hu
    have hrow : rowN bnd u = (bnd[u]).val.map (·.val) := by simp [rowN, List.getElem?_eq_getElem hub]
    rw [hrow]
    constructor
    · rintro ⟨⟨-, hl⟩, hvm⟩
      obtain ⟨uu, huu, hbr⟩ := (hrows.2 u hub).1 hl
      obtain ⟨vv, hvv, rfl⟩ := List.mem_map.mp hvm
      obtain ⟨hav, hs⟩ := (hbr.2 vv).mp hvv
      have hs' := (hseg uu vv (by rw [huu]; exact hu) hv).mpr hs
      rw [huu] at hs'
      exact ⟨(liveL_iff hr hb hu).mp hl, (hlive vv hv).mpr hav, (seg_iff_segment H _ _).mpr hs'⟩
    · rintro ⟨hlu, hlv, hs⟩
      have hl := (liveL_iff hr hb hu).mpr hlu
      obtain ⟨uu, huu, hbr⟩ := (hrows.2 u hub).1 hl
      obtain ⟨vv, hvv⟩ := usize_exists v (by omega)
      have hs1 := (seg_iff_segment H _ _).mp hs
      rw [← huu, ← hvv] at hs1
      have hs2 := (hseg uu vv (by rw [huu]; exact hu) (by rw [hvv]; exact hv)).mp hs1
      have hmem := (hbr.2 vv).mpr ⟨(hlive vv (by rw [hvv]; exact hv)).mp (hvv ▸ hlv), hs2⟩
      exact ⟨⟨hu, hl⟩, List.mem_map.mpr ⟨vv, hmem, hvv⟩⟩
  · -- succ
    intro u v hv
    change v ∈ rowN bnd u at hv
    show (u < d.atoms.val.length ∧ (liveL d)[u]?.getD false = true) ∧
      (v < d.atoms.val.length ∧ (liveL d)[v]?.getD false = true)
    have hub : u < bnd.length := by
      by_contra h
      simp [rowN, List.getElem?_eq_none_iff.mpr (by omega : bnd.length ≤ u)] at hv
    have hrow : rowN bnd u = (bnd[u]).val.map (·.val) := by simp [rowN, List.getElem?_eq_getElem hub]
    rw [hrow] at hv
    cases hl : (liveL d)[u]?.getD false
    · have := (hrows.2 u hub).2 hl; rw [this] at hv; simp at hv
    · obtain ⟨uu, -, hbr⟩ := (hrows.2 u hub).1 hl
      obtain ⟨vv, hvv, rfl⟩ := List.mem_map.mp hv
      obtain ⟨hav, -⟩ := (hbr.2 vv).mp hvv
      have hvl : vv.val < (liveL d).length := alive_lt hav
      have hn : (liveL d).length = d.atoms.val.length := by simp [liveL]
      refine ⟨⟨by rw [← hrows.1]; exact hub, rfl⟩, ⟨by omega, ?_⟩⟩
      rw [← alive_eq]; exact hav
  · -- nodup
    intro u
    show (rowN bnd u).Nodup
    by_cases hub : u < bnd.length
    · have hrow : rowN bnd u = (bnd[u]).val.map (·.val) := by simp [rowN, List.getElem?_eq_getElem hub]
      rw [hrow]
      cases hl : (liveL d)[u]?.getD false
      · rw [(hrows.2 u hub).2 hl]; simp
      · obtain ⟨uu, -, hbr⟩ := (hrows.2 u hub).1 hl
        exact hbr.1
    · simp [rowN, List.getElem?_eq_none_iff.mpr (by omega : bnd.length ≤ u)]
  · -- cover
    intro x hx
    have hm := live_mem hx
    exact ⟨aIdx H x, by rw [hat]; exact idx_lt hm, anc_aIdx hm⟩

end o5s_kernel.DocBridge
