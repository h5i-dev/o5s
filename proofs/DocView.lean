import DocCommit
import DocMaterialize
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The materializer's tables, read through the representation (draft)

What the kernel's materializer computes from a document that encodes `H`:
the globally withdrawn edges are the retracted ones, its edge rows are the
model's `E H`, its liveness is `Live H`. `materialize_refines` composes these
with the Kahn layering.
-/
namespace o5s_kernel.DocView
open DocModel Document DocRep DocKernel

/-- Edge index `1 + j` is withdrawn for everyone exactly when fact `j`'s
operation was retracted. -/
theorem globalOff_iff (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H)
    (j : Nat) (hj : j < (DocModel.facts H).length) :
    (∃ x ∈ d.retractions.val, x.1.val = 1 + j) ↔
      RetractedBy H (fun _ => True) (DocModel.facts H)[j].2.2 := by
  have hu := built_nodup hb
  have oe : ∀ r, IsOrder H r → (orderEdge H r = 1 + j ↔ (DocModel.facts H)[j].2.2 = r) := by
    rintro r ⟨b, a, hm⟩
    obtain ⟨j', hj', he, hf⟩ := orderEdge_spec H hb r b a hm
    rw [he]
    constructor
    · intro h
      have : j' = j := by omega
      subst this
      rw [hf]
    · intro h
      have e1 := order_fact_index H hu r b a hm j hj h
      have e2 := order_fact_index H hu r b a hm j' hj' (by rw [hf])
      omega
  constructor
  · rintro ⟨x, hx, h1⟩
    obtain ⟨o, r, hm, e1, -⟩ := (retr_mem d H hr _ _).mp ⟨x, hx, rfl, rfl⟩
    have hfr : (DocModel.facts H)[j].2.2 = r := (oe r (built_retract_order hb hm)).mp (by rw [← e1, h1])
    exact ⟨o, by rw [hfr]; exact hm, trivial⟩
  · rintro ⟨o, hm, -⟩
    have hor := built_retract_order hb hm
    obtain ⟨x, hx, h1, -⟩ := (retr_mem d H hr _ _).mpr ⟨o, _, hm, rfl, rfl⟩
    exact ⟨x, hx, by rw [h1]; exact (oe _ hor).mpr rfl⟩

/-- No retraction withdraws the sentinel edge. -/
theorem globalOff_zero (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H) :
    ¬ ∃ x ∈ d.retractions.val, x.1.val = 0 := by
  rintro ⟨x, hx, h1⟩
  obtain ⟨o, r, hm, e1, -⟩ := (retr_mem d H hr _ _).mp ⟨x, hx, rfl, rfl⟩
  obtain ⟨j, -, he⟩ := orderEdge_pos H hb r (built_retract_order hb hm)
  omega

/-- A kernel edge from index `a` to index `b` that no retraction withdraws. -/
def KEdge (d : document.Doc) (a b : Nat) : Prop :=
  ∃ i, ∃ h : i < d.edges.val.length, ¬ (∃ x ∈ d.retractions.val, x.1.val = i) ∧
    (d.edges.val[i]).1.val = a ∧ (d.edges.val[i]).2.1.val = b

theorem mem_E (H : Hist) (u v : Anchor) :
    (u, v) ∈ DocModel.E H ↔ (u, v) = (.start, .stop) ∨
      ∃ f ∈ DocModel.facts H, ¬ RetractedBy H (fun _ => True) f.2.2 ∧ f.1 = u ∧ f.2.1 = v := by
  unfold DocModel.E
  simp only [List.mem_cons, List.mem_map, List.mem_filter, decide_eq_true_eq, Prod.mk.injEq]
  constructor
  · rintro (h | ⟨f, ⟨hf, hr⟩, h1, h2⟩)
    · exact Or.inl h
    · exact Or.inr ⟨f, hf, hr, h1, h2⟩
  · rintro (h | ⟨f, hf, hr, h1, h2⟩)
    · exact Or.inl h
    · exact Or.inr ⟨f, ⟨hf, hr⟩, h1, h2⟩

/-- The kernel's non-withdrawn edges are the model's `E H`, through `aIdx`. -/
theorem kedge_iff (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H)
    (u v : Anchor) (hu : u ∈ anchorList H) (hv : v ∈ anchorList H) :
    KEdge d (aIdx H u) (aIdx H v) ↔ (u, v) ∈ DocModel.E H := by
  have hE : d.edges.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) = encEdges H := hr.edges
  have hElen : d.edges.val.length = 1 + (DocModel.facts H).length := by
    have := congrArg List.length hE; simp [encEdges] at this; omega
  have hent : ∀ i (hi : i < d.edges.val.length),
      ((d.edges.val[i]).1.val, (d.edges.val[i]).2.1.val, (d.edges.val[i]).2.2.val) =
        (encEdges H)[i]'(by rw [← hE]; simpa using hi) := by
    intro i hi
    rw [← List.getElem_of_eq hE (by simpa using hi), List.getElem_map]
  have hfa := facts_anchors H hb
  rw [mem_E]
  unfold KEdge
  constructor
  · rintro ⟨i, hi, hoff, ha, hb'⟩
    cases i with
    | zero =>
      left
      have e0 := hent 0 hi
      simp only [encEdges, List.getElem_cons_zero, Prod.mk.injEq] at e0
      have eu : u = .start := aIdx_inj hu (start_mem _) (by rw [aIdx_start, ← ha, e0.1])
      have ev : v = .stop := aIdx_inj hv (stop_mem _) (by rw [aIdx_stop, ← hb', e0.2.1])
      rw [eu, ev]
    | succ j =>
      right
      have hj : j < (DocModel.facts H).length := by omega
      have e := hent (j + 1) hi
      simp only [encEdges, List.getElem_cons_succ, List.getElem_map, Prod.mk.injEq] at e
      have h1 := hfa _ (List.getElem_mem hj)
      refine ⟨(DocModel.facts H)[j], List.getElem_mem hj, ?_, ?_, ?_⟩
      · intro hret
        apply hoff
        rw [Nat.add_comm]
        exact (globalOff_iff d H hr hb j hj).mpr hret
      · exact aIdx_inj h1.1 hu (by rw [← e.1, ha])
      · exact aIdx_inj h1.2 hv (by rw [← e.2.1, hb'])
  · rintro (h0 | ⟨f, hf, hret, h1, h2⟩)
    · simp only [Prod.mk.injEq] at h0
      obtain ⟨rfl, rfl⟩ := h0
      have e0 := hent 0 (by omega)
      simp only [encEdges, List.getElem_cons_zero, Prod.mk.injEq] at e0
      exact ⟨0, by omega, globalOff_zero d H hr hb, by rw [e0.1, aIdx_start], by rw [e0.2.1, aIdx_stop]⟩
    · obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hf
      have e := hent (j + 1) (by omega)
      simp only [encEdges, List.getElem_cons_succ, List.getElem_map, Prod.mk.injEq] at e
      refine ⟨j + 1, by omega, ?_, ?_, ?_⟩
      · intro hoff
        rw [Nat.add_comm] at hoff
        exact hret ((globalOff_iff d H hr hb j hj).mp hoff)
      · rw [e.1, h1]
      · rw [e.2.1, h2]

open Classical in
/-- The kernel's liveness flag of an atom's index is the model's `Live`. -/
theorem liveM_iff (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H)
    (n : NodeId) (hn : Anchor.node n ∈ anchorList H) :
    liveM (coverUpTo d.deletions.val d.atoms.val.length d.deletions.val.length) d.restored.val
      (aIdx H (.node n)) = true ↔ Live H (.node n) := by
  have hat : d.atoms.val.length = (anchorList H).length := by
    have := congrArg List.length (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms)
    simpa [Document.anchorList] using this
  have hdel : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hres : d.restored.val = (deletionList H).map fun x => decide (Restored H x) := hr.restored
  have hdn := deletionList_nodup H (ops_keys_nodup (built_nodup hb))
  have ha : aIdx H (.node n) < d.atoms.val.length := by rw [hat]; exact idx_lt hn
  have h2 := idx_node_ge H n hn
  -- deletion `k` covers the atom exactly when the atom is among its targets
  have cover : ∀ k (hk : k < d.deletions.val.length),
      (∃ t ∈ (d.deletions.val[k]).targets.val, t.val = aIdx H (.node n)) ↔
        ∃ ts, tag H (deletionList H)[k] = some ts ∧ n ∈ ts := by
    intro k hk
    have hk' : k < (deletionList H).length := by omega
    have hx : (deletionList H)[k] ∈ deletionList H := List.getElem_mem hk'
    obtain ⟨ts, hts⟩ := Option.ne_none_iff_exists'.mp ((tag_ne_none_iff H _).mpr hx)
    obtain ⟨hi, he⟩ := deletion_entry d H hr hb _ hx ts hts
    have hik : idx (deletionList H) (deletionList H)[k] = k := idx_getElem hdn k hk'
    have htm := deletion_targets_mem H hb _ ts hts
    constructor
    · rintro ⟨t, ht, htv⟩
      refine ⟨ts, hts, ?_⟩
      have : t.val ∈ (d.deletions.val[idx (deletionList H) (deletionList H)[k]]'hi).targets.val.map (·.val) := by
        simp only [hik]; exact List.mem_map.mpr ⟨t, ht, rfl⟩
      rw [he] at this
      obtain ⟨m, hm, hme⟩ := List.mem_map.mp this
      have := aIdx_inj (htm m hm) hn (by rw [hme, htv])
      simp only [Anchor.node.injEq] at this
      rw [← this]; exact hm
    · rintro ⟨ts', hts', hnt⟩
      rw [hts] at hts'
      simp only [Option.some.injEq] at hts'
      subst hts'
      have : aIdx H (.node n) ∈ (d.deletions.val[idx (deletionList H) (deletionList H)[k]]'hi).targets.val.map (·.val) := by
        rw [he]; exact List.mem_map.mpr ⟨n, hnt, rfl⟩
      simp only [hik] at this
      obtain ⟨t, ht, htv⟩ := List.mem_map.mp this
      exact ⟨t, ht, htv⟩
  unfold liveM Live
  rw [List.getElem?_eq_getElem (by rw [coverUpTo_length]; exact ha)]
  simp only [Option.getD_some, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true]
  constructor
  · rintro ⟨-, hall⟩
    refine ⟨(mem_anchorList_node H n).mp hn, fun dd ts hts hnt => ?_⟩
    have hx : dd ∈ deletionList H := (tag_ne_none_iff H dd).mp (by rw [hts]; simp)
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
    have hkd : k < d.deletions.val.length := by omega
    have hmem : k ∈ (coverUpTo d.deletions.val d.atoms.val.length d.deletions.val.length)[aIdx H (.node n)]'(by
        rw [coverUpTo_length]; exact ha) :=
      (mem_coverUpTo _ _ _ _ ha k).mpr ⟨hkd, hkd, (cover k hkd).mpr ⟨ts, hts, hnt⟩⟩
    have := hall k hmem
    rw [hres, List.getElem?_map, List.getElem?_eq_getElem hk] at this
    simpa using this
  · rintro ⟨-, hall⟩
    refine ⟨h2, fun k hk => ?_⟩
    obtain ⟨-, hkd, hcov⟩ := (mem_coverUpTo _ _ _ _ ha k).mp hk
    obtain ⟨ts, hts, hnt⟩ := (cover k hkd).mp hcov
    have := hall _ ts hts hnt
    rw [hres, List.getElem?_map, List.getElem?_eq_getElem (by omega)]
    simpa using this

/-! ## Dead-interior segments -/

theorem seg_extend (H : Hist) {u x v : Anchor} (h : Seg H u x) (hx : ¬ Live H x)
    (he : (x, v) ∈ DocModel.E H) : Seg H u v := by
  induction h with
  | edge h1 => exact .step h1 hx (.edge he)
  | step h1 hw _ ih => exact .step h1 hw (ih hx he)

theorem segment_prepend {α} {edges : List (α × α)} {live : α → Prop} {u w v : α}
    (h1 : (u, w) ∈ edges) (hw : ¬ live w) (h : Projection.Segment edges live w v) :
    Projection.Segment edges live u v := by
  induction h with
  | edge h2 => exact .extend (.edge h1) hw h2
  | extend _ hx h3 ih => exact .extend ih hx h3

/-- The model's `Seg` is `Projection.Segment` over `E H` with live interiors excluded. -/
theorem seg_iff_segment (H : Hist) (u v : Anchor) :
    Seg H u v ↔ Projection.Segment (DocModel.E H) (Live H) u v := by
  constructor
  · intro h
    induction h with
    | edge h1 => exact .edge h1
    | step h1 hw _ ih => exact segment_prepend h1 hw ih
  · intro h
    induction h with
    | edge h1 => exact .edge h1
    | extend _ hx h3 ih => exact seg_extend H ih hx h3

/-- `Segment` transported along a map that is injective on the carrier. -/
theorem segment_map {α β} (φ : α → β) (S : α → Prop) (edges : List (α × α)) (edges' : List (β × β))
    (live : α → Prop) (live' : β → Prop)
    (hinj : ∀ a b, S a → S b → φ a = φ b → a = b)
    (hE : ∀ a b, S a → S b → ((φ a, φ b) ∈ edges' ↔ (a, b) ∈ edges))
    (hE' : ∀ x y, (x, y) ∈ edges' → ∃ a b, φ a = x ∧ φ b = y ∧ S a ∧ S b)
    (hS : ∀ a b, (a, b) ∈ edges → S a ∧ S b)
    (hlive : ∀ a, S a → (live' (φ a) ↔ live a))
    (u v : α) (hu : S u) (hv : S v) :
    Projection.Segment edges' live' (φ u) (φ v) ↔ Projection.Segment edges live u v := by
  constructor
  · intro h
    have key : ∀ y, Projection.Segment edges' live' (φ u) y →
        ∃ b, φ b = y ∧ S b ∧ Projection.Segment edges live u b := by
      intro y h
      induction h with
      | edge h1 =>
        obtain ⟨a, b, ha, hb, hSa, hSb⟩ := hE' _ _ h1
        have : a = u := hinj _ _ hSa hu ha
        subst this
        refine ⟨b, hb, hSb, .edge ((hE _ _ hSa hSb).mp (by rw [ha, hb]; exact h1))⟩
      | extend _ hx h3 ih =>
        obtain ⟨c, hc, hSc, hseg⟩ := ih
        obtain ⟨a, b, ha, hb, hSa, hSb⟩ := hE' _ _ h3
        have : a = c := hinj _ _ hSa hSc (by rw [ha, hc])
        subst this
        refine ⟨b, hb, hSb, .extend hseg ?_ ((hE _ _ hSa hSb).mp (by rw [ha, hb]; exact h3))⟩
        rw [← hlive _ hSa, ha]; exact hx
    obtain ⟨b, hb, hSb, hseg⟩ := key _ h
    have : b = v := hinj _ _ hSb hv hb
    exact this ▸ hseg
  · intro h
    have key : ∀ y, Projection.Segment edges live u y → S y →
        Projection.Segment edges' live' (φ u) (φ y) := by
      intro y h
      induction h with
      | edge h1 =>
        intro hSy
        exact .edge ((hE _ _ hu hSy).mpr h1)
      | extend h0 hx h3 ih =>
        intro hSy
        have hSx := (hS _ _ h3).1
        exact .extend (ih hSx) (by rw [hlive _ hSx]; exact hx) ((hE _ _ hSx hSy).mpr h3)
    exact key _ h hv

end o5s_kernel.DocView
