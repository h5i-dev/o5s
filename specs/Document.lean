import O5sKernel
import DocModel
open Aeneas Aeneas.Std Result o5s_kernel
/-!
# The extracted document kernel refines the model (M2–M4, statements to prove)

`crates/kernel/src/document.rs` works on dense indices assigned in arrival
order; `model.rs` interns identities and translates references. Here a
history `H : DocModel.Hist` (the admitted patches in arrival order) fixes
that interning, `Tr` relates a model patch to its dense translation, and
`Replayed d H` says `d` is what committing the translations of `H` in order
produced, which is how the adapter builds every document.

Main statements:
- `validate_refines`: the kernel accepts exactly what `DocModel.validate`
  accepts, and refuses with the same error (T1.1, T1.2).
- `commit_succeeds`: a validated patch always commits (no partial state).
- `materialize_refines`: the decoded view is the specified one (T2.0).
- `extracted_view_converges`: two replicas that admitted the same patches in
  different orders materialize the same view (T3.5); it follows from
  `materialize_refines` and the model's `view_perm` and `view_unique`.
-/
namespace o5s_kernel.Document
open DocModel

/-- The kernel's "none" index. -/
def NONE : Nat := Usize.max

/-- Dense index of `x` in `l`, or `NONE`. -/
def idx {α} [DecidableEq α] (l : List α) (x : α) : Nat :=
  if x ∈ l then l.idxOf x else NONE

/-- The interning a history fixes: patches, anchors, insertions, deletions,
each in arrival order. -/
def anchorList (H : Hist) : List Anchor := [.start, .stop] ++ (atoms H).map fun a => .node a.1
def insertionList (H : Hist) : List OpId :=
  (ops H).filterMap fun | (o, .insert ..) => some o | _ => none
def deletionList (H : Hist) : List OpId :=
  (ops H).filterMap fun | (o, .delete _) => some o | _ => none
def orderList (H : Hist) : List OpId :=
  (ops H).filterMap fun | (o, .order ..) => some o | _ => none

def TrRef (r : document.Ref) (patch index : Nat) : Prop :=
  r.patch.val = patch ∧ r.index.val = index

def TrNode (H : Hist) (n : NodeId) (r : document.Ref) : Prop :=
  TrRef r (idx (ids H) n.op.patch) (idx (anchorList H) (.node n))

def TrAnchor (H : Hist) : Anchor → document.Anchor → Prop
  | .start, .Start => True
  | .stop, .End => True
  | .node n, .Node r => TrNode H n r
  | _, _ => False

def TrOp (H : Hist) : Op → document.Op → Prop
  | .insert l r xs, .Insert l' r' xs' =>
    TrAnchor H l l' ∧ TrAnchor H r r' ∧
      xs'.val.map (fun x => x.val.map (·.val)) = xs.map (fun x => x.map (·.toNat))
  | .delete ts, .Delete rs => List.Forall₂ (TrNode H) ts rs.val
  | .restore d, .Restore r => TrRef r (idx (ids H) d.patch) (idx (deletionList H) d)
  | .order b a, .Order b' a' => TrNode H b b' ∧ TrNode H a a'
  | .keep d i, .Keep d' i' =>
    TrRef d' (idx (ids H) d.patch) (idx (deletionList H) d) ∧
    TrRef i' (idx (ids H) i.patch) (idx (insertionList H) i)
  | .retract o, .Retract r => TrRef r (idx (ids H) o.patch) (idx (orderList H) o)
  | _, _ => False

/-- `dp` is the adapter's translation of `P` against the history `H`. -/
def Tr (H : Hist) (P : Patch) (dp : document.Patch) : Prop :=
  dp.deps.val.map (·.val) = P.deps.map (idx (ids H)) ∧ List.Forall₂ (TrOp H) P.ops dp.ops.val

/-- `d` is the document the adapter holds after admitting `H` in order. -/
inductive Replayed : document.Doc → Hist → Prop
  | nil {d} : document.empty = ok d → Replayed d []
  | snoc {d d' H P dp} : Replayed d H → Tr H P dp → document.commit d dp = ok d' →
      Replayed d' (H ++ [P])

/-- The adapter's preconditions before it calls the kernel: a new identity,
distinct known dependencies, at least one operation. -/
def Admissible (H : Hist) (P : Patch) : Prop :=
  P.id ∉ ids H ∧ P.deps.Nodup ∧ P.id ∉ P.deps ∧ (∀ q ∈ P.deps, q ∈ ids H) ∧ P.ops ≠ []

/-- Sizes stay below the machine bound (indices, edge and atom counts). -/
def Room (d : document.Doc) (dp : document.Patch) : Prop :=
  d.atoms.val.length + (dp.ops.val.map fun
    | .Insert _ _ xs => xs.val.length | _ => 0).sum + 2 < Usize.max ∧
  d.edges.val.length + 2 * (dp.ops.val.map fun
    | .Insert _ _ xs => xs.val.length + 1 | _ => 1).sum < Usize.max ∧
  d.deps.val.length + 1 < Usize.max ∧ d.deletions.val.length + dp.ops.val.length < Usize.max ∧
  d.insertions.val.length + dp.ops.val.length < Usize.max

/-- The error a refusal stands for, as the adapter maps it (`model.rs`). -/
def errorOf (P : Patch) (r : document.Refusal) : Option DocModel.Error :=
  match P.ops[r.op.val]? with
  | none => none
  | some o =>
    let anchor : Option Anchor := match o, r.item.val with
      | .insert l _ _, 0 => some l
      | .insert _ r _, _ => some r
      | .delete ts, k => (ts[k]?).map .node
      | .order b _, 0 => some (.node b)
      | .order _ a, _ => some (.node a)
      | _, _ => none
    let op : Option OpId := match o, r.item.val with
      | .restore d, _ => some d
      | .keep d _, 0 => some d
      | .keep _ i, _ => some i
      | .retract o, _ => some o
      | _, _ => none
    match r.kind.val with
    | 1 => match op, anchor with
      | some o, _ => some (.unobservedPatch o.patch)
      | none, some (.node n) => some (.unobservedPatch n.op.patch)
      | _, _ => none
    | 2 => anchor.map .unknownAnchor
    | 3 => some .invalidInsertion
    | 4 => some .unorderedAnchors
    | 5 => some .emptyDeletion
    | 6 => match anchor with | some (.node n) => some (.duplicateTarget n) | _ => none
    | 7 => op.map .unknownDeletion
    | 8 => op.map .unknownInsertion
    | 9 => some .invalidAcknowledgement
    | 10 => some .cycle
    | 11 => op.map .unknownOrder
    | _ => none

/-- T1.1/T1.2: the kernel validates exactly as the model, error included. -/
theorem validate_refines (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp) :
    document.validate d dp ⦃ r =>
      (r.kind.val = 0 ↔ DocModel.validate H P = .ok ()) ∧
      ∀ e, DocModel.validate H P = .error e → errorOf P r = some e ⦄ := by sorry

/-- A validated patch commits. -/
theorem commit_succeeds (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp)
    (hv : DocModel.validate H P = .ok ()) :
    ∃ d', document.commit d dp = ok d' ∧ Replayed d' (H ++ [P]) := by sorry

/-- What the adapter returns, given the kernel's answer (T1.1 at the level
of `model::DocumentGraph::apply`). -/
theorem apply_refines (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Replayed d H) (ht : Tr H P dp) (adm : Admissible H P) (room : Room d dp) :
    document.validate d dp ⦃ r =>
      (r.kind.val = 0 → ∃ d', document.commit d dp = ok d' ∧ Replayed d' (H ++ [P]) ∧
          applySpec H P = .ok (H ++ [P], .inserted)) ∧
      (r.kind.val ≠ 0 → ∃ e, errorOf P r = some e ∧ applySpec H P = .error e) ⦄ := by sorry

/-! ## Materialization -/

def nodeAt (H : Hist) (a : Nat) : Option NodeId :=
  match (anchorList H)[a]? with | some (.node n) => some n | _ => none

def nodes (H : Hist) (xs : List Usize) : Finset NodeId :=
  (xs.filterMap fun a => nodeAt H a.val).toFinset

/-- The view a kernel result stands for, in identities. -/
noncomputable def decode (H : Hist) (v : document.View) : View where
  layers := v.layers.val.map fun l => nodes H l.val
  conflicts := {c |
    (∃ l ∈ v.layers.val, 2 ≤ l.val.length ∧ c = .order (nodes H l.val)) ∨
    (v.blocked.val ≠ [] ∧ c = .cycle (nodes H v.blocked.val)) ∨
    (∃ x ∈ v.delete_inserts.val, ∃ dd ii, (deletionList H)[x.deletion.val]? = some dd ∧
      (insertionList H)[x.insertion.val]? = some ii ∧ c = .deleteInsert dd ii (nodes H x.anchors.val))}
  bytes := if v.clean then some (v.bytes.val.map fun b => UInt8.ofNat b.val) else none

/-- The adapter's materialization bounds. -/
def RoomM (d : document.Doc) : Prop :=
  d.atoms.val.length < Usize.max ∧ d.edges.val.length < Usize.max ∧
  (d.atoms.val.map (·.val.length)).sum < Usize.max ∧ d.deps.val.length < Usize.max

/-- T2.0: the decoded kernel view is the specified view. -/
theorem materialize_refines (d : document.Doc) (H : Hist) (hr : Replayed d H) (room : RoomM d) :
    document.materialize d ⦃ v => IsView H (decode H v) ⦄ := by sorry

/-- T3.5 (main, on the extracted code): two replicas that admitted the same
patches, in any orders, materialize the same view. -/
theorem extracted_view_converges (d₁ d₂ : document.Doc) (H₁ H₂ : Hist)
    (r₁ : Replayed d₁ H₁) (r₂ : Replayed d₂ H₂) (same : ∀ P, P ∈ H₁ ↔ P ∈ H₂)
    (u₁ : (ids H₁).Nodup) (u₂ : (ids H₂).Nodup) (room₁ : RoomM d₁) (room₂ : RoomM d₂) :
    document.materialize d₁ ⦃ v₁ => document.materialize d₂ ⦃ v₂ =>
      decode H₁ v₁ = decode H₂ v₂ ⦄ ⦄ := by
  have n1 : H₁.Nodup := List.Nodup.of_map (·.id) (show (H₁.map (·.id)).Nodup from u₁)
  have n2 : H₂.Nodup := List.Nodup.of_map (·.id) (show (H₂.map (·.id)).Nodup from u₂)
  have hp : H₁.Perm H₂ := (List.perm_ext_iff_of_nodup n1 n2).mpr same
  apply WP.spec_mono (materialize_refines d₁ H₁ r₁ room₁)
  intro v₁ hv₁
  apply WP.spec_mono (materialize_refines d₂ H₂ r₂ room₂)
  intro v₂ hv₂
  exact view_unique H₁ _ _ hv₁ ((view_perm u₁ hp _).mpr hv₂)

end o5s_kernel.Document
