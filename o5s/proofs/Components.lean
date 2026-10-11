import Projection
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Components
open Projection

/-- Isolation follows from closure under both directions of original edges. -/
def Isolated {α : Type} (edges : List (α × α)) (touched : α → Prop) : Prop :=
  ∀ u v, (u,v) ∈ edges → (touched u ↔ touched v)

theorem segment_outside {α : Type} (edges : List (α × α)) (live touched : α → Prop)
    (iso : Isolated edges touched) {u v : α}
    (seg : Segment edges live u v) (hu : ¬ touched u) : ¬ touched v := by
  induction seg with
  | edge he => exact fun hv => hu ((iso _ _ he).2 hv)
  | extend _ _ he ih => exact fun hv => ih ((iso _ _ he).2 hv)

/-- Edge/liveness equality outside the touched component is sufficient; no
assumption about preserved reachability or freshly computed shortcuts is used. -/
theorem segment_transport {α : Type} (before after : List (α × α))
    (live nextLive touched : α → Prop) (iso : Isolated before touched)
    (sameEdges : ∀ u v, ¬ touched u → (u,v) ∈ before → (u,v) ∈ after)
    (sameLive : ∀ u, ¬ touched u → (live u ↔ nextLive u))
    {u v : α} (seg : Segment before live u v) (hu : ¬ touched u) :
    Segment after nextLive u v := by
  induction seg with
  | edge he => exact .edge (sameEdges _ _ hu he)
  | @extend x v prev dead he ih =>
    have hx := segment_outside before live touched iso prev hu
    exact .extend ih (fun hn => dead ((sameLive x hx).2 hn)) (sameEdges x v hx he)

theorem outside_stable {α : Type} (before after : List (α × α))
    (live nextLive touched : α → Prop) (oldIso : Isolated before touched)
    (newIso : Isolated after touched)
    (sameEdges : ∀ u v, ¬ touched u → ((u,v) ∈ before ↔ (u,v) ∈ after))
    (sameLive : ∀ u, ¬ touched u → (live u ↔ nextLive u)) :
    ∀ u v, ¬ touched u →
      (live u ∧ live v ∧ Segment before live u v ↔
       nextLive u ∧ nextLive v ∧ Segment after nextLive u v) := by
  intro u v hu
  constructor
  · rintro ⟨lu,lv,seg⟩
    have hv := segment_outside before live touched oldIso seg hu
    exact ⟨(sameLive u hu).1 lu, (sameLive v hv).1 lv,
      segment_transport before after live nextLive touched oldIso
        (fun a b h => (sameEdges a b h).1) sameLive seg hu⟩
  · rintro ⟨lu,lv,seg⟩
    have hv := segment_outside after nextLive touched newIso seg hu
    exact ⟨(sameLive u hu).2 lu, (sameLive v hv).2 lv,
      segment_transport after before nextLive live touched newIso
        (fun a b h => (sameEdges a b h).2) (fun a h => (sameLive a h).symm) seg hu⟩

theorem incremental_components {α : Type} (before after old fresh : List (α × α))
    (live nextLive touched : α → Prop) [DecidablePred touched]
    (oldSummary : Summary before old live) (freshSummary : Summary after fresh nextLive)
    (oldIso : Isolated before touched) (newIso : Isolated after touched)
    (sameEdges : ∀ u v, ¬ touched u → ((u,v) ∈ before ↔ (u,v) ∈ after))
    (sameLive : ∀ u, ¬ touched u → (live u ↔ nextLive u)) :
    Summary after (rebuild old fresh touched) nextLive :=
  incremental_summary before after old fresh live nextLive touched oldSummary freshSummary
    (outside_stable before after live nextLive touched oldIso newIso sameEdges sameLive)

def weakEdges {α : Type} (edges : List (α × α)) : List (α × α) :=
  edges ++ edges.map Prod.swap

theorem weak_closed {α : Type} (edges : List (α × α)) (seeds : List α) :
    Isolated edges (Reach (weakEdges edges) seeds) := by
  intro u v he
  constructor
  · intro hu
    exact .step hu (List.mem_append_left _ he)
  · intro hv
    exact .step hv (List.mem_append_right _ (List.mem_map.mpr ⟨(u,v), he, rfl⟩))

/-- Batch additions may join formerly separate components. Closure is computed
on the new original graph, so both sides of every newly connecting edge join. -/
theorem batch_incremental {α : Type} (before added old fresh : List (α × α))
    (live nextLive : α → Prop) (seeds : List α)
    [DecidablePred (Reach (weakEdges (before ++ added)) seeds)]
    (oldSummary : Summary before old live)
    (freshSummary : Summary (before ++ added) fresh nextLive)
    (newSeeds : ∀ u v, (u,v) ∈ added → u ∈ seeds)
    (liveSeeds : ∀ u, u ∉ seeds → (live u ↔ nextLive u)) :
    Summary (before ++ added)
      (rebuild old fresh (Reach (weakEdges (before ++ added)) seeds)) nextLive := by
  have iso := weak_closed (before ++ added) seeds
  apply incremental_components before (before ++ added) old fresh live nextLive
    (Reach (weakEdges (before ++ added)) seeds) oldSummary freshSummary
    (fun u v he => iso u v (List.mem_append_left _ he)) iso
  · intro u v hu
    constructor
    · exact List.mem_append_left _
    · intro he
      rcases List.mem_append.mp he with prior | newEdge
      · exact prior
      · exact False.elim (hu (.start (newSeeds u v newEdge)))
  · intro u hu
    apply liveSeeds u
    exact fun hs => hu (.start hs)

@[step] theorem weak_edges_loop_spec (edges : alloc.vec.Vec (U64 × U64))
    (initial : alloc.vec.Vec (U64 × U64)) (same : initial.val = edges.val)
    (room : 2 * edges.val.length < Usize.max) :
    graph.weak_edges_loop edges initial 0#usize ⦃ out => out.val = weakEdges edges.val ⦄ := by
  unfold graph.weak_edges_loop
  apply WP.spec_mono (loop_fold edges.val (fun out : alloc.vec.Vec (U64 × U64) => out.val)
    (fun out e => out ++ [e.swap])
    (fun out i => out.val.length ≤ edges.val.length + i)
    (fun st => graph.weak_edges_loop.body edges st.1 st.2)
    ?_ initial 0#usize (by simp) (by simp [same]))
  · intro out h
    rw [foldl_map] at h
    simpa [same, weakEdges] using h
  · intro out i hi hinv
    unfold graph.weak_edges_loop.body
    h5i_step
    all_goals simp_all [← i2_post, Prod.swap]
    all_goals scalar_tac

@[step] theorem weak_edges_spec (edges : alloc.vec.Vec (U64 × U64))
    (room : 2 * edges.val.length < Usize.max) :
    graph.weak_edges edges ⦃ out => out.val = weakEdges edges.val ⦄ := by
  unfold graph.weak_edges
  h5i_step

@[step] theorem weak_component_spec (edges : alloc.vec.Vec (U64 × U64))
    (seeds : alloc.vec.Vec U64)
    (room : seeds.val.length + 2 * edges.val.length < Usize.max) :
    graph.weak_component edges seeds ⦃ out =>
      ∀ v, v ∈ out.val ↔ Reach (weakEdges edges.val) seeds.val v ⦄ := by
  unfold graph.weak_component
  h5i_step
  all_goals simp_all [weakEdges]
  all_goals scalar_tac


/-- The touched set here is the result of an actual extracted Rust execution. -/
theorem extracted_batch_incremental (before added old fresh : List (U64 × U64))
    (live nextLive : U64 → Prop) (edges : alloc.vec.Vec (U64 × U64))
    (seeds component : alloc.vec.Vec U64) (same : edges.val = before ++ added)
    (room : seeds.val.length + 2 * edges.val.length < Usize.max)
    (run : graph.weak_component edges seeds = ok component)
    (oldSummary : Summary before old live)
    (freshSummary : Summary (before ++ added) fresh nextLive)
    (newSeeds : ∀ u v, (u,v) ∈ added → u ∈ seeds.val)
    (liveSeeds : ∀ u, u ∉ seeds.val → (live u ↔ nextLive u)) :
    Summary (before ++ added) (rebuild old fresh (· ∈ component.val)) nextLive := by
  classical
  have hc := post_of_ok (weak_component_spec edges seeds room) run
  have heq : (fun v => v ∈ component.val) = Reach (weakEdges (before ++ added)) seeds.val := by
    funext v
    exact propext (by simpa [same] using hc v)
  simp only [heq]
  exact batch_incremental before added old fresh live nextLive seeds.val
    oldSummary freshSummary newSeeds liveSeeds
end o5s_kernel.Components
