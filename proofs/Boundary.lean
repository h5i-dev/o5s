import Indexed
import Projection
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Boundary
open Indexed

def alive (live : List Bool) (v : Usize) : Bool := live[v.val]?.getD false

@[step] theorem keep_live_spec (nodes : alloc.vec.Vec Usize) (live : alloc.vec.Vec Bool)
    (bounds : ∀ v ∈ nodes.val, v.val < live.val.length) :
    indexed.keep_live nodes live ⦃ out => out.val = nodes.val.filter (alive live.val) ⦄ := by
  unfold indexed.keep_live indexed.keep_live_loop
  apply WP.spec_mono (loop_fold nodes.val (fun o : alloc.vec.Vec Usize => o.val)
    (fun o v => if alive live.val v then o ++ [v] else o)
    (fun o j => o.length ≤ j)
    (fun st => indexed.keep_live_loop.body nodes live st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h; rw [h, foldl_filter]; simp
  · intro out i hi hinv
    unfold indexed.keep_live_loop.body
    h5i_step
    all_goals have idx : i.val < nodes.val.length := by scalar_tac
    all_goals have hb : nodes.val[i.val].val < live.val.length := bounds _ (List.getElem_mem _)
    · exact ⟨idx, by simp [alive, hb, b_post]⟩
    · exact ⟨⟨idx, by simp [alive, hb, b_post]⟩, by omega⟩

theorem row_reach_bounded (rows : List (alloc.vec.Vec Usize)) (seeds : List Usize)
    (seed_bounds : ∀ v ∈ seeds, v.val < rows.length)
    (bounds : ∀ row ∈ rows, ∀ v ∈ row.val, v.val < rows.length)
    {v : Usize} (h : RowReach rows seeds v) : v.val < rows.length := by
  induction h with
  | seed hm => exact seed_bounds _ hm
  | edge hu hb hm ih => exact bounds _ (List.getElem_mem _) _ hm

/-- Exact adjacency representation of dead-source edges and direct successors. -/
theorem row_segment (edges : List (Usize × Usize)) (live : Usize → Prop)
    (rows : List (alloc.vec.Vec Usize)) (seeds : List Usize) (source : Usize)
    (seed_eq : ∀ v, v ∈ seeds ↔ (source,v) ∈ edges)
    (row_eq : ∀ u v, (∃ h : u.val < rows.length, v ∈ rows[u.val].val) ↔
      (u,v) ∈ edges ∧ ¬ live u) (v : Usize) :
    RowReach rows seeds v ↔ Projection.Segment edges live source v := by
  constructor
  · intro h
    induction h with
    | seed hm => exact .edge ((seed_eq _).1 hm)
    | edge hu hb hm ih =>
      have he := (row_eq _ _).1 ⟨hb,hm⟩
      exact .extend ih he.2 he.1
  · intro h
    induction h with
    | edge he => exact .seed ((seed_eq _).2 he)
    | extend hu hd he ih =>
      obtain ⟨hb,hm⟩ := (row_eq _ _).2 ⟨he,hd⟩
      exact .edge ih hb hm


@[step] theorem boundary_spec (rows : alloc.vec.Vec (alloc.vec.Vec Usize))
    (seeds : alloc.vec.Vec Usize) (live : alloc.vec.Vec Bool) (marks : alloc.vec.Vec Bool)
    (length : marks.val.length = rows.val.length) (live_length : live.val.length = rows.val.length)
    (clean : Clean marks.val) (room : rows.val.length < Usize.max)
    (seed_bounds : ∀ v ∈ seeds.val, v.val < rows.val.length)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    indexed.boundary rows seeds live marks ⦃ out =>
      out.queue.val.Nodup ∧
      (∀ v, v ∈ out.queue.val ↔ RowReach rows.val seeds.val v ∧ alive live.val v = true) ∧
      out.marks.val.length = rows.val.length ∧ Clean out.marks.val ⦄ := by
  unfold indexed.boundary
  h5i_step
  · intro v hv
    exact row_reach_bounded rows.val seeds.val seed_bounds bounds hv
  · exact state_post.filter _


theorem boundary_segment (rows : alloc.vec.Vec (alloc.vec.Vec Usize))
    (seeds : alloc.vec.Vec Usize) (live : alloc.vec.Vec Bool) (marks : alloc.vec.Vec Bool)
    (edges : List (Usize × Usize)) (source : Usize)
    (length : marks.val.length = rows.val.length) (live_length : live.val.length = rows.val.length)
    (clean : Clean marks.val) (room : rows.val.length < Usize.max)
    (seed_bounds : ∀ v ∈ seeds.val, v.val < rows.val.length)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length)
    (seed_eq : ∀ v, v ∈ seeds.val ↔ (source,v) ∈ edges)
    (row_eq : ∀ u v, (∃ h : u.val < rows.val.length, v ∈ rows.val[u.val].val) ↔
      (u,v) ∈ edges ∧ ¬ alive live.val u = true) :
    indexed.boundary rows seeds live marks ⦃ out =>
      (∀ v, v ∈ out.queue.val ↔ alive live.val v = true ∧
        Projection.Segment edges (fun u => alive live.val u = true) source v) ∧
      out.queue.val.Nodup ∧ out.marks.val.length = rows.val.length ∧ Clean out.marks.val ⦄ := by
  apply WP.spec_mono (boundary_spec rows seeds live marks length live_length clean room seed_bounds bounds)
  intro out h
  refine ⟨?_,h.1,h.2.2⟩
  intro v
  rw [h.2.1 v, row_segment edges (fun u => alive live.val u = true) rows.val seeds.val source seed_eq row_eq]
  exact and_comm

end o5s_kernel.Boundary
