import O5sKernel
import StdSpecs
import H5iAppLib.Graph

open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Projection

/-- An edge or a walk whose internal vertices are dead. Endpoints are
unrestricted here; the boundary summary restricts both endpoints to live. -/
inductive Segment {α : Type} (edges : List (α × α)) (live : α → Prop) : α → α → Prop
  | edge {u v} : (u,v) ∈ edges → Segment edges live u v
  | extend {u x v} : Segment edges live u x → ¬ live x →
      (x,v) ∈ edges → Segment edges live u v

/-- Exact live-boundary summary: a pseudo-edge stands for a dead-only
interior walk, never an invented ordering between concurrent vertices. -/
def Summary {α : Type} (edges summary : List (α × α)) (live : α → Prop) : Prop :=
  ∀ u v, (u,v) ∈ summary ↔ live u ∧ live v ∧ Segment edges live u v

 theorem segment_reach {α : Type} {edges : List (α × α)} {live : α → Prop}
    {u v : α} (h : Segment edges live u v) {start : List α}
    (hu : Reach edges start u) : Reach edges start v := by
  induction h with
  | edge he => exact .step hu he
  | extend _ _ he ih => exact .step ih he

 theorem summary_sound {α : Type} {edges summary : List (α × α)} {live : α → Prop}
    (hs : Summary edges summary live) {start : List α} {v : α}
    (h : Reach summary start v) : Reach edges start v := by
  induction h with
  | start hv => exact .start hv
  | step _ he ih => exact segment_reach ((hs _ _).1 he).2.2 ih

/-- During an original walk, remember the last live vertex and a dead-interior
suffix. This is the invariant needed for completeness, including cycles. -/
 theorem last_live {α : Type} {edges summary : List (α × α)} {live : α → Prop}
    (hs : Summary edges summary live) {u v : α} (hu : live u)
    (h : Reach edges [u] v) :
    ∃ a, live a ∧ Reach summary [u] a ∧ (v = a ∨ Segment edges live a v) := by
  classical
  induction h with
  | start hv =>
      simp only [List.mem_singleton] at hv
      subst hv
      exact ⟨_, hu, .start (by simp), .inl rfl⟩
  | @step x y hx he ih =>
      obtain ⟨a, ha, ra, tail⟩ := ih
      by_cases lx : live x
      · have rx : Reach summary [u] x := by
          rcases tail with eq | seg
          · simpa [eq] using ra
          · exact .step ra ((hs a x).2 ⟨ha, lx, seg⟩)
        exact ⟨x, lx, rx, .inr (.edge he)⟩
      · have seg : Segment edges live a x := by
          rcases tail with eq | seg
          · exact False.elim (lx (eq ▸ ha))
          · exact seg
        exact ⟨a, ha, ra, .inr (.extend seg lx he)⟩

/-- Removing dead interiors preserves exactly the reachability relation on
live endpoints. Reflexive walks, cycles and disconnected graphs are covered. -/
 theorem live_reachability {α : Type} {edges summary : List (α × α)} {live : α → Prop}
    (hs : Summary edges summary live) {u v : α} (hu : live u) (hv : live v) :
    Reach edges [u] v ↔ Reach summary [u] v := by
  constructor
  · intro h
    obtain ⟨a, ha, ra, tail⟩ := last_live hs hu h
    rcases tail with eq | seg
    · simpa [eq] using ra
    · exact .step ra ((hs a v).2 ⟨ha, hv, seg⟩)
  · exact summary_sound hs

/-- Replace only summary edges whose source belongs to a touched component. -/
def rebuild {α : Type} (old fresh : List (α × α)) (touched : α → Prop)
    [DecidablePred touched] : List (α × α) :=
  old.filter (fun e => !decide (touched e.1)) ++ fresh.filter (fun e => decide (touched e.1))

/-- Local recomputation preserves the invariant when dead-interior walks outside
its component are unchanged. Weak-component isolation supplies this condition;
the concrete Rust component finder/constructor is exhaustively tested. -/
theorem incremental_summary {α : Type} (oldEdges newEdges old fresh : List (α × α))
    (live newLive touched : α → Prop) [DecidablePred touched]
    (ho : Summary oldEdges old live) (hf : Summary newEdges fresh newLive)
    (stable : ∀ u v, ¬ touched u →
      (live u ∧ live v ∧ Segment oldEdges live u v ↔
       newLive u ∧ newLive v ∧ Segment newEdges newLive u v)) :
    Summary newEdges (rebuild old fresh touched) newLive := by
  intro u v
  by_cases ht : touched u
  · simpa [rebuild, ht] using hf u v
  · simpa [rebuild, ht, ho u v] using stable u v ht

@[step] theorem reachable_spec (edges : alloc.vec.Vec (U64 × U64)) (start : alloc.vec.Vec U64)
    (room : start.val.length + edges.val.length < Usize.max) :
    o5s_kernel.graph.reachable edges start
      ⦃ out => ∀ v, v ∈ out.val ↔ Reach edges.val start.val v ⦄ := by
  unfold o5s_kernel.graph.reachable
  step*

/-- Both sides execute the extracted Rust search. A correct summary constructor
is an explicit precondition, not silently assumed to have been verified. -/
theorem extracted_projection (edges summary : alloc.vec.Vec (U64 × U64))
    (start : alloc.vec.Vec U64) (u v : U64) (live : U64 → Prop)
    (single : start.val = [u]) (hu : live u) (hv : live v)
    (hs : Summary edges.val summary.val live)
    (roomE : start.val.length + edges.val.length < Usize.max)
    (roomS : start.val.length + summary.val.length < Usize.max)
    (original projected : alloc.vec.Vec U64)
    (runE : o5s_kernel.graph.reachable edges start = ok original)
    (runS : o5s_kernel.graph.reachable summary start = ok projected) :
    v ∈ original.val ↔ v ∈ projected.val := by
  have he := post_of_ok (reachable_spec edges start roomE) runE
  have hp := post_of_ok (reachable_spec summary start roomS) runS
  rw [he v, hp v, single]
  exact live_reachability hs hu hv

end o5s_kernel.Projection
