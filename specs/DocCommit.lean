import DocRep
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Commit keeps the representation (draft)

`rep_commit`: committing a validated, translated patch to a document that
encodes `H` gives one that encodes `H ++ [P]`. The kernel commits the
operations one at a time (`DocKernel.commitL`); each step appends one
operation to the history (`Snoc`), and `rep_op` shows every field of the
encoding follows. `Good` collects the facts about earlier operations the
re-encoding needs (their anchors, targets and references are interned).
-/
namespace o5s_kernel.DocRep
open DocModel Document

/-! ## One more operation -/

/-- `H1` is `H0` with operation `(o, op)` appended (by the last patch). -/
structure Snoc (H0 H1 : Hist) (o : OpId) (op : Op) : Prop where
  ops : DocModel.ops H1 = DocModel.ops H0 ++ [(o, op)]
  ids : DocModel.ids H1 = DocModel.ids H0
  deps : H1.map (fun P => P.deps) = H0.map (fun P => P.deps)
  fresh : ∀ x ∈ DocModel.ops H0, x.1 ≠ o
  patch : o.patch ∈ DocModel.ids H0

/-- What the encoding of a history needs to know about its operations. -/
structure Good (H : Hist) : Prop where
  facts : ∀ f ∈ DocModel.facts H, f.1 ∈ anchorList H ∧ f.2.1 ∈ anchorList H
  insertions : ∀ o l r xs, (o, Op.insert l r xs) ∈ DocModel.ops H → l ∈ anchorList H ∧ r ∈ anchorList H
  deletions : ∀ o ts, (o, Op.delete ts) ∈ DocModel.ops H → ∀ t ∈ ts, Anchor.node t ∈ anchorList H
  keys : ((DocModel.ops H).map Prod.fst).Nodup
  restores : ∀ o d, (o, Op.restore d) ∈ DocModel.ops H → d ∈ deletionList H
  keeps : ∀ o d i, (o, Op.keep d i) ∈ DocModel.ops H → d ∈ deletionList H ∧ i ∈ insertionList H
  retracts : ∀ o r, (o, Op.retract r) ∈ DocModel.ops H → r ∈ orderList H

/-- The new operation refers only to what `H0` interned. -/
def OpOK (H0 : Hist) : Op → Prop
  | .insert l r _ => l ∈ anchorList H0 ∧ r ∈ anchorList H0
  | .delete ts => ∀ t ∈ ts, Anchor.node t ∈ anchorList H0
  | .restore d => d ∈ deletionList H0
  | .order b a => Anchor.node b ∈ anchorList H0 ∧ Anchor.node a ∈ anchorList H0
  | .keep d i => d ∈ deletionList H0 ∧ i ∈ insertionList H0
  | .retract r => r ∈ orderList H0

section
variable {H0 H1 : Hist} {o : OpId} {op : Op}

theorem Snoc.atoms_eq (s : Snoc H0 H1 o op) : DocModel.atoms H1 = DocModel.atoms H0 ++ atomsOf (o, op) := by
  simp [DocModel.atoms, s.ops, List.flatMap_append]

theorem Snoc.anchorList_eq (s : Snoc H0 H1 o op) :
    anchorList H1 = anchorList H0 ++ (atomsOf (o, op)).map fun a => .node a.1 := by
  simp [anchorList, s.atoms_eq]

theorem Snoc.anchor_mono (s : Snoc H0 H1 o op) {a : Anchor} (h : a ∈ anchorList H0) : a ∈ anchorList H1 := by
  rw [s.anchorList_eq]; exact List.mem_append_left _ h

theorem Snoc.aIdx_eq (s : Snoc H0 H1 o op) {a : Anchor} (h : a ∈ anchorList H0) : aIdx H1 a = aIdx H0 a := by
  unfold DocRep.aIdx
  rw [idx_of_mem (s.anchor_mono h), idx_of_mem h, s.anchorList_eq, List.idxOf_append_of_mem h]

theorem Snoc.facts_eq (s : Snoc H0 H1 o op) :
    DocModel.facts H1 = DocModel.facts H0 ++ (DocModel.edgesOf (o, op)).map fun e => (e.1, e.2, o) := by
  simp [DocModel.facts, s.ops, List.flatMap_append]

theorem Snoc.filterMap {β} (s : Snoc H0 H1 o op) (g : OpId × Op → Option β) :
    (DocModel.ops H1).filterMap g = (DocModel.ops H0).filterMap g ++ (g (o, op)).toList := by
  rw [s.ops, List.filterMap_append]
  congr 1

theorem Snoc.deletionList_eq (s : Snoc H0 H1 o op) :
    deletionList H1 = deletionList H0 ++ (match op with | .delete _ => [o] | _ => []) := by
  unfold Document.deletionList
  rw [s.filterMap]
  cases op <;> rfl

theorem Snoc.insertionList_eq (s : Snoc H0 H1 o op) :
    insertionList H1 = insertionList H0 ++ (match op with | .insert .. => [o] | _ => []) := by
  unfold Document.insertionList
  rw [s.filterMap]
  cases op <;> rfl

theorem Snoc.orderList_eq (s : Snoc H0 H1 o op) :
    orderList H1 = orderList H0 ++ (match op with | .order .. => [o] | _ => []) := by
  unfold Document.orderList
  rw [s.filterMap]
  cases op <;> rfl

theorem idx_append_mem {α} [DecidableEq α] {l m : List α} {x : α} (h : x ∈ l) : idx (l ++ m) x = idx l x := by
  rw [idx_of_mem (List.mem_append_left _ h), idx_of_mem h, List.idxOf_append_of_mem h]

/-- The patch index of the new operation's patch. -/
theorem Snoc.idx_ids (s : Snoc H0 H1 o op) (q : PId) : idx (DocModel.ids H1) q = idx (DocModel.ids H0) q := by
  rw [s.ids]

/-- Facts carry the ids of operations of the history. -/
theorem facts_key (H : Hist) (f : Anchor × Anchor × OpId) (h : f ∈ DocModel.facts H) :
    f.2.2 ∈ (DocModel.ops H).map Prod.fst := by
  unfold DocModel.facts at h
  obtain ⟨oo, hoo, hm⟩ := List.mem_flatMap.mp h
  obtain ⟨e, -, rfl⟩ := List.mem_map.mp hm
  exact List.mem_map.mpr ⟨oo, hoo, rfl⟩

/-- Old order operations keep their encoded edge. -/
theorem Snoc.orderEdge_eq (s : Snoc H0 H1 o op) (r : OpId) (b a : NodeId)
    (hr : (r, Op.order b a) ∈ DocModel.ops H0) : orderEdge H1 r = orderEdge H0 r := by
  unfold DocRep.orderEdge
  rw [s.ops, List.find?_append]
  generalize hf : List.find? _ (DocModel.ops H0) = res
  rcases res with _ | ⟨r', op'⟩
  · have := List.find?_eq_none.mp hf _ hr
    simp at this
  · simp only [Option.some_or]
    cases op' with
    | order b' a' =>
      have he : r' = r := by simpa using List.find?_some hf
      subst he
      have hm : ((Anchor.node b', Anchor.node a', r') : Anchor × Anchor × OpId) ∈ DocModel.facts H0 := by
        have hmem := List.mem_of_find?_eq_some hf
        unfold DocModel.facts
        exact List.mem_flatMap.mpr ⟨(r', .order b' a'), hmem, by simp [DocModel.edgesOf]⟩
      simp only [s.facts_eq, List.idxOf_append_of_mem hm]
    | _ => rfl

end

/-! ## The encoding after one operation -/

theorem map_set_idxOf {α β} [DecidableEq α] (L : List α) (hn : L.Nodup) (d : α) (hd : d ∈ L)
    (f : α → β) (b : β) :
    (L.map f).set (L.idxOf d) b = L.map fun x => if x = d then b else f x := by
  apply List.ext_getElem
  · simp
  · intro i h1 h2
    simp only [List.getElem_set, List.getElem_map]
    by_cases hi : L.idxOf d = i
    · subst hi
      rw [if_pos rfl, if_pos (List.getElem_idxOf (List.idxOf_lt_length_iff.mpr hd))]
    · rw [if_neg hi, if_neg]
      intro he
      apply hi
      rw [← he]
      exact List.Nodup.idxOf_getElem hn i (by simpa using h2)

/-- Lists of operation ids drawn from distinct keys are distinct. -/
theorem filterMap_keys_nodup (g : OpId × Op → Option OpId) (hg : ∀ x b, g x = some b → b = x.1) :
    ∀ (l : List (OpId × Op)), (l.map Prod.fst).Nodup → (l.filterMap g).Nodup
  | [], _ => by simp
  | x :: t, h => by
    rw [List.map_cons, List.nodup_cons] at h
    rw [List.filterMap_cons]
    rcases hx : g x with _ | b
    · exact filterMap_keys_nodup g hg t h.2
    · refine List.nodup_cons.mpr ⟨?_, filterMap_keys_nodup g hg t h.2⟩
      intro hb
      obtain ⟨y, hy, hgy⟩ := List.mem_filterMap.mp hb
      have e1 := hg x b hx
      have e2 := hg y b hgy
      exact h.1 (List.mem_map.mpr ⟨y, hy, by rw [← e2, e1]⟩)

theorem deletionList_nodup (H : Hist) (hk : ((DocModel.ops H).map Prod.fst).Nodup) : (deletionList H).Nodup :=
  filterMap_keys_nodup _ (fun x b h => by obtain ⟨o, op⟩ := x; cases op <;> simp_all) _ hk

theorem deletionList_key (H : Hist) (x : OpId) (h : x ∈ deletionList H) : x ∈ (DocModel.ops H).map Prod.fst := by
  unfold Document.deletionList at h
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp h
  cases op <;> simp at hf
  subst hf
  exact List.mem_map.mpr ⟨_, hm, rfl⟩

section
variable {H0 H1 : Hist} {o : OpId} {op : Op}

theorem Snoc.deps_enc (s : Snoc H0 H1 o op) :
    H1.map (fun P => P.deps.map (idx (DocModel.ids H1))) = H0.map (fun P => P.deps.map (idx (DocModel.ids H0))) := by
  have e := congrArg (List.map (List.map (idx (DocModel.ids H0)))) s.deps
  simp only [List.map_map] at e
  rw [s.ids]
  exact e

theorem Snoc.encEdges_eq (g : Good H0) (s : Snoc H0 H1 o op) :
    encEdges H1 = encEdges H0 ++ (DocModel.edgesOf (o, op)).map
      (fun e => (aIdx H1 e.1, aIdx H1 e.2, idx (DocModel.ids H0) o.patch)) := by
  unfold DocRep.encEdges
  rw [s.facts_eq, List.map_append, List.cons_append]
  have hA : (DocModel.facts H0).map (fun f => (aIdx H1 f.1, aIdx H1 f.2.1, idx (DocModel.ids H1) f.2.2.patch)) =
      (DocModel.facts H0).map (fun f => (aIdx H0 f.1, aIdx H0 f.2.1, idx (DocModel.ids H0) f.2.2.patch)) := by
    apply List.map_congr_left
    intro f hf
    have := g.facts f hf
    rw [s.aIdx_eq this.1, s.aIdx_eq this.2, s.ids]
  rw [hA]
  simp only [List.map_map, Function.comp_def, s.ids]

theorem Snoc.insertionsOf_eq (g : Good H0) (s : Snoc H0 H1 o op) :
    insertionsOf H1 = insertionsOf H0 ++
      (match op with | .insert l r _ => [(idx (DocModel.ids H0) o.patch, aIdx H1 l, aIdx H1 r)] | _ => []) := by
  unfold DocRep.insertionsOf
  rw [s.filterMap]
  congr 1
  · apply List.filterMap_congr
    intro x hx
    obtain ⟨o', op'⟩ := x
    cases op' with
    | insert l r xs =>
      have := g.insertions o' l r xs hx
      simp only [s.ids, s.aIdx_eq this.1, s.aIdx_eq this.2]
    | _ => rfl
  · cases op <;> simp [s.ids]

theorem Snoc.deletionsOf_eq (g : Good H0) (s : Snoc H0 H1 o op) :
    deletionsOf H1 = deletionsOf H0 ++
      (match op with | .delete ts => [(idx (DocModel.ids H0) o.patch, ts.map fun n => aIdx H1 (.node n))] | _ => []) := by
  unfold DocRep.deletionsOf
  rw [s.filterMap]
  congr 1
  · apply List.filterMap_congr
    intro x hx
    obtain ⟨o', op'⟩ := x
    cases op' with
    | delete ts =>
      have := g.deletions o' ts hx
      simp only [s.ids, Option.some.injEq, Prod.mk.injEq, true_and]
      exact List.map_congr_left fun t ht => s.aIdx_eq (this t ht)
    | _ => rfl
  · cases op <;> simp [s.ids]

theorem Snoc.restored_iff (s : Snoc H0 H1 o op) (x : OpId) :
    Restored H1 x ↔ Restored H0 x ∨ op = .restore x := by
  unfold Restored
  rw [s.ops]
  simp only [List.mem_append, List.mem_singleton, Prod.mk.injEq]
  constructor
  · rintro ⟨o', h | ⟨-, h⟩⟩
    · exact Or.inl ⟨o', h⟩
    · exact Or.inr h.symm
  · rintro (⟨o', h⟩ | h)
    · exact ⟨o', Or.inl h⟩
    · exact ⟨o, Or.inr ⟨rfl, h.symm⟩⟩

theorem Snoc.acked_iff (s : Snoc H0 H1 o op) (d i : OpId) :
    Acked H1 d i ↔ Acked H0 d i ∨ op = .keep d i := by
  unfold Acked
  rw [s.ops]
  simp only [List.mem_append, List.mem_singleton, Prod.mk.injEq]
  constructor
  · rintro ⟨o', h | ⟨-, h⟩⟩
    · exact Or.inl ⟨o', h⟩
    · exact Or.inr h.symm
  · rintro (⟨o', h⟩ | h)
    · exact ⟨o', Or.inl h⟩
    · exact ⟨o, Or.inr ⟨rfl, h.symm⟩⟩

/-- Old order operations keep their edge; old retractions keep their entry. -/
theorem Snoc.retractionsOf_eq (g : Good H0) (s : Snoc H0 H1 o op) :
    retractionsOf H1 = retractionsOf H0 ++
      (match op with | .retract r => [(orderEdge H1 r, idx (DocModel.ids H0) o.patch)] | _ => []) := by
  unfold DocRep.retractionsOf
  rw [s.filterMap]
  congr 1
  · apply List.filterMap_congr
    intro x hx
    obtain ⟨o', op'⟩ := x
    cases op' with
    | retract r =>
      have hm := g.retracts o' r hx
      obtain ⟨b, a, hba⟩ := (isOrder_iff_mem H0 r).mpr hm
      simp only [s.ids, s.orderEdge_eq r b a hba]
    | _ => rfl
  · cases op <;> simp [s.ids]

theorem Snoc.orders_old (s : Snoc H0 H1 o op) :
    (orderList H0).map (orderEdge H1) = (orderList H0).map (orderEdge H0) := by
  apply List.map_congr_left
  intro r hr
  obtain ⟨b, a, hba⟩ := (isOrder_iff_mem H0 r).mpr hr
  exact s.orderEdge_eq r b a hba

theorem Snoc.orderEdge_new {b a : NodeId} (s : Snoc H0 H1 o (.order b a)) :
    orderEdge H1 o = 1 + (DocModel.facts H0).length := by
  unfold DocRep.orderEdge
  rw [s.ops, List.find?_append]
  have hn : (DocModel.ops H0).find? (fun x => decide (x.1 = o)) = none := by
    rw [List.find?_eq_none]
    intro x hx
    simpa using s.fresh x hx
  rw [hn]
  simp only [Option.none_or, List.find?_cons, decide_true, List.find?_nil]
  have hnot : ((Anchor.node b, Anchor.node a, o) : Anchor × Anchor × OpId) ∉ DocModel.facts H0 := by
    intro hm
    obtain ⟨x, hx, he⟩ := List.mem_map.mp (facts_key H0 _ hm)
    exact s.fresh x hx he
  rw [s.facts_eq, List.idxOf_append_of_notMem hnot]
  simp [DocModel.edgesOf]

end

theorem forall₂_map_eq {α β γ} {R : α → β → Prop} {f : β → γ} {g : α → γ} :
    ∀ {ts : List α} {rs : List β}, List.Forall₂ R ts rs → (∀ t ∈ ts, ∀ r, R t r → f r = g t) →
      rs.map f = ts.map g
  | _, _, .nil, _ => rfl
  | t :: _, r :: _, .cons h hs, hf => by
    simp only [List.map_cons, List.cons.injEq]
    exact ⟨hf t (List.mem_cons_self ..) r h,
      forall₂_map_eq hs (fun t' ht' r' h' => hf t' (List.mem_cons_of_mem _ ht') r' h')⟩

theorem rep_op (D : DocKernel.DocL) (H0 H1 : Hist) (o : OpId) (op : Op) (op' : document.Op)
    (hr : RepL D H0) (g : Good H0) (s : Snoc H0 H1 o op) (hok : OpOK H0 op) (htr : TrOp H0 op op') :
    RepL (DocKernel.commitOpL D (idx (DocModel.ids H0) o.patch) op') H1 := by
  classical
  have hdeps : D.deps = H1.map (fun P => P.deps.map (idx (DocModel.ids H1))) := by rw [s.deps_enc]; exact hr.deps
  have hdn := deletionList_nodup H0 g.keys
  -- the parts of the encoding an operation of another kind leaves alone
  have atoms_same : (∀ l r xs, op ≠ .insert l r xs) → DocModel.atoms H1 = DocModel.atoms H0 := by
    intro h; rw [s.atoms_eq]; cases op <;> simp_all [atomsOf]
  have edges_same : (∀ l r xs, op ≠ .insert l r xs) → (∀ b a, op ≠ .order b a) → encEdges H1 = encEdges H0 := by
    intro h1 h2; rw [s.encEdges_eq g]; cases op <;> simp_all [DocModel.edgesOf]
  have ins_same : (∀ l r xs, op ≠ .insert l r xs) → insertionsOf H1 = insertionsOf H0 := by
    intro h; rw [s.insertionsOf_eq g]; cases op <;> simp_all
  have del_same : (∀ ts, op ≠ .delete ts) → deletionsOf H1 = deletionsOf H0 := by
    intro h; rw [s.deletionsOf_eq g]; cases op <;> simp_all
  have dl_same : (∀ ts, op ≠ .delete ts) → deletionList H1 = deletionList H0 := by
    intro h; rw [s.deletionList_eq]; cases op <;> simp_all
  have il_same : (∀ l r xs, op ≠ .insert l r xs) → insertionList H1 = insertionList H0 := by
    intro h; rw [s.insertionList_eq]; cases op <;> simp_all
  have ol_same : (∀ b a, op ≠ .order b a) → orderList H1 = orderList H0 := by
    intro h; rw [s.orderList_eq]; cases op <;> simp_all
  have ret_same : (∀ r, op ≠ .retract r) → retractionsOf H1 = retractionsOf H0 := by
    intro h; rw [s.retractionsOf_eq g]; cases op <;> simp_all
  have rest_same : (∀ ts, op ≠ .delete ts) → (∀ d, op ≠ .restore d) →
      (deletionList H1).map (fun x => decide (Restored H1 x)) = (deletionList H0).map (fun x => decide (Restored H0 x)) := by
    intro h1 h2
    rw [dl_same h1]
    apply List.map_congr_left
    intro x _
    rw [s.restored_iff]
    simp [h2 x]
  -- acknowledgements of old pairs keep their indices
  have acked_old : ∀ a b, (∃ dd ii, Acked H0 dd ii ∧ a = idx (deletionList H1) dd ∧ b = idx (insertionList H1) ii) ↔
      (∃ dd ii, Acked H0 dd ii ∧ a = idx (deletionList H0) dd ∧ b = idx (insertionList H0) ii) := by
    intro a b
    have key : ∀ dd ii, Acked H0 dd ii →
        idx (deletionList H1) dd = idx (deletionList H0) dd ∧ idx (insertionList H1) ii = idx (insertionList H0) ii := by
      rintro dd ii ⟨o', ho'⟩
      have := g.keeps o' dd ii ho'
      exact ⟨by rw [s.deletionList_eq, idx_append_mem this.1], by rw [s.insertionList_eq, idx_append_mem this.2]⟩
    constructor
    · rintro ⟨dd, ii, h, rfl, rfl⟩
      exact ⟨dd, ii, h, (key dd ii h).1, (key dd ii h).2⟩
    · rintro ⟨dd, ii, h, rfl, rfl⟩
      exact ⟨dd, ii, h, (key dd ii h).1.symm, (key dd ii h).2.symm⟩
  have acked_same : (∀ d i, op ≠ .keep d i) → ∀ a b, (a, b) ∈ D.acked ↔
      ∃ dd ii, Acked H1 dd ii ∧ a = idx (deletionList H1) dd ∧ b = idx (insertionList H1) ii := by
    intro h a b
    rw [hr.acked]
    have : ∀ dd ii, Acked H1 dd ii ↔ Acked H0 dd ii := fun dd ii => by rw [s.acked_iff]; simp [h dd ii]
    simp only [this]
    exact (acked_old a b).symm
  have orders_same : (∀ b a, op ≠ .order b a) → D.orders = (orderList H1).map (orderEdge H1) := by
    intro h; rw [ol_same h, s.orders_old]; exact hr.orders
  cases op <;> cases op' <;> simp only [TrOp] at htr
  case insert.Insert l r xs l' r' xs' =>
    obtain ⟨hl, hr'⟩ := hok
    have il : DocKernel.anchorIdx l' = aIdx H1 l := by rw [anchorIdx_tr H0 l l' htr.1, s.aIdx_eq hl]
    have ir : DocKernel.anchorIdx r' = aIdx H1 r := by rw [anchorIdx_tr H0 r r' htr.2.1, s.aIdx_eq hr']
    have hlen : xs'.val.length = xs.length := by
      have := congrArg List.length htr.2.2; simpa using this
    have hal : D.atoms.length = (anchorList H0).length := by
      rw [hr.atoms]; simp [anchorList]
    have hnew : ∀ j < xs.length, aIdx H1 (.node ⟨o, j⟩) = (anchorList H0).length + j := by
      intro j hj
      have hnot : Anchor.node ⟨o, j⟩ ∉ anchorList H0 := by
        intro hm
        rw [mem_anchorList_node] at hm
        unfold DocModel.IsAtom DocModel.atom at hm
        rw [Ne, lookup_none] at hm
        push_neg at hm
        obtain ⟨b, hb⟩ := hm
        unfold DocModel.atoms at hb
        obtain ⟨⟨o', op'⟩, hm', hb⟩ := List.mem_flatMap.mp hb
        cases op' with
        | insert _ _ ys =>
          simp only [atomsOf, List.mem_map] at hb
          obtain ⟨⟨c, j'⟩, -, he⟩ := hb
          simp only [Prod.mk.injEq, NodeId.mk.injEq] at he
          exact s.fresh _ hm' he.1.1
        | _ => simp [atomsOf] at hb
      have hl2 : ((atomsOf (o, .insert l r xs)).map fun a => Anchor.node a.1) =
          (List.range xs.length).map fun i => Anchor.node ⟨o, i⟩ := by
        apply List.ext_getElem
        · simp [atomsOf]
        · intro i h1 h2; simp [atomsOf]
      have hmid : Anchor.node ⟨o, j⟩ ∈ (List.range xs.length).map fun i => Anchor.node ⟨o, i⟩ :=
        List.mem_map.mpr ⟨j, List.mem_range.mpr hj, rfl⟩
      have hm1 : Anchor.node ⟨o, j⟩ ∈ anchorList H1 := by
        rw [s.anchorList_eq, hl2]; exact List.mem_append_right _ hmid
      have hn : ((List.range xs.length).map fun i => Anchor.node ⟨o, i⟩).Nodup :=
        List.Nodup.map (fun a b h => by simpa using h) List.nodup_range
      unfold aIdx
      rw [idx_of_mem hm1, s.anchorList_eq, List.idxOf_append_of_notMem hnot, hl2]
      have := List.Nodup.idxOf_getElem hn j (by simpa using hj)
      simp only [List.getElem_map, List.getElem_range] at this
      rw [this]
    have hn2 : ((List.range xs.length).map fun j => Anchor.node ⟨o, j⟩).map (aIdx H1) =
        (List.range xs.length).map (fun j => (anchorList H0).length + j) := by
      rw [List.map_map]
      exact List.map_congr_left (fun j hj => hnew j (List.mem_range.mp hj))
    refine ⟨hdeps, ?_, ?_, ?_, ?_, hr.deletions.trans (del_same (by simp)).symm,
      hr.restored.trans (rest_same (by simp) (by simp)).symm, acked_same (by simp),
      orders_same (by simp), hr.retractions.trans (ret_same (by simp)).symm⟩
    · show D.atoms ++ _ = _
      rw [hr.atoms, s.atoms_eq, List.map_append, htr.2.2]
      simp only [List.cons_append, List.cons.injEq, List.append_cancel_left_eq, true_and]
      apply List.ext_getElem <;> simp [atomsOf]
    · show D.creator ++ _ = _
      rw [hr.creator, s.atoms_eq, List.map_append, hlen, s.ids]
      simp only [List.cons_append, List.cons.injEq, List.append_cancel_left_eq, true_and]
      apply List.ext_getElem <;> simp [atomsOf]
    · show D.edges ++ _ = _
      rw [hr.edges, s.encEdges_eq g, chainM_closed, il, ir, hal, hlen]
      congr 1
      rw [show (DocModel.edgesOf (o, .insert l r xs)).map
            (fun e => (aIdx H1 e.1, aIdx H1 e.2, idx (DocModel.ids H0) o.patch)) =
          ((DocModel.edgesOf (o, .insert l r xs)).map (fun e => (aIdx H1 e.1, aIdx H1 e.2))).map
            (fun e => (e.1, e.2, idx (DocModel.ids H0) o.patch)) by rw [List.map_map]; rfl]
      unfold DocModel.edgesOf
      dsimp only
      rw [map_zip_pair, List.map_cons, List.map_append, hn2, List.map_singleton]
    · show D.insertions ++ _ = _
      rw [hr.insertions, s.insertionsOf_eq g, il, ir]
  case delete.Delete ts rs =>
    refine ⟨hdeps, hr.atoms.trans (by rw [atoms_same (by simp)]), hr.creator.trans (by rw [atoms_same (by simp), s.ids]),
      hr.edges.trans (edges_same (by simp) (by simp)).symm,
      hr.insertions.trans (ins_same (by simp)).symm, ?_, ?_, acked_same (by simp),
      orders_same (by simp), hr.retractions.trans (ret_same (by simp)).symm⟩
    · show D.deletions ++ _ = _
      rw [hr.deletions, s.deletionsOf_eq g]
      simp only [List.append_cancel_left_eq, List.cons.injEq, and_true, Prod.mk.injEq, true_and]
      exact forall₂_map_eq htr (fun t ht r hr' => by
        rw [hr'.2]; exact (s.aIdx_eq (hok t ht)).symm)
    · show D.restored ++ _ = _
      rw [hr.restored, s.deletionList_eq]
      simp only [List.map_append, List.map_singleton]
      congr 1
      · apply List.map_congr_left
        intro x _
        rw [s.restored_iff]
        simp
      · have : ¬ Restored H1 o := by
          rw [s.restored_iff]
          rintro (⟨o', ho'⟩ | h)
          · have hm := deletionList_key H0 o (g.restores o' o ho')
            obtain ⟨x, hx, he⟩ := List.mem_map.mp hm
            exact s.fresh x hx he
          · cases h
        simp [this]
  case restore.Restore d r =>
    refine ⟨hdeps, hr.atoms.trans (by rw [atoms_same (by simp)]), hr.creator.trans (by rw [atoms_same (by simp), s.ids]),
      hr.edges.trans (edges_same (by simp) (by simp)).symm,
      hr.insertions.trans (ins_same (by simp)).symm, hr.deletions.trans (del_same (by simp)).symm, ?_,
      acked_same (by simp), orders_same (by simp), hr.retractions.trans (ret_same (by simp)).symm⟩
    show D.restored.set r.index.val true = _
    rw [hr.restored, dl_same (by simp), htr.2, idx_of_mem hok, map_set_idxOf _ hdn d hok]
    apply List.map_congr_left
    intro x _
    rw [s.restored_iff]
    by_cases hx : x = d
    · subst hx; simp
    · simp [hx, Ne.symm hx]
  case order.Order b a b' a' =>
    obtain ⟨hb, ha⟩ := hok
    refine ⟨hdeps, hr.atoms.trans (by rw [atoms_same (by simp)]), hr.creator.trans (by rw [atoms_same (by simp), s.ids]),
      ?_, hr.insertions.trans (ins_same (by simp)).symm, hr.deletions.trans (del_same (by simp)).symm,
      hr.restored.trans (rest_same (by simp) (by simp)).symm, acked_same (by simp), ?_,
      hr.retractions.trans (ret_same (by simp)).symm⟩
    · show D.edges ++ _ = _
      rw [hr.edges, s.encEdges_eq g]
      simp only [DocModel.edgesOf, List.map_cons, List.map_nil, List.append_cancel_left_eq, List.cons.injEq,
        Prod.mk.injEq, and_true]
      exact ⟨by rw [htr.1.2, s.aIdx_eq hb]; rfl, by rw [htr.2.2, s.aIdx_eq ha]; rfl⟩
    · show D.orders ++ _ = _
      rw [hr.orders, s.orderList_eq, List.map_append, ← s.orders_old]
      simp only [List.map_cons, List.map_nil, List.append_cancel_left_eq, List.cons.injEq, and_true]
      rw [s.orderEdge_new, hr.edges]
      simp [encEdges]
      omega
  case keep.Keep d i rd ri =>
    have hsame : DocKernel.commitOpL D (idx (DocModel.ids H0) o.patch) (.Keep rd ri) =
        { D with acked := (DocKernel.commitOpL D (idx (DocModel.ids H0) o.patch) (.Keep rd ri)).acked } := by
      simp only [DocKernel.commitOpL]; split_ifs <;> rfl
    have hacc : ∀ a b, (a, b) ∈ (DocKernel.commitOpL D (idx (DocModel.ids H0) o.patch) (.Keep rd ri)).acked ↔
        (a, b) ∈ D.acked ∨ (a = rd.index.val ∧ b = ri.index.val) := by
      intro a b
      simp only [DocKernel.commitOpL]
      split_ifs with hm
      · constructor
        · exact Or.inl
        · rintro (h | ⟨rfl, rfl⟩)
          · exact h
          · exact hm
      · simp
    rw [hsame]
    refine ⟨hdeps, hr.atoms.trans (by rw [atoms_same (by simp)]), hr.creator.trans (by rw [atoms_same (by simp), s.ids]),
      hr.edges.trans (edges_same (by simp) (by simp)).symm,
      hr.insertions.trans (ins_same (by simp)).symm, hr.deletions.trans (del_same (by simp)).symm,
      hr.restored.trans (rest_same (by simp) (by simp)).symm, ?_, orders_same (by simp),
      hr.retractions.trans (ret_same (by simp)).symm⟩
    intro a b
    show (a, b) ∈ (DocKernel.commitOpL D (idx (DocModel.ids H0) o.patch) (.Keep rd ri)).acked ↔ _
    rw [hacc, hr.acked, dl_same (by simp), il_same (by simp)]
    simp only [s.acked_iff, Op.keep.injEq]
    rw [htr.1.2, htr.2.2, idx_of_mem hok.1, idx_of_mem hok.2]
    constructor
    · rintro (⟨dd, ii, h, rfl, rfl⟩ | ⟨rfl, rfl⟩)
      · exact ⟨dd, ii, Or.inl h, rfl, rfl⟩
      · exact ⟨d, i, Or.inr ⟨rfl, rfl⟩, (idx_of_mem hok.1).symm ▸ rfl, (idx_of_mem hok.2).symm ▸ rfl⟩
    · rintro ⟨dd, ii, h | ⟨rfl, rfl⟩, rfl, rfl⟩
      · exact Or.inl ⟨dd, ii, h, rfl, rfl⟩
      · exact Or.inr ⟨idx_of_mem hok.1, idx_of_mem hok.2⟩
  case retract.Retract r0 r =>
    refine ⟨hdeps, hr.atoms.trans (by rw [atoms_same (by simp)]), hr.creator.trans (by rw [atoms_same (by simp), s.ids]),
      hr.edges.trans (edges_same (by simp) (by simp)).symm,
      hr.insertions.trans (ins_same (by simp)).symm, hr.deletions.trans (del_same (by simp)).symm,
      hr.restored.trans (rest_same (by simp) (by simp)).symm, acked_same (by simp), orders_same (by simp), ?_⟩
    show D.retractions ++ _ = _
    rw [hr.retractions, s.retractionsOf_eq g]
    simp only [List.append_cancel_left_eq, List.cons.injEq, and_true, Prod.mk.injEq]
    obtain ⟨b, a, hba⟩ := (isOrder_iff_mem H0 r0).mpr hok
    rw [s.orderEdge_eq r0 b a hba]
    have hi : r.index.val < D.orders.length := by
      rw [hr.orders, List.length_map, htr.2]; exact idx_lt hok
    rw [getElem!_pos D.orders r.index.val hi]
    simp only [hr.orders, List.getElem_map, htr.2]
    rw [getElem_idx hok]

/-! ## Validated operations refer to interned things -/

theorem validOp_ok (H : Hist) (self : PId) (deps : List PId) (own own' : Own) (k : Nat) (op : Op)
    (h : validOp H self deps own k op = .ok own') : OpOK H op := by
  cases op with
  | insert l r xs =>
    unfold validOp at h
    rcases hl : checkAnchor H deps l with _ | e <;> rcases hr : checkAnchor H deps r with _ | e' <;>
      simp only [hl, hr] at h
    · split_ifs at h
      exact ⟨checkAnchor_none_mem H deps l hl, checkAnchor_none_mem H deps r hr⟩
    all_goals cases h
  | delete ts =>
    unfold validOp at h
    simp only at h
    split_ifs at h with h0
    all_goals try (cases h; done)
    generalize hf : List.findSome? _ ts.zipIdx = res at h
    cases res with
    | some e => cases h
    | none =>
      intro t ht
      obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem ht
      have hm : (ts[j], j) ∈ ts.zipIdx := List.mem_zipIdx_iff_getElem?.mpr (by simp [hj])
      have := List.findSome?_eq_none_iff.mp hf _ hm
      simp only at this
      cases hc : checkAnchor H deps (.node ts[j]) with
      | none => exact checkAnchor_none_mem H deps _ hc
      | some e => simp [hc] at this
  | restore d =>
    unfold validOp at h
    simp only at h
    split_ifs at h with h1 h2
    exact (tag_ne_none_iff H d).mp h2
  | order b a =>
    unfold validOp at h
    rcases hl : checkAnchor H deps (.node b) with _ | e <;> rcases hr : checkAnchor H deps (.node a) with _ | e' <;>
      simp only [hl, hr] at h
    · split_ifs at h
      exact ⟨checkAnchor_none_mem H deps _ hl, checkAnchor_none_mem H deps _ hr⟩
    all_goals cases h
  | keep d i =>
    unfold validOp at h
    simp only at h
    split_ifs at h with h1 h2
    rcases ht : tag H d with _ | ts <;> rcases hi : insertionOf H i with _ | ⟨l, r⟩ <;> simp only [ht, hi] at h
    all_goals first
      | cases h
      | exact ⟨(tag_ne_none_iff H d).mp (by rw [ht]; simp),
          (insertionOf_ne_none_iff H i).mp (by rw [hi]; simp)⟩
      | (split_ifs at h; exact ⟨(tag_ne_none_iff H d).mp (by rw [ht]; simp),
          (insertionOf_ne_none_iff H i).mp (by rw [hi]; simp)⟩)
  | retract r =>
    unfold validOp at h
    simp only at h
    split_ifs at h with h1 h2
    exact (isOrder_iff_mem H r).mp ((by first | exact not_not.mp h2 | exact h2))

/-! ## `Good` along the commit -/

theorem Good.snoc {H0 H1 : Hist} {o : OpId} {op : Op} (g : Good H0) (s : Snoc H0 H1 o op)
    (hok : OpOK H0 op) : Good H1 := by
  have am := fun {a} (h : a ∈ anchorList H0) => s.anchor_mono h
  have dm : ∀ {x}, x ∈ deletionList H0 → x ∈ deletionList H1 := fun h => by
    rw [s.deletionList_eq]; exact List.mem_append_left _ h
  have im : ∀ {x}, x ∈ insertionList H0 → x ∈ insertionList H1 := fun h => by
    rw [s.insertionList_eq]; exact List.mem_append_left _ h
  have om : ∀ {x}, x ∈ orderList H0 → x ∈ orderList H1 := fun h => by
    rw [s.orderList_eq]; exact List.mem_append_left _ h
  have mem_ops : ∀ {x}, x ∈ DocModel.ops H1 → x ∈ DocModel.ops H0 ∨ x = (o, op) := fun h => by
    rw [s.ops, List.mem_append, List.mem_singleton] at h; exact h
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro f hf
    rw [s.facts_eq, List.mem_append] at hf
    rcases hf with hf | hf
    · exact ⟨am (g.facts f hf).1, am (g.facts f hf).2⟩
    · obtain ⟨e, he, rfl⟩ := List.mem_map.mp hf
      cases op with
      | insert l r xs =>
        obtain ⟨hl, hr⟩ := hok
        have hnode : ∀ j < xs.length, Anchor.node ⟨o, j⟩ ∈ anchorList H1 := by
          intro j hj
          rw [s.anchorList_eq]
          apply List.mem_append_right
          simp only [atomsOf, List.mem_map]
          exact ⟨(⟨o, j⟩, xs[j]), ⟨(xs[j], j), List.mem_zipIdx_iff_getElem?.mpr (by simp [hj]), rfl⟩, rfl⟩
        unfold DocModel.edgesOf at he
        dsimp only at he
        have hz := List.of_mem_zip he
        constructor
        · rcases List.mem_cons.mp hz.1 with h | h
          · rw [h]; exact am hl
          · obtain ⟨j, hj, hje⟩ := List.mem_map.mp h
            rw [← hje]; exact hnode j (List.mem_range.mp hj)
        · rcases List.mem_append.mp hz.2 with h | h
          · obtain ⟨j, hj, hje⟩ := List.mem_map.mp h
            rw [← hje]; exact hnode j (List.mem_range.mp hj)
          · rw [List.mem_singleton.mp h]; exact am hr
      | order b a =>
        simp only [DocModel.edgesOf, List.mem_singleton] at he
        subst he
        exact ⟨am hok.1, am hok.2⟩
      | _ => simp [DocModel.edgesOf] at he
  · intro o' l r xs h
    rcases mem_ops h with h | h
    · exact ⟨am (g.insertions o' l r xs h).1, am (g.insertions o' l r xs h).2⟩
    · simp only [Prod.mk.injEq] at h
      obtain ⟨-, rfl⟩ := h
      exact ⟨am hok.1, am hok.2⟩
  · intro o' ts h t ht
    rcases mem_ops h with h | h
    · exact am (g.deletions o' ts h t ht)
    · simp only [Prod.mk.injEq] at h
      obtain ⟨-, rfl⟩ := h
      exact am (hok t ht)
  · rw [s.ops, List.map_append, List.map_singleton]
    refine List.nodup_append.mpr ⟨g.keys, List.nodup_singleton _, ?_⟩
    intro a ha b hb
    rw [List.mem_singleton] at hb
    subst hb
    obtain ⟨x, hx, rfl⟩ := List.mem_map.mp ha
    exact s.fresh x hx
  · intro o' d h
    rcases mem_ops h with h | h
    · exact dm (g.restores o' d h)
    · simp only [Prod.mk.injEq] at h
      obtain ⟨-, rfl⟩ := h
      exact dm hok
  · intro o' d i h
    rcases mem_ops h with h | h
    · exact ⟨dm (g.keeps o' d i h).1, im (g.keeps o' d i h).2⟩
    · simp only [Prod.mk.injEq] at h
      obtain ⟨-, rfl⟩ := h
      exact ⟨dm hok.1, im hok.2⟩
  · intro o' r h
    rcases mem_ops h with h | h
    · exact om (g.retracts o' r h)
    · simp only [Prod.mk.injEq] at h
      obtain ⟨-, rfl⟩ := h
      exact om hok

/-! ## Extending a history keeps earlier interning -/

/-- `H0` extends `H`: more operations and more patches, appended. -/
def Ext (H H0 : Hist) : Prop :=
  (∃ X, DocModel.ops H0 = DocModel.ops H ++ X) ∧ ∃ Y, DocModel.ids H0 = DocModel.ids H ++ Y

theorem Ext.anchorList {H H0 : Hist} (e : Ext H H0) : ∃ Z, anchorList H0 = anchorList H ++ Z := by
  obtain ⟨⟨X, hX⟩, -⟩ := e
  exact ⟨(X.flatMap atomsOf).map fun a => .node a.1, by simp [Document.anchorList, DocModel.atoms, hX]⟩

theorem Ext.filterMap {β} {H H0 : Hist} (e : Ext H H0) (g : OpId × Op → Option β) :
    ∃ Z, (DocModel.ops H0).filterMap g = (DocModel.ops H).filterMap g ++ Z := by
  obtain ⟨⟨X, hX⟩, -⟩ := e
  exact ⟨X.filterMap g, by rw [hX, List.filterMap_append]⟩

theorem idx_ext {α} [DecidableEq α] {l m : List α} {x : α} (h : x ∈ l) (hm : ∃ Z, m = l ++ Z) :
    idx m x = idx l x := by
  obtain ⟨Z, rfl⟩ := hm
  exact idx_append_mem h

theorem insertionList_patch_mem (H : Hist) (x : OpId) (hx : x ∈ insertionList H) : x.patch ∈ DocModel.ids H := by
  unfold Document.insertionList at hx
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp hx
  cases op <;> simp at hf
  subst hf
  exact ops_patch_mem hm

theorem TrNode_ext {H H0 : Hist} (e : Ext H H0) (n : NodeId) (r : document.Ref)
    (hn : Anchor.node n ∈ anchorList H) (h : TrNode H n r) : TrNode H0 n r := by
  obtain ⟨h1, h2⟩ := h
  refine ⟨?_, ?_⟩
  · rw [h1, idx_ext (node_patch_mem H n hn) e.2]
  · rw [h2, idx_ext hn e.anchorList]

theorem TrAnchor_ext {H H0 : Hist} (e : Ext H H0) (a : Anchor) (a' : document.Anchor)
    (ha : a ∈ anchorList H) (h : TrAnchor H a a') : TrAnchor H0 a a' := by
  cases a <;> cases a' <;> simp only [TrAnchor] at h ⊢
  exact TrNode_ext e _ _ ha h

theorem forall₂_mono_mem {α β} {R S : α → β → Prop} :
    ∀ {l₁ : List α} {l₂ : List β}, List.Forall₂ R l₁ l₂ → (∀ a ∈ l₁, ∀ b, R a b → S a b) →
      List.Forall₂ S l₁ l₂
  | _, _, .nil, _ => .nil
  | _ :: _, _ :: _, .cons h hs, hf =>
    .cons (hf _ (List.mem_cons_self ..) _ h)
      (forall₂_mono_mem hs (fun a ha b hb => hf a (List.mem_cons_of_mem _ ha) b hb))

theorem TrOp_ext {H H0 : Hist} (e : Ext H H0) (op : Op) (op' : document.Op) (hok : OpOK H op)
    (h : TrOp H op op') : TrOp H0 op op' := by
  have dl : ∀ x ∈ deletionList H, idx (deletionList H0) x = idx (deletionList H) x := fun x hx =>
    idx_ext hx (e.filterMap _)
  have il : ∀ x ∈ insertionList H, idx (insertionList H0) x = idx (insertionList H) x := fun x hx =>
    idx_ext hx (e.filterMap _)
  have ol : ∀ x ∈ orderList H, idx (orderList H0) x = idx (orderList H) x := fun x hx =>
    idx_ext hx (e.filterMap _)
  have pi : ∀ q ∈ DocModel.ids H, idx (DocModel.ids H0) q = idx (DocModel.ids H) q := fun q hq => idx_ext hq e.2
  cases op <;> cases op' <;> simp only [TrOp, OpOK] at h hok ⊢
  case insert.Insert l r xs l' r' xs' =>
    exact ⟨TrAnchor_ext e l l' hok.1 h.1, TrAnchor_ext e r r' hok.2 h.2.1, h.2.2⟩
  case delete.Delete ts rs =>
    exact forall₂_mono_mem h (fun t ht r htr => TrNode_ext e t r (hok t ht) htr)
  case restore.Restore d r =>
    exact ⟨by rw [h.1, pi _ (deletionList_patch_mem H d hok)], by rw [h.2, dl d hok]⟩
  case order.Order b a b' a' =>
    exact ⟨TrNode_ext e b b' hok.1 h.1, TrNode_ext e a a' hok.2 h.2⟩
  case keep.Keep d i rd ri =>
    exact ⟨⟨by rw [h.1.1, pi _ (deletionList_patch_mem H d hok.1)], by rw [h.1.2, dl d hok.1]⟩,
      ⟨by rw [h.2.1, pi _ (insertionList_patch_mem H i hok.2)], by rw [h.2.2, il i hok.2]⟩⟩
  case retract.Retract o r =>
    exact ⟨by rw [h.1, pi _ (orderList_patch_mem H o hok)], by rw [h.2, ol o hok]⟩

theorem OpOK_ext {H H0 : Hist} (e : Ext H H0) (op : Op) (h : OpOK H op) : OpOK H0 op := by
  have am : ∀ {a}, a ∈ anchorList H → a ∈ anchorList H0 := fun ha => by
    obtain ⟨Z, hZ⟩ := e.anchorList; rw [hZ]; exact List.mem_append_left _ ha
  have fm : ∀ (g : OpId × Op → Option OpId) {x}, x ∈ (DocModel.ops H).filterMap g →
      x ∈ (DocModel.ops H0).filterMap g := by
    intro g x hx
    obtain ⟨Z, hZ⟩ := e.filterMap g; rw [hZ]; exact List.mem_append_left _ hx
  cases op <;> simp only [OpOK] at h ⊢
  · exact ⟨am h.1, am h.2⟩
  · exact fun t ht => am (h t ht)
  · exact fm _ h
  · exact ⟨am h.1, am h.2⟩
  · exact ⟨fm _ h.1, fm _ h.2⟩
  · exact fm _ h

/-! ## The patch's prefixes -/

theorem opsOf_prefix (P : Patch) (j : Nat) : opsOf (prefixPatch P j) = (opsOf P).take j := by
  unfold opsOf prefixPatch
  apply List.ext_getElem
  · simp
  · intro i h1 h2
    simp

theorem ids_prefix (H : Hist) (P : Patch) (j : Nat) : ids (H ++ [prefixPatch P j]) = DocModel.ids H ++ [P.id] := by
  simp [ids, prefixPatch]

theorem ext_prefix (H : Hist) (P : Patch) (j : Nat) : Ext H (H ++ [prefixPatch P j]) :=
  ⟨⟨_, by rw [ops_append]⟩, ⟨_, ids_prefix H P j⟩⟩

theorem snoc_prefix (H : Hist) (P : Patch) (hP : P.id ∉ DocModel.ids H) (j : Nat) (hj : j < P.ops.length) :
    Snoc (H ++ [prefixPatch P j]) (H ++ [prefixPatch P (j + 1)]) ⟨P.id, j⟩ P.ops[j] where
  ops := by
    rw [ops_append, ops_append, opsOf_prefix, opsOf_prefix, List.take_succ, List.append_assoc]
    congr 2
    have : (opsOf P)[j]? = some (⟨P.id, j⟩, P.ops[j]) := by simp [opsOf, hj]
    rw [this]; rfl
  ids := by rw [ids_prefix, ids_prefix]
  deps := by simp [prefixPatch]
  fresh := by
    intro x hx he
    rw [ops_append, List.mem_append] at hx
    rcases hx with hx | hx
    · have := ops_patch_mem (op := x.2) hx
      rw [he] at this
      exact hP this
    · rw [opsOf_prefix] at hx
      obtain ⟨k, hk, hkx⟩ := List.getElem_of_mem hx
      have hk' : k < j := by simp at hk; omega
      have hkl : k < (opsOf P).length := by simp at hk ⊢; omega
      have e1 : ((opsOf P).take j)[k] = (opsOf P)[k]'hkl := List.getElem_take ..
      have e2 : ((opsOf P)[k]'hkl).1 = ⟨P.id, k⟩ := by simp [opsOf]
      have : x.1 = ⟨P.id, k⟩ := by rw [← hkx, e1, e2]
      rw [this] at he
      simp only [OpId.mk.injEq] at he
      omega
  patch := by rw [ids_prefix]; simp

theorem prefix_full (P : Patch) : prefixPatch P P.ops.length = P := by
  simp [prefixPatch]

/-! ## The patch before its operations -/

theorem rep_base (D : DocKernel.DocL) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : RepL D H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P) :
    RepL { D with deps := D.deps ++ [dp.deps.val.map (·.val)] } (H ++ [prefixPatch P 0]) := by
  classical
  obtain ⟨cl, -, -⟩ := replay_props hv (by simp [Closed]) (by simp [ids])
  have hops : DocModel.ops (H ++ [prefixPatch P 0]) = DocModel.ops H := by
    rw [ops_append, opsOf_prefix]; simp
  have hids := ids_prefix H P 0
  have pi : ∀ q ∈ DocModel.ids H, idx (ids (H ++ [prefixPatch P 0])) q = idx (DocModel.ids H) q := fun q hq => by
    rw [hids]; exact idx_append_mem hq
  have hat : DocModel.atoms (H ++ [prefixPatch P 0]) = DocModel.atoms H := by simp [DocModel.atoms, hops]
  have hA : anchorList (H ++ [prefixPatch P 0]) = anchorList H := by simp [Document.anchorList, hat]
  have hF : DocModel.facts (H ++ [prefixPatch P 0]) = DocModel.facts H := by simp [DocModel.facts, hops]
  have haI : aIdx (H ++ [prefixPatch P 0]) = aIdx H := by funext a; simp [aIdx, hA]
  have hDL : deletionList (H ++ [prefixPatch P 0]) = deletionList H := by simp [Document.deletionList, hops]
  have hIL : insertionList (H ++ [prefixPatch P 0]) = insertionList H := by simp [Document.insertionList, hops]
  have hOL : orderList (H ++ [prefixPatch P 0]) = orderList H := by simp [Document.orderList, hops]
  have hOE : orderEdge (H ++ [prefixPatch P 0]) = orderEdge H := by funext r; simp [orderEdge, hops, hF]
  have hRes : ∀ x, Restored (H ++ [prefixPatch P 0]) x ↔ Restored H x := fun x => by simp [Restored, hops]
  have hAck : ∀ d i, Acked (H ++ [prefixPatch P 0]) d i ↔ Acked H d i := fun d i => by simp [Acked, hops]
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · show D.deps ++ _ = _
    rw [hr.deps, List.map_append, List.map_singleton]
    congr 1
    · apply List.map_congr_left
      intro Q hQ
      apply List.map_congr_left
      intro q hq
      exact (pi q (cl Q hQ q hq)).symm
    · simp only [List.cons.injEq, and_true]
      rw [ht.1]
      apply List.map_congr_left
      intro q hq
      exact (pi q (adm.2.2.2.1 q hq)).symm
  · show D.atoms = _; rw [hr.atoms, hat]
  · show D.creator = _
    rw [hr.creator, hat]
    congr 2
    apply List.map_congr_left
    intro a ha
    refine (pi _ ?_).symm
    apply node_patch_mem H a.1
    simp only [Document.anchorList, List.cons_append, List.nil_append, List.mem_cons, reduceCtorEq,
      false_or, List.mem_map]
    exact ⟨a, ha, rfl⟩
  · show D.edges = _
    rw [hr.edges]
    unfold encEdges
    rw [hF, haI]
    congr 1
    apply List.map_congr_left
    intro f hf
    have := facts_key H f hf
    obtain ⟨x, hx, he⟩ := List.mem_map.mp this
    rw [pi _ (by rw [← he]; exact ops_patch_mem (op := x.2) hx)]
  · show D.insertions = _
    rw [hr.insertions]
    unfold insertionsOf
    rw [hops, haI]
    apply List.filterMap_congr
    intro x hx
    obtain ⟨o, op⟩ := x
    cases op <;> simp [pi _ (ops_patch_mem hx)]
  · show D.deletions = _
    rw [hr.deletions]
    unfold deletionsOf
    rw [hops, haI]
    apply List.filterMap_congr
    intro x hx
    obtain ⟨o, op⟩ := x
    cases op <;> simp [pi _ (ops_patch_mem hx)]
  · show D.restored = _
    rw [hr.restored, hDL]
    apply List.map_congr_left
    intro x _
    by_cases h : Restored H x
    · rw [decide_eq_true h, decide_eq_true ((hRes x).mpr h)]
    · rw [decide_eq_false h, decide_eq_false (fun h' => h ((hRes x).mp h'))]
  · intro a b
    show (a, b) ∈ D.acked ↔ _
    rw [hr.acked, hDL, hIL]
    simp only [hAck]
  · show D.orders = _; rw [hr.orders, hOL, hOE]
  · show D.retractions = _
    rw [hr.retractions]
    unfold retractionsOf
    rw [hops, hOE]
    apply List.filterMap_congr
    intro x hx
    obtain ⟨o, op⟩ := x
    cases op <;> simp [pi _ (ops_patch_mem hx)]

theorem good_base (H : Hist) (P : Patch) (g : Good H) : Good (H ++ [prefixPatch P 0]) := by
  have hops : DocModel.ops (H ++ [prefixPatch P 0]) = DocModel.ops H := by
    rw [ops_append, opsOf_prefix]; simp
  have hA : anchorList (H ++ [prefixPatch P 0]) = anchorList H := by
    simp [Document.anchorList, DocModel.atoms, hops]
  have hF : DocModel.facts (H ++ [prefixPatch P 0]) = DocModel.facts H := by simp [DocModel.facts, hops]
  have hDL : deletionList (H ++ [prefixPatch P 0]) = deletionList H := by simp [Document.deletionList, hops]
  have hIL : insertionList (H ++ [prefixPatch P 0]) = insertionList H := by simp [Document.insertionList, hops]
  have hOL : orderList (H ++ [prefixPatch P 0]) = orderList H := by simp [Document.orderList, hops]
  exact ⟨by rw [hF, hA]; exact g.facts, by rw [hops, hA]; exact g.insertions,
    by rw [hops, hA]; exact g.deletions, by rw [hops]; exact g.keys,
    by rw [hops, hDL]; exact g.restores, by rw [hops, hDL, hIL]; exact g.keeps,
    by rw [hops, hOL]; exact g.retracts⟩

/-- A built history is `Good`. -/
theorem built_good {H : Hist} (hb : Built H) : Good H := by
  induction hb with
  | nil =>
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_⟩ <;> simp [DocModel.facts, DocModel.ops]
  | snoc hb adm hval ih =>
    rename_i H P
    have hb' : Built (H ++ [P]) := .snoc hb adm hval
    have e : Ext H (H ++ [P]) := ⟨⟨_, ops_append H P⟩, ⟨[P.id], by simp [ids]⟩⟩
    have am : ∀ {a}, a ∈ anchorList H → a ∈ anchorList (H ++ [P]) := fun ha => anchorList_mono H P _ ha
    have fm : ∀ (g : OpId × Op → Option OpId) {x}, x ∈ (DocModel.ops H).filterMap g →
        x ∈ (DocModel.ops (H ++ [P])).filterMap g := by
      intro g x hx
      obtain ⟨Z, hZ⟩ := e.filterMap g; rw [hZ]; exact List.mem_append_left _ hx
    -- the patch's own operations validated against `H`
    have own : ∀ o op, (o, op) ∈ opsOf P → OpOK H op := by
      intro o op hm
      have hP := mem_opsOf_iff.mp hm
      obtain ⟨s, s', hv⟩ := validate_each_op H P hval (op, o.ordinal)
        (List.mem_zipIdx_iff_getElem?.mpr hP.2)
      exact validOp_ok H P.id P.deps s s' o.ordinal op hv
    have split : ∀ {x}, x ∈ DocModel.ops (H ++ [P]) → x ∈ DocModel.ops H ∨ x ∈ opsOf P := fun hx => by
      rw [ops_append, List.mem_append] at hx; exact hx
    refine ⟨facts_anchors _ hb', ?_, ?_, ops_keys_nodup (built_nodup hb'), ?_, ?_, ?_⟩
    · intro o l r xs h
      rcases split h with h | h
      · exact ⟨am (ih.insertions o l r xs h).1, am (ih.insertions o l r xs h).2⟩
      · have := own o _ h; exact ⟨am this.1, am this.2⟩
    · intro o ts h t ht
      rcases split h with h | h
      · exact am (ih.deletions o ts h t ht)
      · exact am (own o _ h t ht)
    · intro o d h
      rcases split h with h | h
      · exact fm _ (ih.restores o d h)
      · exact fm _ (own o _ h)
    · intro o d i h
      rcases split h with h | h
      · exact ⟨fm _ (ih.keeps o d i h).1, fm _ (ih.keeps o d i h).2⟩
      · have := own o _ h; exact ⟨fm _ this.1, fm _ this.2⟩
    · intro o r h
      rcases split h with h | h
      · exact fm _ (ih.retracts o r h)
      · exact fm _ (own o _ h)

theorem commitOpL_deps (D : DocKernel.DocL) (p : Nat) (o : document.Op) :
    (DocKernel.commitOpL D p o).deps = D.deps := by
  cases o <;> simp only [DocKernel.commitOpL] <;> (try split_ifs) <;> rfl

/-- The commit, operation by operation. -/
theorem rep_fold (D : DocKernel.DocL) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : RepL D H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (hval : DocModel.validate H P = .ok ()) :
    ∀ j, j ≤ P.ops.length →
      let Dj := (dp.ops.val.take j).foldl (fun D o => DocKernel.commitOpL D D.deps.length.pred o)
        { D with deps := D.deps ++ [dp.deps.val.map (·.val)] }
      RepL Dj (H ++ [prefixPatch P j]) ∧ Good (H ++ [prefixPatch P j]) ∧ Dj.deps.length = H.length + 1 := by
  have hv := built_valid hb
  have hlen : dp.ops.val.length = P.ops.length := ht.2.length_eq.symm
  have hdl : D.deps.length = H.length := by
    have := congrArg List.length hr.deps; simpa using this
  intro j
  induction j with
  | zero =>
    intro _
    simp only [List.take_zero, List.foldl_nil]
    exact ⟨rep_base D H P dp hr hv ht adm, good_base H P (built_good hb), by simp [hdl]⟩
  | succ j ih =>
    intro hj
    obtain ⟨hR, hG, hD⟩ := ih (by omega)
    have hj' : j < dp.ops.val.length := by omega
    simp only
    rw [List.take_succ, List.getElem?_eq_getElem hj', Option.toList_some, List.foldl_append,
      List.foldl_cons, List.foldl_nil]
    set Dj := (dp.ops.val.take j).foldl (fun D o => DocKernel.commitOpL D D.deps.length.pred o)
      { D with deps := D.deps ++ [dp.deps.val.map (·.val)] } with hDj
    have s := snoc_prefix H P adm.1 j (by omega)
    have hp : Dj.deps.length.pred = idx (ids (H ++ [prefixPatch P j])) (⟨P.id, j⟩ : OpId).patch := by
      rw [hD, ids_prefix]
      simp only [Nat.pred_succ]
      rw [idx_of_mem (by simp), List.idxOf_append_of_notMem adm.1]
      simp [ids]
    -- the operation, validated and translated against `H`
    have htr : TrOp H P.ops[j] dp.ops.val[j] := by
      have := ht.2.get (by omega) hj'
      simp only [List.get_eq_getElem] at this
      exact this
    have hokH : OpOK H P.ops[j] := by
      obtain ⟨s0, s1, hv'⟩ := validate_each_op H P hval (P.ops[j], j)
        (List.mem_zipIdx_iff_getElem?.mpr (by simp [show j < P.ops.length by omega] <;> rfl))
      exact validOp_ok H P.id P.deps s0 s1 j _ hv'
    have e := ext_prefix H P j
    have hok := OpOK_ext e _ hokH
    refine ⟨?_, hG.snoc s hok, by rw [commitOpL_deps, hD]⟩
    rw [hp]
    exact rep_op Dj _ _ _ _ _ hR hG s hok (TrOp_ext e _ _ hokH htr)

/-- Committing the translated patch to a document that encodes `H` yields one
that encodes `H ++ [P]`. -/
theorem rep_commit' (D : DocKernel.DocL) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : RepL D H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (hval : DocModel.validate H P = .ok ()) :
    RepL (DocKernel.commitL D (dp.deps.val.map (·.val)) dp.ops.val) (H ++ [P]) := by
  have hlen : dp.ops.val.length = P.ops.length := ht.2.length_eq.symm
  have := (rep_fold D H P dp hr hb ht adm hval P.ops.length le_rfl).1
  rw [prefix_full, ← hlen, List.take_length] at this
  exact this

/-! ## Validation, operation by operation -/

/-- The kernel's validation state before any operation. -/
def initS : DocKernel.CheckS := ⟨(0#u64, 0, 0), [], 0, []⟩

theorem stateRel_init (H : Hist) (P : Patch) : StateRel H P initS {} 0 := by
  refine ⟨rfl, ⟨by simp [initS], by simp [initS]⟩, by simp [initS, createdBy], by simp, by simp⟩

theorem checkOpS_refused (st : DocKernel.CheckS) (d : document.Doc) (seen : List Bool) (k : Nat)
    (o : document.Op) (h : st.refusal.1 ≠ 0#u64) : DocKernel.checkOpS st d seen k o = st := by
  unfold DocKernel.checkOpS; rw [if_pos h]

theorem zipIdx_take_succ {α} (l : List α) (j : Nat) (hj : j < l.length) :
    l.zipIdx.take (j + 1) = l.zipIdx.take j ++ [(l[j], j)] := by
  rw [List.take_succ]
  congr
  simp [hj]

/-- Kernel and model validation agree on every prefix of the operations. -/
theorem validate_sim (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P) :
    ∀ j, j ≤ P.ops.length →
      match (P.ops.zipIdx.take j).foldlM (fun own (x : Op × Nat) => validOp H P.id P.deps own x.2 x.1)
          ({} : Own) with
      | .ok own => StateRel H P ((dp.ops.val.zipIdx.take j).foldl
          (fun s x => DocKernel.checkOpS s d (DocKernel.seenM d.deps.val dp.deps.val) x.2 x.1) initS) own j
      | .error e =>
        let st := (dp.ops.val.zipIdx.take j).foldl
          (fun s x => DocKernel.checkOpS s d (DocKernel.seenM d.deps.val dp.deps.val) x.2 x.1) initS
        st.refusal.1 ≠ 0#u64 ∧ errorOfT P st.refusal = some e := by
  have hlen : dp.ops.val.length = P.ops.length := ht.2.length_eq.symm
  intro j
  induction j with
  | zero =>
    intro _
    simp only [List.take_zero, List.foldlM_nil, List.foldl_nil]
    exact stateRel_init H P
  | succ j ih =>
    intro hj
    have ih := ih (by omega)
    have hj1 : j < P.ops.length := by omega
    have hj2 : j < dp.ops.val.length := by omega
    rw [zipIdx_take_succ P.ops j hj1, zipIdx_take_succ dp.ops.val j hj2, List.foldlM_append, List.foldl_append]
    simp only [List.foldl_cons, List.foldl_nil]
    revert ih
    rcases hm : (P.ops.zipIdx.take j).foldlM (fun own (x : Op × Nat) => validOp H P.id P.deps own x.2 x.1)
        ({} : Own) with e | own
    · intro ih
      obtain ⟨h1, h2⟩ := ih
      simp only [Bind.bind, Except.bind]
      rw [checkOpS_refused _ _ _ _ _ h1]
      exact ⟨h1, h2⟩
    · intro ih
      simp only [List.foldlM_cons, List.foldlM_nil, bind_pure]
      simp only [Bind.bind, Except.bind]
      have htr : TrOp H P.ops[j] dp.ops.val[j] := by
        have := ht.2.get hj1 hj2
        simp only [List.get_eq_getElem] at this
        exact this
      have hs := step_sim d H P dp hr hb ht adm _ own j ih P.ops[j] dp.ops.val[j] (by simp [hj1]) htr
      dsimp only at hs
      rcases hv : validOp H P.id P.deps own j P.ops[j] with e' | own'
      · rw [hv] at hs; exact hs
      · rw [hv] at hs; exact hs

theorem validate_eq (H : Hist) (P : Patch) : DocModel.validate H P =
    (P.ops.zipIdx.foldlM (fun own (x : Op × Nat) => validOp H P.id P.deps own x.2 x.1) ({} : Own)).map
      fun _ => () := rfl

/-- The kernel's refusal kind is zero exactly when the model validates, and
otherwise names the model's error. -/
theorem validate_tr (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P) :
    ((DocKernel.validateS d dp.ops.val (DocKernel.seenM d.deps.val dp.deps.val)).1 = 0#u64 ↔
        DocModel.validate H P = .ok ()) ∧
      ∀ e, DocModel.validate H P = .error e →
        errorOfT P (DocKernel.validateS d dp.ops.val (DocKernel.seenM d.deps.val dp.deps.val)) = some e := by
  have hlen : dp.ops.val.length = P.ops.length := ht.2.length_eq.symm
  have hs := validate_sim d H P dp hr hb ht adm P.ops.length le_rfl
  have t1 : P.ops.zipIdx.take P.ops.length = P.ops.zipIdx := List.take_of_length_le (by simp)
  have t2 : dp.ops.val.zipIdx.take P.ops.length = dp.ops.val.zipIdx := List.take_of_length_le (by simp [hlen])
  rw [t1, t2] at hs
  rw [validate_eq]
  unfold DocKernel.validateS
  revert hs
  rcases P.ops.zipIdx.foldlM (fun own (x : Op × Nat) => validOp H P.id P.deps own x.2 x.1) ({} : Own) with e | own
  · intro hs
    obtain ⟨h1, h2⟩ := hs
    refine ⟨⟨fun h => absurd h h1, fun h => by cases h⟩, ?_⟩
    intro e' he'
    cases he'
    exact h2
  · intro hs
    refine ⟨⟨fun _ => rfl, fun _ => hs.1⟩, ?_⟩
    intro e' he'
    cases he'

theorem forall₂_mem_right {α β} {R : α → β → Prop} :
    ∀ {l₁ : List α} {l₂ : List β}, List.Forall₂ R l₁ l₂ → ∀ b ∈ l₂, ∃ a ∈ l₁, R a b
  | _, _, .nil, _, hb => absurd hb (by simp)
  | a :: _, _ :: _, .cons h hs, b, hb => by
    rcases List.mem_cons.mp hb with rfl | hb
    · exact ⟨a, List.mem_cons_self .., h⟩
    · obtain ⟨a', ha', h'⟩ := forall₂_mem_right hs b hb
      exact ⟨a', List.mem_cons_of_mem _ ha', h'⟩

theorem growth_eq (o : document.Op) :
    DocKernel.growth o = (match o with | .Insert _ _ xs => xs.val.length + 2 | _ => 2) := by
  cases o <;> rfl

/-- The commit's room condition, from the adapter's bound and validation. -/
theorem commitRoomL_of (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (hval : DocModel.validate H P = .ok ())
    (hroom : CommitRoom d dp) : DocKernel.CommitRoomL (DocKernel.absD d) dp.ops.val := by
  -- every translated operation comes from a validated model operation
  have src : ∀ o' ∈ dp.ops.val, ∃ op, TrOp H op o' ∧ OpOK H op := by
    intro o' ho'
    obtain ⟨op, hop, htr⟩ := forall₂_mem_right ht.2 o' ho'
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hop
    obtain ⟨s0, s1, hv⟩ := validate_each_op H P hval (P.ops[k], k)
      (List.mem_zipIdx_iff_getElem?.mpr (by simp [hk]))
    exact ⟨_, htr, validOp_ok H P.id P.deps s0 s1 k _ hv⟩
  refine ⟨?_, hroom.2.2, ?_, ?_⟩
  · have h1 := hroom.1
    have hg : (dp.ops.val.map DocKernel.growth) = dp.ops.val.map (fun o => match o with
        | .Insert _ _ xs => xs.val.length + 2 | _ => 2) := by
      apply List.map_congr_left; intro o _; exact growth_eq o
    simp only [DocKernel.absD, List.length_map, hg]
    omega
  · intro r hm
    obtain ⟨op, htr, hok⟩ := src _ hm
    cases op <;> simp only [TrOp, OpOK] at htr hok
    rw [htr.2]
    have := congrArg List.length hr.restored
    simp only [List.length_map] at this
    rw [this]
    exact idx_lt hok
  · intro o hm
    obtain ⟨op, htr, hok⟩ := src _ hm
    cases op <;> simp only [TrOp, OpOK] at htr hok
    rw [htr.2]
    have := congrArg List.length hr.orders
    simp only [List.length_map] at this
    rw [this]
    exact idx_lt hok

/-- `Replayed` documents encode their history. -/
theorem rep_replayed (d : document.Doc) (H : Hist) (h : Replayed d H) : Rep d H := by
  induction h with
  | nil he =>
    have := DocKernel.empty_spec
    have hd := post_of_ok this he
    unfold Rep
    rw [hd]
    refine ⟨by simp [DocKernel.emptyL], by simp [DocKernel.emptyL, DocModel.atoms, ops],
      by simp [DocKernel.emptyL, DocModel.atoms, ops, NONE, document.NONE],
      by simp [DocKernel.emptyL, encEdges, facts, ops, NONE, document.NONE],
      by simp [DocKernel.emptyL, insertionsOf, ops], by simp [DocKernel.emptyL, deletionsOf, ops],
      by simp [DocKernel.emptyL, deletionList, ops], ?_,
      by simp [DocKernel.emptyL, orderList, ops], by simp [DocKernel.emptyL, retractionsOf, ops]⟩
    intro a b
    simp [DocKernel.emptyL, Acked, ops]
  | snoc hr' ht adm hval hroom hc ih =>
    rename_i d d' H P dp
    have hb := replayed_built hr'
    have hspec := DocKernel.commit_spec d dp hroom.2.1 (commitRoomL_of d H P dp ih hb ht hval hroom)
    have hd' := post_of_ok hspec hc
    unfold Rep
    rw [hd']
    exact rep_commit' _ H P dp ih hb ht adm hval

end o5s_kernel.DocRep
