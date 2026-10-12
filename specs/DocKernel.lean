import Indexed
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Specifications of the document kernel's helpers

Each theorem gives an extracted helper of `crates/kernel/src/document.rs` a
list-level meaning; `Document.lean` builds the refinement on them.
-/
namespace o5s_kernel.DocKernel

theorem foldl_snoc_const {α β} (x : β) (l : List α) (acc : List β) :
    l.foldl (fun o _ => o ++ [x]) acc = acc ++ List.replicate l.length x := by
  induction l generalizing acc with
  | nil => simp
  | cons a l ih =>
    rw [List.foldl_cons, ih, List.length_cons, List.append_assoc]
    rfl

theorem foldl_band {α} (p : α → Bool) (l : List α) (b : Bool) :
    l.foldl (fun b x => b && p x) b = (b && l.all p) := by
  induction l generalizing b with
  | nil => simp
  | cons a l ih => rw [List.foldl_cons, ih]; simp [Bool.and_assoc]

@[step] theorem falses_spec (n : Usize) :
    document.falses n ⦃ out => out.val = List.replicate n.val false ⦄ := by
  unfold document.falses document.falses_loop
  apply WP.spec_mono (loop_fold (List.range n.val) (fun o : alloc.vec.Vec Bool => o.val)
    (fun o _ => o ++ [false]) (fun o j => o.val.length = j ∧ j ≤ n.val)
    (fun st => document.falses_loop.body n st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro out h; rw [h, foldl_snoc_const]; simp
  · intro o i hi hinv
    unfold document.falses_loop.body
    h5i_step
    all_goals simp_all
    all_goals scalar_tac

@[step] theorem empty_rows_spec (n : Usize) :
    document.empty_rows n ⦃ out => out.val = List.replicate n.val (alloc.vec.Vec.new Usize) ⦄ := by
  unfold document.empty_rows document.empty_rows_loop
  apply WP.spec_mono (loop_fold (List.range n.val) (fun o : alloc.vec.Vec (alloc.vec.Vec Usize) => o.val)
    (fun o _ => o ++ [alloc.vec.Vec.new Usize]) (fun o j => o.val.length = j ∧ j ≤ n.val)
    (fun st => document.empty_rows_loop.body n st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro out h; rw [h, foldl_snoc_const]; simp
  · intro o i hi hinv
    unfold document.empty_rows_loop.body
    h5i_step
    all_goals simp_all
    all_goals scalar_tac

/-- What `check_ref` returns. -/
def checkRefM (r : document.Ref) (seen : List Bool) (count : Nat) (missing : U64) : U64 :=
  if r.patch = document.NONE then document.UNOBSERVED_PATCH
  else if h : r.patch.val < seen.length then
    if seen[r.patch.val] then
      (if r.index = document.NONE ∨ count ≤ r.index.val then missing else 0#u64)
    else document.UNOBSERVED_PATCH
  else document.UNOBSERVED_PATCH

@[step] theorem check_ref_spec (r : document.Ref) (seen : alloc.vec.Vec Bool) (count : Usize)
    (missing : U64) :
    document.check_ref r seen count missing ⦃ k => k = checkRefM r seen.val count.val missing ⦄ := by
  unfold document.check_ref checkRefM
  h5i_step
  all_goals first
    | (simp_all; done)
    | (split_ifs <;> simp_all <;> scalar_tac)
    | (trace_state; sorry)

theorem unknown_order_ne : document.UNKNOWN_ORDER.val ≠ 0 := by
  unfold document.UNKNOWN_ORDER; simp

theorem unknown_anchor_ne : document.UNKNOWN_ANCHOR.val ≠ 0 := by
  unfold document.UNKNOWN_ANCHOR; simp

theorem checkRefM_zero {r : document.Ref} {seen : List Bool} {count : Nat} {missing : U64}
    (hm : missing.val ≠ 0) (h : (checkRefM r seen count missing).val = 0) :
    r.index.val < count ∧ r.index ≠ document.NONE ∧ r.patch ≠ document.NONE ∧
      ∃ hp : r.patch.val < seen.length, seen[r.patch.val] = true := by
  unfold checkRefM at h
  split_ifs at h with h1 h2 h3 h4
  all_goals first
    | (simp [document.UNOBSERVED_PATCH] at h; done)
    | exact absurd h hm
    | (push_neg at h4; exact ⟨by omega, h4.1, h1, h2, h3⟩)

def anchorIdx : document.Anchor → Nat
  | .Start => 0
  | .End => 1
  | .Node r => r.index.val

@[step] theorem anchor_index_spec (a : document.Anchor) :
    document.anchor_index a ⦃ i => i.val = anchorIdx a ⦄ := by
  cases a <;> simp [document.anchor_index, anchorIdx, document.START, document.END]

def checkAnchorM (a : document.Anchor) (atoms : Nat) (seen : List Bool) : U64 :=
  match a with
  | .Node r =>
    let k := checkRefM r seen atoms document.UNKNOWN_ANCHOR
    if k = 0#u64 ∧ r.index.val < 2 then document.UNKNOWN_ANCHOR else k
  | _ => 0#u64

@[step] theorem check_anchor_spec (a : document.Anchor) (doc : document.Doc) (seen : alloc.vec.Vec Bool) :
    document.check_anchor a doc seen ⦃ k => k = checkAnchorM a doc.atoms.val.length seen.val ⦄ := by
  cases a with
  | Node r =>
    unfold document.check_anchor checkAnchorM
    h5i_step
    all_goals simp_all
    all_goals trace_state
    all_goals sorry
  | _ => simp [document.check_anchor, checkAnchorM]

@[step] theorem atoms_nonempty_spec (atoms : alloc.vec.Vec (alloc.vec.Vec U8)) :
    document.atoms_nonempty atoms ⦃ b => b = true ↔ atoms.val ≠ [] ∧ ∀ x ∈ atoms.val, x.val ≠ [] ⦄ := by
  unfold document.atoms_nonempty document.atoms_nonempty_loop
  apply WP.spec_mono (loop_fold atoms.val (fun b : Bool => b)
    (fun b x => b && decide (x.val ≠ [])) (fun _ _ => True)
    (fun st => document.atoms_nonempty_loop.body atoms st.1 st.2) ?_ _ 0#usize (by simp) trivial)
  · intro b h
    rw [h, show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, foldl_band]
    simp only [Bool.and_eq_true, decide_eq_true_eq, List.all_eq_true, ne_eq]
    have : atoms.len > 0#usize ↔ atoms.val ≠ [] := by
      rw [ne_eq, ← List.length_eq_zero_iff]
      have : (atoms.len).val = atoms.val.length := by simp [alloc.vec.Vec.len]
      constructor
      · intro hg; have : 0 < atoms.len.val := hg; omega
      · intro hn; show 0 < atoms.len.val; omega
    rw [this]
  · intro o i hi _
    unfold document.atoms_nonempty_loop.body
    h5i_step
    all_goals first
      | (refine ⟨by scalar_tac, ?_⟩; simp [List.length_pos_iff])
      | (refine ⟨⟨by scalar_tac, ?_⟩, trivial⟩; simp [List.length_pos_iff])
      | (trace_state; sorry)

/-- Adding an edge appends `to` to row `from`. -/
@[step] theorem add_edge_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (src dst : Usize)
    (h : src.val < rows.val.length) (room : rows.val[src.val].val.length < Usize.max) :
    document.add_edge rows src dst ⦃ out => out.val.length = rows.val.length ∧
      ∀ i (hi : i < rows.val.length) (ho : i < out.val.length),
        out.val[i].val = if i = src.val then rows.val[i].val ++ [dst] else rows.val[i].val ⦄ := by
  unfold document.add_edge
  h5i_step
  all_goals first
    | (intro i hi
       simp only [List.getElem_set]
       split_ifs
       all_goals first | omega | rfl | (subst_vars; exact v1_post) | (subst_vars; rw [v1_post]))
    | (trace_state; sorry)

@[step] theorem reaches_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (src dst : Usize)
    (room : rows.val.length < Usize.max) (hs : src.val < rows.val.length)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    document.reaches rows src dst ⦃ b => b = true ↔ Indexed.RowReach rows.val [src] dst ⦄ := by
  unfold document.reaches
  h5i_step
  all_goals first
    | (intro v hv; simp)
    | (trace_state; sorry)

/-! ## Marks, chains and causal rows -/

/-- Mark position `q` true. -/
def setT (o : List Bool) (q : Usize) : List Bool := o.set q.val true

theorem foldl_setT_length (l : List Usize) (o : List Bool) :
    (l.foldl setT o).length = o.length := by
  induction l generalizing o with
  | nil => rfl
  | cons a l ih => rw [List.foldl_cons, ih]; simp [setT]

theorem foldl_setT_get (l : List Usize) (o : List Bool) (i : Nat) (h : i < (l.foldl setT o).length) :
    (l.foldl setT o)[i] = true ↔ o[i]'(by rw [foldl_setT_length] at h; exact h) = true ∨ ∃ q ∈ l, q.val = i := by
  induction l generalizing o with
  | nil => simp
  | cons a l ih =>
    simp only [List.foldl_cons] at h ⊢
    rw [ih (setT o a) h]
    simp only [setT, List.getElem_set, List.mem_cons, exists_eq_or_imp]
    by_cases e : a.val = i <;> simp [e]

theorem observed_loop_spec (found : indexed.Traversal) (out : alloc.vec.Vec Bool)
    (hb : ∀ q ∈ found.queue.val, q.val < out.val.length) :
    document.observed_loop found out 0#usize ⦃ r =>
      r.val = found.queue.val.foldl setT out.val ⦄ := by
  unfold document.observed_loop
  apply WP.spec_mono (loop_fold found.queue.val (fun o : alloc.vec.Vec Bool => o.val) setT
    (fun o _ => o.val.length = out.val.length)
    (fun st => document.observed_loop.body found st.1 st.2) ?_ _ 0#usize (by simp) rfl)
  · intro r h; rw [h]; rfl
  · intro o i hi hinv
    unfold document.observed_loop.body
    h5i_step
    all_goals first
      | exact ⟨by scalar_tac, rfl⟩
      | (refine ⟨⟨by scalar_tac, rfl⟩, ?_⟩; simp [setT, hinv])
      | (trace_state; sorry)

theorem rowReach_lt {rows : List (alloc.vec.Vec Usize)} {seeds : List Usize}
    (sb : ∀ v ∈ seeds, v.val < rows.length) (bounds : ∀ row ∈ rows, ∀ v ∈ row.val, v.val < rows.length)
    {v : Usize} (h : Indexed.RowReach rows seeds v) : v.val < rows.length := by
  induction h with
  | seed hv => exact sb _ hv
  | edge _ hu hv _ => exact bounds _ (List.getElem_mem hu) _ hv

/-- `observed`: the marks of the dependency closure of `roots`. -/
@[step] theorem observed_spec (deps : alloc.vec.Vec (alloc.vec.Vec Usize)) (roots : alloc.vec.Vec Usize)
    (room : deps.val.length < Usize.max) (rb : ∀ v ∈ roots.val, v.val < deps.val.length)
    (bounds : ∀ row ∈ deps.val, ∀ v ∈ row.val, v.val < deps.val.length) :
    document.observed deps roots ⦃ out => out.val.length = deps.val.length ∧
      ∀ i (h : i < out.val.length), out.val[i] = true ↔
        ∃ v : Usize, v.val = i ∧ Indexed.RowReach deps.val roots.val v ⦄ := by
  unfold document.observed
  h5i_step
  all_goals first
    | (intro v hv; simp)
    | (apply WP.spec_mono (observed_loop_spec found out (by
          intro q hq
          rw [out_post]; simp only [List.length_replicate]
          exact rowReach_lt rb bounds ((found_post1 q).mp hq)))
       intro r hr
       have hl : r.val.length = deps.val.length := by
         rw [hr, foldl_setT_length, out_post]; simp
       refine ⟨hl, fun i h => ?_⟩
       have hi : i < (found.queue.val.foldl setT out.val).length := by rw [← hr]; exact h
       rw [List.getElem_of_eq hr, foldl_setT_get found.queue.val out.val i hi]
       constructor
       · rintro (h1 | ⟨q, hq, e⟩)
         · simp [out_post] at h1
         · exact ⟨q, e, (found_post1 q).mp hq⟩
       · rintro ⟨v, e, hv⟩
         exact Or.inr ⟨v, (found_post1 v).mpr hv, e⟩)
    | (trace_state; sorry)

/-- One step of the chain: an edge from the previous anchor to atom `k`. -/
def chainStep (first : Nat) (st : List (Nat × Nat) × Nat) (k : Nat) : List (Nat × Nat) × Nat :=
  (st.1 ++ [(st.2, first + k)], first + k)

/-- The edges an insertion of `n` atoms adds. -/
def chainM (left right first n : Nat) : List (Nat × Nat) :=
  let st := (List.range n).foldl (chainStep first) ([], left)
  st.1 ++ [(st.2, right)]

theorem foldl_chainStep (first : Nat) (l : List Nat) (acc : List (Nat × Nat)) (p : Nat) :
    l.foldl (chainStep first) (acc, p) =
      (acc ++ (l.foldl (chainStep first) ([], p)).1, (l.foldl (chainStep first) ([], p)).2) := by
  induction l generalizing acc p with
  | nil => simp
  | cons a l ih =>
    simp only [List.foldl_cons, chainStep]
    rw [ih, ih (([] : List (Nat × Nat)) ++ [(p, first + a)])]
    simp

def pairVal (e : Usize × Usize) : Nat × Nat := (e.1.val, e.2.val)

theorem foldl_chainStep_length (first : Nat) (l : List Nat) (acc : List (Nat × Nat)) (p : Nat) :
    (l.foldl (chainStep first) (acc, p)).1.length = acc.length + l.length := by
  induction l generalizing acc p with
  | nil => simp
  | cons a l ih => simp only [List.foldl_cons, chainStep]; rw [ih]; simp; omega

theorem chain_loop_spec (out : alloc.vec.Vec (Usize × Usize)) (left first n : Usize)
    (room : first.val + n.val ≤ Usize.max) (room2 : out.val.length + n.val < Usize.max) :
    document.chain_loop out first n left 0#usize ⦃ r =>
      (r.1.val.map pairVal, r.2.val) = (List.range n.val).foldl (chainStep first.val)
        (out.val.map pairVal, left.val) ⦄ := by
  unfold document.chain_loop
  have hl : (List.range n.val).length = n.val := List.length_range
  apply WP.spec_mono (loop_fold2 (List.range n.val)
    (fun (o : alloc.vec.Vec (Usize × Usize)) (p : Usize) => (o.val.map pairVal, p.val))
    (chainStep first.val) (fun o _ k => o.val.length = out.val.length + k)
    (fun st => document.chain_loop.body first n st.1 st.2.1 st.2.2) ?_ out left 0#usize
    (by simp) (by simp))
  · intro r h; simpa using h.1
  · intro o p k hk hinv
    unfold document.chain_loop.body
    h5i_step
    all_goals first
      | (refine ⟨?_, by omega⟩; simp [chainStep, pairVal, i_post])
      | (trace_state; sorry)

@[step] theorem chain_spec (out : alloc.vec.Vec (Usize × Usize)) (left right first n : Usize)
    (room : first.val + n.val ≤ Usize.max) (room2 : out.val.length + n.val + 1 < Usize.max) :
    document.chain out left right first n ⦃ r =>
      r.val.map (fun e => (e.1.val, e.2.val)) =
        out.val.map (fun e => (e.1.val, e.2.val)) ++ chainM left.val right.val first.val n.val ⦄ := by
  unfold document.chain
  apply WP.spec_bind (chain_loop_spec out left first n room (by omega))
  rintro ⟨o, p⟩ h
  simp only at h
  have hlen := congrArg (fun x => x.1.length) h
  simp only [List.length_map, foldl_chainStep_length, List.length_range] at hlen
  rw [foldl_chainStep] at h
  simp only [Prod.mk.injEq] at h
  obtain ⟨h1, h2⟩ := h
  h5i_step
  all_goals first
    | omega
    | (have e : (fun e : Usize × Usize => (e.1.val, e.2.val)) = pairVal := rfl
       rw [e, h1, List.append_assoc]
       rfl)
    | (trace_state; sorry)

/-! ## Withdrawn edges and causal rows -/

/-- One retraction pass: mark edge `e` if its retractor is seen (or `seen` is empty). -/
def withdrawStep (seen : List Bool) (o : List Bool) (x : Usize × Usize) : List Bool :=
  if seen.length = 0 ∨ (∃ h : x.2.val < seen.length, seen[x.2.val] = true) then o.set x.1.val true else o

theorem foldl_withdrawStep_length (seen : List Bool) (l : List (Usize × Usize)) (o : List Bool) :
    (l.foldl (withdrawStep seen) o).length = o.length := by
  induction l generalizing o with
  | nil => rfl
  | cons a l ih => rw [List.foldl_cons, ih]; unfold withdrawStep; split_ifs <;> simp

theorem foldl_withdrawStep_get (seen : List Bool) (l : List (Usize × Usize)) (o : List Bool) (i : Nat)
    (h : i < (l.foldl (withdrawStep seen) o).length) :
    (l.foldl (withdrawStep seen) o)[i] = true ↔
      o[i]'(by rw [foldl_withdrawStep_length] at h; exact h) = true ∨
      ∃ x ∈ l, x.1.val = i ∧ (seen.length = 0 ∨ ∃ h : x.2.val < seen.length, seen[x.2.val] = true) := by
  induction l generalizing o with
  | nil => simp
  | cons a l ih =>
    simp only [List.foldl_cons] at h ⊢
    rw [ih (withdrawStep seen o a) h]
    unfold withdrawStep
    by_cases c : seen.length = 0 ∨ (∃ h : a.2.val < seen.length, seen[a.2.val] = true)
    · simp only [c, ↓reduceIte, List.getElem_set, List.mem_cons, exists_eq_or_imp]
      by_cases e : a.1.val = i <;> simp [e, c]
    · simp only [c, ↓reduceIte, List.mem_cons, exists_eq_or_imp]
      simp [c]

theorem withdrawn_loop0_spec (retractions : alloc.vec.Vec (Usize × Usize)) (seen out : alloc.vec.Vec Bool)
    (hb : ∀ x ∈ retractions.val, x.1.val < out.val.length) :
    document.withdrawn_loop0 retractions seen out 0#usize ⦃ r =>
      r.val = retractions.val.foldl (withdrawStep seen.val) out.val ⦄ := by
  unfold document.withdrawn_loop0
  apply WP.spec_mono (loop_fold retractions.val (fun o : alloc.vec.Vec Bool => o.val)
    (withdrawStep seen.val) (fun o _ => o.val.length = out.val.length)
    (fun st => document.withdrawn_loop0.body retractions seen st.1 st.2) ?_ _ 0#usize (by simp) rfl)
  · intro r h; rw [h]; rfl
  · intro o i hi hinv
    unfold document.withdrawn_loop0.body
    h5i_step
    all_goals first
      | exact hb e p (e_post ▸ List.getElem_mem _)
      | exact hb _ (e_post ▸ List.getElem_mem _)
      | (obtain ⟨x1, x2⟩ := out1
         simp only at out1_post out1_post1
         subst out1_post1
         try simp only
         h5i_step
         all_goals first
           | (refine ⟨by scalar_tac, ?_⟩
              rw [← e_post]
              unfold withdrawStep
              rw [if_pos]
              first
                | (left; scalar_tac)
                | (right; exact ⟨by scalar_tac, by simp_all⟩)
                | (left; simp_all; done))
           | (trace_state; sorry))
      | (refine ⟨by scalar_tac, ?_⟩
         rw [← e_post]
         simp only [withdrawStep]
         split_ifs with c
         · exfalso
           rcases c with c | ⟨h, c⟩
           · simp_all
           · first | omega | simp_all
         · rfl)
      | (trace_state; sorry)

theorem withdrawn_loop1_spec (own : alloc.vec.Vec Usize) (out : alloc.vec.Vec Bool)
    (hb : ∀ e ∈ own.val, e.val < out.val.length) :
    document.withdrawn_loop1 own out 0#usize ⦃ r => r.val = own.val.foldl setT out.val ⦄ := by
  unfold document.withdrawn_loop1
  apply WP.spec_mono (loop_fold own.val (fun o : alloc.vec.Vec Bool => o.val) setT
    (fun o _ => o.val.length = out.val.length)
    (fun st => document.withdrawn_loop1.body own st.1 st.2) ?_ _ 0#usize (by simp) rfl)
  · intro r h; rw [h]; rfl
  · intro o i hi hinv
    unfold document.withdrawn_loop1.body
    h5i_step
    all_goals first
      | exact ⟨by scalar_tac, rfl⟩
      | (refine ⟨⟨by scalar_tac, rfl⟩, ?_⟩; simp [setT, hinv])
      | (trace_state; sorry)

/-- `withdrawn`: edge `i` is off when a seen patch retracted it, or this
patch's earlier operations did. -/
@[step] theorem withdrawn_spec (edges : Usize) (retractions : alloc.vec.Vec (Usize × Usize))
    (seen : alloc.vec.Vec Bool) (own : alloc.vec.Vec Usize)
    (rb : ∀ x ∈ retractions.val, x.1.val < edges.val) (ob : ∀ e ∈ own.val, e.val < edges.val) :
    document.withdrawn edges retractions seen own ⦃ out => out.val.length = edges.val ∧
      ∀ i (h : i < out.val.length), out.val[i] = true ↔
        (∃ x ∈ retractions.val, x.1.val = i ∧
          (seen.val.length = 0 ∨ ∃ h : x.2.val < seen.val.length, seen.val[x.2.val] = true)) ∨
        ∃ e ∈ own.val, e.val = i ⦄ := by
  unfold document.withdrawn
  h5i_step
  all_goals first
    | (apply WP.spec_bind (withdrawn_loop0_spec retractions seen out (by
          intro x hx; rw [out_post]; simpa using rb x.1 x.2 hx))
       intro out1 h1
       apply WP.spec_mono (withdrawn_loop1_spec own out1 (by
          intro e he; rw [h1, foldl_withdrawStep_length, out_post]; simpa using ob e he))
       intro r hr
       have hl : r.val.length = edges.val := by
         rw [hr, foldl_setT_length, h1, foldl_withdrawStep_length, out_post]; simp
       refine ⟨hl, fun i h => ?_⟩
       have hi1 : i < (own.val.foldl setT out1.val).length := by rw [← hr]; exact h
       rw [List.getElem_of_eq hr, foldl_setT_get _ _ i hi1]
       have hi2 : i < (retractions.val.foldl (withdrawStep seen.val) out.val).length := by
         rw [← h1]; rw [foldl_setT_length] at hi1; exact hi1
       rw [List.getElem_of_eq h1, foldl_withdrawStep_get _ _ _ i hi2]
       simp [out_post])
    | (trace_state; sorry)

/-! ## Causal rows -/

/-- Rows as plain lists. -/
def rowsL (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) : List (List Usize) := rows.val.map (·.val)

def addL (rows : List (List Usize)) (a v : Usize) : List (List Usize) := rows.set a.val (rows[a.val]! ++ [v])

theorem addL_length (rows : List (List Usize)) (a v : Usize) : (addL rows a v).length = rows.length := by
  simp [addL]

theorem mem_addL (rows : List (List Usize)) (a v : Usize) (b : Nat) (hb : b < rows.length) (w : Usize)
    (hb' : b < (addL rows a v).length) :
    w ∈ (addL rows a v)[b] ↔ w ∈ rows[b] ∨ (a.val = b ∧ w = v) := by
  simp only [addL, List.getElem_set]
  split_ifs with e
  · subst e; simp [getElem!_pos rows a.val hb]
  · simp [e]

/-- Whether edge `i` is causal: not withdrawn, and from the sentinel or a seen patch. -/
def causalEdge (edges : List (Usize × Usize × Usize)) (off seen : List Bool) (i : Nat) : Prop :=
  ∃ h : i < edges.length, off[i]? = some false ∧
    (edges[i].2.2 = document.NONE ∨ ∃ h : edges[i].2.2.val < seen.length, seen[edges[i].2.2.val] = true)

open Classical in
noncomputable def edgeStep (edges : List (Usize × Usize × Usize)) (off seen : List Bool)
    (rows : List (List Usize)) (i : Nat) : List (List Usize) :=
  if h : causalEdge edges off seen i then addL rows (edges[i]'h.1).1 (edges[i]'h.1).2.1 else rows

theorem edgeStep_length (edges : List (Usize × Usize × Usize)) (off seen : List Bool)
    (rows : List (List Usize)) (i : Nat) : (edgeStep edges off seen rows i).length = rows.length := by
  unfold edgeStep; split_ifs <;> simp [addL_length]

theorem causal_rows_loop0_spec (edges : alloc.vec.Vec (Usize × Usize × Usize)) (off seen : alloc.vec.Vec Bool)
    (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (hoff : off.val.length = edges.val.length)
    (hb : ∀ e ∈ edges.val, e.1.val < rows.val.length)
    (hempty : ∀ row ∈ rows.val, row.val = []) (room : edges.val.length < Usize.max) :
    document.causal_rows_loop0 edges off seen rows 0#usize ⦃ r =>
      rowsL r = (List.range edges.val.length).foldl (edgeStep edges.val off.val seen.val) (rowsL rows) ∧
      ∀ row ∈ r.val, row.val.length ≤ edges.val.length ⦄ := by
  unfold document.causal_rows_loop0
  have hl : (List.range edges.val.length).length = edges.val.length := List.length_range
  apply WP.spec_mono (Indexed.fold_with_inv (List.range edges.val.length) rowsL
    (edgeStep edges.val off.val seen.val)
    (fun o j => o.val.length = rows.val.length ∧ ∀ row ∈ o.val, row.val.length ≤ j)
    (fun st => document.causal_rows_loop0.body edges off seen st.1 st.2) ?_ _ 0#usize (by simp)
    ⟨rfl, fun row h => by simp [hempty row h]⟩)
  · intro r h
    refine ⟨by simpa using h.1, fun row hr => by have := h.2.2 row hr; simpa [hl] using this⟩
  · intro o i hi hinv
    unfold document.causal_rows_loop0.body
    h5i_step
    all_goals (try simp only [Indexed.FoldInvStep, List.getElem_range, List.length_range])
    all_goals first
      -- the loop ends
      | exact ⟨by scalar_tac, trivial, hinv.1, fun row h => by have := hinv.2 row h; omega⟩
      -- a withdrawn edge
      | (refine ⟨by scalar_tac, ?_, by assumption, hinv.1, fun row h => by have := hinv.2 row h; scalar_tac⟩
         unfold edgeStep
         rw [dif_neg]
         rintro ⟨h, ho, -⟩
         rw [List.getElem?_eq_getElem (by scalar_tac)] at ho
         simp_all)
      -- otherwise destructure the edge
      | (rw [← e_post]
         obtain ⟨x1, x2, x3⟩ := e
         h5i_step
         all_goals (try simp only [Indexed.FoldInvStep, List.getElem_range, List.length_range])
         all_goals first
           | exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
           | (have hx : x1.val < o.val.length := by
                rw [hinv.1]; exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
              have := hinv.2 _ (List.getElem_mem hx)
              scalar_tac)
           | (refine ⟨by scalar_tac, ?_, by assumption, hinv.1,
                fun row h => by have := hinv.2 row h; scalar_tac⟩
              unfold edgeStep
              rw [dif_neg]
              rintro ⟨h, -, hn⟩
              trace_state
              sorry)
           | (have hx : x1.val < o.val.length := by
                rw [hinv.1]; exact hb x1 x2 _ (by rw [e_post]; exact List.getElem_mem _)
              have hc : causalEdge edges.val off.val seen.val i.val := by
                refine ⟨by scalar_tac, by rw [List.getElem?_eq_getElem (by scalar_tac), b_post], ?_⟩
                rw [← e_post]
                first | (left; simp_all; done) | (right; exact ⟨by scalar_tac, by simp_all⟩)
              refine ⟨by scalar_tac, ?_, by scalar_tac, by omega, ?_⟩
              · unfold edgeStep
                rw [dif_pos hc]
                trace_state
                sorry
              · intro row hr
                obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hr
                trace_state
                sorry)
           | (trace_state; sorry))
      | (trace_state; sorry)

theorem rowsL_of_add (rows out : alloc.vec.Vec (alloc.vec.Vec Usize)) (src dst : Usize)
    (h : src.val < rows.val.length) (hl : out.val.length = rows.val.length)
    (hp : ∀ i (hi : i < rows.val.length) (ho : i < out.val.length),
      out.val[i].val = if i = src.val then rows.val[i].val ++ [dst] else rows.val[i].val) :
    rowsL out = addL (rowsL rows) src dst := by
  apply List.ext_getElem (by simp [rowsL, addL, hl])
  intro i h1 h2
  have hi : i < rows.val.length := by simpa [rowsL, addL] using h2
  have ho : i < out.val.length := by simpa [rowsL] using h1
  simp only [rowsL, addL, List.getElem_map, List.getElem_set]
  rw [hp i hi ho]
  by_cases e : i = src.val
  · subst e
    simp [List.getElem?_eq_getElem h]
  · simp [e, Ne.symm e]

theorem mem_fold_addL (l : List (Usize × Usize)) (rows : List (List Usize)) (b : Nat)
    (hb : b < rows.length) (w : Usize)
    (hb' : b < (l.foldl (fun r x => addL r x.1 x.2) rows).length) :
    w ∈ (l.foldl (fun r x => addL r x.1 x.2) rows)[b] ↔ w ∈ rows[b] ∨ ∃ x ∈ l, x.1.val = b ∧ x.2 = w := by
  induction l generalizing rows with
  | nil => simp
  | cons a l ih =>
    simp only [List.foldl_cons] at hb' ⊢
    refine (ih (addL rows a.1 a.2) (lt_of_lt_of_eq hb (addL_length _ _ _).symm) hb').trans ?_
    simp only [mem_addL rows a.1 a.2 b hb w, List.mem_cons, exists_eq_or_imp]
    constructor
    · rintro ((h | ⟨e1, e2⟩) | h)
      · exact Or.inl h
      · exact Or.inr (Or.inl ⟨e1, e2.symm⟩)
      · exact Or.inr (Or.inr h)
    · rintro (h | (⟨e1, e2⟩ | h))
      · exact Or.inl (Or.inl h)
      · exact Or.inl (Or.inr ⟨e1, e2.symm⟩)
      · exact Or.inr h

theorem foldl_addL_length (l : List (Usize × Usize)) (rows : List (List Usize)) :
    (l.foldl (fun r x => addL r x.1 x.2) rows).length = rows.length := by
  induction l generalizing rows with
  | nil => rfl
  | cons a l ih => rw [List.foldl_cons, ih, addL_length]

theorem foldl_edgeStep_length (edges : List (Usize × Usize × Usize)) (off seen : List Bool)
    (l : List Nat) (rows : List (List Usize)) :
    (l.foldl (edgeStep edges off seen) rows).length = rows.length := by
  induction l generalizing rows with
  | nil => rfl
  | cons a l ih => rw [List.foldl_cons, ih, edgeStep_length]

theorem mem_fold_edgeStep (edges : List (Usize × Usize × Usize)) (off seen : List Bool)
    (l : List Nat) (rows : List (List Usize)) (b : Nat) (hb : b < rows.length) (w : Usize)
    (hb' : b < (l.foldl (edgeStep edges off seen) rows).length) :
    w ∈ (l.foldl (edgeStep edges off seen) rows)[b] ↔ w ∈ rows[b] ∨
      ∃ i ∈ l, ∃ h : causalEdge edges off seen i,
        (edges[i]'h.1).1.val = b ∧ (edges[i]'h.1).2.1 = w := by
  induction l generalizing rows with
  | nil => simp
  | cons a l ih =>
    simp only [List.foldl_cons] at hb' ⊢
    refine (ih (edgeStep edges off seen rows a) (lt_of_lt_of_eq hb (edgeStep_length _ _ _ _ _).symm) hb').trans ?_
    by_cases c : causalEdge edges off seen a
    · simp only [edgeStep, dif_pos c]
      rw [mem_addL rows _ _ b hb w]
      constructor
      · rintro ((h | ⟨e1, e2⟩) | ⟨i, hi, hc, h1, h2⟩)
        · exact Or.inl h
        · exact Or.inr ⟨a, List.mem_cons_self, c, e1, e2.symm⟩
        · exact Or.inr ⟨i, List.mem_cons_of_mem _ hi, hc, h1, h2⟩
      · rintro (h | ⟨i, hi, hc, h1, h2⟩)
        · exact Or.inl (Or.inl h)
        · rcases List.mem_cons.mp hi with rfl | hi
          · exact Or.inl (Or.inr ⟨h1, h2.symm⟩)
          · exact Or.inr ⟨i, hi, hc, h1, h2⟩
    · simp only [edgeStep, dif_neg c]
      constructor
      · rintro (h | ⟨i, hi, hc, h1, h2⟩)
        · exact Or.inl h
        · exact Or.inr ⟨i, List.mem_cons_of_mem _ hi, hc, h1, h2⟩
      · rintro (h | ⟨i, hi, hc, h1, h2⟩)
        · exact Or.inl h
        · rcases List.mem_cons.mp hi with rfl | hi
          · exact absurd hc c
          · exact Or.inr ⟨i, hi, hc, h1, h2⟩

theorem causal_rows_loop1_spec (pending : alloc.vec.Vec (Usize × Usize))
    (rows : alloc.vec.Vec (alloc.vec.Vec Usize))
    (m : Nat) (hb : ∀ x ∈ pending.val, x.1.val < rows.val.length)
    (hm : ∀ row ∈ rows.val, row.val.length ≤ m) (room : m + pending.val.length < Usize.max) :
    document.causal_rows_loop1 pending rows 0#usize ⦃ r =>
      rowsL r = pending.val.foldl (fun r x => addL r x.1 x.2) (rowsL rows) ⦄ := by
  unfold document.causal_rows_loop1
  apply WP.spec_mono (loop_fold pending.val rowsL (fun r x => addL r x.1 x.2)
    (fun o j => o.val.length = rows.val.length ∧ ∀ row ∈ o.val, row.val.length ≤ m + j)
    (fun st => document.causal_rows_loop1.body pending st.1 st.2) ?_ _ 0#usize (by simp)
    ⟨rfl, fun row h => by simpa using hm row h⟩)
  · intro r h; rw [h]; simp
  · intro o i hi hinv
    unfold document.causal_rows_loop1.body
    h5i_step
    all_goals first
      | exact hb i1 i2 (by rw [i1_post]; exact List.getElem_mem _)
      | (have hx : i1.val < o.val.length := by
           rw [hinv.1]; exact hb i1 i2 (by rw [i1_post]; exact List.getElem_mem _)
         have := hinv.2 _ (List.getElem_mem hx)
         scalar_tac)
      | (have hx : i1.val < o.val.length := by
           rw [hinv.1]; exact hb i1 i2 (by rw [i1_post]; exact List.getElem_mem _)
         refine ⟨⟨by scalar_tac, ?_⟩, ?_⟩
         · rw [rowsL_of_add o rows1 i1 i2 hx (by omega) (fun k hk _ => by rw [rows1_post1 k (by omega)])]
           simp only [← i1_post]
         · intro row hr
           obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hr
           rw [rows1_post1 k (by omega)]
           have := hinv.2 _ (List.getElem_mem (by omega : k < o.val.length))
           split_ifs <;> simp <;> omega)
      | (trace_state; sorry)

/-- `causal_rows`: row `a` lists the targets of the causal edges from `a` and of
this patch's pending edges from `a`. -/
@[step] theorem causal_rows_spec (edges : alloc.vec.Vec (Usize × Usize × Usize))
    (off seen : alloc.vec.Vec Bool) (pending : alloc.vec.Vec (Usize × Usize)) (n : Usize)
    (hoff : off.val.length = edges.val.length)
    (hb : ∀ e ∈ edges.val, e.1.val < n.val) (hp : ∀ x ∈ pending.val, x.1.val < n.val)
    (room : edges.val.length + pending.val.length < Usize.max) :
    document.causal_rows edges off seen pending n ⦃ out => out.val.length = n.val ∧
      ∀ a (h : a < out.val.length) (w : Usize), w ∈ out.val[a].val ↔
        (∃ i, ∃ h : causalEdge edges.val off.val seen.val i,
          (edges.val[i]'h.1).1.val = a ∧ (edges.val[i]'h.1).2.1 = w) ∨
        ∃ x ∈ pending.val, x.1.val = a ∧ x.2 = w ⦄ := by
  unfold document.causal_rows
  h5i_step
  all_goals first
    | (apply WP.spec_bind (causal_rows_loop0_spec edges off seen rows hoff
          (by intro e he; rw [rows_post]; simpa using hb e.1 e.2.1 e.2.2 he)
          (by intro row h; rw [rows_post] at h; simp at h; rw [h]; rfl) (by omega))
       rintro rows1 ⟨h1, h1b⟩
       have hl1 : rows1.val.length = n.val := by
         have := congrArg List.length h1
         simp only [rowsL, List.length_map, foldl_edgeStep_length, rows_post, List.length_replicate] at this
         exact this
       apply WP.spec_mono (causal_rows_loop1_spec pending rows1 edges.val.length
          (by intro x hx; rw [hl1]; exact hp x.1 x.2 hx) h1b (by omega))
       intro r hr
       have hlr : r.val.length = n.val := by
         have := congrArg List.length hr
         simp only [rowsL, List.length_map, foldl_addL_length, hl1] at this
         exact this
       refine ⟨hlr, fun a h w => ?_⟩
       have ha1 : a < (rowsL rows1).length := by simp [rowsL]; omega
       have ha0 : a < (rowsL rows).length := by simp [rowsL, rows_post]; omega
       have k1 : w ∈ r.val[a].val ↔ w ∈ (rowsL r)[a]'(by simp [rowsL]; omega) := by simp [rowsL]
       rw [k1, List.getElem_of_eq hr, mem_fold_addL _ _ a ha1 w, List.getElem_of_eq h1,
         mem_fold_edgeStep _ _ _ _ _ a ha0 w]
       have k0 : (rowsL rows)[a] = [] := by simp [rowsL, rows_post]
       rw [k0]
       simp only [List.not_mem_nil, false_or, List.mem_range]
       constructor
       · rintro (⟨i, -, hc, h1, h2⟩ | ⟨x, hx, h1, h2⟩)
         · exact Or.inl ⟨i, hc, h1, h2⟩
         · exact Or.inr ⟨x.1, by rw [← h2]; exact hx, h1⟩
       · rintro (⟨i, hc, h1, h2⟩ | ⟨y, hy, h1⟩)
         · exact Or.inl ⟨i, hc.1, hc, h1, h2⟩
         · exact Or.inr ⟨(y, w), hy, h1, rfl⟩)
    | (trace_state; sorry)


/-! ## Validation state and the simple checks -/

/-- The validation state, as lists. -/
structure CheckS where
  refusal : U64 × Nat × Nat
  pending : List (Nat × Nat)
  created : Nat
  retracted : List Nat

def absC (c : document.Check) : CheckS :=
  ⟨(c.refusal.kind, c.refusal.op.val, c.refusal.item.val), c.pending.val.map pairVal, c.created.val,
    c.retracted.val.map (·.val)⟩

def refuseS (st : CheckS) (k : U64) (op item : Nat) : CheckS :=
  { st with refusal := (k, op, item) }

@[step] theorem refuse_spec (k : U64) (op item : Usize) :
    document.refuse k op item ⦃ r => r = ⟨k, op, item⟩ ⦄ := by
  simp [document.refuse]

def checkRestoreS (st : CheckS) (deletions : Nat) (seen : List Bool) (op : Nat)
    (deletion : document.Ref) : CheckS :=
  let k := checkRefM deletion seen deletions document.UNKNOWN_DELETION
  if k ≠ 0#u64 then refuseS st k op 0 else st

@[step] theorem check_restore_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (deletion : document.Ref) :
    document.check_restore st doc seen op deletion ⦃ out =>
      absC out = checkRestoreS (absC st) doc.deletions.val.length seen.val op.val deletion ⦄ := by
  unfold document.check_restore checkRestoreS
  h5i_step
  all_goals first
    | (simp_all [absC, refuseS]; done)
    | (trace_state; sorry)

def checkRetractS (st : CheckS) (orders : List Usize) (seen : List Bool) (op : Nat)
    (order : document.Ref) : CheckS :=
  let k := checkRefM order seen orders.length document.UNKNOWN_ORDER
  if k ≠ 0#u64 then refuseS st k op 0
  else { st with retracted := st.retracted ++ [(orders[order.index.val]!).val] }

@[step] theorem check_retract_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (order : document.Ref) (room : st.retracted.val.length < Usize.max) :
    document.check_retract st doc seen op order ⦃ out =>
      absC out = checkRetractS (absC st) doc.orders.val seen.val op.val order ⦄ := by
  unfold document.check_retract checkRetractS
  h5i_step
  all_goals first
    | (simp_all [absC, refuseS]; done)
    | exact (checkRefM_zero (missing := document.UNKNOWN_ORDER) unknown_order_ne (by assumption)).1
    | (have hz := checkRefM_zero (missing := document.UNKNOWN_ORDER) unknown_order_ne (by assumption)
       simp [absC, v_post, List.getElem?_eq_getElem hz.1])
    | (trace_state; sorry)

def checkKeepS (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (deletion insertion : document.Ref) : CheckS :=
  let od := checkRefM deletion seen document.NONE.val 0#u64
  let oi := checkRefM insertion seen document.NONE.val 0#u64
  if od ≠ 0#u64 then refuseS st od op 0
  else if oi ≠ 0#u64 then refuseS st oi op 1
  else if hd : doc.deletions.val.length ≤ deletion.index.val then
    refuseS st document.UNKNOWN_DELETION op 0
  else if hi : doc.insertions.val.length ≤ insertion.index.val then
    refuseS st document.UNKNOWN_INSERTION op 1
  else
    let d := doc.deletions.val[deletion.index.val]'(by omega)
    let ins := doc.insertions.val[insertion.index.val]'(by omega)
    if ins.left ∈ d.targets.val ∨ ins.right ∈ d.targets.val then st
    else refuseS st document.INVALID_ACKNOWLEDGEMENT op 0

@[step] theorem check_keep_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (deletion insertion : document.Ref) :
    document.check_keep st doc seen op deletion insertion ⦃ out =>
      absC out = checkKeepS (absC st) doc seen.val op.val deletion insertion ⦄ := by
  unfold document.check_keep checkKeepS
  h5i_step
  all_goals first
    | (simp_all [absC, refuseS]; done)
    | (trace_state; sorry)


/-! ## Reachability along rows -/

/-- One row step. -/
def RowEdge (rows : List (alloc.vec.Vec Usize)) (a b : Usize) : Prop :=
  ∃ h : a.val < rows.length, b ∈ rows[a.val].val

theorem rowReach_single_iff (rows : List (alloc.vec.Vec Usize)) (s v : Usize) :
    Indexed.RowReach rows [s] v ↔ Relation.ReflTransGen (RowEdge rows) s v := by
  constructor
  · intro h
    induction h with
    | seed hv => simp at hv; subst hv; exact .refl
    | edge _ hu hv ih => exact ih.tail ⟨hu, hv⟩
  · intro h
    induction h with
    | refl => exact .seed (by simp)
    | tail _ e ih => obtain ⟨hu, hv⟩ := e; exact .edge ih hu hv

/-- `reaches`, as a reflexive-transitive closure. -/
theorem reaches_iff (rows : alloc.vec.Vec (alloc.vec.Vec Usize)) (src dst : Usize)
    (room : rows.val.length < Usize.max) (hs : src.val < rows.val.length)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    document.reaches rows src dst ⦃ b => b = true ↔ Relation.ReflTransGen (RowEdge rows.val) src dst ⦄ := by
  apply WP.spec_mono (reaches_spec rows src dst room hs bounds)
  intro b h
  rw [h, rowReach_single_iff]

/-! ## The insert and order checks -/

/-- Edge `i` is withdrawn for this patch. -/
def OffP (doc : document.Doc) (seen : List Bool) (st : CheckS) (i : Nat) : Prop :=
  (∃ x ∈ doc.retractions.val, x.1.val = i ∧
    (seen.length = 0 ∨ ∃ h : x.2.val < seen.length, seen[x.2.val] = true)) ∨ i ∈ st.retracted

/-- The edges a patch may rely on, as a relation on anchor indices. -/
def CausalRel (doc : document.Doc) (seen : List Bool) (st : CheckS) (a b : Nat) : Prop :=
  (∃ i, ∃ h : i < doc.edges.val.length, ¬ OffP doc seen st i ∧
    (doc.edges.val[i].2.2 = document.NONE ∨
      ∃ h : doc.edges.val[i].2.2.val < seen.length, seen[doc.edges.val[i].2.2.val] = true) ∧
    doc.edges.val[i].1.val = a ∧ doc.edges.val[i].2.1.val = b) ∨
  (a, b) ∈ st.pending

/-- Well-formed documents: every stored index is in range. -/
structure DocWF (doc : document.Doc) : Prop where
  two : 2 ≤ doc.atoms.val.length
  edges : ∀ e ∈ doc.edges.val, e.1.val < doc.atoms.val.length ∧ e.2.1.val < doc.atoms.val.length
  retractions : ∀ x ∈ doc.retractions.val, x.1.val < doc.edges.val.length
  orders : ∀ o ∈ doc.orders.val, o.val < doc.edges.val.length

/-- Well-formed validation states over `doc`. -/
structure CheckWF (doc : document.Doc) (st : CheckS) : Prop where
  pending : ∀ e ∈ st.pending, e.1 < doc.atoms.val.length + st.created ∧
    e.2 < doc.atoms.val.length + st.created
  retracted : ∀ e ∈ st.retracted, e < doc.edges.val.length

open Classical in
noncomputable def checkInsertS (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (l r : document.Anchor) (atoms : List (alloc.vec.Vec U8)) : CheckS :=
  let n := doc.atoms.val.length
  let kl := checkAnchorM l n seen
  let kr := checkAnchorM r n seen
  if kl ≠ 0#u64 then refuseS st kl op 0
  else if kr ≠ 0#u64 then refuseS st kr op 1
  else if ¬ (atoms ≠ [] ∧ ∀ x ∈ atoms, x.val ≠ []) ∨ anchorIdx l = 1 ∨ anchorIdx r = 0 ∨
      anchorIdx l = anchorIdx r then refuseS st document.INVALID_INSERTION op 0
  else if ¬ Relation.ReflTransGen (CausalRel doc seen st) (anchorIdx l) (anchorIdx r) then
    refuseS st document.UNORDERED_ANCHORS op 0
  else { st with pending := st.pending ++ chainM (anchorIdx l) (anchorIdx r) (n + st.created) atoms.length,
                 created := st.created + atoms.length }

open Classical in
noncomputable def checkOrderS (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (before after : document.Ref) : CheckS :=
  let n := doc.atoms.val.length
  let kb := checkAnchorM (.Node before) n seen
  let ka := checkAnchorM (.Node after) n seen
  if kb ≠ 0#u64 then refuseS st kb op 0
  else if ka ≠ 0#u64 then refuseS st ka op 1
  else if Relation.ReflTransGen (CausalRel doc seen st) after.index.val before.index.val then
    refuseS st document.CYCLE op 0
  else { st with pending := st.pending ++ [(before.index.val, after.index.val)] }

/-- A relation on `Usize` that mirrors a relation on their values has the same
reflexive-transitive closure. -/
theorem rtg_val_iff {R : Usize → Usize → Prop} {S : Nat → Nat → Prop}
    (hRS : ∀ a b, R a b ↔ S a.val b.val) (hS : ∀ a b, S a b → ∃ b' : Usize, b'.val = b) (s t : Usize) :
    Relation.ReflTransGen R s t ↔ Relation.ReflTransGen S s.val t.val := by
  constructor
  · intro h
    induction h with
    | refl => exact .refl
    | tail _ e ih => exact ih.tail ((hRS _ _).mp e)
  · intro h
    have key : ∀ t : Nat, Relation.ReflTransGen S s.val t →
        ∃ t' : Usize, t'.val = t ∧ Relation.ReflTransGen R s t' := by
      intro t h
      induction h with
      | refl => exact ⟨s, rfl, .refl⟩
      | tail _ e ih =>
        obtain ⟨c', hc, hr⟩ := ih
        rename_i d _
        obtain ⟨d', hd'⟩ := hS _ _ e
        refine ⟨d', hd', hr.tail ?_⟩
        rw [hRS, hc, hd']
        exact e
    obtain ⟨t', ht, hr⟩ := key t.val h
    have : t' = t := UScalar.eq_of_val_eq ht
    exact this ▸ hr


/-! ## The delete check -/

/-- A fold step whose loop exit keeps only the first state component. -/
def FoldStep2d {α σ τ β} (l : List α) (abs : σ → τ → β) (g : β → α → β) (Inv : σ → τ → Nat → Prop)
    (a : σ) (b : τ) (i : Usize) : ControlFlow (σ × τ × Usize) σ → Prop
  | .done a' => l.length ≤ i.val ∧ a' = a
  | .cont (a', b', j) => ∃ h : i.val < l.length,
      abs a' b' = g (abs a b) l[i.val] ∧ j.val = i.val + 1 ∧ Inv a' b' j.val

theorem loop_fold2d {α σ τ β : Type} (l : List α) (abs : σ → τ → β) (g : β → α → β)
    (Inv : σ → τ → Nat → Prop)
    (body : σ × τ × Usize → Result (ControlFlow (σ × τ × Usize) σ))
    (hstep : ∀ (a : σ) (b : τ) (i : Usize), i.val ≤ l.length → Inv a b i.val →
      body (a, b, i) ⦃ FoldStep2d l abs g Inv a b i ⦄)
    (a : σ) (b : τ) (i : Usize) (hi : i.val ≤ l.length) (hinv : Inv a b i.val) :
    loop body (a, b, i) ⦃ r => ∃ b', abs r b' = (l.drop i.val).foldl g (abs a b) ⦄ := by
  apply loop.spec_decr_nat (measure := fun (x : σ × τ × Usize) => l.length - x.2.2.val)
    (inv := fun x => x.2.2.val ≤ l.length ∧ Inv x.1 x.2.1 x.2.2.val ∧
      (l.drop i.val).foldl g (abs a b) = (l.drop x.2.2.val).foldl g (abs x.1 x.2.1))
  · rintro ⟨t, u, j⟩ ⟨hj, hI, heq⟩
    simp only at hj hI heq
    apply WP.spec_mono (hstep t u j hj hI)
    intro r h
    cases r with
    | done y =>
      obtain ⟨hle, rfl⟩ := h
      exact ⟨u, by rw [heq, List.drop_eq_nil_of_le hle]; rfl⟩
    | cont x =>
      obtain ⟨t', u', k⟩ := x
      obtain ⟨hlt, habs, hk, hI'⟩ := h
      simp only
      refine ⟨⟨by omega, hI', ?_⟩, by omega⟩
      rw [heq, List.drop_eq_getElem_cons hlt, List.foldl_cons, ← habs, hk]
  · exact ⟨hi, hinv, rfl⟩

def deleteStep (doc : document.Doc) (seen : List Bool) (op : Nat) (targets : List document.Ref)
    (st : (U64 × Nat × Nat) × List Usize) (i : Nat) : (U64 × Nat × Nat) × List Usize :=
  if h : st.1.1 = 0#u64 ∧ i < targets.length then
    let r := targets[i]'h.2
    let k := checkAnchorM (.Node r) doc.atoms.val.length seen
    if k ≠ 0#u64 then ((k, op, i), st.2)
    else if r.index ∈ st.2 then ((document.DUPLICATE_TARGET, op, i), st.2)
    else (st.1, st.2 ++ [r.index])
  else st

def checkDeleteS (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (targets : List document.Ref) : CheckS :=
  if targets = [] then refuseS st document.EMPTY_DELETION op 0
  else { st with refusal :=
    ((List.range targets.length).foldl (deleteStep doc seen op targets) (st.refusal, [])).1 }

def absR (r : document.Refusal) : U64 × Nat × Nat := (r.kind, r.op.val, r.item.val)

@[step] theorem check_delete_loop_spec (doc : document.Doc) (seen : alloc.vec.Vec Bool) (op : Usize)
    (targets : alloc.vec.Vec document.Ref) (refusal : document.Refusal) :
    document.check_delete_loop doc seen op targets refusal (alloc.vec.Vec.new Usize) 0#usize ⦃ r =>
      absR r = ((List.range targets.val.length).foldl (deleteStep doc seen.val op.val targets.val)
        (absR refusal, [])).1 ⦄ := by
  unfold document.check_delete_loop
  have hl : (List.range targets.val.length).length = targets.val.length := List.length_range
  apply WP.spec_mono (loop_fold2d (List.range targets.val.length)
    (fun (r : document.Refusal) (f : alloc.vec.Vec Usize) => (absR r, f.val))
    (deleteStep doc seen.val op.val targets.val) (fun _ f j => f.val.length ≤ j)
    (fun st => document.check_delete_loop.body doc seen op targets st.1 st.2.1 st.2.2) ?_ refusal
    (alloc.vec.Vec.new Usize) 0#usize (by simp) (by simp))
  · intro r h
    obtain ⟨f, hf⟩ := h
    have := congrArg Prod.fst hf
    simpa using this
  · intro r f i hi hinv
    unfold document.check_delete_loop.body
    h5i_step
    all_goals (try simp only [FoldStep2d])
    all_goals first
      | (trace_state; sorry)

@[step] theorem check_delete_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (targets : alloc.vec.Vec document.Ref) :
    document.check_delete st doc seen op targets ⦃ out =>
      absC out = checkDeleteS (absC st) doc seen.val op.val targets.val ⦄ := by
  unfold document.check_delete checkDeleteS
  h5i_step
  all_goals first
    | (simp_all [absC, refuseS]; done)
    | (trace_state; sorry)


/-! ## Kernel specs of the insert and order checks -/

/-- Row edges of the causal rows are causal relations. -/
theorem rowEdge_iff (doc : document.Doc) (seen : List Bool) (st : CheckS)
    (off : List Bool) (hoff : off.length = doc.edges.val.length)
    (hoffP : ∀ i (h : i < off.length), off[i] = true ↔ OffP doc seen st i)
    (rows : List (alloc.vec.Vec Usize)) (n : Nat) (hn : rows.length = n)
    (hrows : ∀ a (h : a < rows.length) (w : Usize), w ∈ rows[a].val ↔
      (∃ i, ∃ h : causalEdge doc.edges.val off seen i,
        (doc.edges.val[i]'h.1).1.val = a ∧ (doc.edges.val[i]'h.1).2.1 = w) ∨
      ∃ x ∈ st.pending, x.1 = a ∧ x.2 = w.val)
    (wf : ∀ a b, CausalRel doc seen st a b → a < n) (a b : Usize) :
    RowEdge rows a b ↔ CausalRel doc seen st a.val b.val := by
  unfold RowEdge
  constructor
  · rintro ⟨ha, hb⟩
    rcases (hrows a.val ha b).mp hb with ⟨i, ⟨hi, ho, hs⟩, h1, h2⟩ | ⟨x, hx, h1, h2⟩
    · left
      refine ⟨i, hi, ?_, hs, h1, by rw [h2]⟩
      intro hp
      have hio : i < off.length := by omega
      have := (hoffP i hio).mpr hp
      rw [List.getElem?_eq_getElem hio] at ho
      simp_all
    · right
      rw [← h1, ← h2]
      exact hx
  · intro h
    have ha : a.val < rows.length := by rw [hn]; exact wf _ _ h
    refine ⟨ha, (hrows a.val ha b).mpr ?_⟩
    rcases h with ⟨i, hi, hn', hs, h1, h2⟩ | hp
    · left
      have hio : i < off.length := by omega
      refine ⟨i, ⟨hi, ?_, hs⟩, h1, by rw [← UScalar.eq_of_val_eq h2.symm]⟩
      rw [List.getElem?_eq_getElem hio]
      cases hc : off[i]
      · rfl
      · exact absurd ((hoffP i hio).mp hc) hn'
    · exact Or.inr ⟨(a.val, b.val), hp, rfl, rfl⟩

@[step] theorem check_insert_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (l r : document.Anchor) (atoms : alloc.vec.Vec (alloc.vec.Vec U8))
    (wf : DocWF doc) (cwf : CheckWF doc (absC st))
    (room : doc.atoms.val.length + st.created.val + atoms.val.length + 2 < Usize.max)
    (room2 : doc.edges.val.length + st.pending.val.length + atoms.val.length + 2 < Usize.max) :
    document.check_insert st doc seen op l r atoms ⦃ out =>
      absC out = checkInsertS (absC st) doc seen.val op.val l r atoms.val ⦄ := by
  sorry

@[step] theorem check_order_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (before after : document.Ref)
    (wf : DocWF doc) (cwf : CheckWF doc (absC st))
    (room : doc.atoms.val.length + st.created.val + 2 < Usize.max)
    (room2 : doc.edges.val.length + st.pending.val.length + 2 < Usize.max) :
    document.check_order st doc seen op before after ⦃ out =>
      absC out = checkOrderS (absC st) doc seen.val op.val before after ⦄ := by
  unfold document.check_order checkOrderS
  h5i_step
  all_goals first
    | (simp_all [absC, refuseS]; done)
    | (trace_state; sorry)

/-! ## One operation, and validation -/

noncomputable def checkOpS (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (o : document.Op) : CheckS :=
  if st.refusal.1 ≠ 0#u64 then st
  else match o with
    | .Insert l r atoms => checkInsertS st doc seen op l r atoms.val
    | .Delete targets => checkDeleteS st doc seen op targets.val
    | .Restore d => checkRestoreS st doc.deletions.val.length seen op d
    | .Order b a => checkOrderS st doc seen op b a
    | .Keep d i => checkKeepS st doc seen op d i
    | .Retract o => checkRetractS st doc.orders.val seen op o

/-- Well-formedness of the state is kept by every check. -/
theorem checkAnchorM_zero_node {r : document.Ref} {n : Nat} {seen : List Bool}
    (h : checkAnchorM (.Node r) n seen = 0#u64) : r.index.val < n ∧ 2 ≤ r.index.val := by
  unfold checkAnchorM at h
  simp only at h
  split_ifs at h with h1
  · simp [document.UNKNOWN_ANCHOR] at h
  · have hz := checkRefM_zero (missing := document.UNKNOWN_ANCHOR) unknown_anchor_ne (by rw [h]; rfl)
    push_neg at h1
    exact ⟨hz.1, h1 (by rw [h])⟩

theorem checkAnchorM_zero_idx {a : document.Anchor} {n : Nat} {seen : List Bool} (hn : 2 ≤ n)
    (h : checkAnchorM a n seen = 0#u64) : anchorIdx a < n := by
  cases a with
  | Start => simp [anchorIdx]; omega
  | End => simp [anchorIdx]; omega
  | Node r => exact (checkAnchorM_zero_node h).1

theorem foldl_chainStep_bound (first B : Nat) (l : List Nat) (st : List (Nat × Nat) × Nat)
    (hl : ∀ k ∈ l, first + k < B) (h1 : ∀ e ∈ st.1, e.1 < B ∧ e.2 < B) (h2 : st.2 < B) :
    (∀ e ∈ (l.foldl (chainStep first) st).1, e.1 < B ∧ e.2 < B) ∧ (l.foldl (chainStep first) st).2 < B := by
  induction l generalizing st with
  | nil => exact ⟨h1, h2⟩
  | cons k l ih =>
    rw [List.foldl_cons]
    apply ih _ (fun k' hk' => hl k' (List.mem_cons_of_mem _ hk'))
    · intro e he
      simp only [chainStep, List.mem_append, List.mem_singleton] at he
      rcases he with he | rfl
      · exact h1 e he
      · exact ⟨h2, hl k (List.mem_cons_self ..)⟩
    · exact hl k (List.mem_cons_self ..)

theorem chainM_bound (l r first k B : Nat) (hl : l < B) (hr : r < B) (hf : first + k ≤ B) :
    ∀ e ∈ chainM l r first k, e.1 < B ∧ e.2 < B := by
  have := foldl_chainStep_bound first B (List.range k) ([], l)
    (fun k' hk' => by simp at hk'; omega) (by simp) hl
  intro e he
  simp only [chainM, List.mem_append, List.mem_singleton] at he
  rcases he with he | rfl
  · exact this.1 e he
  · exact ⟨this.2, hr⟩

theorem checkOpS_wf (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (o : document.Op) (wf : DocWF doc) (cwf : CheckWF doc st) :
    CheckWF doc (checkOpS st doc seen op o) := by
  dsimp only [checkOpS]
  split_ifs
  · exact cwf
  cases o with
  | Insert l r atoms =>
    dsimp only [checkInsertS]
    split_ifs with h1 h2 h3 h4
    all_goals first
      | exact ⟨cwf.pending, cwf.retracted⟩
      | skip
    refine ⟨?_, cwf.retracted⟩
    have hl := checkAnchorM_zero_idx wf.two (not_not.mp h1)
    have hr := checkAnchorM_zero_idx wf.two (not_not.mp h2)
    intro e he
    simp only [List.mem_append] at he
    rcases he with he | he
    · have := cwf.pending e he; dsimp only; omega
    · have := chainM_bound _ _ _ _ (doc.atoms.val.length + (st.created + atoms.val.length))
        (by omega) (by omega) (by omega) e he
      omega
  | Delete targets => dsimp only [checkDeleteS]; split_ifs <;> exact ⟨cwf.pending, cwf.retracted⟩
  | Restore d => dsimp only [checkRestoreS]; split_ifs <;> exact ⟨cwf.pending, cwf.retracted⟩
  | Order b a =>
    dsimp only [checkOrderS]
    split_ifs with h1 h2 h3
    all_goals first
      | exact ⟨cwf.pending, cwf.retracted⟩
      | skip
    refine ⟨?_, cwf.retracted⟩
    have hb := checkAnchorM_zero_node (not_not.mp h1)
    have ha := checkAnchorM_zero_node (not_not.mp h2)
    intro e he
    simp only [List.mem_append, List.mem_singleton] at he
    rcases he with he | rfl
    · exact cwf.pending e he
    · simp only; omega
  | Keep d i => dsimp only [checkKeepS]; split_ifs <;> exact ⟨cwf.pending, cwf.retracted⟩
  | Retract o =>
    dsimp only [checkRetractS]
    split_ifs with h1
    · exact ⟨cwf.pending, cwf.retracted⟩
    refine ⟨cwf.pending, ?_⟩
    intro e he
    simp only [List.mem_append, List.mem_singleton] at he
    rcases he with he | rfl
    · exact cwf.retracted e he
    · have hz := checkRefM_zero (missing := document.UNKNOWN_ORDER) unknown_order_ne (by rw [not_not.mp h1]; rfl)
      have hm : doc.orders.val[o.index.val]! ∈ doc.orders.val := by
        rw [getElem!_pos doc.orders.val o.index.val hz.1]; exact List.getElem_mem _
      exact wf.orders _ hm


/-- The marks `observed` returns. -/
noncomputable def seenM (deps : List (alloc.vec.Vec Usize)) (roots : List Usize) : List Bool :=
  open Classical in
  (List.range deps.length).map fun i => decide (∃ v : Usize, v.val = i ∧ Indexed.RowReach deps roots v)

/-- What `validate` computes: the first refusal over the operations. -/
noncomputable def validateS (doc : document.Doc) (ops : List document.Op) (seen : List Bool) :
    U64 × Nat × Nat :=
  (ops.zipIdx.foldl (fun st x => checkOpS st doc seen x.2 x.1) ⟨(0#u64, 0, 0), [], 0, []⟩).refusal

@[step] theorem check_op_spec (st : document.Check) (doc : document.Doc) (seen : alloc.vec.Vec Bool)
    (op : Usize) (o : document.Op) (wf : DocWF doc) (cwf : CheckWF doc (absC st))
    (room : doc.atoms.val.length + st.created.val + (match o with | .Insert _ _ a => a.val.length | _ => 0) + 2
      < Usize.max)
    (room2 : doc.edges.val.length + st.pending.val.length + st.retracted.val.length +
      (match o with | .Insert _ _ a => a.val.length | _ => 0) + 2 < Usize.max) :
    document.check_op st doc seen op o ⦃ out => absC out = checkOpS (absC st) doc seen.val op.val o ⦄ := by
  unfold document.check_op checkOpS
  by_cases hk : st.refusal.kind = 0#u64
  · have hk' : ¬ (st.refusal.kind != 0#u64) = true := by simp [hk]
    have hk'' : ¬ (absC st).refusal.1 ≠ 0#u64 := by simp [absC, hk]
    simp only [hk', hk'', ↓reduceIte]
    cases o with
    | Insert l r atoms => exact check_insert_spec st doc seen op l r atoms wf cwf (by simpa using room) (by simp at room2; omega)
    | Delete targets => exact check_delete_spec st doc seen op targets
    | Restore d => exact check_restore_spec st doc seen op d
    | Order b a => exact check_order_spec st doc seen op b a wf cwf (by simpa using room) (by simp at room2; omega)
    | Keep d i => exact check_keep_spec st doc seen op d i
    | Retract o => exact check_retract_spec st doc seen op o (by simp at room2; omega)
  · have hk' : (st.refusal.kind != 0#u64) = true := by simp [hk]
    have hk'' : (absC st).refusal.1 ≠ 0#u64 := by simp [absC, hk]
    simp [hk', hk'']


/-- Atoms an operation inserts. -/
def insSize : document.Op → Nat
  | .Insert _ _ a => a.val.length
  | _ => 0

theorem chainM_length (l r f k : Nat) : (chainM l r f k).length = k + 1 := by
  have := foldl_chainStep_length f (List.range k) [] l
  simp [chainM, this]

/-- How much one check can grow the state. -/
theorem checkOpS_growth (st : CheckS) (doc : document.Doc) (seen : List Bool) (op : Nat)
    (o : document.Op) :
    (checkOpS st doc seen op o).created ≤ st.created + insSize o ∧
    (checkOpS st doc seen op o).pending.length ≤ st.pending.length + insSize o + 1 ∧
    (checkOpS st doc seen op o).retracted.length ≤ st.retracted.length + 1 := by
  dsimp only [checkOpS]
  split_ifs
  · exact ⟨by omega, by omega, by omega⟩
  cases o with
  | Insert l r atoms =>
    dsimp only [checkInsertS]
    split_ifs <;> simp [refuseS, insSize, chainM_length] <;> omega
  | Delete t => dsimp only [checkDeleteS]; split_ifs <;> simp [refuseS] <;> simp [insSize] <;> omega
  | Restore x => dsimp only [checkRestoreS]; split_ifs <;> simp [refuseS] <;> simp [insSize] <;> omega
  | Order b a => dsimp only [checkOrderS]; split_ifs <;> simp [refuseS] <;> simp [insSize] <;> omega
  | Keep x y => dsimp only [checkKeepS]; split_ifs <;> simp [refuseS] <;> simp [insSize] <;> omega
  | Retract x => dsimp only [checkRetractS]; split_ifs <;> simp [refuseS] <;> simp [insSize] <;> omega

/-- The validation state after `j` operations stays within the patch's sizes. -/
def VInv (doc : document.Doc) (ops : List document.Op) (s : document.Check) (j : Nat) : Prop :=
  CheckWF doc (absC s) ∧ s.created.val ≤ ((ops.take j).map insSize).sum ∧
    s.pending.val.length ≤ ((ops.take j).map (insSize · + 1)).sum ∧ s.retracted.val.length ≤ j

theorem validate_loop_spec (doc : document.Doc) (ops : alloc.vec.Vec document.Op) (seen : alloc.vec.Vec Bool)
    (wf : DocWF doc)
    (room : doc.atoms.val.length + (ops.val.map insSize).sum + 2 < Usize.max)
    (room2 : doc.edges.val.length + (ops.val.map (insSize · + 1)).sum + ops.val.length + 2 < Usize.max) :
    document.validate_loop doc.deps doc.creator doc.atoms doc.edges doc.insertions doc.deletions
      doc.restored doc.acked doc.orders doc.retractions ops seen
      ⟨⟨0#u64, 0#usize, 0#usize⟩, alloc.vec.Vec.new _, 0#usize, alloc.vec.Vec.new _⟩ 0#usize ⦃ st =>
      absC st = ops.val.zipIdx.foldl (fun s x => checkOpS s doc seen.val x.2 x.1)
        ⟨(0#u64, 0, 0), [], 0, []⟩ ⦄ := by
  unfold document.validate_loop
  have hl : ops.val.zipIdx.length = ops.val.length := List.length_zipIdx
  apply WP.spec_mono (Indexed.fold_with_inv ops.val.zipIdx absC
    (fun s x => checkOpS s doc seen.val x.2 x.1) (fun s j => VInv doc ops.val s j)
    (fun st => document.validate_loop.body doc.deps doc.creator doc.atoms doc.edges doc.insertions
      doc.deletions doc.restored doc.acked doc.orders doc.retractions ops seen st.1 st.2) ?_ _ 0#usize
    (by simp) ⟨⟨by simp [absC], by simp [absC]⟩, by simp, by simp, by simp⟩)
  · intro r h
    simpa [absC] using h.1
  · intro s i hi hinv
    unfold document.validate_loop.body
    h5i_step
    all_goals (try simp only [Indexed.FoldInvStep])
    all_goals first
      | (trace_state; sorry)


/-! ## Commit, at the list level -/

/-- The document's fields as plain lists of numbers. -/
structure DocL where
  deps : List (List Nat)
  creator : List Nat
  atoms : List (List Nat)
  edges : List (Nat × Nat × Nat)
  insertions : List (Nat × Nat × Nat)
  deletions : List (Nat × List Nat)
  restored : List Bool
  acked : List (Nat × Nat)
  orders : List Nat
  retractions : List (Nat × Nat)

def absD (d : document.Doc) : DocL where
  deps := d.deps.val.map fun r => r.val.map (·.val)
  creator := d.creator.val.map (·.val)
  atoms := d.atoms.val.map fun a => a.val.map (·.val)
  edges := d.edges.val.map fun e => (e.1.val, e.2.1.val, e.2.2.val)
  insertions := d.insertions.val.map fun x => (x.patch.val, x.left.val, x.right.val)
  deletions := d.deletions.val.map fun x => (x.patch.val, x.targets.val.map (·.val))
  restored := d.restored.val
  acked := d.acked.val.map pairVal
  orders := d.orders.val.map (·.val)
  retractions := d.retractions.val.map pairVal

/-- One committed operation of patch `p`. -/
def commitOpL (D : DocL) (p : Nat) : document.Op → DocL
  | .Insert l r atoms =>
    let k := atoms.val.length
    { D with creator := D.creator ++ List.replicate k p,
             atoms := D.atoms ++ atoms.val.map (fun a => a.val.map (·.val)),
             edges := D.edges ++ (chainM (anchorIdx l) (anchorIdx r) D.atoms.length k).map
               (fun e => (e.1, e.2, p)),
             insertions := D.insertions ++ [(p, anchorIdx l, anchorIdx r)] }
  | .Delete ts =>
    { D with deletions := D.deletions ++ [(p, ts.val.map (·.index.val))],
             restored := D.restored ++ [false] }
  | .Restore d => { D with restored := D.restored.set d.index.val true }
  | .Order b a =>
    { D with orders := D.orders ++ [D.edges.length],
             edges := D.edges ++ [(b.index.val, a.index.val, p)] }
  | .Keep d i =>
    if (d.index.val, i.index.val) ∈ D.acked then D
    else { D with acked := D.acked ++ [(d.index.val, i.index.val)] }
  | .Retract o =>
    { D with retractions := D.retractions ++ [(D.orders[o.index.val]!, p)] }

/-- A committed patch: its dependency row, then its operations in order. -/
def commitL (D : DocL) (deps : List Nat) (ops : List document.Op) : DocL :=
  ops.foldl (fun D o => commitOpL D D.deps.length.pred o)
    { D with deps := D.deps ++ [deps] }


/-- The empty document: the two sentinels and the sentinel edge. -/
def emptyL : DocL where
  deps := []
  creator := [document.NONE.val, document.NONE.val]
  atoms := [[], []]
  edges := [(0, 1, document.NONE.val)]
  insertions := []
  deletions := []
  restored := []
  acked := []
  orders := []
  retractions := []

@[step] theorem empty_spec : document.empty ⦃ d => absD d = emptyL ⦄ := by
  unfold document.empty
  h5i_step
  all_goals first
    | (simp_all [absD, emptyL, pairVal, document.START, document.END]; done)
    | (trace_state; sorry)


/-! ## Kernel specs of commit -/

theorem foldl_pair_snoc {α β γ} (p : β) (f : α → γ) : ∀ (l : List α) (a : List β) (b : List γ),
    l.foldl (fun acc x => (acc.1 ++ [p], acc.2 ++ [f x])) (a, b) = (a ++ List.replicate l.length p, b ++ l.map f)
  | [], a, b => by simp
  | x :: l, a, b => by
    simp only [List.foldl_cons, foldl_pair_snoc p f l, List.length_cons, List.replicate_succ, List.map_cons]
    simp

theorem commit_insert_loop0_spec (creator : alloc.vec.Vec Usize) (atomsD : alloc.vec.Vec (alloc.vec.Vec U8))
    (patch : Usize) (atoms : alloc.vec.Vec (alloc.vec.Vec U8))
    (room : creator.val.length + atoms.val.length < Usize.max)
    (room2 : atomsD.val.length + atoms.val.length < Usize.max) :
    document.commit_insert_loop0 creator atomsD patch atoms 0#usize ⦃ r =>
      r.1.val.map (·.val) = creator.val.map (·.val) ++ List.replicate atoms.val.length patch.val ∧
      r.2.val.map (fun a => a.val.map (·.val)) =
        atomsD.val.map (fun a => a.val.map (·.val)) ++ atoms.val.map (fun a => a.val.map (·.val)) ⦄ := by
  unfold document.commit_insert_loop0
  apply WP.spec_mono (loop_fold2 atoms.val
    (fun (c : alloc.vec.Vec Usize) (a : alloc.vec.Vec (alloc.vec.Vec U8)) =>
      (c.val.map (·.val), a.val.map (fun a => a.val.map (·.val))))
    (fun acc x => (acc.1 ++ [patch.val], acc.2 ++ [x.val.map (·.val)]))
    (fun c a j => c.val.length = creator.val.length + j ∧ a.val.length = atomsD.val.length + j)
    (fun st => document.commit_insert_loop0.body patch atoms st.1 st.2.1 st.2.2) ?_ creator atomsD
    0#usize (by simp) (by simp))
  · intro r h
    obtain ⟨h, -⟩ := h
    rw [show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, foldl_pair_snoc] at h
    simp only [Prod.mk.injEq] at h
    exact h
  · intro c a k hk hinv
    unfold document.commit_insert_loop0.body
    h5i_step
    all_goals simp_all [FoldStep2]
    all_goals scalar_tac

theorem foldl_snoc_map {α β} (f : α → β) : ∀ (l : List α) (acc : List β),
    l.foldl (fun o r => o ++ [f r]) acc = acc ++ l.map f
  | [], acc => by simp
  | x :: l, acc => by simp [foldl_snoc_map f l]

theorem foldl_or_any {α} (p : α → Bool) : ∀ (l : List α) (init : Bool),
    l.foldl (fun f x => f || p x) init = (init || l.any p)
  | [], init => by simp
  | x :: l, init => by simp [foldl_or_any p l, Bool.or_assoc]

@[step] theorem ref_indices_spec (rs : alloc.vec.Vec document.Ref) :
    document.ref_indices rs ⦃ out => out.val.map (·.val) = rs.val.map (·.index.val) ⦄ := by
  unfold document.ref_indices document.ref_indices_loop
  apply WP.spec_mono (loop_fold rs.val (fun o : alloc.vec.Vec Usize => o.val.map (·.val))
    (fun o r => o ++ [r.index.val]) (fun o j => o.val.length ≤ j)
    (fun st => document.ref_indices_loop.body rs st.1 st.2) ?_ (alloc.vec.Vec.new _) 0#usize
    (by simp) (by simp))
  · intro out h
    rw [h]
    simp [foldl_snoc_map (fun r : document.Ref => r.index.val)]
  · intro o i hi hinv
    unfold document.ref_indices_loop.body
    h5i_step
    all_goals simp_all [FoldStep]
    all_goals scalar_tac

@[step] theorem has_pair_spec (pairs : alloc.vec.Vec (Usize × Usize)) (a b : Usize) :
    document.has_pair pairs a b ⦃ r => r = true ↔ (a.val, b.val) ∈ pairs.val.map pairVal ⦄ := by
  unfold document.has_pair document.has_pair_loop
  apply WP.spec_mono (loop_fold pairs.val (fun f : Bool => f)
    (fun f x => f || (x.1 == a && x.2 == b)) (fun _ _ => True)
    (fun st => document.has_pair_loop.body pairs a b st.1 st.2) ?_ false 0#usize (by simp) trivial)
  · intro r h
    rw [h]
    simp only [List.drop_zero, foldl_or_any (fun x : Usize × Usize => x.1 == a && x.2 == b),
      Bool.false_or, List.any_eq_true, Bool.and_eq_true, beq_iff_eq, List.mem_map, pairVal]
    constructor
    · rintro ⟨x, hx, h1, h2⟩
      exact ⟨x, hx, by rw [h1, h2]⟩
    · rintro ⟨x, hx, he⟩
      simp only [Prod.mk.injEq] at he
      exact ⟨x, hx, UScalar.eq_of_val_eq he.1, UScalar.eq_of_val_eq he.2⟩
  · intro f i hi _
    unfold document.has_pair_loop.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (split
         h5i_step
         all_goals first
           | (simp_all [FoldStep]; done)
           | (constructor <;> simp_all [FoldStep] <;> scalar_tac)
           | (trace_state; sorry))
      | (trace_state; sorry)

end o5s_kernel.DocKernel
