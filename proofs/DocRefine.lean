import DocCommit
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The extracted document kernel refines the model (validation and commit)

`validate_refines`: on a replayed document, the extracted `validate` refuses
exactly when the model's `validate` does, and its refusal names the model's
error. `commit_succeeds`: a validated patch commits, and the result is again a
replayed document. `apply_refines` combines the two at the level of
`applySpec`. The proofs go through the dense encoding `Rep` (`DocRep.lean`)
and the kernel specifications (`DocKernel.lean`).
-/
namespace o5s_kernel.DocRefine
open DocModel Document DocRep DocKernel

/-! ## The kernel's `validate` -/

theorem validate_spec (doc : document.Doc) (p : document.Patch) (wf : DocWF doc)
    (droom : doc.deps.val.length < Usize.max) (rb : ∀ v ∈ p.deps.val, v.val < doc.deps.val.length)
    (bounds : ∀ row ∈ doc.deps.val, ∀ v ∈ row.val, v.val < doc.deps.val.length)
    (room : doc.atoms.val.length + (p.ops.val.map insSize).sum + 2 < Usize.max)
    (room2 : doc.edges.val.length + (p.ops.val.map (insSize · + 1)).sum + p.ops.val.length + 2 < Usize.max) :
    document.validate doc p ⦃ r =>
      absR r = validateS doc p.ops.val (seenM doc.deps.val p.deps.val) ⦄ := by
  unfold document.validate
  apply WP.spec_bind (observed_spec doc.deps p.deps droom rb bounds)
  rintro seen ⟨hlen, hseen⟩
  have hs : seen.val = seenM doc.deps.val p.deps.val := by
    apply List.ext_getElem
    · simp [seenM, hlen]
    · intro i h1 h2
      simp only [seenM, List.getElem_map, List.getElem_range]
      apply Bool.eq_iff_iff.mpr
      rw [hseen i h1]
      simp
  apply WP.spec_bind (refuse_spec 0#u64 0#usize 0#usize)
  intro r hr
  subst hr
  apply WP.spec_bind (validate_loop_spec doc p.ops seen wf room room2)
  intro st hst
  simp only [WP.spec_ok]
  unfold validateS
  rw [← hs, ← hst]
  rfl

/-! ## Well-formedness from the representation -/

theorem docWF_of_rep (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H) : DocWF d := by
  have hat : d.atoms.val.length = (anchorList H).length := by
    have := congrArg List.length (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms)
    simpa [Document.anchorList] using this
  have hE : d.edges.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) = encEdges H := hr.edges
  have hEl : d.edges.val.length = 1 + (DocModel.facts H).length := by
    have := congrArg List.length hE; simp [encEdges] at this; omega
  have hfa := facts_anchors H hb
  refine ⟨?_, ?_, ?_, ?_⟩
  · rw [hat]; simp [Document.anchorList]
  · intro e he
    have hm : (e.1.val, e.2.1.val, e.2.2.val) ∈ encEdges H := by
      rw [← hE]; exact List.mem_map.mpr ⟨e, he, rfl⟩
    unfold encEdges at hm
    rw [List.mem_cons] at hm
    rcases hm with hm | hm
    · simp only [Prod.mk.injEq] at hm
      rw [hat, hm.1, hm.2.1]; simp [Document.anchorList]
    · obtain ⟨f, hf, he'⟩ := List.mem_map.mp hm
      simp only [Prod.mk.injEq] at he'
      have h1 := hfa f hf
      rw [hat, ← he'.1, ← he'.2.1]
      exact ⟨idx_lt h1.1, idx_lt h1.2⟩
  · intro x hx
    obtain ⟨o, r, hm, e1, -⟩ := (retr_mem d H hr x.1.val x.2.val).mp ⟨x, hx, rfl, rfl⟩
    obtain ⟨j, hj, he⟩ := orderEdge_pos H hb r (built_retract_order hb hm)
    rw [hEl, e1, he]; omega
  · intro o ho
    have hm : o.val ∈ (orderList H).map (orderEdge H) := by
      rw [← hr.orders]; exact List.mem_map.mpr ⟨o, ho, rfl⟩
    obtain ⟨r, hr', he⟩ := List.mem_map.mp hm
    obtain ⟨j, hj, he'⟩ := orderEdge_pos H hb r ((isOrder_iff_mem H r).mpr hr')
    rw [hEl, ← he, he']; omega

/-! ## Validation -/

theorem insSize_eq (o : document.Op) :
    insSize o = (match o with | .Insert _ _ xs => xs.val.length | _ => 0) := by
  cases o <;> rfl

theorem validate_refines (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp) :
    document.validate d dp ⦃ r =>
      (r.kind.val = 0 ↔ DocModel.validate H P = .ok ()) ∧
      ∀ e, DocModel.validate H P = .error e → errorOf P r = some e ⦄ := by
  have hrep := rep_replayed d H hr
  have hb := replayed_built hr
  have hv := built_valid hb
  obtain ⟨cl, -, -⟩ := replay_props hv (by simp [Closed]) (by simp [ids])
  have hdl : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hrep.deps)
    simpa using this
  obtain ⟨r1, r2, r3, -, -⟩ := room
  have hsum : (dp.ops.val.map insSize).sum =
      (dp.ops.val.map fun | .Insert _ _ xs => xs.val.length | _ => 0).sum := by
    congr 1 <;> (try (apply List.map_congr_left; intro o _; exact insSize_eq o))
  have hroots : ∀ v ∈ dp.deps.val, v.val < d.deps.val.length := by
    intro v hv'
    have : v.val ∈ dp.deps.val.map (·.val) := List.mem_map.mpr ⟨v, hv', rfl⟩
    rw [ht.1] at this
    obtain ⟨q, hq, he⟩ := List.mem_map.mp this
    rw [← he, hdl]
    have := idx_lt (adm.2.2.2.1 q hq)
    simpa [ids] using this
  have hbounds : ∀ row ∈ d.deps.val, ∀ v ∈ row.val, v.val < d.deps.val.length := by
    intro row hrow v hv'
    have hm : row.val.map (·.val) ∈ d.deps.val.map (fun r => r.val.map (·.val)) :=
      List.mem_map.mpr ⟨row, hrow, rfl⟩
    rw [show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hrep.deps] at hm
    obtain ⟨Q, hQ, he⟩ := List.mem_map.mp hm
    have hvm : v.val ∈ row.val.map (·.val) := List.mem_map.mpr ⟨v, hv', rfl⟩
    rw [← he] at hvm
    obtain ⟨q, hq, he'⟩ := List.mem_map.mp hvm
    rw [← he', hdl]
    have := idx_lt (cl Q hQ q hq)
    simpa [ids] using this
  have key : ∀ f : document.Op → Nat, (∀ o, insSize o + 1 ≤ f o) →
      d.edges.val.length + 2 * (dp.ops.val.map f).sum + 2 < Usize.max →
      d.edges.val.length + (dp.ops.val.map (insSize · + 1)).sum + dp.ops.val.length + 2 < Usize.max := by
    intro f hf h
    have hle : (dp.ops.val.map (insSize · + 1)).sum + dp.ops.val.length ≤ 2 * (dp.ops.val.map f).sum := by
      clear h r1 r2 r3 hsum
      induction dp.ops.val with
      | nil => simp
      | cons o l ih => have := hf o; simp at ih ⊢; omega
    omega
  apply WP.spec_mono (validate_spec d dp (docWF_of_rep d H hrep hb) (by omega) hroots hbounds
    (by rw [hsum]; exact r1) (key _ (fun o => by cases o <;> simp [insSize]) r2))
  intro r hr'
  obtain ⟨h1, h2⟩ := validate_tr d H P dp hrep hb ht adm
  rw [← hr'] at h1 h2
  refine ⟨?_, fun e he => h2 e he⟩
  rw [← h1]
  simp only [absR]
  constructor
  · intro h; exact UScalar.eq_of_val_eq h
  · intro h; rw [h]; rfl

/-! ## Commit -/

theorem commit_succeeds (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp)
    (croom : CommitRoom d dp) (hv : DocModel.validate H P = .ok ()) :
    ∃ d', document.commit d dp = ok d' ∧ Replayed d' (H ++ [P]) := by
  have hrep := rep_replayed d H hr
  have hb := replayed_built hr
  obtain ⟨d', hd'⟩ := ok_of (commit_spec d dp croom.2.1 (commitRoomL_of d H P dp hrep hb ht hv croom))
  exact ⟨d', hd', .snoc hr ht adm hv croom hd'⟩

theorem applySpec_error {H : Hist} {R : Patch} (hn : R.id ∉ ids H) (hd : R.deps.Nodup)
    (hs : R.id ∉ R.deps) (hm : ∀ d ∈ R.deps, d ∈ ids H) (he : R.ops ≠ []) {e : DocModel.Error}
    (hv : DocModel.validate H R = .error e) : applySpec H R = .error e := by
  unfold applySpec
  rw [find_none hn]
  dsimp only
  rw [if_neg (by simp [hd, hs])]
  have hmiss : ¬ (R.deps.filter (· ∉ ids H) ≠ []) := by
    simp only [ne_eq, not_not, List.filter_eq_nil_iff, decide_eq_true_eq, not_not]
    exact hm
  rw [if_neg hmiss, if_neg he, hv]
  rfl

theorem apply_refines (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp)
    (croom : CommitRoom d dp) :
    document.validate d dp ⦃ r =>
      (r.kind.val = 0 → ∃ d', document.commit d dp = ok d' ∧ Replayed d' (H ++ [P]) ∧
          applySpec H P = .ok (H ++ [P], .inserted)) ∧
      (r.kind.val ≠ 0 → ∃ e, errorOf P r = some e ∧ applySpec H P = .error e) ⦄ := by
  apply WP.spec_mono (validate_refines d H P dp hr ht adm room)
  rintro r ⟨h1, h2⟩
  refine ⟨fun h0 => ?_, fun h0 => ?_⟩
  · have hv := h1.mp h0
    obtain ⟨d', hd', hrep'⟩ := commit_succeeds d H P dp hr ht adm room croom hv
    exact ⟨d', hd', hrep', applySpec_inserted adm.1 adm.2.1 adm.2.2.1 adm.2.2.2.1 adm.2.2.2.2 hv⟩
  · rcases hval : DocModel.validate H P with e | u
    · refine ⟨e, h2 e hval, ?_⟩
      exact applySpec_error adm.1 adm.2.1 adm.2.2.1 adm.2.2.2.1 adm.2.2.2.2 hval
    · exact absurd (h1.mpr (by rw [hval])) h0

end o5s_kernel.DocRefine
