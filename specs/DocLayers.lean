import DocKahn
import DocMaterialize
import DocKernelCommit
import Boundary
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The loops of `materialize` around the Kahn layering

`delete_inserts` lists the open delete/insert conflicts (`deleteInsertsM`).
`materialize_loop1` builds, for each live atom, its boundary row (the live
atoms reached through dead interiors, `BRow`), the predecessor counts and the
list of live atoms (`L1Inv`). The remaining loops initialise the counts,
collect the blocked atoms and test whether every layer is a single atom.
-/
namespace o5s_kernel.DocKernel

theorem foldl_toList {α β} (f : α → Option β) (l : List α) (init : List β) :
    l.foldl (fun acc i => acc ++ (f i).toList) init = init ++ l.filterMap f := by
  induction l generalizing init with
  | nil => simp
  | cons x l ih => rw [List.foldl_cons, ih]; cases h : f x <;> simp [h, List.filterMap_cons]

open Classical in
theorem conflictAt_some (doc : document.Doc) {d i : Nat} (hd : d < doc.deletions.val.length)
    (hi : i < doc.insertions.val.length) :
    conflictAt doc d i =
      if sharedM ((doc.deletions.val[d]).targets.val.map (·.val)) (doc.insertions.val[i]).left.val
          (doc.insertions.val[i]).right.val ≠ [] ∧ (d, i) ∉ doc.acked.val.map pairVal ∧
          concurrentM doc.deps.val (doc.deletions.val[d]).patch (doc.insertions.val[i]).patch
      then some (d, i, sharedM ((doc.deletions.val[d]).targets.val.map (·.val))
        (doc.insertions.val[i]).left.val (doc.insertions.val[i]).right.val)
      else none := by
  unfold conflictAt
  rw [List.getElem?_eq_getElem hd, List.getElem?_eq_getElem hi]

theorem deletion_conflicts_spec (out : alloc.vec.Vec document.DeleteInsert) (doc : document.Doc) (d : Usize)
    (wf : MatWF doc) (hd : d.val < doc.deletions.val.length)
    (room : out.val.length + doc.insertions.val.length < Usize.max) :
    document.deletion_conflicts out doc d ⦃ r =>
      r.val.map diVal = out.val.map diVal ++ conflictsOf doc d.val ⦄ := by
  unfold document.deletion_conflicts document.deletion_conflicts_loop
  have hpatch : (doc.deletions.val[d.val]).patch.val < doc.deps.val.length :=
    wf.delPatch _ (List.getElem_mem _)
  h5i_step
  apply WP.spec_mono (loop_fold (List.range doc.insertions.val.length)
    (fun o : alloc.vec.Vec document.DeleteInsert => o.val.map diVal)
    (fun acc i => acc ++ (conflictAt doc d.val i).toList)
    (fun o j => o.val.length ≤ out.val.length + j)
    (fun st => document.deletion_conflicts_loop.body doc.deps doc.insertions doc.deletions doc.acked d
      (doc.deletions.val[d.val]).patch st.1 st.2) ?_ out 0#usize (by simp) (by simp))
  · intro r h
    rw [h, show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, foldl_toList]
    simp [conflictsOf]
  · intro o i hi hinv
    unfold document.deletion_conflicts_loop.body
    h5i_step
    all_goals first
      | (refine ⟨?_, by scalar_tac, by omega⟩
         rw [conflictAt_some doc hd (by scalar_tac), if_neg]
         rintro ⟨-, hn, -⟩; apply hn; simp only [List.mem_map, Prod.exists]; assumption)
      | (refine ⟨?_, by scalar_tac, by omega⟩
         rw [conflictAt_some doc hd (by scalar_tac), if_neg]
         rintro ⟨h, -, -⟩; exact h anchors_post)
      | (apply WP.spec_bind (concurrent_spec doc.deps _ _ wf.room hpatch
           (wf.insPatch _ (List.getElem_mem _)) wf.deps)
         intro x hx
         h5i_step
         all_goals first
           | (refine ⟨?_, by scalar_tac, by omega⟩
              rw [conflictAt_some doc hd (by scalar_tac), if_neg]
              rintro ⟨-, -, h⟩; exact hx h)
           | (refine ⟨?_, by scalar_tac, by omega⟩
              have hi' : i.val < doc.insertions.val.length := by scalar_tac
              have hc : concurrentM doc.deps.val (doc.deletions.val[d.val]).patch
                  (doc.insertions.val[i.val]).patch := hx
              have hne : sharedM ((doc.deletions.val[d.val]).targets.val.map (·.val))
                  (doc.insertions.val[i.val]).left.val (doc.insertions.val[i.val]).right.val ≠ [] := by
                rw [← anchors_post]
                intro h0
                have : anchors.val = [] := List.map_eq_nil_iff.mp h0
                have hl0 : anchors.val.length = 0 := by rw [this]; rfl
                omega
              have hna : (d.val, i.val) ∉ doc.acked.val.map pairVal := by
                simp only [List.mem_map, Prod.exists, not_exists, not_and]; assumption
              rw [conflictAt_some doc hd hi', if_pos ⟨hne, hna, hc⟩]
              simp [diVal, anchors_post])
          )

theorem foldl_append_flatMap {α β} (f : α → List β) (l : List α) (init : List β) :
    l.foldl (fun acc d => acc ++ f d) init = init ++ l.flatMap f := by
  induction l generalizing init with
  | nil => simp
  | cons x l ih => rw [List.foldl_cons, ih]; simp

theorem conflictsOf_length (doc : document.Doc) (d : Nat) :
    (conflictsOf doc d).length ≤ doc.insertions.val.length := by
  unfold conflictsOf
  exact le_trans (List.length_filterMap_le _ _) (by simp)

theorem delete_inserts_spec (doc : document.Doc) (wf : MatWF doc) :
    document.delete_inserts doc ⦃ out => out.val.map diVal = deleteInsertsM doc ⦄ := by
  unfold document.delete_inserts document.delete_inserts_loop
  apply WP.spec_mono (loop_fold (List.range doc.deletions.val.length)
    (fun o : alloc.vec.Vec document.DeleteInsert => o.val.map diVal)
    (fun acc d => acc ++ (if doc.restored.val[d]?.getD false then [] else conflictsOf doc d))
    (fun o j => o.val.length ≤ j * doc.insertions.val.length)
    (fun st => document.delete_inserts_loop.body doc st.1 st.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r h
    rw [h, show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, foldl_append_flatMap]
    simp [deleteInsertsM]
  · intro o d hd hinv
    unfold document.delete_inserts_loop.body
    have hres := wf.restored
    have hout := wf.out
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (apply WP.spec_bind (deletion_conflicts_spec o doc d wf (by scalar_tac) (by
           have : d.val + 1 ≤ doc.deletions.val.length := by scalar_tac
           have h2 : (d.val + 1) * doc.insertions.val.length ≤
               doc.deletions.val.length * doc.insertions.val.length := Nat.mul_le_mul_right _ this
           rw [Nat.succ_mul] at h2
           have h3 : o.val.length ≤ d.val * doc.insertions.val.length := hinv
           omega))
         intro r hr
         h5i_step
         all_goals first
           | (have hd' : d.val < doc.restored.val.length := by scalar_tac
              have hb : (doc.restored.val[d.val]?.getD false) = false := by
                rw [List.getElem?_eq_getElem hd']; simpa using b_post
              have hl := congrArg List.length hr
              simp only [List.length_map, List.length_append] at hl
              have := conflictsOf_length doc d.val
              refine ⟨fun h => (by rw [hb] at h; cases h), by scalar_tac, ?_⟩
              have key : o.val.length + (conflictsOf doc d.val).length ≤ (d.val + 1) * doc.insertions.val.length :=
                calc o.val.length + (conflictsOf doc d.val).length
                    ≤ d.val * doc.insertions.val.length + doc.insertions.val.length := Nat.add_le_add hinv this
                  _ = (d.val + 1) * doc.insertions.val.length := (Nat.succ_mul _ _).symm
              omega)
          )
      | (have hd' : d.val < doc.restored.val.length := by scalar_tac
         have hb : (doc.restored.val[d.val]?.getD false) = true := by
           rw [List.getElem?_eq_getElem hd']; simpa using b_post
         refine ⟨fun h => (by rw [hb] at h; cases h), by scalar_tac, ?_⟩
         calc o.val.length ≤ d.val * doc.insertions.val.length := hinv
           _ ≤ (d.val + 1) * doc.insertions.val.length := Nat.mul_le_mul_right _ (by omega))

/-! ## Edge rows -/

theorem mem_addL_getD (rows : List (List Usize)) (a w : Usize) (u : Nat) (hu : u < rows.length)
    (ha : a.val < rows.length) (v : Usize) :
    v ∈ (addL rows a w)[u]?.getD [] ↔ v ∈ rows[u]?.getD [] ∨ (a.val = u ∧ v = w) := by
  unfold addL
  rw [List.getElem?_set]
  by_cases h : a.val = u
  · subst h
    simp [ha, List.getElem?_eq_getElem ha, getElem!_pos rows a.val ha]
  · simp [h]

open Classical in
theorem allStep_mem (edges : List (Usize × Usize × Usize)) (off : List Bool) (l : List Nat)
    (rows0 : List (List Usize)) (hb : ∀ e ∈ edges, e.1.val < rows0.length) (u : Nat)
    (hu : u < rows0.length) (v : Usize) :
    v ∈ (l.foldl (allStep edges off) rows0)[u]?.getD [] ↔
      v ∈ rows0[u]?.getD [] ∨ ∃ i ∈ l, ∃ h : liveEdge edges off i,
        (edges[i]'h.1).1.val = u ∧ (edges[i]'h.1).2.1 = v := by
  induction l generalizing rows0 with
  | nil => simp
  | cons i l ih =>
    rw [List.foldl_cons]
    have hlen : (allStep edges off rows0 i).length = rows0.length := by
      unfold allStep; split_ifs <;> simp [addL_length]
    rw [ih (allStep edges off rows0 i) (by rw [hlen]; exact hb) (by rw [hlen]; exact hu)]
    unfold allStep
    split_ifs with hl
    · rw [mem_addL_getD rows0 _ _ u hu (hb _ (List.getElem_mem _)) v]
      constructor
      · rintro ((h | ⟨h1, h2⟩) | ⟨j, hj, hl', h1, h2⟩)
        · exact .inl h
        · exact .inr ⟨i, by simp, hl, h1, h2.symm⟩
        · exact .inr ⟨j, by simp [hj], hl', h1, h2⟩
      · rintro (h | ⟨j, hj, hl', h1, h2⟩)
        · exact .inl (.inl h)
        · rcases List.mem_cons.mp hj with rfl | hj
          · exact .inl (.inr ⟨h1, h2.symm⟩)
          · exact .inr ⟨j, hj, hl', h1, h2⟩
    · constructor
      · rintro (h | ⟨j, hj, hl', h1, h2⟩)
        · exact .inl h
        · exact .inr ⟨j, by simp [hj], hl', h1, h2⟩
      · rintro (h | ⟨j, hj, hl', h1, h2⟩)
        · exact .inl h
        · rcases List.mem_cons.mp hj with rfl | hj
          · exact absurd hl' hl
          · exact .inr ⟨j, hj, hl', h1, h2⟩

open Classical in
theorem allStep_fold_length (edges : List (Usize × Usize × Usize)) (off : List Bool) (l : List Nat)
    (rows0 : List (List Usize)) : (l.foldl (allStep edges off) rows0).length = rows0.length := by
  induction l generalizing rows0 with
  | nil => rfl
  | cons i l ih => rw [List.foldl_cons, ih]; unfold allStep; split_ifs <;> simp [addL_length]

/-- Edge `u → v` of the kernel's edge table that no retraction withdraws. -/
def AliveEdge (doc : document.Doc) (u : Nat) (v : Usize) : Prop :=
  ∃ i, ∃ h : i < doc.edges.val.length, ¬ (∃ x ∈ doc.retractions.val, x.1.val = i) ∧
    (doc.edges.val[i]).1.val = u ∧ (doc.edges.val[i]).2.1 = v

/-- The rows of `all_rows`: row `u` holds the targets of the live edges out of `u`. -/
theorem all_rows_spec (doc : document.Doc)
    (hb : ∀ e ∈ doc.edges.val, e.1.val < doc.atoms.val.length)
    (hr : ∀ x ∈ doc.retractions.val, x.1.val < doc.edges.val.length)
    (room : doc.edges.val.length < Usize.max) :
    document.all_rows doc ⦃ rows => rows.val.length = doc.atoms.val.length ∧
      ∀ (u : Nat) (hu : u < rows.val.length) (v : Usize), v ∈ (rows.val[u]).val ↔ AliveEdge doc u v ⦄ := by
  unfold document.all_rows
  h5i_step
  all_goals first
    | (intro x hx; exact hr x hx)
    | (intro e he; simp at he)
    | skip
  all_goals first
    | (have hb' : ∀ e ∈ doc.edges.val, e.1.val < rows.val.length := by
         intro e he; rw [rows_post, List.length_replicate]
         first | exact hb e he | exact hb _ _ _ he
       have hempty : ∀ row ∈ rows.val, row.val = [] := by
         intro row h; rw [rows_post] at h; rw [List.eq_of_mem_replicate h]; rfl
       apply WP.spec_mono (all_rows_loop_spec doc.edges off rows (by rw [off_post]) hb' hempty room)
       rintro r ⟨hr1, -⟩
       have hlen : r.val.length = rows.val.length := by
         have := congrArg List.length hr1
         simp only [rowsL, List.length_map, allStep_fold_length] at this
         exact this
       have hrows0 : rowsL rows = List.replicate doc.atoms.val.length [] := by
         simp [rowsL, rows_post]
       refine ⟨by rw [hlen, rows_post, List.length_replicate], fun u hu v => ?_⟩
       have hu0 : u < (rowsL rows).length := by rw [hrows0, List.length_replicate]; rw [hlen, rows_post] at hu; simpa using hu
       have e1 : (r.val[u]).val = (rowsL r)[u]?.getD [] := by
         simp [rowsL, List.getElem?_map, List.getElem?_eq_getElem hu]
       rw [e1, hr1, allStep_mem _ _ _ _ (by rw [hrows0, List.length_replicate]; intro e he; exact (by
         first | exact hb e he | exact hb _ _ _ he)) u hu0 v]
       rw [hrows0]
       have hnil : (List.replicate doc.atoms.val.length ([] : List Usize))[u]?.getD [] = [] := by
         rw [List.getElem?_replicate]; split_ifs <;> rfl
       rw [hnil]
       unfold AliveEdge
       constructor
       · rintro (h | ⟨i, -, ⟨hi', hoff⟩, h1, h2⟩)
         · simp at h
         · refine ⟨i, hi', fun hw => ?_, h1, h2⟩
           obtain ⟨x, hx, hx1⟩ := hw
           have ht := (off_post1 i hi').mpr ⟨x.1, ⟨x.2, by simpa using hx⟩, hx1⟩
           rw [List.getElem?_eq_getElem (by rw [off_post]; exact hi'), ht] at hoff
           cases hoff
       · rintro ⟨i, hi', hw, h1, h2⟩
         refine .inr ⟨i, List.mem_range.mpr hi', ⟨hi', ?_⟩, h1, h2⟩
         rw [List.getElem?_eq_getElem (by rw [off_post]; exact hi')]
         cases hc : (off.val[i]'(by rw [off_post]; exact hi'))
         · rfl
         · exfalso; apply hw
           obtain ⟨a, ⟨x, hx⟩, ha⟩ := (off_post1 i hi').mp hc
           exact ⟨(a, x), hx, ha⟩)

end o5s_kernel.DocKernel

namespace o5s_kernel.DocKahn

/-- `v'` is `v` with one increment at each vertex of `l`. -/
def AddRel (v' v : List Usize) (l : List Nat) : Prop :=
  ∀ w, valAt v' w = valAt v w + if w ∈ l then 1 else 0

theorem addrel_step (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : AddRel v' v (segL rv t k))
    (h3 : (rv[k]).val < v'.length) (x : Usize) (hx : x.val = (v'[(rv[k]).val]'h3).val + 1) :
    AddRel (v'.set (rv[k]).val x) v (segL rv t (k+1)) := by
  intro w
  rw [valAt_set, segL_succ rv htk hk]
  have hnot := segL_not_mem rv htk hk nd
  by_cases hw : (rv[k]).val = w
  · subst hw
    have e : valAt v' (rv[k]).val = (v'[(rv[k]).val]'h3).val := by
      simp [valAt, List.getElem?_eq_getElem h3]
    have := hval (rv[k]).val
    rw [if_neg hnot, Nat.add_zero] at this
    simp only [h3, and_self, if_true, List.mem_append, List.mem_singleton, or_true, hx]
    omega
  · have hw' : w ≠ (rv[k]).val := fun e => hw e.symm
    simp only [hw, false_and, if_false, List.mem_append, List.mem_singleton, hw', or_false]
    exact hval w

/-- The inner loop of `materialize_loop1`: one increment per boundary vertex. -/
@[step] theorem bump_spec (q v : alloc.vec.Vec Usize) (t : Usize) (ht : t.val ≤ q.val.length)
    (nd : ((q.val.map (·.val)).drop t.val).Nodup)
    (hb : ∀ w ∈ (q.val.map (·.val)).drop t.val, w < v.val.length ∧ valAt v.val w + 1 ≤ Usize.max) :
    document.materialize_loop1_loop0 v q t ⦃ out =>
      out.val.length = v.val.length ∧ AddRel out.val v.val ((q.val.map (·.val)).drop t.val) ⦄ := by
  unfold document.materialize_loop1_loop0
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Usize × Usize => q.val.length - st.2.val)
    (inv := fun st => t.val ≤ st.2.val ∧ st.2.val ≤ q.val.length ∧ st.1.val.length = v.val.length ∧
      AddRel st.1.val v.val (segL q.val t.val st.2.val))
  · rintro ⟨v', k⟩ ⟨htk, hkl, hlen, hval⟩
    simp only at htk hkl hlen hval
    unfold document.materialize_loop1_loop0.body
    h5i_step
    all_goals first
      | (rw [hlen]; exact (hb _ (mem_drop_of q.val htk (by scalar_tac))).1)
      | (have hk' : k.val < q.val.length := by scalar_tac
         have h3 : (q.val[k.val]).val < v'.val.length := by rw [hlen]; exact (hb _ (mem_drop_of q.val htk hk')).1
         have e : valAt v'.val (q.val[k.val]).val = valAt v.val (q.val[k.val]).val := by
           rw [hval, if_neg (segL_not_mem q.val htk hk' nd), Nat.add_zero]
         have h4 := (hb _ (mem_drop_of q.val htk hk')).2
         rw [← e] at h4
         simp only [valAt, List.getElem?_eq_getElem h3, Option.map_some, Option.getD_some] at h4
         scalar_tac)
      | (subst_vars
         have hk' : k.val < q.val.length := by scalar_tac
         have h3 : (q.val[k.val]).val < v'.val.length := by rw [hlen]; exact (hb _ (mem_drop_of q.val htk hk')).1
         first
           | exact ⟨by omega, by omega, by simp [hlen], addrel_step _ htk hk' _ _ nd hval h3 _ (by scalar_tac), by omega⟩
           | exact ⟨by omega, hk', addrel_step _ htk hk' _ _ nd hval h3 _ (by scalar_tac), by omega⟩)
      | (rw [segL_full _ (by scalar_tac)] at hval; first | exact ⟨hlen, hval⟩ | exact hval)
  · exact ⟨le_refl _, ht, rfl, fun w => by simp [segL]⟩

open Boundary Indexed in
/-- Row `u` of `boundaries`: the live atoms reached from `u` through dead interiors. -/
def BRow (live : List Bool) (edges : List (Usize × Usize)) (u : Usize) (q : List Usize) : Prop :=
  (q.map (·.val)).Nodup ∧
  ∀ v : Usize, v ∈ q ↔ alive live v = true ∧ Projection.Segment edges (fun x => alive live x = true) u v

/-- Live predecessors among the first `s` rows. -/
def cntUpTo (live : List Bool) (bnd : List (alloc.vec.Vec Usize)) (s w : Nat) : Nat :=
  ((Finset.range s).filter (fun u => (live[u]?.getD false) = true ∧ w ∈ rowN bnd u)).card

open Boundary Indexed in
/-- The state of `materialize_loop1` after the first `s` atoms. -/
structure L1Inv (live : List Bool) (edges : List (Usize × Usize)) (n s : Nat)
    (marks : List Bool) (bnd : List (alloc.vec.Vec Usize)) (indeg : List Usize) (rem : List Usize)
    (left : Nat) : Prop where
  mlen : marks.length = n
  clean : Clean marks
  blen : bnd.length = s
  rows : ∀ u (h : u < bnd.length), (live[u]?.getD false = true →
    ∃ uu : Usize, uu.val = u ∧ BRow live edges uu (bnd[u]).val) ∧
    (live[u]?.getD false = false → (bnd[u]).val = [])
  ilen : indeg.length = n
  deg : ∀ w, valAt indeg w = cntUpTo live bnd s w
  rem : rem.map (·.val) = (List.range s).filter (fun u => live[u]?.getD false)
  left : left = ((List.range s).filter (fun u => live[u]?.getD false)).length

theorem rowN_append_lt (bnd : List (alloc.vec.Vec Usize)) (x : alloc.vec.Vec Usize) {u : Nat}
    (h : u < bnd.length) : rowN (bnd ++ [x]) u = rowN bnd u := by
  simp [rowN, List.getElem?_append_left h]

theorem rowN_append_eq (bnd : List (alloc.vec.Vec Usize)) (x : alloc.vec.Vec Usize) :
    rowN (bnd ++ [x]) bnd.length = x.val.map (·.val) := by
  simp [rowN]

theorem cntUpTo_succ (live : List Bool) (bnd : List (alloc.vec.Vec Usize)) (x : alloc.vec.Vec Usize)
    (w : Nat) :
    cntUpTo live (bnd ++ [x]) (bnd.length + 1) w = cntUpTo live bnd bnd.length w +
      if live[bnd.length]?.getD false = true ∧ w ∈ x.val.map (·.val) then 1 else 0 := by
  unfold cntUpTo
  rw [Finset.range_add_one, Finset.filter_insert]
  have e : (Finset.range bnd.length).filter (fun u => (live[u]?.getD false) = true ∧ w ∈ rowN (bnd ++ [x]) u)
      = (Finset.range bnd.length).filter (fun u => (live[u]?.getD false) = true ∧ w ∈ rowN bnd u) := by
    apply Finset.filter_congr
    intro u hu
    rw [rowN_append_lt bnd x (Finset.mem_range.mp hu)]
  rw [rowN_append_eq] at *
  split_ifs with h
  · rw [Finset.card_insert_of_notMem (by simp), e]
  · rw [e]; simp

theorem l1_live {live : List Bool} {edges : List (Usize × Usize)} {n s : Nat} {marks : List Bool}
    {bnd : List (alloc.vec.Vec Usize)} {indeg rem : List Usize} {left : Nat}
    (hI : L1Inv live edges n s marks bnd indeg rem left) (hl : live[s]?.getD false = true)
    (ss : Usize) (hss : ss.val = s) (qv : alloc.vec.Vec Usize) (hq : BRow live edges ss qv.val)
    (marks' : List Bool) (hm : marks'.length = n) (hc : Indexed.Clean marks')
    (indeg' : List Usize) (hil : indeg'.length = indeg.length)
    (hadd : AddRel indeg' indeg (qv.val.map (·.val))) :
    L1Inv live edges n (s+1) marks' (bnd ++ [qv]) indeg' (rem ++ [ss]) (left + 1) := by
  have hb := hI.blen
  refine ⟨hm, hc, by simp [hb], ?_, by rw [hil, hI.ilen], ?_, ?_, ?_⟩
  · intro u hu
    by_cases hus : u < bnd.length
    · rw [List.getElem_append_left hus]; exact hI.rows u hus
    · have : u = bnd.length := by simp at hu; omega
      subst this
      rw [List.getElem_append_right (le_refl _)]
      simp only [Nat.sub_self, List.getElem_cons_zero]
      refine ⟨fun _ => ⟨ss, by rw [hss, hb], hq⟩, fun h => ?_⟩
      rw [hb] at h; rw [h] at hl; cases hl
  · intro w
    rw [hadd, hI.deg, ← hb, cntUpTo_succ, hb, hl]
    simp
  · rw [List.map_append, hI.rem, List.range_succ, List.filter_append]; simp [hss, hl]
  · rw [hI.left, List.range_succ, List.filter_append]; simp [hl]

theorem l1_dead {live : List Bool} {edges : List (Usize × Usize)} {n s : Nat} {marks : List Bool}
    {bnd : List (alloc.vec.Vec Usize)} {indeg rem : List Usize} {left : Nat}
    (hI : L1Inv live edges n s marks bnd indeg rem left) (hl : live[s]?.getD false = false)
    (qv : alloc.vec.Vec Usize) (hq : qv.val = []) :
    L1Inv live edges n (s+1) marks (bnd ++ [qv]) indeg rem left := by
  have hb := hI.blen
  refine ⟨hI.mlen, hI.clean, by simp [hb], ?_, hI.ilen, ?_, ?_, ?_⟩
  · intro u hu
    by_cases hus : u < bnd.length
    · rw [List.getElem_append_left hus]; exact hI.rows u hus
    · have : u = bnd.length := by simp at hu; omega
      subst this
      rw [List.getElem_append_right (le_refl _)]
      simp only [Nat.sub_self, List.getElem_cons_zero]
      refine ⟨fun h => ?_, fun _ => hq⟩
      rw [hb] at h; rw [h] at hl; cases hl
  · intro w
    rw [hI.deg, ← hb, cntUpTo_succ, hb, hl]
    simp
  · rw [hI.rem, List.range_succ, List.filter_append]; simp [hl]
  · rw [hI.left, List.range_succ, List.filter_append]; simp [hl]

theorem cntUpTo_le (live : List Bool) (bnd : List (alloc.vec.Vec Usize)) (s w : Nat) :
    cntUpTo live bnd s w ≤ s := by
  unfold cntUpTo
  exact le_trans (Finset.card_filter_le _ _) (by simp)

theorem nodup_map_val {l : List Usize} (h : l.Nodup) : (l.map (·.val)).Nodup :=
  h.map (fun _ _ e => UScalar.eq_of_val_eq e)

theorem alive_lt {live : List Bool} {v : Usize} (h : Boundary.alive live v = true) : v.val < live.length := by
  unfold Boundary.alive at h
  by_contra hc
  rw [List.getElem?_eq_none_iff.mpr (by omega)] at h
  simp at h

open Boundary Indexed in
/-- `materialize_loop1`: the boundary rows of the live atoms, their predecessor
counts, and the live atoms in order. -/
theorem loop1_spec (live : alloc.vec.Vec Bool) (rows dead : alloc.vec.Vec (alloc.vec.Vec Usize))
    (n : Usize) (edges : List (Usize × Usize))
    (hn : live.val.length = n.val) (hrl : rows.val.length = n.val) (hdl : dead.val.length = n.val)
    (hnm : n.val < Usize.max)
    (seed_b : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < n.val)
    (dead_b : ∀ row ∈ dead.val, ∀ v ∈ row.val, v.val < n.val)
    (seed_eq : ∀ (s : Usize) (h : s.val < rows.val.length) (v : Usize),
      v ∈ (rows.val[s.val]).val ↔ (s, v) ∈ edges)
    (row_eq : ∀ u v : Usize, (∃ h : u.val < dead.val.length, v ∈ (dead.val[u.val]).val) ↔
      (u, v) ∈ edges ∧ ¬ alive live.val u = true)
    (marks : alloc.vec.Vec Bool) (bnd : alloc.vec.Vec (alloc.vec.Vec Usize))
    (indeg rem : alloc.vec.Vec Usize) (left s : Usize) (hs : s.val ≤ n.val)
    (hI : L1Inv live.val edges n.val s.val marks.val bnd.val indeg.val rem.val left.val) :
    document.materialize_loop1 live rows dead n marks bnd indeg rem left s ⦃ out =>
      ∃ m, L1Inv live.val edges n.val n.val m out.1.val out.2.1.val out.2.2.1.val out.2.2.2.val ⦄ := by
  have hseg : ∀ (s : Usize) (h : s.val < rows.val.length) (v : Usize),
      RowReach dead.val (rows.val[s.val]).val v ↔
        Projection.Segment edges (fun x => alive live.val x = true) s v :=
    fun s h v => row_segment edges _ dead.val _ s (seed_eq s h) row_eq v
  unfold document.materialize_loop1
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Bool × alloc.vec.Vec (alloc.vec.Vec Usize) × alloc.vec.Vec Usize ×
      alloc.vec.Vec Usize × Usize × Usize => n.val - st.2.2.2.2.2.val)
    (inv := fun st => st.2.2.2.2.2.val ≤ n.val ∧ L1Inv live.val edges n.val st.2.2.2.2.2.val st.1.val
      st.2.1.val st.2.2.1.val st.2.2.2.1.val st.2.2.2.2.1.val)
  · rintro ⟨mk, bd, ind, rm, lf, ss⟩ ⟨hss, hJ⟩
    simp only at hss hJ
    have hrml : rm.val.length ≤ ss.val := by
      have := congrArg List.length hJ.rem
      simp only [List.length_map] at this
      rw [this]; exact le_trans (List.length_filter_le _ _) (by simp)
    have hlfl : lf.val ≤ ss.val := by
      rw [hJ.left]; exact le_trans (List.length_filter_le _ _) (by simp)
    have hbdl := hJ.blen
    have hcnt : ∀ w, valAt ind.val w ≤ ss.val := fun w => by rw [hJ.deg]; exact cntUpTo_le _ _ _ _
    unfold document.materialize_loop1.body
    h5i_step
    all_goals first
      | exact hJ.mlen
      | exact hJ.clean
      | exact seed_b _ (List.getElem_mem _)
      | (apply nodup_map_val; assumption)
      | (intro w x _ ha hx; subst hx
         exact ⟨by rw [hJ.ilen, ← hn]; exact alive_lt ha, by have := hcnt x.val; omega⟩)
      | (refine ⟨by scalar_tac, ?_, by scalar_tac⟩
         have hl : live.val[ss.val]?.getD false = true := by
           rw [List.getElem?_eq_getElem (by scalar_tac)]; simpa using b_post
         exact l1_live hJ hl ss rfl t.queue
           ⟨nodup_map_val t_post, fun v => by rw [t_post1, hseg ss (by scalar_tac) v]; exact and_comm⟩
           _ t_post2 t_post3 _ indegree1_post indegree1_post1)
      | (refine ⟨by scalar_tac, ?_, by scalar_tac⟩
         have hl : live.val[ss.val]?.getD false = false := by
           rw [List.getElem?_eq_getElem (by scalar_tac)]; simpa using b_post
         exact l1_dead hJ hl _ (by simp))
      | (have e : ss.val = n.val := by scalar_tac
         exact ⟨_, e ▸ hJ⟩)
      | scalar_tac
  · exact ⟨hs, hI⟩

/-! ## The small loops of `materialize` -/

theorem loop0_spec (n : Usize) :
    document.materialize_loop0 n (alloc.vec.Vec.new Usize) 0#usize ⦃ out =>
      out.val = List.replicate n.val 0#usize ⦄ := by
  unfold document.materialize_loop0
  apply WP.spec_mono (loop_fold (List.range n.val) (fun o : alloc.vec.Vec Usize => o.val)
    (fun o _ => o ++ [0#usize]) (fun o j => o.val.length = j ∧ j ≤ n.val)
    (fun st => document.materialize_loop0.body n st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro out h; rw [h, DocKernel.foldl_snoc_const]; simp
  · intro o i hi hinv
    unfold document.materialize_loop0.body
    h5i_step
    all_goals simp_all
    all_goals scalar_tac

theorem foldl_filter_snoc (p : Nat → Bool) (l : List Nat) (acc : List Nat) :
    l.foldl (fun a x => if p x then a ++ [x] else a) acc = acc ++ l.filter p := by
  induction l generalizing acc with
  | nil => simp
  | cons x l ih => rw [List.foldl_cons, ih]; by_cases h : p x <;> simp [h, List.filter_cons]

/-- `materialize_loop3`: the remaining atoms, when the Kahn loop reported a block. -/
theorem loop3_spec (n : Usize) (v : alloc.vec.Vec Bool) (b : Bool) (hv : v.val.length = n.val) :
    document.materialize_loop3 n v b (alloc.vec.Vec.new Usize) 0#usize ⦃ out =>
      out.val.map (·.val) = (List.range n.val).filter (fun x => b && remF v.val x) ⦄ := by
  unfold document.materialize_loop3
  apply WP.spec_mono (loop_fold (List.range n.val) (fun o : alloc.vec.Vec Usize => o.val.map (·.val))
    (fun o x => if b && remF v.val x then o ++ [x] else o) (fun o j => o.val.length ≤ j ∧ j ≤ n.val)
    (fun st => document.materialize_loop3.body n v b st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro out h; rw [h, show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, foldl_filter_snoc]; simp
  · intro o i hi hinv
    unfold document.materialize_loop3.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (simp only [FoldStep]
         have hi' : i.val < v.val.length := by scalar_tac
         refine ⟨by simp; scalar_tac, ?_, by scalar_tac, by simp_all; scalar_tac⟩
         simp_all [remF, List.getElem?_eq_getElem hi'])
      | (simp only [FoldStep]
         refine ⟨by simp; scalar_tac, ?_, by scalar_tac, by simp_all; scalar_tac⟩
         simp_all)
      | (have hi' : i.val < v.val.length := by scalar_tac
         exact ⟨by unfold remF; rw [List.getElem?_eq_getElem hi']; simpa, by scalar_tac⟩)
      | (have hi' : i.val < v.val.length := by scalar_tac
         exact ⟨by unfold remF; rw [List.getElem?_eq_getElem hi']; simpa, by scalar_tac, by omega⟩)

/-- `materialize_loop4`: every layer is a single atom. -/
theorem loop4_spec (v : alloc.vec.Vec (alloc.vec.Vec Usize)) :
    document.materialize_loop4 v true 0#usize ⦃ out =>
      out = v.val.all (fun L => L.val.length = 1) ⦄ := by
  unfold document.materialize_loop4
  apply WP.spec_mono (loop_fold v.val (fun o : Bool => o) (fun o L => o && decide (L.val.length = 1))
    (fun _ _ => True) (fun st => document.materialize_loop4.body v st.1 st.2) ?_ true 0#usize
    (by simp) trivial)
  · intro out h; rw [h, show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, DocKernel.foldl_band]; simp
  · intro o i hi _
    unfold document.materialize_loop4.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (simp only [FoldStep]; refine ⟨by scalar_tac, ?_, by scalar_tac, trivial⟩; simp_all)
      | (refine ⟨by scalar_tac, ?_⟩
         constructor <;> intro h
         · have := congrArg UScalar.val h; simpa using this
         · apply UScalar.eq_of_val_eq; simpa using h)

end o5s_kernel.DocKahn
