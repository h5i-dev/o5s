import Components
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Indexed

structure Seen (marks : List Bool) (queue : List Usize) : Prop where
  nodup : queue.Nodup
  bounded : ∀ v ∈ queue, v.val < marks.length
  tracks : ∀ v (h : v.val < marks.length), marks[v.val] = true ↔ v ∈ queue

theorem queue_size {marks : List Bool} {queue : List Usize} (h : Seen marks queue) :
    queue.length ≤ marks.length := by
  have hn : (queue.map (fun x => x.val)).Nodup := h.nodup.map (fun _ _ he => UScalar.eq_of_val_eq he)
  have hs : queue.map (fun x => x.val) ⊆ List.range marks.length := by
    intro v hv
    obtain ⟨x,hx,rfl⟩ := List.mem_map.mp hv
    exact List.mem_range.mpr (h.bounded x hx)
  have := (hn.subperm hs).length_le
  simpa using this

theorem seen_insert {marks : List Bool} {queue : List Usize} (h : Seen marks queue)
    (v : Usize) (hv : v.val < marks.length) (fresh : marks[v.val] = false) :
    Seen (marks.set v.val true) (queue ++ [v]) := by
  have hn : v ∉ queue := by
    intro he
    have ht := (h.tracks v hv).2 he
    simp_all
  refine ⟨List.Nodup.append h.nodup (List.nodup_singleton _) (by simpa using hn), ?_, ?_⟩
  · intro x hx
    have hm : (marks.set v.val true).length = marks.length := by simp
    rw [hm]
    rcases List.mem_append.mp hx with hx | hx
    · exact h.bounded x hx
    · simpa using List.mem_singleton.mp hx ▸ hv
  · intro x hx
    have hxb : x.val < marks.length := by simpa using hx
    by_cases he : x = v
    · subst x; simp [List.getElem_set, hv]
    · have hne : x.val ≠ v.val := fun e => he (UScalar.eq_of_val_eq e)
      simpa [List.getElem_set, hne, Ne.symm hne, he] using h.tracks x hxb

def FoldInvStep {α σ β : Type} (l : List α) (abs : σ → β) (g : β → α → β)
    (Inv : σ → Nat → Prop) (s : σ) (i : Usize) : ControlFlow (σ × Usize) σ → Prop
  | .done out => l.length ≤ i.val ∧ abs out = abs s ∧ Inv out l.length
  | .cont (out,j) => ∃ h : i.val < l.length,
      abs out = g (abs s) l[i.val] ∧ j.val = i.val + 1 ∧ Inv out j.val

theorem fold_with_inv {α σ β : Type} (l : List α) (abs : σ → β) (g : β → α → β)
    (Inv : σ → Nat → Prop) (body : σ × Usize → Result (ControlFlow (σ × Usize) σ))
    (hstep : ∀ s i, i.val ≤ l.length → Inv s i.val → body (s,i) ⦃ FoldInvStep l abs g Inv s i ⦄)
    (s : σ) (i : Usize) (hi : i.val ≤ l.length) (hinv : Inv s i.val) :
    loop body (s,i) ⦃ out => abs out = (l.drop i.val).foldl g (abs s) ∧ Inv out l.length ⦄ := by
  apply loop.spec_decr_nat (measure := fun st : σ × Usize => l.length - st.2.val)
    (inv := fun st => st.2.val ≤ l.length ∧ Inv st.1 st.2.val ∧
      (l.drop i.val).foldl g (abs s) = (l.drop st.2.val).foldl g (abs st.1))
  · rintro ⟨t,j⟩ ⟨hj,hI,heq⟩
    apply WP.spec_mono (hstep t j hj hI)
    intro r h
    cases r with
    | done t' =>
      obtain ⟨hle,habs,hI'⟩ := h
      refine ⟨?_, hI'⟩
      rw [heq, List.drop_eq_nil_of_le hle, habs]; rfl
    | cont st =>
      obtain ⟨t',k⟩ := st
      obtain ⟨hlt,habs,hk,hI'⟩ := h
      refine ⟨⟨by simpa using (show k.val ≤ l.length by omega),hI',?_⟩,?_⟩
      · rw [heq, List.drop_eq_getElem_cons hlt, List.foldl_cons, ← habs, hk]
      · change l.length - k.val < l.length - j.val
        omega
  · exact ⟨hi,hinv,rfl⟩

def StateInv (n : Nat) (s : indexed.Traversal) : Prop :=
  s.marks.val.length = n ∧ Seen s.marks.val s.queue.val

@[step] theorem enqueue_spec (n : Nat) (state : indexed.Traversal) (neighbors : alloc.vec.Vec Usize)
    (inv : StateInv n state) (room : n < Usize.max)
    (bounds : ∀ v ∈ neighbors.val, v.val < n) :
    indexed.enqueue state neighbors ⦃ out =>
      out.queue.val = neighbors.val.foldl insertStep state.queue.val ∧ StateInv n out ⦄ := by
  unfold indexed.enqueue indexed.enqueue_loop
  apply WP.spec_mono (fold_with_inv neighbors.val
    (fun st : indexed.Traversal => st.queue.val) insertStep (fun st _ => StateInv n st)
    (fun st => indexed.enqueue_loop.body neighbors st.1 st.2) ?_ state 0#usize (by simp) inv)
  · intro out h; simpa using h
  · intro st i hi hinv
    unfold indexed.enqueue_loop.body
    h5i_step
    · have idx : i.val < neighbors.val.length := by scalar_tac
      rw [hinv.1]
      exact bounds _ (List.getElem_mem _)
    · have idx : i.val < neighbors.val.length := by scalar_tac
      have hb : neighbors.val[i.val].val < st.marks.val.length := by
        rw [hinv.1]; exact bounds _ (List.getElem_mem _)
      have hm := (hinv.2.tracks _ hb).1 b_post
      refine ⟨idx, ?_, by scalar_tac, hinv⟩
      simp [insertStep, hm]
    · have idx : i.val < neighbors.val.length := by scalar_tac
      rw [hinv.1]
      exact bounds _ (List.getElem_mem _)
    · have idx : i.val < neighbors.val.length := by scalar_tac
      have hb : neighbors.val[i.val].val < st.marks.val.length := by
        rw [hinv.1]; exact bounds _ (List.getElem_mem _)
      obtain ⟨old,back⟩ := v
      simp only at v1 v_post
      rw [v_post]
      h5i_step
      · have hs := queue_size hinv.2
        rw [hinv.1] at hs
        omega
      · have fresh : neighbors.val[i.val] ∉ st.queue.val := by
          intro he
          have ht := (hinv.2.tracks _ hb).2 he
          simp_all
        refine ⟨idx, ?_, i2_post, ?_⟩
        · simp [insertStep, fresh, x_post]
        · constructor
          · simpa [alloc.vec.Vec.set] using hinv.1
          · simpa [alloc.vec.Vec.set, x_post] using seen_insert hinv.2 _ hb b_post
    · exact ⟨by scalar_tac, rfl, hinv⟩

def eraseStep (marks : List Bool) (v : Usize) := marks.set v.val false

@[step] theorem clear_spec (state : indexed.Traversal)
    (bounds : ∀ v ∈ state.queue.val, v.val < state.marks.val.length) :
    indexed.clear state ⦃ out => out.queue = state.queue ∧
      out.marks.val = state.queue.val.foldl eraseStep state.marks.val ⦄ := by
  unfold indexed.clear indexed.clear_loop
  have run : loop (fun st : indexed.Traversal × Usize => indexed.clear_loop.body st.1 st.2)
      (state, 0#usize) ⦃ out => out.2 = state.queue ∧
      out.1.val = state.queue.val.foldl eraseStep state.marks.val ⦄ := by
    apply loop.spec_decr_nat
      (measure := fun st : indexed.Traversal × Usize => state.queue.val.length - st.2.val)
      (inv := fun st => st.1.queue = state.queue ∧ st.2.val ≤ state.queue.val.length ∧
        st.1.marks.val.length = state.marks.val.length ∧
        state.queue.val.foldl eraseStep state.marks.val =
          (state.queue.val.drop st.2.val).foldl eraseStep st.1.marks.val)
    · rintro ⟨st,i⟩ ⟨hq,hi,hl,he⟩
      simp only at hq hi hl he
      unfold indexed.clear_loop.body
      h5i_step
      have idx : i.val < state.queue.val.length := by scalar_tac
      refine ⟨idx, ?_, by omega⟩
      rw [List.drop_eq_getElem_cons idx, List.foldl_cons]
      rfl
    · simp
  h5i_step

theorem erase_length (queue : List Usize) (marks : List Bool) :
    (queue.foldl eraseStep marks).length = marks.length := by
  induction queue generalizing marks with
  | nil => rfl
  | cons v tail ih => simpa [List.foldl_cons, eraseStep] using ih (marks.set v.val false)

theorem erase_lookup (queue : List Usize) (marks : List Bool) (v : Usize)
    (hv : v.val < marks.length) :
    (queue.foldl eraseStep marks)[v.val]'(by rw [erase_length]; exact hv) =
      if v ∈ queue then false else marks[v.val] := by
  induction queue generalizing marks with
  | nil => simp
  | cons x tail ih =>
    simp only [List.foldl_cons]
    have h := ih (marks.set x.val false) (by simpa using hv)
    simp only [eraseStep] at *
    simp only [h]
    by_cases he : v = x
    · subst x; simp [eraseStep, List.getElem_set]
    · have hn : v.val ≠ x.val := fun h => he (UScalar.eq_of_val_eq h)
      simp [eraseStep, List.getElem_set, he, hn, Ne.symm hn]


inductive RowReach (rows : List (alloc.vec.Vec Usize)) (seeds : List Usize) : Usize → Prop
  | seed {v} : v ∈ seeds → RowReach rows seeds v
  | edge {u v} : RowReach rows seeds u → (h : u.val < rows.length) →
      v ∈ rows[u.val].val → RowReach rows seeds v

theorem prefix_insert (neighbors queue : List Usize) :
    queue <+: neighbors.foldl insertStep queue := by
  induction neighbors generalizing queue with
  | nil => exact List.prefix_refl _
  | cons v tail ih =>
    rw [List.foldl_cons]
    apply List.IsPrefix.trans _ (ih _)
    unfold insertStep; split
    · exact List.prefix_refl _
    · exact List.prefix_append _ _

structure SearchInv (rows : List (alloc.vec.Vec Usize)) (seeds : List Usize)
    (state : indexed.Traversal) (cursor : Nat) : Prop where
  seen : StateInv rows.length state
  cursor_bound : cursor ≤ state.queue.val.length
  sound : ∀ v ∈ state.queue.val, RowReach rows seeds v
  seeds_mem : seeds ⊆ state.queue.val
  processed : ∀ (j : Nat) (hj : j < state.queue.val.length), j < cursor →
    ∀ (hv : state.queue.val[j].val < rows.length),
      ∀ v ∈ rows[state.queue.val[j].val].val, v ∈ state.queue.val


theorem search_advance {rows : List (alloc.vec.Vec Usize)} {seeds : List Usize}
    {state out : indexed.Traversal} {i : Nat} (inv : SearchInv rows seeds state i)
    (hi : i < state.queue.val.length)
    (qeq : out.queue.val = (rows[state.queue.val[i].val]'(by
      rw [← inv.seen.1]; exact inv.seen.2.bounded _ (List.getElem_mem _))).val.foldl insertStep state.queue.val)
    (seen : StateInv rows.length out) : SearchInv rows seeds out (i + 1) := by
  have hb : state.queue.val[i].val < rows.length := by
    rw [← inv.seen.1]; exact inv.seen.2.bounded _ (List.getElem_mem _)
  have pref : state.queue.val <+: out.queue.val := by rw [qeq]; exact prefix_insert _ _
  have members (v : Usize) : v ∈ out.queue.val ↔
      v ∈ state.queue.val ∨ v ∈ rows[state.queue.val[i].val].val := by
    rw [qeq]; exact mem_foldl_insertStep _ _ _
  refine ⟨seen, by have := pref.length_le; omega, ?_, ?_, ?_⟩
  · intro v hv
    rcases (members v).1 hv with hv | hv
    · exact inv.sound _ hv
    · exact RowReach.edge (inv.sound _ (List.getElem_mem _)) hb hv
  · intro v hv; exact (members v).2 (.inl (inv.seeds_mem hv))
  · intro j hj hc hv v hmem
    have hjold : j < state.queue.val.length := by omega
    have eqv : state.queue.val[j] = out.queue.val[j] := pref.getElem hjold
    have hb' : state.queue.val[j].val < rows.length := by
      rw [← inv.seen.1]; exact inv.seen.2.bounded _ (List.getElem_mem _)
    have roweq : rows[state.queue.val[j].val] = rows[out.queue.val[j].val] := by
      congr 1
      exact congrArg (fun x : Usize => x.val) eqv
    have hm : v ∈ rows[state.queue.val[j].val].val := by
      simpa only [roweq] using hmem
    by_cases he : j = i
    · subst j; exact (members v).2 (.inr hm)
    · exact (members v).2 (.inl (inv.processed j hjold (by omega) hb' v hm))


theorem search_complete {rows : List (alloc.vec.Vec Usize)} {seeds : List Usize}
    {state : indexed.Traversal} {i : Nat} (inv : SearchInv rows seeds state i)
    (done : state.queue.val.length ≤ i) :
    ∀ v, v ∈ state.queue.val ↔ RowReach rows seeds v := by
  intro v
  constructor
  · exact inv.sound v
  · intro hr
    induction hr with
    | seed hm => exact inv.seeds_mem hm
    | edge hu hb hm ih =>
      obtain ⟨j,hj,eqv⟩ := List.getElem_of_mem ih
      have hbj : state.queue.val[j].val < rows.length := by simpa only [eqv] using hb
      apply inv.processed j hj (by omega) hbj
      simpa only [eqv] using hm

@[step] theorem search_loop_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize))
    (seeds : List Usize) (state : indexed.Traversal) (i : Usize)
    (inv : SearchInv rows.val seeds state i.val) (room : rows.val.length < Usize.max)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    indexed.reachable_loop rows state i ⦃ out =>
      StateInv rows.val.length {marks := out.1, queue := out.2} ∧
      ∀ v, v ∈ out.2.val ↔ RowReach rows.val seeds v ⦄ := by
  unfold indexed.reachable_loop
  apply loop.spec_decr_nat
    (measure := fun st : indexed.Traversal × Usize => rows.val.length + 1 - st.2.val)
    (inv := fun st => SearchInv rows.val seeds st.1 st.2.val)
  · rintro ⟨st,j⟩ hI
    simp only at hI
    unfold indexed.reachable_loop.body
    h5i_step
    · rw [← hI.seen.1]
      exact hI.seen.2.bounded _ (List.getElem_mem _)
    · exact hI.seen
    · exact bounds _ (List.getElem_mem _)
    · have hj : j.val < st.queue.val.length := by scalar_tac
      refine ⟨search_advance hI hj state1_post state1_post1, ?_⟩
      have hs := queue_size hI.seen.2
      rw [hI.seen.1] at hs
      omega
    · exact ⟨hI.seen, search_complete hI (by scalar_tac)⟩
  · exact inv


def Clean (marks : List Bool) : Prop := ∀ (v : Usize) (h : v.val < marks.length), marks[v.val] = false

theorem empty_seen (marks : alloc.vec.Vec Bool) (clean : Clean marks.val) : Seen marks.val [] := by
  refine ⟨by simp, by simp, ?_⟩
  intro v hv; simp [clean v hv]

@[step] theorem clear_clean_spec (state : indexed.Traversal) (seen : Seen state.marks.val state.queue.val) :
    indexed.clear state ⦃ out => out.queue = state.queue ∧
      out.marks.val.length = state.marks.val.length ∧ Clean out.marks.val ⦄ := by
  apply WP.spec_mono (clear_spec state seen.bounded)
  intro out h
  refine ⟨h.1, ?_, ?_⟩
  · rw [h.2, erase_length]
  · intro v hv
    have hb : v.val < state.marks.val.length := by simpa [h.2, erase_length] using hv
    have lookup := erase_lookup state.queue.val state.marks.val v hb
    simp only [← h.2] at lookup
    rw [lookup]
    split
    · rfl
    · rename_i hn
      have ht := seen.tracks v hb
      cases he : state.marks.val[v.val] <;> simp_all

@[step] theorem reachable_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize))
    (seeds : alloc.vec.Vec Usize) (marks : alloc.vec.Vec Bool)
    (length : marks.val.length = rows.val.length) (clean : Clean marks.val)
    (room : rows.val.length < Usize.max)
    (seed_bounds : ∀ v ∈ seeds.val, v.val < rows.val.length)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    indexed.reachable rows seeds marks ⦃ out =>
      out.queue.val.Nodup ∧ (∀ v, v ∈ out.queue.val ↔ RowReach rows.val seeds.val v) ∧
      out.marks.val.length = rows.val.length ∧ Clean out.marks.val ⦄ := by
  unfold indexed.reachable
  h5i_step
  · exact seeds.val
  · exact ⟨length, empty_seen marks clean⟩
  · refine ⟨state_post1, by simp, ?_, ?_, ?_⟩
    · intro v hv
      rw [state_post] at hv
      have hm := (mem_foldl_insertStep seeds.val [] v).1 hv
      exact RowReach.seed (by simpa using hm)
    · intro v hv
      rw [state_post]
      exact (mem_foldl_insertStep seeds.val [] v).2 (.inr hv)
    · intro j hj hc; omega
  · exact ⟨v_post.2.nodup, fun _ => Iff.rfl, v_post.1⟩

end o5s_kernel.Indexed
