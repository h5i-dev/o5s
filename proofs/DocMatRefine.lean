import DocDecode
import DocRefine
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The extracted `materialize` refines the model's view

`materialize_refines` (T2.0): on a replayed document, the decoded result of the
extracted `materialize` is the specified view. It composes the kernel proof
(`materialize_kernel`), the bridge from the extracted boundary rows to the
model's relation (`bridge`, so the model's depth and blocking satisfy the Kahn
axioms) and the decoding (`isView_of`, `di_iff`, `atom_bytes`).
`extracted_view_converges` (T3.5) follows with the model's uniqueness and
permutation invariance of the view.
-/
namespace o5s_kernel.DocMatRefine
open DocModel Document DocRep DocKernel DocKahn DocMatModel DocBridge DocDecode DocRefine

variable {d : document.Doc} {H : Hist}

theorem insertionList_patch_mem (y : OpId) (hy : y ∈ insertionList H) : y.patch ∈ ids H := by
  unfold insertionList at hy
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp hy
  cases op <;> simp at hf
  subst hf
  unfold ops at hm
  obtain ⟨P, hP, hm⟩ := List.mem_flatMap.mp hm
  exact List.mem_map.mpr ⟨P, hP, ((mem_opsOf_iff.mp hm).1).symm⟩

theorem matWF_of (hr : Rep d H) (hb : Built H) (cl : Closed H) (hdr : d.deps.val.length < Usize.max)
    (hout : d.deletions.val.length * d.insertions.val.length < Usize.max) : MatWF d := by
  have hu := built_nodup hb
  have hdl := deps_len hr
  have hdlen : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hilen : d.insertions.val.length = (insertionList H).length := by
    have := congrArg List.length (show d.insertions.val.map
      (fun x => (x.patch.val, x.left.val, x.right.val)) = _ from hr.insertions)
    rw [insertionsOf_eq_map H hb] at this; simpa using this
  refine ⟨?_, ?_, ?_, ?_, hdr, hout⟩
  · have := congrArg List.length (show d.restored.val = _ from hr.restored)
    rw [this, hdlen]; simp
  · intro x hx
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
    have hk' : k < (deletionList H).length := hdlen ▸ hk
    have hm := List.getElem_mem hk'
    have := deletion_patch hr hb hm (by rw [idx_getElem (deletionList_nodup H (ops_keys_nodup hu)) k hk']; exact hk)
    simp only [idx_getElem (deletionList_nodup H (ops_keys_nodup hu)) k hk'] at this
    rw [this, hdl]
    simpa [ids] using idx_lt (deletionList_patch_mem H _ hm)
  · intro x hx
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
    have hk' : k < (insertionList H).length := hilen ▸ hk
    have hin := insertionList_nodup' (ops_keys_nodup hu) (H := H)
    have hm := List.getElem_mem hk'
    have := insertion_patch hr hb hm (by rw [idx_getElem hin k hk']; exact hk)
    simp only [idx_getElem hin k hk'] at this
    rw [this, hdl]
    have hpy : ((insertionList H)[k]).patch ∈ ids H := insertionList_patch_mem _ hm
    simpa [ids] using idx_lt hpy
  · intro row hrow v hv
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hrow
    obtain ⟨q, hq, he⟩ := (deps_row hr hk v).mp hv
    rw [he, hdl]
    simpa [ids] using idx_lt (cl _ (List.getElem_mem _) q hq)

theorem sum_atomB (atoms : List (alloc.vec.Vec U8)) :
    ∑ v ∈ Finset.range atoms.length, (atomB atoms v).length = (atoms.map (·.val.length)).sum := by
  induction atoms using List.reverseRecOn with
  | nil => simp
  | append_singleton l x ih =>
    rw [List.length_append, List.length_singleton, Finset.sum_range_succ, List.map_append, List.sum_append]
    have e1 : ∀ v ∈ Finset.range l.length, atomB (l ++ [x]) v = atomB l v := by
      intro v hv; simp [atomB, List.getElem?_append_left (Finset.mem_range.mp hv)]
    rw [Finset.sum_congr rfl (fun v hv => by rw [e1 v hv]), ih]
    simp [atomB]

theorem hdiv_of (hr : Rep d H) (hb : Built H) {di : List document.DeleteInsert}
    (hdi : di.map diVal = deleteInsertsM d) :
    ∀ x ∈ di, x.deletion.val < (deletionList H).length ∧ x.insertion.val < (insertionList H).length := by
  have hdlen : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hilen : d.insertions.val.length = (insertionList H).length := by
    have := congrArg List.length (show d.insertions.val.map
      (fun x => (x.patch.val, x.left.val, x.right.val)) = _ from hr.insertions)
    rw [insertionsOf_eq_map H hb] at this; simpa using this
  intro x hx
  have hmem : diVal x ∈ deleteInsertsM d := by rw [← hdi]; exact List.mem_map.mpr ⟨x, hx, rfl⟩
  obtain ⟨dI, iI, hd, -, hi, hc⟩ := (mem_deleteInsertsM _).mp hmem
  rw [conflictAt_some d hd hi] at hc
  split_ifs at hc
  have hv := (Option.some.inj hc).symm
  simp only [diVal, Prod.mk.injEq] at hv
  rw [hv.1, ← hdlen, hv.2.1, ← hilen]
  exact ⟨hd, hi⟩

/-- T2.0: the decoded kernel view is the specified view. -/
theorem materialize_refines (d : document.Doc) (H : Hist) (hrp : Replayed d H) (room : RoomM d) :
    document.materialize d ⦃ v => IsView H (decode H v) ⦄ := by
  have hr := rep_replayed d H hrp
  have hb := replayed_built hrp
  obtain ⟨cl, -, -⟩ := replay_props (built_valid hb) (by simp [Closed]) (by simp [ids])
  have wf := docWF_of_rep d H hr hb
  obtain ⟨hnm, eroom, hsum, hdr, droom, hout⟩ := room
  have mwf := matWF_of hr hb cl hdr hout
  have hdel : ∀ x ∈ d.deletions.val, ∀ t ∈ x.targets.val, t.val < d.atoms.val.length := by
    intro x hx t ht
    have hdn := deletionList_nodup H (ops_keys_nodup (built_nodup hb))
    have hdlen : d.deletions.val.length = (deletionList H).length := by
      have := congrArg List.length (show d.deletions.val.map
        (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
      simpa [deletionsOf_length] using this
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
    have hk' : k < (deletionList H).length := hdlen ▸ hk
    have hm := List.getElem_mem hk'
    obtain ⟨ts, hts⟩ := Option.ne_none_iff_exists'.mp ((tag_ne_none_iff H _).mpr hm)
    obtain ⟨hi, he⟩ := deletion_entry d H hr hb _ hm ts hts
    simp only [idx_getElem hdn k hk'] at he
    have : t.val ∈ (d.deletions.val[k]).targets.val.map (·.val) := List.mem_map.mpr ⟨t, ht, rfl⟩
    rw [he] at this
    obtain ⟨n, hn, hen⟩ := List.mem_map.mp this
    rw [← hen, atoms_len hr]
    exact idx_lt (deletion_targets_mem H hb _ ts hts n hn)
  have roomT : ∑ v ∈ Finset.range d.atoms.val.length, (atomB d.atoms.val v).length ≤ Usize.max := by
    rw [sum_atomB]; omega
  apply WP.spec_mono (materialize_kernel d wf mwf hdel droom hnm eroom roomT (DH H) (BH H)
    (fun bnd hrows => ax (bridge hr hb wf bnd hrows)))
  rintro v ⟨rest, b, hF, hbl, hdi, hclean⟩
  exact isView_of hr hb v rest b hF hbl hclean (di_iff hr hb cl v.delete_inserts.val hdi)
    (hdiv_of hr hb hdi) (fun a ha m hm => atom_bytes hr hb ha hm)

/-- T3.5 (main, on the extracted code): two replicas that admitted the same
patches, in any orders, materialize the same view. -/
theorem extracted_view_converges (d₁ d₂ : document.Doc) (H₁ H₂ : Hist)
    (r₁ : Replayed d₁ H₁) (r₂ : Replayed d₂ H₂) (same : ∀ P, P ∈ H₁ ↔ P ∈ H₂)
    (u₁ : (ids H₁).Nodup) (u₂ : (ids H₂).Nodup) (room₁ : RoomM d₁) (room₂ : RoomM d₂) :
    document.materialize d₁ ⦃ v₁ => document.materialize d₂ ⦃ v₂ =>
      decode H₁ v₁ = decode H₂ v₂ ⦄ ⦄ := by
  have n1 : H₁.Nodup := List.Nodup.of_map (·.id) (show (H₁.map (·.id)).Nodup from u₁)
  have n2 : H₂.Nodup := List.Nodup.of_map (·.id) (show (H₂.map (·.id)).Nodup from u₂)
  have hp : H₁.Perm H₂ := (List.perm_ext_iff_of_nodup n1 n2).mpr same
  apply WP.spec_mono (materialize_refines d₁ H₁ r₁ room₁)
  intro v₁ hv₁
  apply WP.spec_mono (materialize_refines d₂ H₂ r₂ room₂)
  intro v₂ hv₂
  exact view_unique H₁ _ _ hv₁ ((view_perm u₁ hp _).mpr hv₂)

end o5s_kernel.DocMatRefine
