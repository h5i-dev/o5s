import DocKernel
import Tags
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Specifications of the document kernel's materializer (draft)

List-level meanings of `document::{liveness, delete_inserts, summaries,
kahn, emit, materialize}`; `DocView.lean` relates them to `DocModel.IsView`
through the representation of `DocRep.lean`.
-/
namespace o5s_kernel.DocKernel

/-! ## Liveness -/

/-- Append `d` to row `t`. -/
def addAt (rows : List (List Nat)) (t d : Nat) : List (List Nat) :=
  rows.mapIdx fun i r => if i = t then r ++ [d] else r

/-- The covering rows after deletion `d`'s targets. -/
def coverStep (d : Nat) (rows : List (List Nat)) (t : Usize) : List (List Nat) := addAt rows t.val d

def coverRows (dels : List document.Deletion) (n : Nat) : List (List Nat) :=
  (List.range dels.length).foldl
    (fun rows d => (dels[d]?.map (·.targets.val)).getD [] |>.foldl (coverStep d) rows)
    (List.replicate n [])

theorem addAt_length (rows : List (List Nat)) (t d : Nat) : (addAt rows t d).length = rows.length := by
  simp [addAt]

theorem mem_addAt (rows : List (List Nat)) (t d a : Nat) (ha : a < rows.length) (x : Nat) :
    x ∈ (addAt rows t d)[a]'(by simpa [addAt_length] using ha) ↔ x ∈ rows[a] ∨ (a = t ∧ x = d) := by
  simp only [addAt, List.getElem_mapIdx]
  split_ifs with h
  · simp [h]
  · simp [h]

theorem foldl_coverStep_length (d : Nat) (ts : List Usize) (rows : List (List Nat)) :
    (ts.foldl (coverStep d) rows).length = rows.length := by
  induction ts generalizing rows with
  | nil => rfl
  | cons t ts ih => simp [ih, coverStep, addAt_length]

theorem mem_foldl_coverStep (d : Nat) (ts : List Usize) (rows : List (List Nat)) (a : Nat)
    (ha : a < rows.length) (x : Nat) :
    x ∈ (ts.foldl (coverStep d) rows)[a]'(by rw [foldl_coverStep_length]; exact ha) ↔
      x ∈ rows[a] ∨ (x = d ∧ ∃ t ∈ ts, t.val = a) := by
  induction ts generalizing rows with
  | nil => simp
  | cons t ts ih =>
    simp only [List.foldl_cons]
    rw [ih (coverStep d rows t) (by simp [coverStep, addAt_length, ha])]
    simp only [coverStep]
    rw [mem_addAt rows t.val d a ha]
    constructor
    · rintro ((h | ⟨rfl, rfl⟩) | ⟨rfl, t', ht', e⟩)
      · exact Or.inl h
      · exact Or.inr ⟨rfl, t, List.mem_cons_self .., rfl⟩
      · exact Or.inr ⟨rfl, t', List.mem_cons_of_mem _ ht', e⟩
    · rintro (h | ⟨rfl, t', ht', e⟩)
      · exact Or.inl (Or.inl h)
      · rcases List.mem_cons.mp ht' with rfl | ht'
        · exact Or.inl (Or.inr ⟨e.symm, rfl⟩)
        · exact Or.inr ⟨rfl, t', ht', e⟩

/-- The abstract rows of a covering table. -/
def rowsV (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) : List (List Nat) := rows.val.map (·.val.map (·.val))

theorem rowsV_add (rows out : alloc.vec.Vec (alloc.vec.Vec Usize)) (src dst : Usize)
    (hl : out.val.length = rows.val.length)
    (h : ∀ i (hi : i < rows.val.length) (ho : i < out.val.length),
      out.val[i].val = if i = src.val then rows.val[i].val ++ [dst] else rows.val[i].val) :
    rowsV out = addAt (rowsV rows) src.val dst.val := by
  apply List.ext_getElem
  · simp [rowsV, addAt_length, hl]
  · intro i h1 h2
    simp only [rowsV, List.length_map] at h1
    simp only [rowsV, addAt, List.getElem_map, List.getElem_mapIdx]
    rw [h i (by omega) h1]
    split_ifs <;> simp

theorem liveness_inner_spec (dels : alloc.vec.Vec document.Deletion) (d : Usize)
    (hd : d.val < dels.val.length) (covering : alloc.vec.Vec (alloc.vec.Vec Usize))
    (hb : ∀ t ∈ (dels.val[d.val]).targets.val, t.val < covering.val.length)
    (room : ∀ i (h : i < covering.val.length),
      (covering.val[i]).val.length + (dels.val[d.val]).targets.val.length < Usize.max) :
    document.liveness_loop0_loop0 dels covering d 0#usize ⦃ out =>
      rowsV out = (dels.val[d.val]).targets.val.foldl (coverStep d.val) (rowsV covering) ∧
      out.val.length = covering.val.length ⦄ := by
  unfold document.liveness_loop0_loop0
  have hlen : ∀ x : alloc.vec.Vec (alloc.vec.Vec Usize), (rowsV x).length = x.val.length := by
    intro x; simp [rowsV]
  apply WP.spec_mono (loop_fold (dels.val[d.val]).targets.val rowsV (coverStep d.val)
    (fun cov j => cov.val.length = covering.val.length ∧
      ∀ i (h1 : i < cov.val.length) (h2 : i < covering.val.length),
        (cov.val[i]).val.length ≤ (covering.val[i]).val.length + j)
    (fun st => document.liveness_loop0_loop0.body dels d st.1 st.2) ?_ covering 0#usize
    (by simp) ⟨rfl, by simp⟩)
  · intro out h
    rw [show ((0#usize : Usize).val) = 0 by simp, List.drop_zero] at h
    refine ⟨h, ?_⟩
    have := congrArg List.length h
    rw [hlen, foldl_coverStep_length, hlen] at this
    exact this
  · intro cov j hj hinv
    unfold document.liveness_loop0_loop0.body
    obtain ⟨hl, hrow⟩ := hinv
    by_cases hjt : j.val < (dels.val[d.val]).targets.val.length
    · have hsrc : ((dels.val[d.val]).targets.val[j.val]).val < cov.val.length := by
        rw [hl]; exact hb _ (List.getElem_mem _)
      have hroom : (cov.val[((dels.val[d.val]).targets.val[j.val]).val]).val.length < Usize.max := by
        have h1 := hrow ((dels.val[d.val]).targets.val[j.val]).val hsrc (by rw [← hl]; exact hsrc)
        have h2 := room ((dels.val[d.val]).targets.val[j.val]).val (by rw [← hl]; exact hsrc)
        omega
      h5i_step
      all_goals first
        | (simp_all; done)
        | (refine ⟨?_, ?_⟩
           · unfold coverStep
             exact rowsV_add cov covering1 _ d (by omega) (fun i hi ho => covering1_post1 i (by omega))
           · intro i h1
             have := hrow i h1
             split_ifs <;> simp <;> omega)
        | (trace_state; sorry)
    · h5i_step
      all_goals first
        | (simp_all [FoldStep]; done)
        | (simp only [FoldStep]; constructor <;> first | (simp; omega) | rfl)
        | (trace_state; sorry)

/-- Rows of the covering table, deletion by deletion. -/
def coverUpTo (dels : List document.Deletion) (n j : Nat) : List (List Nat) :=
  (List.range j).foldl
    (fun rows d => (dels[d]?.map (·.targets.val)).getD [] |>.foldl (coverStep d) rows)
    (List.replicate n [])

theorem coverUpTo_length (dels : List document.Deletion) (n j : Nat) : (coverUpTo dels n j).length = n := by
  induction j with
  | zero => simp [coverUpTo]
  | succ j ih =>
    unfold coverUpTo at ih ⊢
    rw [List.range_succ, List.foldl_append]
    simp [foldl_coverStep_length, ih]

theorem mem_coverUpTo (dels : List document.Deletion) (n j a : Nat) (ha : a < n) (x : Nat) :
    x ∈ (coverUpTo dels n j)[a]'(by rw [coverUpTo_length]; exact ha) ↔
      x < j ∧ ∃ h : x < dels.length, ∃ t ∈ (dels[x]).targets.val, t.val = a := by
  induction j with
  | zero => simp [coverUpTo]
  | succ j ih =>
    have hfold : coverUpTo dels n (j + 1) =
        ((dels[j]?.map (·.targets.val)).getD []).foldl (coverStep j) (coverUpTo dels n j) := by
      unfold coverUpTo; rw [List.range_succ, List.foldl_append]; rfl
    simp only [hfold]
    rw [mem_foldl_coverStep j _ _ a (by rw [coverUpTo_length]; exact ha), ih]
    constructor
    · rintro (⟨hx, hd, t, ht, e⟩ | ⟨rfl, t, ht, e⟩)
      · exact ⟨by omega, hd, t, ht, e⟩
      · by_cases hj : x < dels.length
        · simp only [List.getElem?_eq_getElem hj, Option.map_some, Option.getD_some] at ht
          exact ⟨by omega, hj, t, ht, e⟩
        · simp [List.getElem?_eq_none (by omega : dels.length ≤ x)] at ht
    · rintro ⟨hx, hd, t, ht, e⟩
      by_cases hxj : x < j
      · exact Or.inl ⟨hxj, hd, t, ht, e⟩
      · have : x = j := by omega
        subst this
        right
        refine ⟨rfl, t, ?_, e⟩
        simpa [List.getElem?_eq_getElem hd] using ht

theorem sum_take_le {l : List Nat} (j : Nat) : ((l.take j).sum) ≤ l.sum := by
  induction l generalizing j with
  | nil => simp
  | cons x l ih => cases j <;> simp [ih] <;> omega

/-- A row of the covering table has at most one entry per target so far. -/
theorem coverUpTo_row_le (dels : List document.Deletion) (n j a : Nat) (ha : a < n) :
    ((coverUpTo dels n j)[a]'(by rw [coverUpTo_length]; exact ha)).length ≤
      ((dels.take j).map (·.targets.val.length)).sum := by
  induction j with
  | zero => simp [coverUpTo]
  | succ j ih =>
    have hfold : coverUpTo dels n (j + 1) =
        ((dels[j]?.map (·.targets.val)).getD []).foldl (coverStep j) (coverUpTo dels n j) := by
      unfold coverUpTo; rw [List.range_succ, List.foldl_append]; rfl
    have grow : ∀ (ts : List Usize) (rows : List (List Nat)) (ha' : a < rows.length),
        ((ts.foldl (coverStep j) rows)[a]'(by rw [foldl_coverStep_length]; exact ha')).length ≤
          (rows[a]).length + ts.length := by
      intro ts
      induction ts with
      | nil => intro rows ha'; simp
      | cons t ts ih' =>
        intro rows ha'
        simp only [List.foldl_cons]
        have := ih' (coverStep j rows t) (by simp [coverStep, addAt_length, ha'])
        simp only [coverStep, addAt, List.getElem_mapIdx] at this ⊢
        split_ifs at this <;> simp at this ⊢ <;> omega
    simp only [hfold]
    refine le_trans (grow _ _ (by rw [coverUpTo_length]; exact ha)) ?_
    by_cases hj : j < dels.length
    · rw [List.take_succ, List.getElem?_eq_getElem hj]
      simp only [Option.map_some, Option.getD_some, Option.toList_some, List.map_append, List.map_cons,
        List.map_nil, List.sum_append, List.sum_cons, List.sum_nil]
      omega
    · rw [List.getElem?_eq_none (by omega : dels.length ≤ j)]
      simp only [Option.map_none, Option.getD_none, List.length_nil, add_zero]
      refine le_trans ih ?_
      rw [List.take_of_length_le (by omega : dels.length ≤ j), List.take_of_length_le (by omega)]

theorem liveness_outer_spec (dels : alloc.vec.Vec document.Deletion) (n : Nat)
    (hb : ∀ x ∈ dels.val, ∀ t ∈ x.targets.val, t.val < n)
    (room : (dels.val.map (·.targets.val.length)).sum < Usize.max)
    (covering : alloc.vec.Vec (alloc.vec.Vec Usize)) (hc : rowsV covering = List.replicate n []) :
    document.liveness_loop0 dels covering 0#usize ⦃ out =>
      rowsV out = coverUpTo dels.val n dels.val.length ∧ out.val.length = n ⦄ := by
  unfold document.liveness_loop0
  have hcl : covering.val.length = n := by
    have := congrArg List.length hc; simpa [rowsV] using this
  have hlen : ∀ x : alloc.vec.Vec (alloc.vec.Vec Usize), (rowsV x).length = x.val.length := by
    intro x; simp [rowsV]
  apply WP.spec_mono (loop_fold (List.range dels.val.length) rowsV
    (fun rows d => (dels.val[d]?.map (·.targets.val)).getD [] |>.foldl (coverStep d) rows)
    (fun cov j => rowsV cov = coverUpTo dels.val n j)
    (fun st => document.liveness_loop0.body dels st.1 st.2) ?_ covering 0#usize
    (by simp) (by simp [coverUpTo, hc]))
  · intro out h
    rw [show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, hc] at h
    refine ⟨h, ?_⟩
    have := congrArg List.length h
    rw [hlen] at this
    rw [this]
    exact coverUpTo_length _ _ _
  · intro cov j hj hinv
    unfold document.liveness_loop0.body
    have hcovl : cov.val.length = n := by
      have := congrArg List.length hinv; rw [hlen, coverUpTo_length] at this; exact this
    by_cases hjd : j.val < dels.val.length
    · have hrow : ∀ i (h : i < cov.val.length),
          (cov.val[i]).val.length + (dels.val[j.val]).targets.val.length < Usize.max := by
        intro i h
        have e := congrArg (fun l => l[i]?) hinv
        simp only [rowsV, List.getElem?_map, List.getElem?_eq_getElem h, Option.map_some] at e
        have hi' : i < n := by omega
        rw [List.getElem?_eq_getElem (by rw [coverUpTo_length]; exact hi')] at e
        simp only [Option.some.injEq] at e
        have hl := congrArg List.length e
        simp only [List.length_map] at hl
        have := coverUpTo_row_le dels.val n j.val i hi'
        rw [← hl] at this
        have hsum : ((dels.val.take j.val).map (·.targets.val.length)).sum + (dels.val[j.val]).targets.val.length ≤
            (dels.val.map (·.targets.val.length)).sum := by
          have h1 := sum_take_le (l := dels.val.map (·.targets.val.length)) (j.val + 1)
          rw [List.sum_take_succ _ _ (by simpa using hjd)] at h1
          simpa using h1
        omega
      h5i_step
      all_goals first
        | (simp_all; done)
        | (apply WP.spec_bind (liveness_inner_spec dels j hjd cov
              (fun t ht => by rw [hcovl]; exact hb _ (List.getElem_mem _) t ht) (fun i h => hrow i (by omega)))
           rintro cov1 ⟨h1, h2⟩
           h5i_step
           all_goals first
             | (simp only [FoldStep]
                refine ⟨by simpa using hjd, ?_, by scalar_tac, ?_⟩
                · rw [h1, hinv]; simp [List.getElem?_eq_getElem hjd]
                · rw [h1, hinv]
                  unfold coverUpTo
                  rw [List.range_succ, List.foldl_append]
                  simp [List.getElem?_eq_getElem hjd])
             | (unfold coverUpTo
                rw [List.range_succ, List.foldl_append]
                simp [List.getElem?_eq_getElem hjd])
             | (trace_state; sorry))
        | (trace_state; sorry)
    · h5i_step
      all_goals first
        | (simp only [FoldStep]; exact ⟨by simp; omega, rfl⟩)
        | (simp_all [FoldStep]; done)
        | (trace_state; sorry)

/-- Atom `a` is live: not a sentinel, and every deletion covering it was restored. -/
def liveM (rows : List (List Nat)) (restored : List Bool) (a : Nat) : Bool :=
  decide (2 ≤ a) && (rows[a]?.getD []).all fun d => restored[d]?.getD false

theorem liveness_last_spec (atoms : alloc.vec.Vec (alloc.vec.Vec U8)) (restored : alloc.vec.Vec Bool)
    (covering : alloc.vec.Vec (alloc.vec.Vec Usize)) (hl : covering.val.length = atoms.val.length)
    (hb : ∀ r ∈ covering.val, ∀ t ∈ r.val, t.val < restored.val.length) :
    document.liveness_loop1 atoms restored covering (alloc.vec.Vec.new Bool) 0#usize ⦃ live =>
      live.val = (List.range atoms.val.length).map (liveM (rowsV covering) restored.val) ⦄ := by
  unfold document.liveness_loop1
  apply WP.spec_mono (loop_fold (List.range atoms.val.length) (fun l : alloc.vec.Vec Bool => l.val)
    (fun acc a => acc ++ [liveM (rowsV covering) restored.val a]) (fun l j => l.val.length = j)
    (fun st => document.liveness_loop1.body atoms restored covering st.1 st.2) ?_ _ 0#usize
    (by simp) (by simp))
  · intro live h
    rw [h, show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, foldl_snoc_map]
    simp
  · intro l a ha hinv
    unfold document.liveness_loop1.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep, liveM]; done)
      | (intro tag htag; exact hb _ (List.getElem_mem _) tag htag)
      | (refine ⟨by scalar_tac, ?_⟩
         have hac : a.val < covering.val.length := by scalar_tac
         apply Bool.eq_iff_iff.mpr
         rw [b_post]
         simp only [liveM, rowsV, List.getElem?_map, List.getElem?_eq_getElem hac, Option.map_some,
           Option.getD_some, Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true, List.mem_map,
           Tags.restoredTag, forall_exists_index, and_imp, forall_apply_eq_imp_iff₂]
         constructor
         · intro h; exact ⟨by omega, h⟩
         · intro h; exact h.2)
      | (trace_state; sorry)

theorem coverUpTo_mem_lt (dels : List document.Deletion) (n j a : Nat) (ha : a < n) (x : Nat)
    (hx : x ∈ (coverUpTo dels n j)[a]'(by rw [coverUpTo_length]; exact ha)) : x < dels.length := by
  obtain ⟨-, hd, -⟩ := (mem_coverUpTo dels n j a ha x).mp hx
  exact hd

/-- `liveness`: atom `a` is live when it is not a sentinel and every deletion
covering it was restored. -/
theorem liveness_spec (doc : document.Doc)
    (hb : ∀ x ∈ doc.deletions.val, ∀ t ∈ x.targets.val, t.val < doc.atoms.val.length)
    (room : (doc.deletions.val.map (·.targets.val.length)).sum < Usize.max)
    (hres : doc.restored.val.length = doc.deletions.val.length) :
    document.liveness doc ⦃ live =>
      live.val = (List.range doc.atoms.val.length).map
        (liveM (coverUpTo doc.deletions.val doc.atoms.val.length doc.deletions.val.length) doc.restored.val) ⦄ := by
  unfold document.liveness
  apply WP.spec_bind (empty_rows_spec doc.atoms.len)
  intro covering hcov
  apply WP.spec_bind (liveness_outer_spec doc.deletions doc.atoms.val.length hb room covering
    (by simp [rowsV, hcov]))
  rintro covering1 ⟨h1, h2⟩
  apply WP.spec_mono (liveness_last_spec doc.atoms doc.restored covering1 h2 ?_)
  · intro live hl
    rw [hl, h1]
  · intro r hr t ht
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hr
    have hk' : k < doc.atoms.val.length := by omega
    have := congrArg (fun l => l[k]?) h1
    simp only [rowsV, List.getElem?_map, List.getElem?_eq_getElem hk, Option.map_some,
      List.getElem?_eq_getElem (by rw [coverUpTo_length]; exact hk' : k < (coverUpTo doc.deletions.val
        doc.atoms.val.length doc.deletions.val.length).length), Option.some.injEq] at this
    have htm : t.val ∈ (coverUpTo doc.deletions.val doc.atoms.val.length doc.deletions.val.length)[k]'(by
        rw [coverUpTo_length]; exact hk') := by
      rw [← this]; exact List.mem_map.mpr ⟨t, ht, rfl⟩
    rw [hres]
    exact coverUpTo_mem_lt _ _ _ _ hk' _ htm

/-! ## Delete–insert conflicts -/

/-- The insertion's anchors among a deletion's targets. -/
def sharedM (targets : List Nat) (l r : Nat) : List Nat :=
  (if l ∈ targets then [l] else []) ++ (if r ∈ targets then [r] else [])

@[step] theorem shared_anchors_spec (targets : alloc.vec.Vec Usize) (left right : Usize) :
    document.shared_anchors targets left right ⦃ out =>
      out.val.map (·.val) = sharedM (targets.val.map (·.val)) left.val right.val ⦄ := by
  sorry

/-- `concurrent`: distinct patches, neither in the other's causal past. -/
def concurrentM (deps : List (alloc.vec.Vec Usize)) (a b : Usize) : Prop :=
  a ≠ b ∧ ¬ Indexed.RowReach deps [a] b ∧ ¬ Indexed.RowReach deps [b] a

open Classical in
theorem concurrent_spec (deps : alloc.vec.Vec (alloc.vec.Vec Usize)) (a b : Usize)
    (room : deps.val.length < Usize.max) (ha : a.val < deps.val.length) (hb : b.val < deps.val.length)
    (bounds : ∀ row ∈ deps.val, ∀ v ∈ row.val, v.val < deps.val.length) :
    document.concurrent deps a b ⦃ r => r = decide (concurrentM deps.val a b) ⦄ := by
  sorry

/-- A delete–insert conflict, by indices. -/
def diVal (x : document.DeleteInsert) : Nat × Nat × List Nat :=
  (x.deletion.val, x.insertion.val, x.anchors.val.map (·.val))

open Classical in
/-- The conflict of deletion `d` with insertion `i`, if any. -/
noncomputable def conflictAt (doc : document.Doc) (d i : Nat) : Option (Nat × Nat × List Nat) :=
  match doc.deletions.val[d]?, doc.insertions.val[i]? with
  | some del, some ins =>
    let anchors := sharedM (del.targets.val.map (·.val)) ins.left.val ins.right.val
    if anchors ≠ [] ∧ (d, i) ∉ doc.acked.val.map pairVal ∧
        concurrentM doc.deps.val del.patch ins.patch then some (d, i, anchors) else none
  | _, _ => none

/-- Deletion `d`'s conflicts, insertion by insertion. -/
noncomputable def conflictsOf (doc : document.Doc) (d : Nat) : List (Nat × Nat × List Nat) :=
  (List.range doc.insertions.val.length).filterMap (conflictAt doc d)

/-- What `delete_inserts` returns: the conflicts of every deletion that was not restored. -/
noncomputable def deleteInsertsM (doc : document.Doc) : List (Nat × Nat × List Nat) :=
  (List.range doc.deletions.val.length).flatMap fun d =>
    if doc.restored.val[d]?.getD false then [] else conflictsOf doc d

/-- Well-formed for materialization: indices stored in the tables are in range. -/
structure MatWF (doc : document.Doc) : Prop where
  restored : doc.restored.val.length = doc.deletions.val.length
  delPatch : ∀ x ∈ doc.deletions.val, x.patch.val < doc.deps.val.length
  insPatch : ∀ x ∈ doc.insertions.val, x.patch.val < doc.deps.val.length
  deps : ∀ row ∈ doc.deps.val, ∀ v ∈ row.val, v.val < doc.deps.val.length
  room : doc.deps.val.length < Usize.max
  out : doc.deletions.val.length * doc.insertions.val.length < Usize.max

theorem deletion_conflicts_spec (out : alloc.vec.Vec document.DeleteInsert) (doc : document.Doc) (d : Usize)
    (wf : MatWF doc) (hd : d.val < doc.deletions.val.length)
    (room : out.val.length + doc.insertions.val.length < Usize.max) :
    document.deletion_conflicts out doc d ⦃ r =>
      r.val.map diVal = out.val.map diVal ++ conflictsOf doc d.val ⦄ := by
  sorry

theorem delete_inserts_spec (doc : document.Doc) (wf : MatWF doc) :
    document.delete_inserts doc ⦃ out => out.val.map diVal = deleteInsertsM doc ⦄ := by
  sorry

/-! ## All edges, as rows -/

/-- Edge `i` is not withdrawn by any retraction. -/
def liveEdge (edges : List (Usize × Usize × Usize)) (off : List Bool) (i : Nat) : Prop :=
  ∃ h : i < edges.length, off[i]? = some false

open Classical in
noncomputable def allStep (edges : List (Usize × Usize × Usize)) (off : List Bool)
    (rows : List (List Usize)) (i : Nat) : List (List Usize) :=
  if h : liveEdge edges off i then addL rows (edges[i]'h.1).1 (edges[i]'h.1).2.1 else rows

theorem all_rows_loop_spec (edges : alloc.vec.Vec (Usize × Usize × Usize)) (off : alloc.vec.Vec Bool)
    (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (hoff : off.val.length = edges.val.length)
    (hb : ∀ e ∈ edges.val, e.1.val < rows.val.length)
    (hempty : ∀ row ∈ rows.val, row.val = []) (room : edges.val.length < Usize.max) :
    document.all_rows_loop edges off rows 0#usize ⦃ r =>
      rowsL r = (List.range edges.val.length).foldl (allStep edges.val off.val) (rowsL rows) ∧
      ∀ row ∈ r.val, row.val.length ≤ edges.val.length ⦄ := by
  unfold document.all_rows_loop
  have hl : (List.range edges.val.length).length = edges.val.length := List.length_range
  apply WP.spec_mono (Indexed.fold_with_inv (List.range edges.val.length) rowsL
    (allStep edges.val off.val)
    (fun o j => o.val.length = rows.val.length ∧ ∀ row ∈ o.val, row.val.length ≤ j)
    (fun st => document.all_rows_loop.body edges off st.1 st.2) ?_ _ 0#usize (by simp)
    ⟨rfl, fun row h => by simp [hempty row h]⟩)
  · intro r h
    refine ⟨by simpa using h.1, fun row hr => by have := h.2.2 row hr; simpa [hl] using this⟩
  · intro o i hi hinv
    unfold document.all_rows_loop.body
    h5i_step
    all_goals (try simp only [Indexed.FoldInvStep, List.getElem_range, List.length_range])
    all_goals first
      | exact ⟨by scalar_tac, trivial, hinv.1, fun row h => by have := hinv.2 row h; omega⟩
      | (refine ⟨by scalar_tac, ?_, by assumption, hinv.1, fun row h => by have := hinv.2 row h; scalar_tac⟩
         unfold allStep
         rw [dif_neg]
         rintro ⟨h, ho⟩
         rw [List.getElem?_eq_getElem (by scalar_tac)] at ho
         simp_all)
      | (rw [← rows1_post]
         obtain ⟨x1, x2, x3⟩ := rows1
         have e_post := rows1_post
         h5i_step
         all_goals (try simp only [Indexed.FoldInvStep, List.getElem_range, List.length_range])
         all_goals first
           | exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
           | (have hx : x1.val < o.val.length := by
                rw [hinv.1]; exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
              have hall : ∀ k (hk : k < o.val.length), (o.val[k]).val.length < Usize.max := by
                intro k hk
                have := hinv.2 _ (List.getElem_mem hk)
                omega
              exact hall _ hx)
           | (have hx : x1.val < o.val.length := by
                rw [hinv.1]; exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
              have hc : liveEdge edges.val off.val i.val :=
                ⟨by scalar_tac, by rw [List.getElem?_eq_getElem (by scalar_tac), b_post]⟩
              refine ⟨by scalar_tac, ?_, by scalar_tac, by omega, ?_⟩
              · unfold allStep
                rw [dif_pos hc, ← e_post]
                exact rowsL_of_add o rows1 x1 x2 hx (by omega) (fun k hk _ => by rw [rows1_post1 k (by omega)])
              · intro row hr
                obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hr
                rw [rows1_post1 k (by omega), i2_post]
                have := hinv.2 _ (List.getElem_mem (by omega : k < o.val.length))
                split_ifs <;> simp <;> omega)
           | (trace_state; sorry))
      | (trace_state; sorry)

/-! ## Rows of dead atoms -/

theorem dead_rows_inner_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (a : Usize)
    (ha : a.val < rows.val.length) :
    document.dead_rows_loop0_loop0 rows a (alloc.vec.Vec.new Usize) 0#usize ⦃ r =>
      r.val = (rows.val[a.val]).val ⦄ := by
  unfold document.dead_rows_loop0_loop0
  apply WP.spec_mono (loop_fold (rows.val[a.val]).val (fun r : alloc.vec.Vec Usize => r.val)
    (fun acc x => acc ++ [x]) (fun r j => r.val.length = j)
    (fun st => document.dead_rows_loop0_loop0.body rows a st.1 st.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r h
    rw [h, show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, foldl_snoc]
    simp
  · intro row k hk hinv
    unfold document.dead_rows_loop0_loop0.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (simp only [FoldStep]; refine ⟨by scalar_tac, by simp_all, by scalar_tac, by simp_all⟩)
      | (trace_state; sorry)

/-- Rows of dead atoms, by index: empty for a live atom, its row otherwise. -/
def deadRow (rows : List (alloc.vec.Vec Usize)) (live : List Bool) (a : Nat) : List Usize :=
  if live[a]?.getD false then [] else (rows[a]?.map (·.val)).getD []

theorem dead_rows_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (live : alloc.vec.Vec Bool)
    (hl : live.val.length = rows.val.length) :
    document.dead_rows rows live ⦃ out =>
      out.val.map (·.val) = (List.range rows.val.length).map (deadRow rows.val live.val) ⦄ := by
  unfold document.dead_rows document.dead_rows_loop0
  apply WP.spec_mono (loop_fold (List.range rows.val.length)
    (fun o : alloc.vec.Vec (alloc.vec.Vec Usize) => o.val.map (·.val))
    (fun acc a => acc ++ [deadRow rows.val live.val a]) (fun o j => o.val.length = j)
    (fun st => document.dead_rows_loop0.body rows live st.1 st.2) ?_ _ 0#usize (by simp) (by simp))
  · intro out h
    rw [h, show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, foldl_snoc_map]
    simp
  · intro o a ha hinv
    unfold document.dead_rows_loop0.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (apply WP.spec_bind (dead_rows_inner_spec rows a (by scalar_tac))
         intro row hrow
         h5i_step
         all_goals first
           | (simp only [FoldStep]
              refine ⟨by simp; scalar_tac, ?_, by scalar_tac, by simp_all⟩
              have h1 : a.val < live.val.length := by scalar_tac
              have h2 : a.val < rows.val.length := by scalar_tac
              simp_all [deadRow, List.getElem?_eq_getElem h1, List.getElem?_eq_getElem h2])
           | (trace_state; sorry))
      | (simp only [FoldStep]
         refine ⟨by simp; scalar_tac, ?_, by scalar_tac, by simp_all⟩
         have h1 : a.val < live.val.length := by scalar_tac
         simp_all [deadRow, List.getElem?_eq_getElem h1])
      | (refine ⟨by scalar_tac, ?_⟩
         have h1 : a.val < live.val.length := by scalar_tac
         have h2 : a.val < rows.val.length := by scalar_tac
         simp_all [deadRow, List.getElem?_eq_getElem h1, List.getElem?_eq_getElem h2])
      | (trace_state; sorry)

end o5s_kernel.DocKernel
