import Indexed
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Causal searches (M5, statements to prove)

`causality::frontier` gives the graph dependencies of a document edit and
the nearest file heads (`documents::nearest`); `causality::sole_head` is the
check `prepare_lines` makes before accepting line ranges against a base. Row
`v` lists the dependencies of patch `v`.
-/
namespace o5s_kernel.Causality

abbrev Rows := alloc.vec.Vec (alloc.vec.Vec Usize)

/-- `u` is a dependency of `v`. -/
def Dep (rows : Rows) (u v : Nat) : Prop :=
  ∃ h : v < rows.val.length, ∃ x ∈ (rows.val[v]).val, x.val = u

/-- `u` is in the causal past of `v` (or is `v`). -/
def Past (rows : Rows) (u v : Nat) : Prop := Relation.ReflTransGen (fun a b => Dep rows a b) u v

/-- The causal past of the roots. -/
def InPast (rows : Rows) (roots : List Usize) (u : Nat) : Prop := ∃ r ∈ roots, Past rows u r.val

/-- `u` is reachable from a root along dependencies whose earlier vertices
(the root included, `u` excluded) do not stop the search. -/
inductive Open (rows : Rows) (stop : List Bool) (roots : List Usize) : Nat → Prop
  | root {r} : r ∈ roots → Open rows stop roots r.val
  | step {u v} : Open rows stop roots v → stop[v]! = false → Dep rows u v → Open rows stop roots u

/-- Indices in range, marks clean: what the native adapter supplies. -/
def Wf (rows : Rows) (stop : List Bool) (roots : List Usize) (marks : alloc.vec.Vec Bool) : Prop :=
  stop.length = rows.val.length ∧ marks.val.length = rows.val.length ∧
  (∀ m ∈ marks.val, m = false) ∧ (∀ r ∈ roots, r.val < rows.val.length) ∧
  (∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length) ∧
  rows.val.length < Usize.max

/-- `frontier` returns exactly the stopping patches the search reaches, each
once, and clean marks. -/
@[step] theorem frontier_spec (rows : Rows) (stop : alloc.vec.Vec Bool)
    (roots : alloc.vec.Vec Usize) (marks : alloc.vec.Vec Bool)
    (wf : Wf rows stop.val roots.val marks) :
    causality.frontier rows stop roots marks ⦃ t =>
      (∀ v, (∃ x ∈ t.queue.val, x.val = v) ↔ Open rows stop.val roots.val v ∧ stop.val[v]! = true) ∧
      (t.queue.val.map (·.val)).Nodup ∧
      t.marks.val.length = rows.val.length ∧ ∀ m ∈ t.marks.val, m = false ⦄ := by sorry

/-- On an acyclic history (dependencies older than dependents), the nearest
stopping patches include every maximal one: a stopping patch in the past of
the roots lies in the past of some nearest one. -/
theorem frontier_covers (rows : Rows) (stop : List Bool) (roots : List Usize)
    (dag : ∀ u v, Dep rows u v → u < v) (u : Nat) (hu : InPast rows roots u)
    (hs : stop[u]! = true) :
    ∃ q, Open rows stop roots q ∧ stop[q]! = true ∧ Past rows u q := by sorry

/-- A causal head: an editing patch in the past of the roots that is not a
strict ancestor of another one. -/
def Head (rows : Rows) (edits : List Bool) (roots : List Usize) (h : Nat) : Prop :=
  InPast rows roots h ∧ edits[h]! = true ∧
  ∀ w, InPast rows roots w → edits[w]! = true → w ≠ h → ¬ Past rows h w

/-- `sole_head` decides that `base` is the only causal head of the file. -/
@[step] theorem sole_head_spec (rows : Rows) (edits : alloc.vec.Vec Bool)
    (roots : alloc.vec.Vec Usize) (base : Usize) (marks : alloc.vec.Vec Bool)
    (wf : Wf rows edits.val roots.val marks) (hb : base.val < rows.val.length)
    (dag : ∀ u v, Dep rows u v → u < v) :
    causality.sole_head rows edits roots base marks ⦃ r =>
      (r.1 = true ↔ ∀ h, Head rows edits.val roots.val h ↔ h = base.val) ∧
      r.2.val.length = rows.val.length ∧ ∀ m ∈ r.2.val, m = false ⦄ := by sorry

end o5s_kernel.Causality
