# Verification plan: document graph, materialization, convergence and line merging

Status: plan only. No Rust or Lean source has changed. Every file and function named below exists at commit `1475585`. The registry refinement work (`crates/registry/proofs/Model.lean`, `Refine.lean`, `Guarantees.lean` on the `registry-proofs` branch) is the template: a pure Lean `Model`, and a `*_refines` theorem saying that whenever the extracted function returns, its result is the model.

This plan covers four areas that are tested but have no proof:

1. The `DocumentGraph` adapter: validation, the mapping to dense indices, and the invariants after `apply`.
2. Materialization: the Kahn frontier loop and the `Order`, `Cycle` and `DeleteInsert` conflict judgments.
3. Convergence: the same admitted patch set gives the same view whatever the delivery order.
4. Line-edit merging (`LineVersion::merge_all`) and proposal preparation (`Repository::prepare`, `prepare_lines`, `validate_graph_edits`).

---

## 0. Constraints from the code that is already extracted

These are the conventions in `crates/kernel/src/{patch,graph,indexed,order,tags,interval,coordinates}.rs` and `crates/registry/src/lib.rs`, which `h5i-app.toml` and `crates/registry/h5i-app.toml` extract:

- `while i < v.len()` index loops only. No iterators, closures, `?`, `for`, `match` guards over borrowed maps, or recursion.
- Data is `Vec`, tuples, plain structs and enums. There is no `BTreeMap`, `BTreeSet` or `Arc`. Set and map operations come from `h5i_app_std::{set, map, bytes, graph}`: association lists with linear `contains`, `insert`, `position`, `get` and `bytes::eq`.
- Loop state is passed and returned by value (`fn enqueue(mut state: Traversal, ..) -> Traversal`). This gives Aeneas a clean functional loop and the proofs a `FoldInvStep`-style invariant (`proofs/Indexed.lean: fold_with_inv`). `&mut` exists (`registry::commit`, `admit_one`), but it is used only where the loop body is simple.
- Dense `usize` indices with an all-false reusable `marks: Vec<bool>` (see `indexed::reachable`, `reachable_spec`, `clear_clean_spec`). Proof preconditions are index bounds plus `len < Usize.max`.
- `u64`/`usize` arithmetic is overflow-checked in the extracted semantics. Every `+ 1` needs a bound, like `inc_val` in `Refine.lean`.
- `[u8; 32]` arrays would extract, but ordered comparison of them has no proved primitive. Strings (`String`, `&str`, `Arc<str>`) are not used in any extracted crate.

Consequences for every area:
- `model.rs` currently uses `BTreeMap<NodeId, ..>`, `BTreeSet<..>`, `Option::is_none_or`, closures in `reachable_in`, `iter().enumerate()` and `std::mem::take`. It needs a rewrite, not small edits.
- The rewrite must give up the canonical `BTreeMap` state. As a result, structural `==` on the state stops being order-independent, which affects area 3 (risk R3.2).

---

## 1. DocumentGraph adapter: validation, dense indices, invariants

### 1.1 What moves into an extractable crate

Create `crates/kernel/src/document.rs`, an extractable dense kernel. Keep `model.rs` as a thin native adapter with the same public API (`DocumentGraph::apply`, `materialize`, `atom`, `patch_count`) so that `crates/platform/src/documents.rs`, the CLI and the server do not change.

Functions that move out of `crates/kernel/src/model.rs` (with their current behaviour):

| Current (native) | New extractable function | Data-structure change |
|---|---|---|
| `DocumentGraph::apply` (dependency checks, op loop, journal) | `document::apply(doc: Doc, p: &DensePatch) -> Result<Doc, Error>`, written as **validate-then-commit** | The journal (`rollback_operation`, `rollback_edge`) is removed. Validation runs against `doc` plus a pending list of the patch's own edges, and only an all-valid patch is appended. Atomicity then holds by construction. |
| `dependency_closure` | `document::closure(dep_rows: &Vec<Vec<usize>>, roots: &Vec<usize>, marks: Vec<bool>) -> Traversal`, which calls `indexed::reachable` | `BTreeMap<PatchId, Patch>` becomes `dep_rows: Vec<Vec<usize>>`, where patch `i` lists its dependencies' dense indices, all `< i` |
| `observe`, `anchor` | `document::observed(closure_marks, patch)`, `document::anchor_index(doc, a: DenseAnchor) -> Option<usize>` | A `NodeId` is resolved through `op_first_atom: Vec<usize>` and `op_atom_count: Vec<u32>` per dense op index. This replaces the `atoms: BTreeMap<NodeId, Vec<u8>>` lookup. |
| `reachable_in(from, to, Some(causal))` | `document::causal_rows(doc, causal: &Vec<bool>, pending: &Vec<(usize,usize)>) -> Vec<Vec<usize>>` followed by `indexed::reachable` | `edges: BTreeMap<Anchor, BTreeSet<Anchor>>` and `edge_sources: BTreeMap<(Anchor,Anchor), BTreeSet<OperationId>>` become one append-only fact list `edge_facts: Vec<(usize, usize, usize)>` (from, to, creating op). Rows are derived per call. |
| `edge` | push onto `edge_facts` | – |
| `apply_operation` (5 arms) | `validate_insert`, `validate_delete`, `validate_restore`, `validate_order`, `validate_keep`, then `commit_op` | `insertions`, `insertions_at`, `deletions`, `restored`, `acknowledged` become `Vec<Insertion{op,left,right}>`, `Vec<Deletion{op, targets: Vec<usize>}>`, `restored: Vec<bool>` (indexed by deletion), and `acked: Vec<(usize,usize)>` (an `h5i_app_std::set`). `insertions_at` is derived in `materialize`. |
| Duplicate-target check (`BTreeSet unique`) | linear `set::contains` over the targets so far | quadratic per op, but ops are capped at 4096 (`validate_graph_edits`) |

Native code that stays in `model.rs`, as the adapter:
- **Interning.** `PatchId([u8;32]) -> usize` (dense, in admission order), `OperationId -> op index`, and `NodeId -> atom index`. This uses a native `BTreeMap<PatchId, usize>` and is the only map left.
- **Identity check.** `IdentityCollision` / `AlreadyPresent`: if the interner already has the id, compare the stored `Patch` structurally.
- **Translation.** `Operation` becomes `DenseOp`. Any unknown `PatchId` maps to `Error::UnobservedPatch` before the kernel is called.
- **Canonical sorting** of the kernel's output (layers, conflict lists) by external `NodeId`/`OperationId`, so that diagnostic order does not depend on admission order (see R3.2).

Optional: extract the interner too. `intern(keys: &Vec<[u8;32]>, key: &[u8;32]) -> Option<usize>` as a linear scan, or a binary search over a sorted `Vec<([u8;32], usize)>`, using a proved lexicographic `lt32` loop. I recommend proving an **equivariance** theorem instead (T1.6). It shows that any injective interning gives the same canonical output. That leaves injectivity of a `BTreeMap` as the only native obligation, and it is easy to test.

Platform pieces in `crates/platform/src/documents.rs`:
- `dependencies(repo, initial, path)`, which finds the nearest op-carrying graph patches on `path`, and `nearest` (file-state heads). Both are breadth searches over `repo.patches: BTreeMap<u64, Patch>`. To extract them, give the registry view dense `dep_rows` (handles are already dense and topological, since `document_at` checks `dep < id`) and a per-path `Vec<bool>` "edits path with ops" flag. Then add `documents::graph_frontier(dep_rows, has_ops, roots, marks)`, a search that does not expand marked vertices. It is a small variant of `indexed::reachable`, about 40 lines of Rust.
- `identity` (hex decoding) and `render_output`/`overlay_outputs` (string paths) stay native.

**Size estimate.** About 450–550 lines of subset Rust in `document.rs`, plus about 250 lines of native adapter replacing the current 620 non-test lines of `model.rs`. Lean: about 300 lines of pure model (`DocModel.lean`), and about 1,500–2,000 lines of refinement and invariant proofs (`DocApply.lean`, `DocInv.lean`). For calibration, the registry `Refine.lean` is 647 lines for seven commands with simpler loops.

### 1.2 Pure model (Lean, no Aeneas types)

```lean
namespace o5s_kernel.DocModel
abbrev PId := Nat                                   -- interned PatchId
structure OpId  where patch : PId; ordinal : Nat   deriving DecidableEq
structure NodeId where op : OpId; ordinal : Nat     deriving DecidableEq
inductive Anchor | start | node (n : NodeId) | stop deriving DecidableEq
inductive Op
  | insert (l r : Anchor) (atoms : List (List UInt8))
  | delete (targets : List NodeId)
  | restore (d : OpId)
  | order (before after : NodeId)
  | keep (d i : OpId)
structure Patch where id : PId; deps : List PId; ops : List Op

/-- A history is a list of admitted patches; every definition below is
    invariant under `List.Perm` (proved, T3.1). -/
abbrev Hist := List Patch

def ops (H : Hist) : List (OpId × Op)
def atom (H : Hist) (n : NodeId) : Option (List UInt8)
def facts (H : Hist) : List (Anchor × Anchor × OpId)       -- insert chains + order edges
def E (H : Hist) : List (Anchor × Anchor) := (.start, .stop) :: (facts H).map (fun f => (f.1, f.2.1))
def tag (H : Hist) (d : OpId) : Option (List NodeId)
def Restored (H : Hist) (d : OpId) : Prop := (d, .restore d) ∈ ... -- some restore op names d
def Acked (H : Hist) (d i : OpId) : Prop
def Live (H : Hist) : Anchor → Prop
  | .node n => atom H n ≠ none ∧ ∀ d T, tag H d = some T → n ∈ T → Restored H d
  | _       => False
/-- `a` is a strict causal ancestor of `b`. -/
def Anc (H : Hist) (a b : PId) : Prop :=
  Relation.TransGen (fun x y => ∃ P ∈ H, P.id = y ∧ x ∈ P.deps) a b
def Concurrent (H : Hist) (a b : PId) : Prop := a ≠ b ∧ ¬ Anc H a b ∧ ¬ Anc H b a
/-- The patch's causal context: its dependencies and their ancestors. -/
def past (H : Hist) (P : Patch) : Hist := H.filter (fun Q => Q.id ∈ P.deps ∨ ∃ d ∈ P.deps, Anc H Q.id d)

/-- Per-op validation, mirroring `apply_operation` exactly, including the error
    returned. `own` is the edges created by earlier ops of the same patch, which
    the current code admits through `causal_edges.insert(patch.id)`. -/
def validOp (C : Hist) (self : PId) (own : List (Anchor × Anchor)) (k : Nat) : Op → Except Error Unit
def validate (C : Hist) (P : Patch) : Except Error Unit      -- left fold over P.ops, threading `own`
def applySpec (H : Hist) (P : Patch) : Except Error (Hist × Applied) :=
  match H.find? (·.id = P.id) with
  | some Q => if Q = P then .ok (H, .alreadyPresent) else .error .identityCollision
  | none =>
    if ¬ P.deps.Nodup ∨ P.id ∈ P.deps then .error .invalidDependencies
    else if (missing H P) ≠ [] then .error (.missingDependencies (missing H P))
    else if P.ops = [] then .error .emptyPatch
    else (validate (past H P) P).map (fun _ => (H ++ [P], .inserted))
```

`validOp` for `insert` requires `Relation.TransGen` reachability from `l` to `r` in `E (past H P) ++ own`. For `order b a` it requires that `a` does **not** reach `b` there. This is the semantics of `reachable_in(.., Some(causal_edges))` once T1.3 is proved.

### 1.3 Theorems

```lean
-- T1.1 Refinement of the extracted apply (the analogue of `transition_refines`).
theorem apply_refines (doc : document.Doc) (H : Hist) (p : document.DensePatch)
    (abs : Abs doc H) (inv : Inv doc) (room : Room doc p) :
    document.apply doc p ⦃ r =>
      liftApply r = (applySpec H (model p)).map Prod.fst ∧
      (∀ doc', r = .Ok doc' → Abs doc' (H ++ [model p]) ∧ Inv doc') ⦄

-- T1.2 Atomicity. With validate-then-commit, `document::validate` is a separate
-- pure function and `commit` runs only after it accepts, so refusal is decided
-- before any state is built:
theorem apply_validates_first (doc p) :
    document.apply doc p = ok (.Err e) ↔ document.validate doc p = ok (.Err e)
theorem commit_total (h : document.validate doc p = ok (.Ok ())) (room : Room doc p) :
    ∃ doc', document.apply doc p = ok (.Ok doc')
-- Native obligation (tested today by `journal_rolls_back_every_mutation_kind_after_later_refusal`):
-- the adapter assigns `self` only on `Ok`.

-- T1.3 Causal-provenance reachability equals reachability in the causal past.
-- This is the key lemma behind locality. It needs the invariant that both
-- endpoints of an edge fact were created in the causal past of its creating op.
theorem causal_reach (inv : WellCaused H) (P : Patch) (a b : Anchor) :
    Relation.TransGen (edgesBy H (fun o => o.patch ∈ ids (past H P) ∨ o.patch = P.id) ++ own) a b ↔
    Relation.TransGen (E (past H P) ++ own) a b

-- T1.4 Validation is local: it reads only the patch's causal past.
theorem validate_local (H : Hist) (P : Patch) (closed : DepClosed H) :
    validate H P = validate (past H P) P

-- T1.5 Invariants preserved by every successful apply.
structure Inv (doc : document.Doc) : Prop where
  dep_topo     : ∀ i d, d ∈ doc.dep_rows[i] → d < i               -- dense ids are a topological order
  facts_bound  : ∀ (u,v,o) ∈ doc.edge_facts, u < anchors doc ∧ v < anchors doc ∧ o < ops doc
  targets_atom : ∀ del ∈ doc.deletions, ∀ t ∈ del.targets, isAtom doc t
  targets_nodup: ∀ del ∈ doc.deletions, del.targets.Nodup
  restored_len : doc.restored.length = doc.deletions.length
  well_caused  : ∀ (u,v,o) ∈ doc.edge_facts, createdIn u ⊆ pastOrSelf o ∧ createdIn v ⊆ pastOrSelf o
  insert_path  : ∀ ins ∈ doc.insertions, TransGen (E (pastOf ins.op)) ins.left ins.right
  atoms_nonempty : ∀ a, (atomBytes doc a).length > 0
  sentinels    : anchor 0 = start ∧ anchor 1 = stop ∧ (0,1) sentinel edge present
theorem apply_preserves_inv : Inv doc → document.apply doc p = ok (.Ok doc') → Inv doc'

-- T1.6 Interning is irrelevant: materialization commutes with injective renaming.
theorem materialize_equivariant (σ : Renaming) (inj : σ.Injective) :
    canon (materialize (rename σ doc)) = canon (materialize doc)

-- T1.7 (platform) Graph dependencies are causality restricted to op-carrying patches.
theorem graph_deps_spec (R : Registry) (roots : List Nat) (path) :
    ∀ q, q ∈ graphClosure R (dependencies R roots path) ↔
         HasOps R q path ∧ (q ∈ roots ∨ ∃ r ∈ roots, AncR R q r)
```

T1.7 is what makes `DocModel.Concurrent` (graph-level) agree with registry causality. Without it, `DeleteInsert` could report two patches as concurrent when the registry orders them through a patch that does not touch the path.

### 1.4 Existing proofs to reuse

- `Indexed.reachable_spec` and `clear_clean_spec`: the closure and causal-reachability searches are calls to `indexed::reachable`.
- `Patch.Closed` and `dependency_closed`: the dependency-closure notion behind `DepClosed H`.
- `Order.frontier_exact`'s `Ready` predicate, `Tags.restoredTag`, and the `h5i_app_std` set specs in `proofs/generated/StdSpecs.lean`.
- The registry proof helpers `push_ok_val` and the `fork_loop_ok`-style loop lemmas (`Refine.lean`), copied or moved into a shared helpers module.

### 1.5 Risks

- **R1.1 Own-patch edges.** `causal_edges.insert(patch.id)` means an `Order` op can enable a later `Insert` in the same patch, but anchors that name the patch's own nodes are rejected (`self_references_are_rejected_without_a_relative_wire_form`). The model must thread `own` edges and keep "own nodes unobservable". If `own` is forgotten, T1.1 is false.
- **R1.2 Error identity.** The current errors carry data: `MissingDependencies(Vec<PatchId>)` is sorted by `BTreeSet` order, and `UnknownAnchor(anchor)` and `DuplicateTarget(n)` name the first failing item. The dense kernel will find the *first* failing item in op order, which is the same. But `missing` must be reported sorted by external id, so the adapter sorts it. State T1.1 modulo `canonError`, or move the missing-dependency check into the adapter, since it involves non-interned ids anyway.
- **R1.3 Quadratic validation.** `causal_rows` rebuilds rows from all `edge_facts` per `Insert`/`Order` op: O(ops × E). Today's `reachable_in` is also O(E) per op. If this is too slow, keep a persistent row index and prove it equal to the derived rows. That proof is the same shape as `Components.extracted_batch_incremental`.
- **R1.4 The `PatchId` arrays.** If the interner is extracted, `[u8;32]` comparison needs a new proved loop. The equivariance route (T1.6) avoids that but adds a renaming theorem across the whole materializer. Both are moderate; equivariance is the more reusable of the two.
- **R1.5 Trust boundary in the platform.** Graph operations are validated only in `Repository::prepare → validate_graph_edits`. `Repository::project` and `body::decode` accept any committed body. If a body bypasses `prepare` (for example rows imported into PostgreSQL), `IncrementalView::integrate` returns `Err` for the whole view. The theorem can only say "every body that passed `prepare`". Write that precondition down (see T4.7).

---

## 2. Materialization and conflict semantics

### 2.1 What moves

All of `DocumentGraph::materialize` (`model.rs`, lines 474–622) moves to `document::materialize(doc: &Doc, marks: Vec<bool>) -> Dense`, built from these extractable pieces:

| Step in the current code | Extractable function | Change |
|---|---|---|
| `DeleteInsert` scan over `deletions × insertions_at × concurrent` | `document::delete_insert(doc, marks) -> (Vec<(usize,usize,Vec<usize>)>, Vec<bool>)` | `insertions_at` is derived by a scan over `doc.insertions`. `concurrent(a,b)` is two `indexed::reachable` queries over `dep_rows`, or one query because dense ids are topological (`a < b` ⇒ only "is `a` an ancestor of `b`" is possible). |
| `anchors`/`indices` maps | none: anchor index = dense atom index + 2 | removes the `BTreeMap<Anchor, usize>` |
| `rows` from `edges` | `document::rows(doc) -> Vec<Vec<usize>>` from `edge_facts`, deduplicated per row | `edges` stored a set, but the fact list can repeat (u,v) for different ops. `indexed::reachable` tolerates duplicate neighbours (marks), and indegree counting does not use rows directly, so deduplication is optional for correctness. |
| `covering`, `restored`, `live` | `document::liveness(doc) -> Vec<bool>`, which calls `tags::visible` per atom | – |
| `dead_rows` | `document::dead_rows(rows, live)` | – |
| boundary loop and `indegree` | `document::summaries(dead_rows, rows, live, marks) -> (Vec<Vec<usize>>, Vec<usize>, Vec<bool>)`, which calls `indexed::boundary` | – |
| Kahn loop (`BTreeSet ready/remaining`, `mem::take`) | `document::kahn(boundaries, indegree, live) -> Kahn{layers: Vec<Vec<usize>>, blocked: Vec<usize>}` | `remaining: BTreeSet` becomes `emitted: Vec<bool>` plus a count. `ready: BTreeSet` becomes a `Vec<usize>` per layer. The first layer comes from `order::frontier_counts` over the live indices. Later layers are collected while decrementing. **Layers are unsorted.** |
| byte output | `document::emit(doc, layers) -> Option<Vec<u8>>` using `interval::append_range` | – |

The native adapter then sorts each layer, `blocked` and the conflict list by external id. That reproduces today's "canonical ID order" on `Materialized`.

**Size estimate.** About 250–300 lines of subset Rust. Lean: 1,800–2,500 lines. The Kahn layering proof is the single largest item in this plan.

### 2.2 Specification

```lean
open o5s_kernel.Projection   -- Segment, Summary, live_reachability

/-- Live-boundary summary: the order relation the materializer actually uses. -/
def Sum (H : Hist) (u v : Anchor) : Prop :=
  Live H u ∧ Live H v ∧ Segment (E H) (fun x => ¬ Live H x) u v
def Prec (H : Hist) := Relation.TransGen (Sum H)
def OnCycle (H : Hist) (c : Anchor) : Prop := Prec H c c
def Blocked (H : Hist) (v : Anchor) : Prop := ∃ c, OnCycle H c ∧ Relation.ReflTransGen (Sum H) c v
/-- Longest Sum-path into v; well defined when ¬Blocked v. -/
def depth (H : Hist) (v : Anchor) (h : ¬ Blocked H v) : Nat

inductive Conflict
  | cycle (blocked : Finset NodeId)
  | order (frontier : Finset NodeId)
  | deleteInsert (d i : OpId) (anchors : Finset NodeId)

def DI (H : Hist) (d i : OpId) (A : Finset NodeId) : Prop :=
  ∃ T ins, tag H d = some T ∧ ¬ Restored H d ∧ insertionOf H i = some ins ∧
    A = ({ins.left, ins.right}.filterMap nodeOf) ∩ T.toFinset ∧ A.Nonempty ∧
    Concurrent H d.patch i.patch ∧ ¬ Acked H d i

structure View where
  layers    : List (Finset NodeId)
  conflicts : Finset Conflict
  bytes     : Option (List UInt8)
def materializeSpec (H : Hist) : View      -- defined from Sum/depth/DI; Perm-invariant
```

### 2.3 Theorems

```lean
-- T2.0 Refinement (after native canonical sorting).
theorem materialize_refines (abs : Abs doc H) (inv : Inv doc) (clean : Clean marks) (room : Room doc) :
    document.materialize doc marks ⦃ out => canon out = materializeSpec H ⦄

-- T2.1 Summary edges are exact (direct reuse of Boundary.boundary_segment
--      + Tags.visible_iff_no_active_tag for `live`).
theorem summaries_exact : ∀ u v, v ∈ boundaries[u] ↔ Sum H (anchorOf u) (anchorOf v)

-- T2.2 Live order through dead atoms = full-graph reachability between live atoms
--      (Projection.live_reachability, instantiated).
theorem prec_iff_reach (u v) : Prec H u v ↔ Live H u ∧ Live H v ∧ Relation.TransGen (E H) u v

-- T2.3 Kahn layering.
theorem layers_spec (k : Nat) (v : NodeId) :
    v ∈ (materializeSpec H).layers[k]! ↔ Live H (.node v) ∧ ∃ h : ¬ Blocked H (.node v), depth H _ h = k
theorem layers_partition : ∀ v, Live H (.node v) → ¬ Blocked H (.node v) → ∃! k, v ∈ layers[k]!

-- T2.4 Cycle judgment.
theorem cycle_iff : (∃ B, .cycle B ∈ conflicts) ↔ ∃ c, OnCycle H c
theorem cycle_blocked : .cycle B ∈ conflicts → ∀ v, v ∈ B ↔ Live H (.node v) ∧ Blocked H (.node v)
-- (B is "everything downstream of a cycle", not a minimal witness; docs/patch-model.md says so.)

-- T2.5 Order judgment.
theorem order_iff : .order F ∈ conflicts ↔ ∃ k, layers[k]! = F ∧ 2 ≤ F.card

-- T2.6 Delete/insert judgment.
theorem delete_insert_iff : .deleteInsert d i A ∈ conflicts ↔ DI H d i A
theorem restore_clears (h : applySpec H R = .ok (H', _)) (hr : R restores d) :
    ∀ i A, ¬ DI H' d i A                                   -- only tag d's conflicts disappear
theorem ack_pair_specific (h : R acks (d,i)) : ∀ d' i' A, (d',i') ≠ (d,i) → (DI H' d' i' A ↔ DI H d' i' A)

-- T2.7 Bytes. Output exists iff there are no conflicts, and is then the unique
--      linear extension of Prec.
theorem bytes_iff : (materializeSpec H).bytes.isSome ↔ (materializeSpec H).conflicts = ∅
theorem clean_iff_total : conflicts = ∅ ↔
    (∀ d i A, ¬ DI H d i A) ∧ IsStrictTotalOrder {a // Live H a} (Prec H)
theorem bytes_eq (h : bytes = some b) :
    b = (linearize H).flatMap (fun n => (atom H n).get!) ∧
    ∀ u v, Prec H u v → pos u (linearize H) < pos v (linearize H)

-- T2.8 Local-edit sanity (pins down "it does what an editor expects").
theorem insert_adjacent_splices (clean : (materializeSpec H).bytes = some b)
    (adj : Live H a ∧ Live H c ∧ Sum H a c ∧ ¬ ∃ x, Live H x ∧ Prec H a x ∧ Prec H x c)
    (h : applySpec H P = .ok (H', _)) (hP : P = ⟨_, heads H, [.insert a c xs]⟩) :
    (materializeSpec H').bytes = some (splice b (endOf a) xs)
theorem delete_hides (h : P deletes T, observed) : (materializeSpec H').bytes = b.removeAtoms T  -- when H was clean
theorem restore_other_cannot_revive_view  -- lifts Tags.restore_other_cannot_revive to the view
```

### 2.4 Existing proofs to reuse

| Lemma | Used for |
|---|---|
| `Boundary.boundary_spec`, `boundary_segment`, `keep_live_spec` | T2.1, directly; the materializer already calls `indexed::boundary` |
| `Indexed.reachable_spec`, `clear_clean_spec` | mark reuse across the summaries loop (`marks` threaded today) |
| `Projection.Segment`, `Summary`, `live_reachability` | T2.2; `Sum` is `Summary` with `live := Live H` |
| `Order.frontier_counts_exact` | the first Kahn layer; its `counts` hypothesis is exactly the indegree invariant |
| `Tags.visible_iff_no_active_tag`, `active_tag_hides`, `restore_other_cannot_revive` | `Live`, T2.6, T2.8 |
| `Interval.append_range_spec`, `Rendering.finish` | the byte-emission loop |
| `Components.*` | only if an incremental summary is added later (not needed for a fresh materialize) |

### 2.5 Risks

- **R2.1 Layer order is a set, not a list.** Today `layers` and `Conflict::Order.frontier` are sorted because `BTreeSet<usize>` follows `NodeId` order, and dense indices are assigned in `NodeId` order (`self.atoms.keys()`). With admission-ordered dense indices, the kernel's layer order depends on delivery. The spec must use `Finset` (as above), and the native canonical sort becomes a trust step, which is small and testable. Otherwise T3.x is false for the diagnostic fields.
- **R2.2 `Cycle` with `break`.** After the cycle is detected the loop stops, so `layers` contains only the non-blocked prefix. `layers_spec` as written is consistent with that. A spec that says "all live atoms appear in layers" would be false.
- **R2.3 Dead cycles are invisible.** A cycle made only of deleted atoms (which concurrent `Order` ops can create) gives no `Cycle` conflict: `Segment` passes through it. This is intended (the bytes are still well defined), but it should be an explicit theorem (`dead_cycle_harmless`) so that nobody "fixes" it.
- **R2.4 `insert_adjacent_splices` needs "adjacent".** Inserting between non-adjacent live atoms `a` and `c` makes the new atom concurrent with the live atoms between them, which gives an `Order` conflict. That is today's semantics (`docs/patch-model.md`), not a bug. Expect the first attempt to state T2.8 to be false without the adjacency hypothesis.
- **R2.5 `DeleteInsert` only looks at direct anchors.** A concurrent insertion whose anchors are live, but which sits next to a deleted region, gives no conflict. The theorem should match the code (`anchors ∩ targets`), and the docs should say this is a design choice.
- **R2.6 Duplicate summary edges.** Indegree counting relies on `boundary` queues having no duplicates. `Indexed.reachable_spec` gives `Nodup`, and `keep_live` preserves it. If rows are built from `edge_facts` without deduplication, the marks still prevent duplicate queue entries, so this is fine, but it must be carried through the proof.
- **R2.7 Indegree overflow.** `indegree[t] += 1` is bounded by the number of live sources, which is `< Usize.max` under the usual `room` hypothesis. It needs an explicit bound lemma.

---

## 3. Order independence and convergence

### 3.1 What moves

No new Rust beyond areas 1 and 2. The theorems are about `document::apply` and `document::materialize` composed with the native canonicalisation. Change one **test**: `all_120_deliveries_of_independent_changes_have_equal_state_and_view` (`model.rs`) asserts `state == state`. That holds only because the current state is made of `BTreeMap`s. After the rewrite, compare `materialize()` and a canonical export (sorted fact sets) instead (R3.2).

The platform also needs a separate but related statement. `IncrementalView::integrate` (`crates/platform/src/lib.rs`) applies graph patches in registry-handle order, which is a topological order because `document_at` and `integrate` reject `dep >= id`. Across replicas, handles differ but `PatchId`s (SHA-256 of the body) agree. So the cross-replica statement belongs at the `DocumentGraph` level (here). The platform statement is "incremental sync equals fresh replay" (T3.6).

### 3.2 Theorems

```lean
def Topological (l : List Patch) : Prop :=
  ∀ i P, l[i]? = some P → ∀ d ∈ P.deps, ∃ j < i, (l[j]?.map (·.id)) = some d
def IdsInjective (l : List Patch) : Prop := ∀ P ∈ l, ∀ Q ∈ l, P.id = Q.id → P = Q
def replaySpec : Hist → List Patch → Except Error Hist
  | H, [] => .ok H
  | H, P :: ps => (applySpec H P) >>= fun (H', _) => replaySpec H' ps

-- T3.1 The view is a function of the set (the definitional heart).
theorem materializeSpec_perm (h : H₁.Perm H₂) : materializeSpec H₁ = materializeSpec H₂

-- T3.2 Locality ⇒ two independent admissions commute (cf. Patch.successful_orders_converge).
theorem apply_comm (hp : applySpec H P = .ok (H₁, .inserted)) (hq : applySpec H₁ Q = .ok (H₂, .inserted))
    (indep : P.id ∉ Q.deps) :
    ∃ H₁', applySpec H Q = .ok (H₁', .inserted) ∧ applySpec H₁' P = .ok (H₂', .inserted) ∧ H₂.Perm H₂'

-- T3.3 Admissibility is order independent; so is the rejection of any individual patch.
theorem admissible_any_order (l₁ l₂ : List Patch) (same : ∀ P, P ∈ l₁ ↔ P ∈ l₂)
    (t₂ : Topological l₂) (u : IdsInjective l₁)
    (h : replaySpec [] l₁ = .ok H₁) : ∃ H₂, replaySpec [] l₂ = .ok H₂ ∧ (∀ P, P ∈ H₁ ↔ P ∈ H₂)
theorem rejection_local (h : applySpec H P = .error e) (closed : DepClosed H) (hP : P.deps ⊆ ids H)
    (H' ⊇ H) (closed' : DepClosed H') (fresh : P.id ∉ ids H') : applySpec H' P = .error e
-- (except `missingDependencies`, which is the retry signal and depends on what has arrived)

-- T3.4 MAIN: view convergence on the model.
theorem view_converges (l₁ l₂ : List Patch) (same : ∀ P, P ∈ l₁ ↔ P ∈ l₂)
    (t₂ : Topological l₂) (u : IdsInjective l₁)
    (h₁ : replaySpec [] l₁ = .ok H₁) :
    ∃ H₂, replaySpec [] l₂ = .ok H₂ ∧ materializeSpec H₁ = materializeSpec H₂

-- T3.5 MAIN: the same on extracted code (T1.1 + T2.0 + T3.4).
theorem extracted_view_converges (ps₁ ps₂ : List document.DensePatch) ...
    (r₁ : replayDoc empty ps₁ = ok (.Ok d₁)) :
    ∃ d₂, replayDoc empty ps₂ = ok (.Ok d₂) ∧
      canon (materialize d₁ marks) = canon (materialize d₂ marks)
-- The dense patches are interned per replica in arrival order; T1.6 removes that dependence.

-- T3.6 (platform, optional) Incremental equals fresh for graph documents.
theorem sync_eq_fresh : (syncs over any sequence of channel prefixes).documents[path]
    ≈ document_at path (channel tips)          -- equal up to Abs, by T3.4
```

Duplicates (`AlreadyPresent`) are covered: `same` is about membership, so a list may repeat a patch.

### 3.3 Reuse

- `Patch.delivery_converges` and `insert_commutes`/`insert_idempotent` give the shape of the induction. Here the inductive step is T3.2 rather than set insertion.
- `Patch.dependency_closed` gives that each prefix of a topological replay is dependency-closed, which is the hypothesis of T1.4.
- The registry `Guarantees.lean` style (`model_of`, then reasoning only on the model) carries over directly: prove T3.1–T3.4 purely on `DocModel`, then lift once.

### 3.4 Risks

- **R3.1 False as stated without id injectivity.** `apply` keeps the **first** record under a given `PatchId` and returns `IdentityCollision` for a different second one. With two distinct records under one id, the surviving record depends on delivery order. `IdsInjective` is a real hypothesis (SHA-256 collision resistance, enforced by `Repository::project`'s `body::hash(&b.bytes) != patch.hash` check). Keep it explicit; it cannot be proved.
- **R3.2 State equality stops being order independent.** Dense vectors are in arrival order. Today's test `assert_eq!(forward, reverse)` (`same_anchor_insertions_retain_an_order_conflict_in_both_deliveries`, `concurrent_opposite_order_resolutions_converge_to_explicit_cycle` and the 120-permutation test) will fail after the rewrite unless the comparison goes through `canon`/`Abs`. This is a spec change. Announce it in `docs/patch-model.md`.
- **R3.3 "Same set" must mean "same admitted set".** A patch rejected for `MissingDependencies` in one order and retried later is not a counterexample, because the theorem quantifies over successful replays. A patch with a *non-retryable* error is rejected in every topological order (T3.3). This must be proved, not assumed. It depends on T1.4, so a slip in `own`-edge handling (R1.1) would break it.
- **R3.4 Diagnostics.** Convergence of `layers` and `conflicts` holds only after canonical sorting (R2.1). Convergence of `bytes` holds without it, because bytes exist only when every layer is a singleton.
- **R3.5 Platform conflict alternatives carry local handles.** `Alternative.patch: u64` in `render_output` and `overlay_outputs` is a per-registry handle. Two replicas' `View`s are equal only up to the handle↔hash bijection. State any platform-level convergence modulo that renaming.

---

## 4. Line-edit merging and proposal preparation

### 4.1 What moves

New module `crates/kernel/src/lines.rs` (extractable). The platform's `crates/platform/src/lines.rs` keeps the `String`/`Arc<str>` handling and calls it.

| Current (native) | Extractable replacement | Data-structure change |
|---|---|---|
| `prepare_lines`: `group.windows(2).any(overlaps)` | `lines::ranges_valid(ranges: &Vec<(u64,u64)>) -> bool`: sorted by `(start,end)` and adjacent pairs non-conflicting (via `interval::conflict`) | slices become `Vec` |
| `LineVersion::merge_all` (a `BTreeMap<(u64,u64), LineEdit>` with `range(..key).next_back()` / `range(key..).next()` neighbour checks) | `lines::merge_two(a: &Vec<Range>, b: &Vec<Range>) -> Option<Vec<Range>>`, a linear merge of two sorted lists that checks each emitted range against the last one emitted and collapses equal ranges; `lines::merge_all(vs: &Vec<Vec<Range>>) -> Option<Vec<Range>>` as a fold | `Range = (u64, u64, Vec<u8>)`; content equality via `h5i_app_std::bytes::eq`. Native code keeps the `base`/`original` equality checks (`Arc::ptr_eq` or string equality) and builds the `Vec<Range>` from `LineEdit`. |
| `render` (`lines.rs`): `usize::try_from`, `start < cursor ∥ end < start ∥ end >= boundaries.len()`, then `boundaries[e.start]` | `lines::to_chunks(boundaries: &Vec<usize>, ranges: &Vec<(u64,u64,Vec<u8>)>) -> Option<Vec<(usize,usize,Vec<u8>)>>`, and `lines::render(original: &Vec<u8>, ranges) -> Option<Vec<u8>>` = `line_boundaries` ; `to_chunks` ; `checked_render` | closes the "native range conversion" gap named in `docs/proofs.md` |
| `prepare_lines` head check (two nested `BTreeSet` stacks) | `lines::sole_head(dep_rows: &Vec<Vec<usize>>, edits_path: &Vec<bool>, deps: &Vec<usize>, base: usize, marks) -> bool`, using `indexed::reachable` | registry handles become dense row indices (they are already dense and topological) |

What stays native in `Repository::prepare` and `prepare_lines`:
- Command destructuring, `intent` and count limits, `valid_path`, sorting edits by `String` path, base64 decoding, `body::encode` and `body::hash`.
- UTF-8 decoding of the rendered bytes, and line-base lookup by path string.

These are input-format checks. The theorems below take their results as hypotheses.

`validate_graph_edits` stays native glue. After area 1 it calls the extracted `document::apply` through `document_at`, so its soundness theorem (T4.7) is proved at model level and composed.

**Size estimate.** About 200 lines of subset Rust. Lean: 900–1,300 lines, most of it the `merge_two` loop invariant and the `to_chunks` bounds proof. This is the smallest and most self-contained area.

### 4.2 Theorems

```lean
open o5s_kernel.Interval   -- Conflict

abbrev Range := Nat × Nat × List UInt8
def Conf (x y : Range) : Prop := Conflict x.1 x.2.1 y.1 y.2.1
def key (x : Range) := (x.1, x.2.1)
/-- Strict separation: the relation sorted, adjacent-non-conflicting ranges satisfy. -/
def Sep (x y : Range) : Prop :=
  if x.1 = x.2.1 ∨ y.1 = y.2.1 then x.2.1 < y.1 else x.2.1 ≤ y.1
def OK (rs : List Range) : Prop := ∀ x ∈ rs, ∀ y ∈ rs, x ≠ y → ¬ Conf x y
def canon (rs : List Range) : List Range := (rs.dedup).mergeSort (key · ≤ key ·)

-- T4.1 Adjacent checks suffice (justifies both `windows(2)` and the current neighbour check).
theorem sep_trans : Sep x y → Sep y z → Sep x z
theorem adjacent_sufficient (s : rs.Sorted (key · ≤ key ·)) :
    (∀ i, i + 1 < rs.length → ¬ Conf rs[i] rs[i+1]) ↔ rs.Chain' Sep ∧ OK rs
theorem conf_refl (x : Range) : Conf x x      -- equal keys always conflict (so a key clash ⇒ conflict unless identical)

-- T4.2 Extracted validators.
theorem ranges_valid_spec (rs) : lines.ranges_valid rs ⦃ b => b = true ↔ rs.Sorted (key · < key ·) ∧ OK rs ⦄

-- T4.3 Merging.
theorem merge_two_spec (a b) (ha : a.Chain' Sep) (hb : b.Chain' Sep) (room) :
    lines.merge_two a b ⦃ r => (r = some m ↔ OK (a ++ b) ∧ m = canon (a ++ b)) ∧ (r = none ↔ ¬ OK (a ++ b)) ⦄
theorem merge_all_spec (vs) (h : ∀ v ∈ vs, v.Chain' Sep) :
    lines.merge_all vs ⦃ r => r = if OK vs.join then some (canon vs.join) else none ⦄
-- Corollaries (the property the 6-permutation test samples):
theorem merge_all_perm (h : vs₁.Perm vs₂) : mergeModel vs₁ = mergeModel vs₂
theorem merge_all_idem (hv : v ∈ vs) : mergeModel (vs ++ [v]) = mergeModel vs
theorem merge_single (h : v.Chain' Sep) : mergeModel [v] = some v   -- single head ⇒ stored preview

-- T4.4 Rendering through line coordinates (composition of proved pieces).
theorem to_chunks_spec (bs : List Nat) (rs) :
    lines.to_chunks bs rs ⦃ r => r.isSome ↔ rs.Chain' Sep ∧ ∀ x ∈ rs, x.2.1 < bs.length ⦄
theorem render_lines_spec (orig rs) (h : rs.Chain' Sep) (hb : ∀ x ∈ rs, x.2.1 < (Coordinates.model orig).length) :
    lines.render orig rs ⦃ r => r = some (Rendering.model orig (chunks (Coordinates.model orig) rs)) ⦄

-- T4.5 Merged render = sequential application (what users mean by "both edits apply").
theorem render_merge_sequential (x y : Range) (sep : Sep x y) :
    renderModel orig [x, y] = renderModel (renderModel orig [x]) [shift x y]   -- via Splice.disjoint_blocks_commute
theorem render_preserves_untouched (h : renderModel orig rs = some out) :
    ∀ line ℓ of orig not covered by any range in rs, ℓ occurs in out, in order

-- T4.6 Sole causal head (prepare_lines' last loop).
theorem sole_head_spec : lines.sole_head rows edits deps base marks ⦃ b =>
    b = true ↔ edits[base] ∧ base ∈ Closure deps ∧
      ∀ q ∈ Closure deps, edits[q] → q = base ∨ AncR q base ⦄

-- T4.7 Prepared graph patches never break a view (area 1 locality).
theorem prepared_graph_admissible
    (hv : validateGraphSpec R deps edits hash = .ok ()) (P := graphPatch R hash deps edit)
    (H : Hist) (closed : DepClosed H) (hpast : past H P = past (documentAt R path deps) P)
    (fresh : P.id ∉ ids H) : ∃ H', applySpec H P = .ok (H', .inserted)
```

T4.7 is the guarantee that `IncrementalView::integrate`'s `documents::apply_graph_patch(..)?` never fails for an admitted patch that went through `prepare` (see R1.5).

### 4.3 Reuse

| Lemma | Used for |
|---|---|
| `Interval.conflict_spec`, `conflict_symmetric`, `strict_gap_safe` | `Conf`, `sep_trans`, `adjacent_sufficient` |
| `Checked.checked_render_spec`, `valid_chunks_safe`, `Good` | `render_lines_spec` (`to_chunks` output satisfies `Good`) |
| `Rendering.render_chunks_spec`, `model` | T4.4, T4.5 |
| `Coordinates.line_boundaries_spec`, `line_boundaries_ordered` | T4.4 (strictly increasing boundaries make `chunks` sorted when ranges are) |
| `Splice.disjoint_blocks_commute`, `extracted_disjoint_commutes` | T4.5 |
| `Indexed.reachable_spec` | T4.6 |

### 4.4 Risks

- **R4.1 The current neighbour check is sound, but only for sorted, pairwise-OK maps.** Checking only the predecessor and successor keys in the `BTreeMap` is complete because `Sep` is transitive (T4.1). I checked this by case analysis on empty and non-empty ranges: an intermediate key cannot exist when a far neighbour conflicts. Moving to a linear `merge_two` removes the need to prove anything about `BTreeMap::range`. If the `BTreeMap` code is kept, the theorem must be stated about an abstract ordered map, with `range` modelled by hand. That is not recommended.
- **R4.2 Identical concurrent edits collapse.** `existing == edit` → `continue` compares the whole `LineEdit`, including content. So two agents inserting the *same* line at the same point give one copy, while different contents conflict. The spec must state this (`canon` uses `dedup`). Some users would expect two copies.
- **R4.3 Insertion at a replacement boundary conflicts.** `Conflict` treats `(2,2)` versus `(2,4)` as a conflict (proved, with a mutation test `line_conflict_boundary`). T4.3 inherits this. It is intended, but it means `merge_all_spec` is not "merge iff the byte spans are disjoint".
- **R4.4 Line semantics are byte semantics.** Replacement `content` is not required to end in `\n`. A merged render can join lines, so "line" theorems must be phrased on the base's boundaries only (as above), never on the result's lines.
- **R4.5 `render_path` only tries `merge_all` when `heads.len() > 1` and all heads share a mode.** A single head uses the stored preview. Its consistency needs `merge_single` plus "the stored preview is exactly `render(version)`". That holds because `prepare_lines` stores `version.render()?`, but it is a native fact. Add a test that recomputes it, or move the preview computation onto the extracted `lines::render`.
- **R4.6 Mixed-kind paths.** `prepare` refuses a path appearing in both `edits` and `graph_edits` of one proposal, and the registry kernel's `register_paths` fixes each path's kind (`GraphDocument` / `LegacyDocument` errors). The line theorems assume a legacy path. The registry refinement already proves the kind gate, so cite it rather than re-proving it.
- **R4.7 Base/original equality.** `merge_all` compares `original` by pointer or string equality and `base` by handle. In the kernel version this check stays native, so T4.3 is stated per base, with "all versions share `base`" as a hypothesis.

---

## 5. Recommended order and milestones

Model-first: each milestone ends with theorems registered in `h5i-app.toml [check]` and at least one new `[[mutant]]`.

| # | Milestone | Content | Exit criterion | Effort |
|---|---|---|---|---|
| M0 | **Pure document model and convergence on the model** | `proofs/DocModel.lean`: §1.2 definitions, `materializeSpec` (§2.2), T3.1–T3.4, T2.2, T2.4–T2.7 on the model only. No extraction, no Rust change. | `view_converges` and `clean_iff_total` proved. Spec bugs (R1.1, R2.4, R3.1) are caught here, cheaply. | 2–3 weeks |
| M1 | **Line merging kernel** (area 4, independent of M0) | `crates/kernel/src/lines.rs` (`ranges_valid`, `merge_two`, `merge_all`, `to_chunks`, `render`). The platform's `lines.rs` calls it. `Lines.lean`: T4.1–T4.5. Add `start-from` entries. Mutants: drop the last-emitted check; `<` vs `≤` in `Sep`. | Existing `crates/platform/tests/lines.rs` (including the 6-permutation and 10,000-agent tests) pass unchanged. `merge_all_spec` and `render_lines_spec` checked. | 1–2 weeks |
| M2 | **Document kernel: apply** (area 1) | `document.rs` validate-then-commit, native adapter with interning. `DocApply.lean`: T1.1, T1.3, T1.4, T1.5. Rewrite the three `assert_eq!(state, state)` tests to compare `canon(materialize)` (R3.2). Mutants: drop the `own` edges; reverse the cycle check; skip the `observed` check. | All `model.rs` tests plus `crates/platform/tests/documents.rs` and `crates/cli/tests/workflow.rs` graph tests pass. `apply_refines` checked. | 3–4 weeks |
| M3 | **Document kernel: materialize** (area 2) | `document::{delete_insert, liveness, summaries, kahn, emit}`. `DocMaterialize.lean`: T2.0, T2.1, T2.3, T2.6. Mutants: `frontier.len() > 1` → `> 2`; drop the `concurrent` test; drop the `acknowledged` test. | `materialize_refines` checked. The existing exhaustive test `indexed_boundary_frontiers_match_full_history_reachability` still passes. | 3–4 weeks |
| M4 | **Extracted convergence** (area 3) | T1.6 (equivariance) and T3.5 by composing M0, M2 and M3. Update `docs/proofs.md`: retire the sentence "The materializer has not been extracted or proved". | `extracted_view_converges` in `[check]`. | 1 week |
| M5 | **Preparation and platform glue** | `lines::sole_head` (T4.6), `documents::graph_frontier` (T1.7), T4.7. Optional: T3.6 incremental = fresh. | `prepared_graph_admissible` checked. The "native glue" list in `docs/proofs.md` reduces to parsing, hashing, UTF-8 and path strings. | 2 weeks |

Rationale:
- M0 comes first because most of the risk is in the *statements*, not the extraction (R1.1, R2.1–R2.4, R3.1–R3.3). It costs no Rust churn.
- M1 is independent and quick. It closes the gap that `docs/proofs.md` already names ("native range conversion … and combining versions remain tested trust-boundary code"), which gives an early visible result while M0 settles.
- M2 then M3, because `materialize_refines` needs the `Abs`/`Inv` relation from M2.
- M4 is mostly composition once M0, M2 and M3 exist.

Total estimate: about 1,000–1,200 lines of new subset Rust (and about 250 lines of native adapter replacing about 620), plus 5,000–7,000 lines of Lean. That is roughly 12–16 person-weeks, at the pace of the registry refinement work.

Operational note: each milestone adds Lean modules under `proofs/` (or a new `crates/kernel/proofs` package, if the kernel lakefile grows too slow). Build them on `/local/rcs/ht2673` because of home-quota and `/tmp` limits, and not while another Lean proof job is running: memory is the bottleneck.
