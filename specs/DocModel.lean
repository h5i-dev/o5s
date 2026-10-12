import Mathlib.Data.List.Basic
import Mathlib.Logic.Relation
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Card
/-!
# Graph documents: the model (M0, statements to prove)

A pure model of one graph document, over replicated identities: no dense
indices, no arrival order. `applySpec` mirrors `model::DocumentGraph::apply`
(the adapter's identity and dependency checks, then `document::validate`
and `document::commit`), operation by operation and error by error.
`materializeSpec` gives the live text and conflicts as functions of the
*set* of admitted patches.

The main results to prove:
- `materializeSpec_perm`: the view depends only on the set of patches.
- `apply_comm`, `admissible_any_order`: independent admissions commute.
- `view_converges`: any two complete topological deliveries of the same
  patches (identities injective) give the same view.
- `clean_iff`, `bytes_spec`: bytes exist exactly when the live atoms are
  totally ordered and no delete/insert conflict is unacknowledged.

Assumption, explicit and unprovable here: patch identities are injective
(`IdsInjective`), which SHA-256 collision resistance provides in o5s.
-/
namespace o5s_kernel.DocModel

abbrev PId := Nat

structure OpId where
  patch : PId
  ordinal : Nat
deriving DecidableEq, Repr

structure NodeId where
  op : OpId
  ordinal : Nat
deriving DecidableEq, Repr

inductive Anchor
  | start
  | stop
  | node (n : NodeId)
deriving DecidableEq, Repr

inductive Op
  | insert (l r : Anchor) (atoms : List (List UInt8))
  | delete (targets : List NodeId)
  | restore (d : OpId)
  | order (before after : NodeId)
  | keep (d i : OpId)
  /-- Withdraw an order operation's edge. -/
  | retract (o : OpId)
deriving DecidableEq, Repr

structure Patch where
  id : PId
  deps : List PId
  ops : List Op
deriving DecidableEq, Repr

/-- The admitted patches. Every definition below is invariant under
`List.Perm` (`materializeSpec_perm`). -/
abbrev Hist := List Patch

/-- Errors of `model::Error`, in the same order of precedence. -/
inductive Error
  | identityCollision
  | invalidDependencies
  | missingDependencies (ids : List PId)
  | emptyPatch
  | invalidInsertion
  | unknownAnchor (a : Anchor)
  | unobservedPatch (p : PId)
  | unorderedAnchors
  | emptyDeletion
  | duplicateTarget (n : NodeId)
  | unknownDeletion (d : OpId)
  | unknownInsertion (i : OpId)
  | invalidAcknowledgement
  | cycle
  | unknownOrder (o : OpId)
deriving DecidableEq, Repr

inductive Applied | inserted | alreadyPresent
deriving DecidableEq, Repr

/-! ## What a history contains -/

def ids (H : Hist) : List PId := H.map (·.id)

/-- Operations with their identities. -/
def opsOf (P : Patch) : List (OpId × Op) :=
  P.ops.zipIdx.map fun (o, k) => (⟨P.id, k⟩, o)

def ops (H : Hist) : List (OpId × Op) := H.flatMap opsOf

/-- Atoms and their bytes. -/
def atomsOf : OpId × Op → List (NodeId × List UInt8)
  | (o, .insert _ _ xs) => xs.zipIdx.map fun (b, k) => (⟨o, k⟩, b)
  | _ => []

def atoms (H : Hist) : List (NodeId × List UInt8) := (ops H).flatMap atomsOf

def atom (H : Hist) (n : NodeId) : Option (List UInt8) := (atoms H).lookup n

def IsAtom (H : Hist) (n : NodeId) : Prop := atom H n ≠ none

/-- The order edges an operation adds: an insertion's chain, or an order. -/
def edgesOf : OpId × Op → List (Anchor × Anchor)
  | (o, .insert l r xs) =>
    let nodes := (List.range xs.length).map fun k => Anchor.node ⟨o, k⟩
    (l :: nodes).zip (nodes ++ [r])
  | (_, .order b a) => [(.node b, .node a)]
  | _ => []

/-- Edge facts with the operation that added them. -/
def facts (H : Hist) : List (Anchor × Anchor × OpId) :=
  (ops H).flatMap fun oo => (edgesOf oo).map fun e => (e.1, e.2, oo.1)

def IsOrder (H : Hist) (o : OpId) : Prop := ∃ b a, (o, Op.order b a) ∈ ops H

/-- Some patch with `ok` retracted order operation `o`. -/
def RetractedBy (H : Hist) (ok : PId → Prop) (o : OpId) : Prop :=
  ∃ r, (r, Op.retract o) ∈ ops H ∧ ok r.patch

open Classical in
/-- All edges no patch retracted, including the sentinel edge. -/
noncomputable def E (H : Hist) : List (Anchor × Anchor) :=
  (.start, .stop) :: ((facts H).filter fun f => ¬ RetractedBy H (fun _ => True) f.2.2).map
    fun f => (f.1, f.2.1)

def insertionOf (H : Hist) (i : OpId) : Option (Anchor × Anchor) :=
  (ops H).findSome? fun | (o, .insert l r _) => if o = i then some (l, r) else none | _ => none

/-- A deletion's targets (its tag). -/
def tag (H : Hist) (d : OpId) : Option (List NodeId) :=
  (ops H).findSome? fun | (o, .delete ts) => if o = d then some ts else none | _ => none

def Restored (H : Hist) (d : OpId) : Prop := ∃ o, (o, Op.restore d) ∈ ops H

def Acked (H : Hist) (d i : OpId) : Prop := ∃ o, (o, Op.keep d i) ∈ ops H

/-! ## Causality -/

/-- `a` is a strict causal ancestor of `b`. -/
def Anc (H : Hist) (a b : PId) : Prop :=
  Relation.TransGen (fun x y => ∃ P ∈ H, P.id = y ∧ x ∈ P.deps) a b

def Concurrent (H : Hist) (a b : PId) : Prop := a ≠ b ∧ ¬ Anc H a b ∧ ¬ Anc H b a

/-- Observed by a patch with dependencies `deps`: a dependency or one of
their ancestors. -/
def Observed (H : Hist) (deps : List PId) (q : PId) : Prop :=
  q ∈ deps ∨ ∃ d ∈ deps, Anc H q d

/-- What a patch's earlier operations added: edges, and retracted orders. -/
structure Own where
  edges : List (Anchor × Anchor) := []
  retracted : List OpId := []

/-- The edges a patch with `deps` may rely on: the sentinel edge, edges of
observed patches that neither an observed patch nor its own earlier
operations retracted, and its own edges. -/
def CausalEdge (H : Hist) (deps : List PId) (own : Own) (u v : Anchor) : Prop :=
  (u, v) = (.start, .stop) ∨
  (∃ o, (u, v, o) ∈ facts H ∧ Observed H deps o.patch ∧
    ¬ RetractedBy H (Observed H deps) o ∧ o ∉ own.retracted) ∨
  (u, v) ∈ own.edges

/-- Reflexive-transitive reachability, as `document::reaches` decides it. -/
def Reaches (H : Hist) (deps : List PId) (own : Own) (u v : Anchor) : Prop :=
  Relation.ReflTransGen (CausalEdge H deps own) u v

/-! ## Validation and application -/

open Classical in
/-- An anchor reference: observed patch, then an existing atom. -/
noncomputable def checkAnchor (H : Hist) (deps : List PId) : Anchor → Option Error
  | .node n => if ¬ Observed H deps n.op.patch then some (.unobservedPatch n.op.patch)
      else if ¬ IsAtom H n then some (.unknownAnchor (.node n)) else none
  | _ => none

open Classical in
/-- One operation of patch `self`, given the edges `own` its earlier
operations add. Returns the new `own`. Mirrors `document::check_op`. -/
noncomputable def validOp (H : Hist) (self : PId) (deps : List PId)
    (own : Own) (k : Nat) : Op → Except Error Own
  | .insert l r xs =>
    match checkAnchor H deps l, checkAnchor H deps r with
    | some e, _ => .error e
    | none, some e => .error e
    | none, none =>
      if xs = [] ∨ (∃ x ∈ xs, x = []) ∨ l = .stop ∨ r = .start ∨ l = r then .error .invalidInsertion
      else if ¬ Reaches H deps own l r then .error .unorderedAnchors
      else .ok { own with edges := own.edges ++ edgesOf (⟨self, k⟩, .insert l r xs) }
  | .delete ts =>
    if ts = [] then .error .emptyDeletion
    else
      -- The first target that is unobserved, unknown or repeated.
      match (ts.zipIdx.findSome? fun (t, j) =>
          match checkAnchor H deps (.node t) with
          | some e => some e
          | none => if t ∈ ts.take j then some (.duplicateTarget t) else none) with
      | some e => .error e
      | none => .ok own
  | .restore d =>
    if ¬ Observed H deps d.patch then .error (.unobservedPatch d.patch)
    else if tag H d = none then .error (.unknownDeletion d) else .ok own
  | .order b a =>
    match checkAnchor H deps (.node b), checkAnchor H deps (.node a) with
    | some e, _ => .error e
    | none, some e => .error e
    | none, none =>
      if Reaches H deps own (.node a) (.node b) then .error .cycle
      else .ok { own with edges := own.edges ++ [(.node b, .node a)] }
  | .keep d i =>
    if ¬ Observed H deps d.patch then .error (.unobservedPatch d.patch)
    else if ¬ Observed H deps i.patch then .error (.unobservedPatch i.patch)
    else match tag H d, insertionOf H i with
      | none, _ => .error (.unknownDeletion d)
      | some _, none => .error (.unknownInsertion i)
      | some ts, some (l, r) =>
        if (∃ n ∈ ts, l = .node n ∨ r = .node n) then .ok own else .error .invalidAcknowledgement
  | .retract o =>
    if ¬ Observed H deps o.patch then .error (.unobservedPatch o.patch)
    else if ¬ IsOrder H o then .error (.unknownOrder o)
    else .ok { own with retracted := own.retracted ++ [o] }

/-- All operations, threading the patch's own edges; the first error wins. -/
noncomputable def validate (H : Hist) (P : Patch) : Except Error Unit :=
  (P.ops.zipIdx.foldlM (fun own (o, k) => validOp H P.id P.deps own k o) {}).map fun _ => ()

open Classical in
noncomputable def applySpec (H : Hist) (P : Patch) : Except Error (Hist × Applied) :=
  match H.find? (·.id = P.id) with
  | some Q => if Q = P then .ok (H, .alreadyPresent) else .error .identityCollision
  | none =>
    if ¬ P.deps.Nodup ∨ P.id ∈ P.deps then .error .invalidDependencies
    else
      let missing := P.deps.filter (· ∉ ids H)
      if missing ≠ [] then .error (.missingDependencies missing)
      else if P.ops = [] then .error .emptyPatch
      else (validate H P).map fun _ => (H ++ [P], .inserted)

/-- Deliver patches in order, stopping at the first error. -/
noncomputable def replay : Hist → List Patch → Except Error Hist
  | H, [] => .ok H
  | H, P :: ps => (applySpec H P) >>= fun (H', _) => replay H' ps

/-! ## Materialization -/

/-- A live atom: it exists and every deletion covering it was restored. -/
def Live (H : Hist) : Anchor → Prop
  | .node n => IsAtom H n ∧ ∀ d ts, tag H d = some ts → n ∈ ts → Restored H d
  | _ => False

/-- Live-to-live order through dead interiors: an edge path from `u` to `v`
whose inner vertices are all dead (`Projection.Segment`). -/
inductive Seg (H : Hist) : Anchor → Anchor → Prop
  | edge {u v} : (u, v) ∈ E H → Seg H u v
  | step {u w v} : (u, w) ∈ E H → ¬ Live H w → Seg H w v → Seg H u v

/-- The boundary relation `materialize` orders by. -/
def Sum (H : Hist) (u v : Anchor) : Prop := Live H u ∧ Live H v ∧ Seg H u v

def Prec (H : Hist) (u v : Anchor) : Prop := Relation.TransGen (Sum H) u v

/-- A live atom that some cycle of `Sum` reaches (or lies on). -/
def Blocked (H : Hist) (v : Anchor) : Prop := ∃ c, Prec H c c ∧ Relation.ReflTransGen (Sum H) c v

/-- The Kahn depth of an unblocked live atom: the longest `Sum` path ending
at it. Well defined because no cycle reaches it. -/
def Depth (H : Hist) (v : Anchor) (k : Nat) : Prop :=
  (∃ path : List Anchor, path.length = k + 1 ∧ path.getLast? = some v ∧
    path.IsChain (Sum H)) ∧
  ∀ path : List Anchor, path.getLast? = some v → path.IsChain (Sum H) → path.length ≤ k + 1

inductive Conflict
  /-- Live atoms downstream of a cycle (not a minimal witness). -/
  | cycle (blocked : Finset NodeId)
  /-- Two or more live atoms without an established order. -/
  | order (frontier : Finset NodeId)
  /-- A concurrent insertion anchored on atoms an active deletion removes. -/
  | deleteInsert (d i : OpId) (anchors : Finset NodeId)

/-- An unacknowledged delete/insert conflict: an unrestored deletion, a
concurrent insertion, and the insertion's anchors among its targets. -/
def DI (H : Hist) (d i : OpId) (A : Finset NodeId) : Prop :=
  ∃ ts l r, tag H d = some ts ∧ ¬ Restored H d ∧ insertionOf H i = some (l, r) ∧
    (∀ n, n ∈ A ↔ n ∈ ts ∧ (l = .node n ∨ r = .node n)) ∧ A.Nonempty ∧
    Concurrent H d.patch i.patch ∧ ¬ Acked H d i

structure View where
  layers : List (Finset NodeId)
  conflicts : Set Conflict
  bytes : Option (List UInt8)

/-- Layer `k`: the unblocked live atoms of depth `k`. -/
def layer (H : Hist) (k : Nat) : Set NodeId :=
  {n | Live H (.node n) ∧ ¬ Blocked H (.node n) ∧ Depth H (.node n) k}

/-- The specified view, as a set-level predicate on a candidate result. -/
structure IsView (H : Hist) (V : View) : Prop where
  layers : ∀ k (hk : k < V.layers.length), ↑(V.layers[k]) = layer H k
  layers_all : ∀ n, Live H (.node n) → ¬ Blocked H (.node n) → ∃ k < V.layers.length, n ∈ V.layers[k]!
  layers_nonempty : ∀ L ∈ V.layers, L.Nonempty
  cycle : ∀ B, Conflict.cycle B ∈ V.conflicts ↔
    (∃ c, Blocked H c) ∧ ∀ n, n ∈ B ↔ Live H (.node n) ∧ Blocked H (.node n)
  order : ∀ F, Conflict.order F ∈ V.conflicts ↔ F ∈ V.layers ∧ 2 ≤ F.card
  deleteInsert : ∀ d i A, Conflict.deleteInsert d i A ∈ V.conflicts ↔ DI H d i A
  bytes_none : V.bytes = none ↔ V.conflicts.Nonempty
  bytes_some : ∀ b, V.bytes = some b →
    b = (V.layers.flatMap fun L => L.toList).flatMap fun n => (atom H n).getD []

/-! ## Admission facts -/

theorem applySpec_ok {H H' : Hist} {R : Patch} (h : applySpec H R = .ok (H', .inserted)) :
    H' = H ++ [R] := by
  unfold applySpec at h
  split at h
  · split_ifs at h <;> simp at h
  · by_cases h1 : ¬ R.deps.Nodup ∨ R.id ∈ R.deps
    · rw [if_pos h1] at h; cases h
    · rw [if_neg h1] at h
      try dsimp only at h
      by_cases h2 : R.deps.filter (· ∉ ids H) ≠ []
      · rw [if_pos h2] at h; cases h
      · rw [if_neg h2] at h
        by_cases h3 : R.ops = []
        · rw [if_pos h3] at h; cases h
        · rw [if_neg h3] at h
          cases hv : validate H R <;> simp [hv, Except.map] at h
          exact h.symm

theorem ops_append (H : Hist) (R : Patch) : ops (H ++ [R]) = ops H ++ opsOf R := by
  simp [ops, List.flatMap_append]

theorem mem_opsOf {R : Patch} {op : Op} (h : op ∈ R.ops) : ∃ k, (⟨R.id, k⟩, op) ∈ opsOf R := by
  obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem h
  refine ⟨k, ?_⟩
  unfold opsOf
  rw [List.mem_map]
  refine ⟨(R.ops[k], k), ?_, rfl⟩
  have hk' : k < R.ops.zipIdx.length := by simpa using hk
  have := List.getElem_mem hk'
  simpa [List.getElem_zipIdx] using this

theorem opsOf_single {R : Patch} {op : Op} (hr : R.ops = [op]) :
    opsOf R = [(⟨R.id, 0⟩, op)] := by
  simp [opsOf, hr]

/-! ## Invariance under permutation

Every definition reads the history through membership, except the
first-match lookups `tag`, `insertionOf` and `atom`. Those agree across
permutations because patch identities are unique in any admitted history. -/

section Perm
variable {H₁ H₂ : Hist}

theorem mem_opsOf_iff {P : Patch} {o : OpId} {op : Op} :
    (o, op) ∈ opsOf P ↔ o.patch = P.id ∧ P.ops[o.ordinal]? = some op := by
  obtain ⟨p, k⟩ := o
  constructor
  · intro h
    unfold opsOf at h
    obtain ⟨⟨x, j⟩, hm, he⟩ := List.mem_map.mp h
    dsimp only at he
    simp only [Prod.mk.injEq, OpId.mk.injEq] at he
    obtain ⟨⟨rfl, rfl⟩, rfl⟩ := he
    exact ⟨rfl, List.mem_zipIdx_iff_getElem?.mp hm⟩
  · rintro ⟨h1, h2⟩
    unfold opsOf
    refine List.mem_map.mpr ⟨(op, k), List.mem_zipIdx_iff_getElem?.mpr h2, ?_⟩
    simp only at h1
    simp [h1]

theorem op_unique {H : Hist} (hu : (ids H).Nodup) {o : OpId} {op₁ op₂ : Op}
    (h₁ : (o, op₁) ∈ ops H) (h₂ : (o, op₂) ∈ ops H) : op₁ = op₂ := by
  unfold ops at h₁ h₂
  obtain ⟨P, hP, h₁⟩ := List.mem_flatMap.mp h₁
  obtain ⟨Q, hQ, h₂⟩ := List.mem_flatMap.mp h₂
  rw [mem_opsOf_iff] at h₁ h₂
  unfold ids at hu
  have hPQ : P = Q := List.inj_on_of_nodup_map hu hP hQ (h₁.1.symm.trans h₂.1)
  subst hPQ
  exact Option.some.inj (h₁.2.symm.trans h₂.2)

theorem findSome?_perm_unique {α β} {l₁ l₂ : List α} (p : l₁.Perm l₂) (f : α → Option β)
    (u : ∀ x ∈ l₁, ∀ y ∈ l₁, ∀ b c, f x = some b → f y = some c → b = c) :
    l₁.findSome? f = l₂.findSome? f := by
  rcases h1 : l₁.findSome? f with _ | b
  · rcases h2 : l₂.findSome? f with _ | c
    · rfl
    · obtain ⟨y, hy, hfy⟩ := List.exists_of_findSome?_eq_some h2
      rw [List.findSome?_eq_none_iff] at h1
      rw [h1 y (p.symm.subset hy)] at hfy
      cases hfy
  · obtain ⟨x, hx, hfx⟩ := List.exists_of_findSome?_eq_some h1
    rcases h2 : l₂.findSome? f with _ | c
    · rw [List.findSome?_eq_none_iff] at h2
      rw [h2 x (p.subset hx)] at hfx
      cases hfx
    · obtain ⟨y, hy, hfy⟩ := List.exists_of_findSome?_eq_some h2
      rw [u x hx y (p.symm.subset hy) b c hfx hfy]

theorem ids_nodup_perm (h : H₁.Perm H₂) (hu : (ids H₁).Nodup) : (ids H₂).Nodup :=
  (h.map _).nodup_iff.mp hu

theorem ops_perm (h : H₁.Perm H₂) : (ops H₁).Perm (ops H₂) := by
  unfold ops; exact List.Perm.flatMap_right _ h

theorem facts_perm (h : H₁.Perm H₂) : (facts H₁).Perm (facts H₂) := by
  unfold facts; exact List.Perm.flatMap_right _ (ops_perm h)

theorem atoms_perm (h : H₁.Perm H₂) : (atoms H₁).Perm (atoms H₂) := by
  unfold atoms; exact List.Perm.flatMap_right _ (ops_perm h)

theorem tag_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (d : OpId) : tag H₁ d = tag H₂ d := by
  unfold tag
  apply findSome?_perm_unique (ops_perm h)
  rintro ⟨o₁, op₁⟩ h₁ ⟨o₂, op₂⟩ h₂ b c hb hc
  cases op₁ <;> simp at hb
  cases op₂ <;> simp at hc
  obtain ⟨rfl, rfl⟩ := hb
  obtain ⟨rfl, rfl⟩ := hc
  have := op_unique hu h₁ h₂
  simpa using this

theorem insertionOf_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (i : OpId) :
    insertionOf H₁ i = insertionOf H₂ i := by
  unfold insertionOf
  apply findSome?_perm_unique (ops_perm h)
  rintro ⟨o₁, op₁⟩ h₁ ⟨o₂, op₂⟩ h₂ b c hb hc
  cases op₁ <;> simp at hb
  cases op₂ <;> simp at hc
  obtain ⟨rfl, rfl⟩ := hb
  obtain ⟨rfl, rfl⟩ := hc
  have := op_unique hu h₁ h₂
  simp only [Op.insert.injEq] at this
  obtain ⟨rfl, rfl, -⟩ := this
  rfl

theorem lookup_some_mem {l : List (NodeId × List UInt8)} {n : NodeId} {b : List UInt8}
    (h : l.lookup n = some b) : (n, b) ∈ l := by
  induction l with
  | nil => simp [List.lookup] at h
  | cons p l ih =>
    obtain ⟨m, c⟩ := p
    by_cases hm : n = m
    · subst hm; simp [List.lookup] at h; simp [h]
    · have : (n == m) = false := by simpa using hm
      simp only [List.lookup, this] at h
      exact List.mem_cons_of_mem _ (ih h)

theorem lookup_none {l : List (NodeId × List UInt8)} {n : NodeId} :
    l.lookup n = none ↔ ∀ b, (n, b) ∉ l := by
  induction l with
  | nil => simp [List.lookup]
  | cons p l ih =>
    obtain ⟨m, c⟩ := p
    by_cases hm : n = m
    · subst hm
      constructor
      · intro h; simp [List.lookup] at h
      · intro h; exact absurd (by simp) (h c)
    · have : (n == m) = false := by simpa using hm
      simp [List.lookup, this, ih, hm]

theorem mem_atomsOf {x : OpId × Op} {n : NodeId} {b : List UInt8} (h : (n, b) ∈ atomsOf x) :
    ∃ l r xs, x = (n.op, Op.insert l r xs) ∧ xs[n.ordinal]? = some b := by
  obtain ⟨o, op⟩ := x
  cases op with
  | insert l r xs =>
    unfold atomsOf at h
    obtain ⟨⟨b', k⟩, hm, he⟩ := List.mem_map.mp h
    dsimp only at he
    simp only [Prod.mk.injEq] at he
    obtain ⟨rfl, rfl⟩ := he
    exact ⟨l, r, xs, rfl, List.mem_zipIdx_iff_getElem?.mp hm⟩
  | _ => simp [atomsOf] at h

theorem atom_unique {H : Hist} (hu : (ids H).Nodup) {n : NodeId} {b₁ b₂ : List UInt8}
    (h₁ : (n, b₁) ∈ atoms H) (h₂ : (n, b₂) ∈ atoms H) : b₁ = b₂ := by
  unfold atoms at h₁ h₂
  obtain ⟨x, hx, hx1⟩ := List.mem_flatMap.mp h₁
  obtain ⟨y, hy, hy1⟩ := List.mem_flatMap.mp h₂
  obtain ⟨l, r, xs, rfl, e1⟩ := mem_atomsOf hx1
  obtain ⟨l', r', xs', rfl, e2⟩ := mem_atomsOf hy1
  have := op_unique hu hx hy
  simp only [Op.insert.injEq] at this
  obtain ⟨-, -, rfl⟩ := this
  rw [e1] at e2
  exact Option.some.inj e2

theorem atom_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (n : NodeId) :
    atom H₁ n = atom H₂ n := by
  unfold atom
  rcases h1 : (atoms H₁).lookup n with _ | b
  · symm
    rw [lookup_none] at h1 ⊢
    intro b hb
    exact h1 b ((atoms_perm h).symm.subset hb)
  · have m1 := lookup_some_mem h1
    rcases h2 : (atoms H₂).lookup n with _ | c
    · rw [lookup_none] at h2
      exact absurd ((atoms_perm h).subset m1) (h2 b)
    · have m2 := (atoms_perm h).symm.subset (lookup_some_mem h2)
      rw [atom_unique hu m1 m2]

theorem isAtom_perm (h : H₁.Perm H₂) (n : NodeId) : IsAtom H₁ n ↔ IsAtom H₂ n := by
  unfold IsAtom atom
  rw [Ne, Ne, lookup_none, lookup_none]
  simp only [(atoms_perm h).mem_iff]

theorem retracted_perm (h : H₁.Perm H₂) (o : OpId) :
    RetractedBy H₁ (fun _ => True) o ↔ RetractedBy H₂ (fun _ => True) o := by
  unfold RetractedBy; simp only [(ops_perm h).mem_iff]

theorem restored_perm (h : H₁.Perm H₂) (d : OpId) : Restored H₁ d ↔ Restored H₂ d := by
  unfold Restored; simp only [(ops_perm h).mem_iff]

theorem acked_perm (h : H₁.Perm H₂) (d i : OpId) : Acked H₁ d i ↔ Acked H₂ d i := by
  unfold Acked; simp only [(ops_perm h).mem_iff]

theorem anc_perm (h : H₁.Perm H₂) : Anc H₁ = Anc H₂ := by
  funext a b
  unfold Anc
  simp only [h.mem_iff]

theorem mem_E_perm (h : H₁.Perm H₂) (x : Anchor × Anchor) : x ∈ E H₁ ↔ x ∈ E H₂ := by
  unfold E
  simp only [List.mem_cons, List.mem_map, List.mem_filter, (facts_perm h).mem_iff,
    decide_eq_true_eq, retracted_perm h]

/-- T3.1: everything the view is made of depends on the patch set only. -/
theorem live_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (a : Anchor) :
    Live H₁ a ↔ Live H₂ a := by
  cases a with
  | node n =>
    unfold Live
    simp only [isAtom_perm h, tag_perm hu h, restored_perm h]
  | _ => simp [Live]

theorem seg_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) {u v : Anchor} (hs : Seg H₁ u v) :
    Seg H₂ u v := by
  induction hs with
  | edge he => exact .edge ((mem_E_perm h _).mp he)
  | step he hl _ ih =>
    exact .step ((mem_E_perm h _).mp he) (fun x => hl ((live_perm hu h _).mpr x)) ih

theorem sum_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (u v : Anchor) :
    Sum H₁ u v ↔ Sum H₂ u v := by
  have hu₂ := ids_nodup_perm h hu
  unfold Sum
  rw [live_perm hu h, live_perm hu h]
  exact and_congr_right fun _ => and_congr_right fun _ =>
    ⟨seg_perm hu h, seg_perm hu₂ h.symm⟩

theorem di_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (d i A) : DI H₁ d i A ↔ DI H₂ d i A := by
  unfold DI Concurrent
  simp only [tag_perm hu h, insertionOf_perm hu h, restored_perm h, acked_perm h, anc_perm h]

/-- `IsView` with the history's ingredients as parameters. -/
structure IsViewOf (lay : Nat → Set NodeId) (live blocked : Anchor → Prop)
    (di : OpId → OpId → Finset NodeId → Prop) (atm : NodeId → Option (List UInt8))
    (V : View) : Prop where
  layers : ∀ k (hk : k < V.layers.length), ↑(V.layers[k]) = lay k
  layers_all : ∀ n, live (.node n) → ¬ blocked (.node n) → ∃ k < V.layers.length, n ∈ V.layers[k]!
  layers_nonempty : ∀ L ∈ V.layers, L.Nonempty
  cycle : ∀ B, Conflict.cycle B ∈ V.conflicts ↔
    (∃ c, blocked c) ∧ ∀ n, n ∈ B ↔ live (.node n) ∧ blocked (.node n)
  order : ∀ F, Conflict.order F ∈ V.conflicts ↔ F ∈ V.layers ∧ 2 ≤ F.card
  deleteInsert : ∀ d i A, Conflict.deleteInsert d i A ∈ V.conflicts ↔ di d i A
  bytes_none : V.bytes = none ↔ V.conflicts.Nonempty
  bytes_some : ∀ b, V.bytes = some b →
    b = (V.layers.flatMap fun L => L.toList).flatMap fun n => (atm n).getD []

theorem isView_iff (H : Hist) (V : View) :
    IsView H V ↔ IsViewOf (layer H) (Live H) (Blocked H) (DI H) (atom H) V :=
  ⟨fun h => ⟨h.1, h.2, h.3, h.4, h.5, h.6, h.7, h.8⟩,
   fun h => ⟨h.1, h.2, h.3, h.4, h.5, h.6, h.7, h.8⟩⟩

theorem view_perm (hu : (ids H₁).Nodup) (h : H₁.Perm H₂) (V : View) :
    IsView H₁ V ↔ IsView H₂ V := by
  have hs : Sum H₁ = Sum H₂ := by funext u v; exact propext (sum_perm hu h u v)
  have hl : Live H₁ = Live H₂ := by funext a; exact propext (live_perm hu h a)
  have hd : DI H₁ = DI H₂ := by funext d i A; exact propext (di_perm hu h d i A)
  have ha : atom H₁ = atom H₂ := by funext n; exact atom_perm hu h n
  have hb : Blocked H₁ = Blocked H₂ := by
    funext v; unfold Blocked Prec; rw [hs]
  have hdp : Depth H₁ = Depth H₂ := by
    funext v k; unfold Depth; rw [hs]
  have hly : layer H₁ = layer H₂ := by
    funext k; unfold layer; rw [hl, hb, hdp]
  rw [isView_iff, isView_iff, hly, hl, hb, hd, ha]

end Perm

/-! ## Theorems to prove -/

def Topological (l : List Patch) : Prop :=
  ∀ i (h : i < l.length), ∀ d ∈ l[i].deps, ∃ j < i, ∃ hj : j < l.length, l[j].id = d

def IdsInjective (l : List Patch) : Prop := ∀ P ∈ l, ∀ Q ∈ l, P.id = Q.id → P = Q

/-- Retracting an order on a cycle removes that cycle's edge: if every other
edge of the cycle stays, the atoms it blocked are no longer blocked by it. -/
theorem retract_breaks (H H' : Hist) (R : Patch) (o : OpId) (b a : NodeId)
    (h : applySpec H R = .ok (H', .inserted)) (hr : R.ops = [Op.retract o])
    (ho : (o, Op.order b a) ∈ ops H) :
    ((Anchor.node b, Anchor.node a) ∈ E H' ↔
      ∃ o', o' ≠ o ∧ (Anchor.node b, Anchor.node a, o') ∈ facts H ∧
        ¬ RetractedBy H (fun _ => True) o') := by sorry

/-- The view exists and is unique (layers are a function of `H`). -/
theorem view_exists (H : Hist) : ∃ V, IsView H V := by sorry
theorem depth_unique {H : Hist} {v : Anchor} {k m : Nat} (hk : Depth H v k) (hm : Depth H v m) :
    k = m := by
  obtain ⟨⟨p, hp, hl, hc⟩, hall⟩ := hk
  obtain ⟨⟨q, hq, hl', hc'⟩, hall'⟩ := hm
  have h1 := hall q hl' hc'
  have h2 := hall' p hl hc
  omega

/-- Every view has a layer at each depth of an unblocked live atom. -/
theorem layers_cover {H : Hist} {V W : View} (hv : IsView H V) (hw : IsView H W)
    (hlt : V.layers.length < W.layers.length) : False := by
  obtain ⟨n, hn⟩ := hw.layers_nonempty _ (List.getElem_mem hlt)
  have hn' : n ∈ (↑(W.layers[V.layers.length]) : Set NodeId) := hn
  rw [hw.layers _ hlt] at hn'
  obtain ⟨hl, hb, hd⟩ := hn'
  obtain ⟨k, hk, hnk⟩ := hv.layers_all n hl hb
  rw [getElem!_pos V.layers k hk] at hnk
  have hnk' : n ∈ (↑(V.layers[k]) : Set NodeId) := hnk
  rw [hv.layers k hk] at hnk'
  have := depth_unique hnk'.2.2 hd
  omega

theorem view_unique (H : Hist) (V W : View) (hv : IsView H V) (hw : IsView H W) : V = W := by
  have hlen : V.layers.length = W.layers.length := by
    rcases Nat.lt_trichotomy V.layers.length W.layers.length with h | h | h
    · exact (layers_cover hv hw h).elim
    · exact h
    · exact (layers_cover hw hv h).elim
  have hlay : V.layers = W.layers := List.ext_getElem hlen fun k h1 h2 => by
    apply Finset.coe_injective
    rw [hv.layers k h1, hw.layers k h2]
  have hconf : V.conflicts = W.conflicts := by
    ext c
    cases c with
    | cycle B => rw [hv.cycle, hw.cycle]
    | order F => rw [hv.order, hw.order, hlay]
    | deleteInsert d i A => rw [hv.deleteInsert, hw.deleteInsert]
  have hbytes : V.bytes = W.bytes := by
    rcases h1 : V.bytes with _ | b
    · symm
      rw [hw.bytes_none, ← hconf, ← hv.bytes_none]
      exact h1
    · rcases h2 : W.bytes with _ | c
      · rw [hw.bytes_none, ← hconf, ← hv.bytes_none, h1] at h2
        cases h2
      · rw [hv.bytes_some b h1, hw.bytes_some c h2, hlay]
  cases V; cases W
  simp only at hlay hconf hbytes
  subst hlay hconf hbytes
  rfl

/-- T1.4: validation reads only the patch's causal past. -/
theorem validate_local (H H' : Hist) (P : Patch) (sub : ∀ Q ∈ H, Q ∈ H')
    (past : ∀ Q ∈ H', Observed H' P.deps Q.id → Q ∈ H)
    (closed : ∀ Q ∈ H, ∀ d ∈ Q.deps, d ∈ ids H) :
    validate H P = validate H' P := by sorry

/-- T3.2: two independent admissions commute. -/
theorem apply_comm (H H₁ H₂ : Hist) (P Q : Patch)
    (hp : applySpec H P = .ok (H₁, .inserted)) (hq : applySpec H₁ Q = .ok (H₂, .inserted))
    (indep : P.id ∉ Q.deps) :
    ∃ H₁' H₂', applySpec H Q = .ok (H₁', .inserted) ∧ applySpec H₁' P = .ok (H₂', .inserted) ∧
      H₂.Perm H₂' := by sorry

/-- T3.3: a patch refused for anything but missing dependencies is refused
in every history it could be delivered to. -/
theorem rejection_stable (H H' : Hist) (P : Patch) (e : Error)
    (h : applySpec H P = .error e) (notMissing : ∀ ms, e ≠ .missingDependencies ms)
    (notCollision : e ≠ .identityCollision)
    (sub : ∀ Q ∈ H, Q ∈ H') (past : ∀ Q ∈ H', Observed H' P.deps Q.id → Q ∈ H)
    (closed : ∀ Q ∈ H, ∀ d ∈ Q.deps, d ∈ ids H) (fresh : P.id ∉ ids H') :
    applySpec H' P = .error e := by sorry

/-- T3.3: admissibility does not depend on the delivery order. -/
theorem admissible_any_order (l₁ l₂ : List Patch) (same : ∀ P, P ∈ l₁ ↔ P ∈ l₂)
    (t₂ : Topological l₂) (u : IdsInjective l₁) (H₁ : Hist) (h : replay [] l₁ = .ok H₁) :
    ∃ H₂, replay [] l₂ = .ok H₂ ∧ ∀ P, P ∈ H₁ ↔ P ∈ H₂ := by sorry

/-- T3.4 (main): any two complete deliveries of the same patches have the
same view. -/
theorem view_converges (l₁ l₂ : List Patch) (same : ∀ P, P ∈ l₁ ↔ P ∈ l₂)
    (t₂ : Topological l₂) (u : IdsInjective l₁) (H₁ : Hist) (h₁ : replay [] l₁ = .ok H₁) :
    ∃ H₂, replay [] l₂ = .ok H₂ ∧ ∀ V, IsView H₁ V ↔ IsView H₂ V := by sorry

/-- T2.7: bytes exist exactly when the live atoms are totally ordered and no
delete/insert conflict is open. -/
theorem clean_iff (H : Hist) (V : View) (hv : IsView H V) :
    V.bytes.isSome ↔ (∀ d i A, ¬ DI H d i A) ∧ (∀ a, ¬ Blocked H a) ∧
      ∀ u v, Live H u → Live H v → u ≠ v → Prec H u v ∨ Prec H v u := by sorry

/-- T2.7: clean bytes list every live atom once, in `Prec` order. -/
theorem bytes_order (H : Hist) (V : View) (hv : IsView H V) (b : List UInt8)
    (hb : V.bytes = some b) :
    ∀ (k₁ k₂ : Nat) (n₁ n₂ : NodeId), n₁ ∈ V.layers[k₁]! → n₂ ∈ V.layers[k₂]! →
      Prec H (.node n₁) (.node n₂) → k₁ < k₂ := by sorry

/-- R2.3: a cycle made only of deleted atoms blocks nothing: whatever is
blocked is downstream of a cycle through a live atom. -/
theorem dead_cycle_harmless (H : Hist) (v : Anchor) (hv : Blocked H v) :
    ∃ c, Live H c ∧ Prec H c c := by
  obtain ⟨c, hc, -⟩ := hv
  obtain ⟨b, hb, -⟩ := Relation.TransGen.head'_iff.mp hc
  exact ⟨c, hb.1, hc⟩

/-- T2.6: restoring a deletion removes exactly its conflicts. -/
theorem restore_clears (H H' : Hist) (R : Patch) (d : OpId)
    (h : applySpec H R = .ok (H', .inserted)) (hr : Op.restore d ∈ R.ops) :
    ∀ i A, ¬ DI H' d i A := by
  rintro i A ⟨ts, l, r, -, nr, -⟩
  apply nr
  obtain ⟨k, hk⟩ := mem_opsOf hr
  refine ⟨⟨R.id, k⟩, ?_⟩
  rw [applySpec_ok h, ops_append]
  exact List.mem_append_right _ hk

/-- T2.6: an acknowledgement is specific to its pair. -/
theorem ack_pair_specific (H H' : Hist) (R : Patch) (d i : OpId)
    (h : applySpec H R = .ok (H', .inserted)) (only : R.ops = [Op.keep d i]) :
    ∀ d' i' A, (d', i') ≠ (d, i) → (DI H' d' i' A ↔ DI H d' i' A) := by sorry

/-- `Tags.restore_other_cannot_revive`, lifted to the view: restoring one
deletion never makes an atom another active deletion covers live. -/
theorem restore_other_cannot_revive (H H' : Hist) (R : Patch) (d d' : OpId) (n : NodeId)
    (ts : List NodeId) (h : applySpec H R = .ok (H', .inserted)) (hr : R.ops = [Op.restore d])
    (ht : tag H d' = some ts) (hn : n ∈ ts) (active : ¬ Restored H d') (ne : d ≠ d') :
    ¬ Live H' (.node n) := by
  rintro ⟨-, hl⟩
  have ht' : tag H' d' = some ts := by
    rw [applySpec_ok h]
    unfold tag at ht ⊢
    rw [ops_append, List.findSome?_append, ht]
    rfl
  obtain ⟨o, ho⟩ := hl d' ts ht' hn
  rw [applySpec_ok h, ops_append, opsOf_single hr] at ho
  rcases List.mem_append.mp ho with ho | ho
  · exact active ⟨o, ho⟩
  · simp at ho
    exact ne ho.2.symm

end o5s_kernel.DocModel
