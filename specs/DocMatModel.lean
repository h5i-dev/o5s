import DocKahn
import DocDepth
import Document
open Aeneas Aeneas.Std o5s_kernel
/-!
# The model's depth and blocking satisfy the Kahn axioms

`Bridge` states what a kernel graph must agree on with the history: liveness
by index, the boundary relation as the model's `Sum`, and every live anchor
indexed. Under it, `ax` derives the Kahn axioms from `DocDepth`. Also the
index/anchor lemmas used to decode the kernel's view.
-/
namespace o5s_kernel.DocMatModel
open DocModel Document DocKahn DocDepth

/-- The anchor at index `a` of the document's anchor table. -/
def anc (H : Hist) (a : Nat) : Anchor := (anchorList H)[a]?.getD .start

def DH (H : Hist) (a k : Nat) : Prop := a < (anchorList H).length ∧ Depth H (anc H a) k
def BH (H : Hist) (a : Nat) : Prop := a < (anchorList H).length ∧ Blocked H (anc H a)

/-- What the kernel graph must agree on with the model for the Kahn axioms. -/
structure Bridge (H : Hist) (g : KG) : Prop where
  n : g.n = (anchorList H).length
  D : g.D = DH H
  B : g.B = BH H
  live : ∀ a < g.n, g.liveF a = true ↔ Live H (anc H a)
  sum : ∀ u v, u < g.n → v < g.n → (g.S u v ↔ Sum H (anc H u) (anc H v))
  succ : ∀ u v, v ∈ g.bs u → g.L u ∧ g.L v
  nodup : ∀ u, (g.bs u).Nodup
  cover : ∀ x, Live H x → ∃ a < g.n, anc H a = x

variable {H : Hist} {g : KG}

theorem L_iff (hb : Bridge H g) {a : Nat} : g.L a ↔ a < g.n ∧ Live H (anc H a) := by
  unfold KG.L
  constructor
  · rintro ⟨h1, h2⟩; exact ⟨h1, (hb.live a h1).mp h2⟩
  · rintro ⟨h1, h2⟩; exact ⟨h1, (hb.live a h1).mpr h2⟩

theorem S_lt (hb : Bridge H g) {u v : Nat} (h : g.S u v) : u < g.n ∧ v < g.n :=
  ⟨(hb.succ u v h.2).1.1, (hb.succ u v h.2).2.1⟩

/-- Every model predecessor of an indexed atom is indexed. -/
theorem pred_of_sum (hb : Bridge H g) {x : Anchor} {v : Nat} (hv : v < g.n)
    (h : Sum H x (anc H v)) : ∃ u, g.S u v ∧ anc H u = x := by
  obtain ⟨u, hu, rfl⟩ := hb.cover x h.1
  exact ⟨u, (hb.sum u v hu hv).mpr h, rfl⟩

theorem ax (hb : Bridge H g) : g.Ax := by
  have hD := hb.D; have hB := hb.B
  have hlen : ∀ a, a < g.n → a < (anchorList H).length := fun a h => hb.n ▸ h
  refine ⟨hb.succ, hb.nodup, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · -- d0
    intro v hl hbv
    rw [hD]
    constructor
    · rintro ⟨-, hd⟩ u hs
      exact (depth_zero_iff.mp hd) _ ((hb.sum u v (S_lt hb hs).1 hl.1).mp hs)
    · intro h
      refine ⟨hlen v hl.1, depth_zero_iff.mpr fun x hx => ?_⟩
      obtain ⟨u, hs, rfl⟩ := pred_of_sum hb hl.1 hx
      exact h u hs
  · -- dsucc
    intro v r hl hbv
    have hlv := ((L_iff hb).mp hl).2
    have hbv' : ¬ Blocked H (anc H v) := fun h => hbv (hB ▸ ⟨hlen v hl.1, h⟩)
    rw [hD]
    constructor
    · rintro ⟨-, hd⟩
      obtain ⟨h1, x, hx, hdx⟩ := (depth_succ_iff hlv hbv').mp hd
      refine ⟨fun u hs => ?_, ?_⟩
      · obtain ⟨k, hk, hdk⟩ := h1 _ ((hb.sum u v (S_lt hb hs).1 hl.1).mp hs)
        exact ⟨k, hk, hlen u (S_lt hb hs).1, hdk⟩
      · obtain ⟨u, hs, rfl⟩ := pred_of_sum hb hl.1 hx
        exact ⟨u, hs, hlen u (S_lt hb hs).1, hdx⟩
    · rintro ⟨h1, u, hs, -, hdu⟩
      refine ⟨hlen v hl.1, (depth_succ_iff hlv hbv').mpr ⟨fun x hx => ?_, _,
        (hb.sum u v (S_lt hb hs).1 hl.1).mp hs, hdu⟩⟩
      obtain ⟨u', hs', rfl⟩ := pred_of_sum hb hl.1 hx
      obtain ⟨k, hk, -, hdk⟩ := h1 u' hs'
      exact ⟨k, hk, hdk⟩
  · -- bpred
    intro u v hs hbv
    rw [hB] at hbv ⊢
    rintro ⟨-, hbu⟩
    exact hbv ⟨hlen v (S_lt hb hs).2, blocked_of_reach hbu
      (Relation.ReflTransGen.single ((hb.sum u v (S_lt hb hs).1 (S_lt hb hs).2).mp hs))⟩
  · -- bunb
    intro v hl h
    rw [hB] at h ⊢
    rintro ⟨-, hbv⟩
    apply unblocked_of_preds (fun x hx => ?_) hbv
    obtain ⟨u, hs, rfl⟩ := pred_of_sum hb hl.1 hx
    exact fun hbu => h u hs ⟨hlen u (S_lt hb hs).1, hbu⟩
  · -- dex
    intro v hl hbv
    have hlv := ((L_iff hb).mp hl).2
    have hbv' : ¬ Blocked H (anc H v) := fun h => hbv (hB ▸ ⟨hlen v hl.1, h⟩)
    obtain ⟨k, hk⟩ := depth_exists hlv hbv'
    exact ⟨k, hD ▸ ⟨hlen v hl.1, hk⟩⟩
  · -- duniq
    intro v k m hk hm
    rw [hD] at hk hm
    exact depth_unique hk.2 hm.2
  · -- dbelow
    intro v k j hl hbv hd hjk
    have hlv := ((L_iff hb).mp hl).2
    have hbv' : ¬ Blocked H (anc H v) := fun h => hbv (hB ▸ ⟨hlen v hl.1, h⟩)
    rw [hD] at hd
    obtain ⟨⟨p, hpl, hpl', hpc⟩, -⟩ := id hd.2
    have hj : j < p.length := by omega
    have hdj := depth_at hd.2 hlv hbv' hpl hpl' hpc hj
    have hlj : Live H p[j] := chain_live hpc hpl' hlv _ (List.getElem_mem hj)
    have hbj : ¬ Blocked H p[j] := fun b => hbv' (blocked_of_reach b (chain_reach hpc hpl' hj))
    obtain ⟨w, hw, he⟩ := hb.cover _ hlj
    refine ⟨w, (L_iff hb).mpr ⟨hw, he ▸ hlj⟩, ?_, ?_⟩
    · rw [hB]; rintro ⟨-, h⟩; exact hbj (he ▸ h)
    · rw [hD]; exact ⟨hlen w hw, he ▸ hdj⟩

/-- Blocked anchors are live. -/
theorem blocked_live {c : Anchor} (h : Blocked H c) : Live H c := by
  obtain ⟨c', hc, hcc⟩ := h
  rcases Relation.ReflTransGen.cases_tail hcc with rfl | ⟨y, -, hy⟩
  · obtain ⟨y, -, hy⟩ := Relation.TransGen.tail'_iff.mp hc
    exact hy.2.1
  · exact hy.2.1

theorem anc_get {a : Nat} (h : a < (anchorList H).length) : anc H a = (anchorList H)[a] := by
  simp [anc, List.getElem?_eq_getElem h]

theorem nodeAt_iff {a : Nat} {n : NodeId} :
    nodeAt H a = some n ↔ a < (anchorList H).length ∧ anc H a = .node n := by
  unfold nodeAt
  by_cases h : a < (anchorList H).length
  · rw [List.getElem?_eq_getElem h, anc_get h]
    constructor
    · intro e
      refine ⟨h, ?_⟩
      revert e
      cases (anchorList H)[a] <;> simp
    · rintro ⟨-, e⟩; rw [e]
  · rw [List.getElem?_eq_none_iff.mpr (by omega)]
    simp [h]

theorem mem_nodes {xs : List Usize} {n : NodeId} :
    n ∈ nodes H xs ↔ ∃ a ∈ xs.map (·.val), a < (anchorList H).length ∧ anc H a = .node n := by
  unfold nodes
  simp only [List.mem_toFinset, List.mem_filterMap, List.mem_map]
  constructor
  · rintro ⟨x, hx, e⟩; exact ⟨x.val, ⟨x, hx, rfl⟩, nodeAt_iff.mp e⟩
  · rintro ⟨a, ⟨x, hx, rfl⟩, h⟩; exact ⟨x, hx, nodeAt_iff.mpr h⟩

theorem atomsOf_fst (x : OpId × Op) : ∀ m ∈ (atomsOf x).map Prod.fst, m.op = x.1 := by
  obtain ⟨o, op⟩ := x
  cases op <;> simp [atomsOf]
  intro m x k _ e; subst e; rfl

theorem atomsOf_nodup (x : OpId × Op) : ((atomsOf x).map Prod.fst).Nodup := by
  obtain ⟨o, op⟩ := x
  cases op with
  | insert l r xs =>
    have e : ((atomsOf (o, .insert l r xs)).map Prod.fst) =
        (List.range xs.length).map fun k => (⟨o, k⟩ : NodeId) := by
      apply List.ext_getElem
      · simp [atomsOf]
      · intro i h1 h2; simp [atomsOf]
    rw [e]
    exact List.Nodup.map (fun a b h => by simpa using h) List.nodup_range
  | _ => simp [atomsOf]

theorem flatMap_atomsOf_op (L : List (OpId × Op)) :
    ∀ m ∈ (L.flatMap atomsOf).map Prod.fst, m.op ∈ L.map Prod.fst := by
  intro m hm
  simp only [List.mem_map, List.mem_flatMap] at hm
  obtain ⟨⟨m', b⟩, ⟨x, hx, hmx⟩, rfl⟩ := hm
  have := atomsOf_fst x m' (List.mem_map.mpr ⟨(m', b), hmx, rfl⟩)
  exact List.mem_map.mpr ⟨x, hx, this.symm⟩

theorem flatMap_atomsOf_nodup (L : List (OpId × Op)) (h : (L.map Prod.fst).Nodup) :
    ((L.flatMap atomsOf).map Prod.fst).Nodup := by
  induction L with
  | nil => simp
  | cons x t ih =>
    rw [List.flatMap_cons, List.map_append]
    rw [List.map_cons, List.nodup_cons] at h
    refine List.nodup_append.mpr ⟨atomsOf_nodup x, ih h.2, ?_⟩
    intro a ha b hb e
    subst e
    have h1 := atomsOf_fst x a ha
    have h2 := flatMap_atomsOf_op t a hb
    rw [h1] at h2
    exact h.1 h2

theorem anchorList_nodup (hk : ((ops H).map Prod.fst).Nodup) : (anchorList H).Nodup := by
  have hn : ((DocModel.atoms H).map Prod.fst).Nodup := flatMap_atomsOf_nodup _ hk
  unfold anchorList
  have e : (DocModel.atoms H).map (fun a => Anchor.node a.1) = ((DocModel.atoms H).map Prod.fst).map Anchor.node := by
    simp
  rw [e]
  simp only [List.cons_append, List.nil_append, List.nodup_cons, List.mem_cons, List.mem_map, reduceCtorEq,
    and_false, exists_false, or_false, not_false_eq_true, true_and]
  exact List.Nodup.map (fun a b h => by cases h; rfl) hn

end o5s_kernel.DocMatModel
