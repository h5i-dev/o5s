import Projection

open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Constructor

def dead (edges : List (U64 × U64)) (live : List U64) :=
  edges.filter (fun e => decide (e.1 ∉ live))

def next (edges : List (U64 × U64)) (source : U64) :=
  (edges.filter (fun e => decide (e.1 = source))).map Prod.snd

@[step] theorem dead_edges_spec (edges : alloc.vec.Vec (U64 × U64))
    (live : alloc.vec.Vec U64) :
    graph.dead_edges edges live ⦃ out => out.val = dead edges.val live.val ⦄ := by
  unfold graph.dead_edges graph.dead_edges_loop
  apply WP.spec_mono (loop_fold edges.val (fun o : alloc.vec.Vec (U64 × U64) => o.val)
    (fun o e => if decide (e.1 ∉ live.val) then o ++ [e] else o)
    (fun o j => o.length ≤ j)
    (fun st => graph.dead_edges_loop.body edges live st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h; rw [h, foldl_filter]; simp [dead]
  · intro o i hi hinv
    unfold graph.dead_edges_loop.body
    h5i_step
    all_goals try simp_all only [← i2_post]
    all_goals simp_all [alloc.vec.Vec.len]
    all_goals scalar_tac

@[step] theorem successors_spec (edges : alloc.vec.Vec (U64 × U64)) (source : U64) :
    graph.successors edges source ⦃ out => out.val = next edges.val source ⦄ := by
  unfold graph.successors graph.successors_loop
  apply WP.spec_mono (loop_fold edges.val (fun o : alloc.vec.Vec U64 => o.val)
    (fun o e => if decide (e.1 = source) then o ++ [e.2] else o)
    (fun o j => o.length ≤ j)
    (fun st => graph.successors_loop.body edges source st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h; rw [h, foldl_filter_map]; simp [next]
  · intro o i hi hinv
    unfold graph.successors_loop.body
    h5i_step
    all_goals try simp_all only [← i2_post]
    all_goals simp_all [alloc.vec.Vec.len]
    all_goals scalar_tac

@[step] theorem keep_live_spec (nodes live : alloc.vec.Vec U64) :
    graph.keep_live nodes live ⦃ out => out.val = nodes.val.filter (fun v => decide (v ∈ live.val)) ⦄ := by
  unfold graph.keep_live graph.keep_live_loop
  apply WP.spec_mono (loop_fold nodes.val (fun o : alloc.vec.Vec U64 => o.val)
    (fun o v => if decide (v ∈ live.val) then o ++ [v] else o)
    (fun o j => o.length ≤ j)
    (fun st => graph.keep_live_loop.body nodes live st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h; rw [h, foldl_filter]; simp
  · intro o i hi hinv
    unfold graph.keep_live_loop.body
    h5i_step

@[simp] theorem mem_dead (edges : List (U64 × U64)) (live : List U64) (u v : U64) :
    (u,v) ∈ dead edges live ↔ (u,v) ∈ edges ∧ u ∉ live := by
  simp [dead]

@[simp] theorem mem_next (edges : List (U64 × U64)) (u v : U64) :
    v ∈ next edges u ↔ (u,v) ∈ edges := by
  simp only [next, List.mem_map, List.mem_filter, decide_eq_true_eq]
  constructor
  · rintro ⟨⟨a,b⟩, ⟨he, rfl⟩, rfl⟩; exact he
  · intro he; exact ⟨(u,v), ⟨he, rfl⟩, rfl⟩

/-- The executable constructor searches exactly nonempty dead-interior walks. -/
theorem dead_search_segment (edges : List (U64 × U64)) (live : List U64) (u v : U64) :
    Reach (dead edges live) (next edges u) v ↔
      Projection.Segment edges (· ∈ live) u v := by
  constructor
  · intro h
    induction h with
    | start hv => exact .edge ((mem_next edges _ _).1 hv)
    | step _ he ih =>
      have he' := (mem_dead edges live _ _).1 he
      exact .extend ih he'.2 he'.1
  · intro h
    induction h with
    | edge he => exact .start ((mem_next edges _ _).2 he)
    | extend _ hn he ih => exact .step ih ((mem_dead edges live _ _).2 ⟨he, hn⟩)

@[step] theorem boundary_spec (edges : alloc.vec.Vec (U64 × U64))
    (live : alloc.vec.Vec U64) (u : U64)
    (room : 2 * edges.val.length < Usize.max) :
    graph.boundary edges live u ⦃ out => ∀ v, v ∈ out.val ↔
      v ∈ live.val ∧ Projection.Segment edges.val (· ∈ live.val) u v ⦄ := by
  unfold graph.boundary
  step*
  · have hd : (Constructor.dead edges.val live.val).length ≤ edges.val.length := List.length_filter_le ..
    have hn : (next edges.val u).length ≤ edges.val.length := by
      simp only [next, List.length_map]; exact List.length_filter_le ..
    simp only [dead_post, start_post]
    omega
  · rename_i v
    rw [out_post]
    simp only [List.mem_filter, decide_eq_true_eq]
    rw [reached_post v, dead_post, start_post, dead_search_segment]
    exact and_comm

def rowPredicates (rows : alloc.vec.Vec (alloc.vec.Vec U64)) : List (U64 → Prop) :=
  rows.val.map (fun row v => v ∈ row.val)

theorem fold_append_map {α β : Type} (f : α → β) (xs : List α) (initial : List β) :
    xs.foldl (fun out x => out ++ [f x]) initial = initial ++ xs.map f := by
  induction xs generalizing initial with
  | nil => simp
  | cons x xs ih => simp [ih, List.append_assoc]

@[step] theorem summary_rows_spec (edges : alloc.vec.Vec (U64 × U64))
    (live : alloc.vec.Vec U64) (room : 2 * edges.val.length < Usize.max) :
    graph.summary_rows edges live ⦃ rows => rowPredicates rows =
      live.val.map (fun u v => v ∈ live.val ∧ Projection.Segment edges.val (· ∈ live.val) u v) ⦄ := by
  unfold graph.summary_rows graph.summary_rows_loop
  apply WP.spec_mono (loop_fold live.val rowPredicates
    (fun out u => out ++ [fun v => v ∈ live.val ∧ Projection.Segment edges.val (· ∈ live.val) u v])
    (fun out j => out.val.length ≤ j)
    (fun st => graph.summary_rows_loop.body edges live st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h
    rw [h]
    change (live.val.drop 0).foldl _ [] = _
    rw [List.drop_zero, fold_append_map]
    simp
  · intro out i hi hinv
    unfold graph.summary_rows_loop.body
    h5i_step
    all_goals simp_all [rowPredicates, List.map_append, alloc.vec.Vec.len]
    all_goals try scalar_tac

end o5s_kernel.Constructor
