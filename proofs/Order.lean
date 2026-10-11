import Constructor

open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Order
attribute [local instance] Classical.propDecidable

/-- An emitted vertex has no predecessor still in the remaining work set. -/
def Ready (edges : List (U64 × U64)) (remaining : List U64) (vertex : U64) : Prop :=
  ∀ predecessor, (predecessor, vertex) ∈ edges → predecessor ∉ remaining

def incoming (edges : List (U64 × U64)) (vertex : U64) : List U64 :=
  (edges.filter (fun edge => decide (edge.2 = vertex))).map Prod.fst

@[simp] theorem mem_incoming (edges : List (U64 × U64)) (vertex predecessor : U64) :
    predecessor ∈ incoming edges vertex ↔ (predecessor, vertex) ∈ edges := by
  simp only [incoming, List.mem_map, List.mem_filter, decide_eq_true_eq]
  constructor
  · rintro ⟨⟨a, b⟩, ⟨he, rfl⟩, rfl⟩
    exact he
  · intro he
    exact ⟨(predecessor, vertex), ⟨he, rfl⟩, rfl⟩

@[step] theorem predecessors_spec (edges : alloc.vec.Vec (U64 × U64)) (vertex : U64) :
    order.predecessors edges vertex ⦃ out => out.val = incoming edges.val vertex ⦄ := by
  unfold order.predecessors order.predecessors_loop
  apply WP.spec_mono (loop_fold edges.val (fun out : alloc.vec.Vec U64 => out.val)
    (fun out edge => if decide (edge.2 = vertex) then out ++ [edge.1] else out)
    (fun out i => out.length ≤ i)
    (fun st => order.predecessors_loop.body edges vertex st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h
    rw [h, foldl_filter_map]
    simp [incoming]
  · intro out i hi hinv
    unfold order.predecessors_loop.body
    h5i_step
    all_goals try simp_all only [← i2_post]
    all_goals simp_all [alloc.vec.Vec.len]
    all_goals scalar_tac

@[step] theorem ready_spec (predecessors remaining : alloc.vec.Vec U64) :
    order.ready predecessors remaining ⦃ ready =>
      ready = decide (∀ vertex ∈ predecessors.val, vertex ∉ remaining.val) ⦄ := by
  unfold order.ready
  h5i_step

@[step] theorem frontier_spec (edges : alloc.vec.Vec (U64 × U64))
    (remaining : alloc.vec.Vec U64) :
    order.frontier edges remaining ⦃ out =>
      out.val = remaining.val.filter (fun vertex => decide (Ready edges.val remaining.val vertex)) ⦄ := by
  unfold order.frontier order.frontier_loop
  apply WP.spec_mono (loop_fold remaining.val (fun out : alloc.vec.Vec U64 => out.val)
    (fun out vertex => if decide (Ready edges.val remaining.val vertex) then out ++ [vertex] else out)
    (fun out i => out.length ≤ i)
    (fun st => order.frontier_loop.body edges remaining st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h
    rw [h, foldl_filter]
    simp
  · intro out i hi hinv
    unfold order.frontier_loop.body
    h5i_step
    all_goals simp_all [Ready, mem_incoming]
    all_goals scalar_tac

/-- Membership is tied to the extracted Rust function, including ambiguous
frontiers: every ready alternative is retained. -/
theorem frontier_exact (edges : alloc.vec.Vec (U64 × U64))
    (remaining : alloc.vec.Vec U64) :
    order.frontier edges remaining ⦃ out => ∀ vertex,
      vertex ∈ out.val ↔ vertex ∈ remaining.val ∧
        ∀ predecessor, (predecessor, vertex) ∈ edges.val → predecessor ∉ remaining.val ⦄ := by
  apply WP.spec_mono (frontier_spec edges remaining)
  intro out h vertex
  simp [h, Ready]

def zeroCount (indegree : List Usize) (vertex : Usize) : Bool :=
  decide (indegree[vertex.val]?.getD 1#usize = 0#usize)

@[step] theorem frontier_counts_spec (indegree remaining : alloc.vec.Vec Usize)
    (bounds : ∀ vertex ∈ remaining.val, vertex.val < indegree.val.length) :
    order.frontier_counts indegree remaining ⦃ out =>
      out.val = remaining.val.filter (zeroCount indegree.val) ⦄ := by
  unfold order.frontier_counts order.frontier_counts_loop
  apply WP.spec_mono (loop_fold remaining.val (fun out : alloc.vec.Vec Usize => out.val)
    (fun out vertex => if zeroCount indegree.val vertex then out ++ [vertex] else out)
    (fun out i => out.length ≤ i)
    (fun st => order.frontier_counts_loop.body indegree remaining st.1 st.2)
    ?_ (alloc.vec.Vec.new _) 0#usize (by simp) (by simp))
  · intro out h
    rw [h, foldl_filter]
    simp
  · intro out i hi hinv
    unfold order.frontier_counts_loop.body
    h5i_step
    all_goals have idx : i.val < remaining.val.length := by scalar_tac
    all_goals have hb : remaining.val[i.val].val < indegree.val.length := bounds _ (List.getElem_mem _)
    · exact ⟨idx, by simp_all [zeroCount, hb]⟩
    · exact ⟨⟨idx, by simp_all [zeroCount, hb]⟩, by omega⟩

/-- The count invariant is a separate caller obligation. Under that invariant,
the production scanner retains exactly the remaining ready graph vertices. -/
theorem frontier_counts_exact (edges : List (Usize × Usize))
    (indegree remaining : alloc.vec.Vec Usize)
    (bounds : ∀ vertex ∈ remaining.val, vertex.val < indegree.val.length)
    (counts : ∀ vertex ∈ remaining.val,
      zeroCount indegree.val vertex = true ↔
        ∀ predecessor, (predecessor, vertex) ∈ edges → predecessor ∉ remaining.val) :
    order.frontier_counts indegree remaining ⦃ out => ∀ vertex,
      vertex ∈ out.val ↔ vertex ∈ remaining.val ∧
        ∀ predecessor, (predecessor, vertex) ∈ edges → predecessor ∉ remaining.val ⦄ := by
  apply WP.spec_mono (frontier_counts_spec indegree remaining bounds)
  intro out h vertex
  rw [h, List.mem_filter]
  constructor
  · intro hv
    exact ⟨hv.1, (counts vertex hv.1).1 hv.2⟩
  · intro hv
    exact ⟨hv.1, (counts vertex hv.1).2 hv.2⟩

end o5s_kernel.Order
