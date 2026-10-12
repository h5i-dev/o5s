import Narrow
/-!
# PostgreSQL serves narrow loads (item 6, statements to prove)

`Narrow.narrow_load` assumes the rows a command loads agree with the whole
tenant on every lookup the command makes. This file models what the
PostgreSQL loader (`load` in `crates/pgwire/src/service.rs`) actually returns
and states that it satisfies `Narrow`, which removes that assumption except
for PostgreSQL's own query semantics:

- `SELECT … WHERE tenant AND col = v ORDER BY <primary key>` returns the
  tenant's rows satisfying the condition, sorted by primary key (`query`).
- `rows_where` appends each returned row that is not already loaded
  (`appendNew`); a command with more than `POINT_LOADS` keys loads the whole
  table instead (`query` with a true condition).

`Narrow` compares lists, so order matters: a filtered table must be in
primary-key order in the committed state too (`KeyOrdered`). This holds for
every table except possibly `o5s_dependencies`, whose rows are stored in the
order a proposal lists its dependencies. The native `prepare` sorts them,
but the kernel does not require it; `deps_key_ordered` is stated under that
hypothesis (`DepsAscending`), and making `propose` refuse unsorted
dependencies would discharge it.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Narrow

namespace o5s_registry.Served

/-- More keys than this load the whole table (`POINT_LOADS`). -/
def pointLoads : Nat := 32

/-- PostgreSQL's answer to a filtered, primary-key-ordered `SELECT`. -/
def query {α κ} [LinearOrder κ] (key : α → κ) (table : List α) (P : α → Bool) : List α :=
  (table.mergeSort fun a b => decide (key a ≤ key b)).filter P

/-- `rows_where`: append the returned rows not already loaded. -/
def appendNew {α} [DecidableEq α] (acc res : List α) : List α :=
  res.foldl (fun out r => if r ∈ out then out else out ++ [r]) acc

/-- Primary keys, as the schema declares them. -/
def patchKey (p : PatchRow) : Nat := p.id.val
def depKey (d : Dependency) : Nat × Nat := (d.patch.val, d.dependency.val)
def approvalKey (a : Approval) : Nat × Nat := (a.patch.val, a.reviewer.val)
def checkKey (c : CheckReport) : Nat := c.patch.val
def channelKey (c : Channel) : Nat := c.id.val
def memberKey (m : Member) : Nat × Nat := (m.channel.val, m.position.val)
def pathKey (k : PathKind) : Nat := k.id.val
def listingKey (l : Listing) : Nat := l.repo.val

/-- The rows `load` returns for command `c` from the committed tenant `full`,
mirroring `crates/pgwire/src/service.rs` branch by branch. Tables a command
does not load are empty. -/
noncomputable def pgLoad (c : Command) (full : St) : St :=
  let none : St := ⟨full.repo, [], [], [], [], [], [], [], [], [], [], []⟩
  -- Every repository command also loads the whole role table.
  let base : St := { none with roles := full.roles }
  let channelsNamed (n : alloc.vec.Vec U8) (acc : List Channel) : List Channel :=
    appendNew acc (query channelKey full.channels fun c => decide (c.label.val = n.val))
  let membersOf (chs : List Channel) (n : alloc.vec.Vec U8) : List Member :=
    match chs.find? (fun c => decide (c.label.val = n.val)) with
    | some c => query memberKey full.members fun m => decide (m.channel = c.id)
    | Option.none => []
  match c with
  | .Register n repo =>
    { none with listings := appendNew
        (query listingKey full.listings fun l => decide (l.label.val = n.val))
        (query listingKey full.listings fun l => decide (l.repo = repo)) }
  | .Propose _ h deps uses _ =>
    { base with
      patches :=
        if deps.val.length > pointLoads then query patchKey full.patches fun _ => true
        else deps.val.foldl (fun acc d => appendNew acc (query patchKey full.patches fun p => decide (p.id = d)))
          (query patchKey full.patches fun p => decide (p.hash.val = h.val)),
      paths :=
        if uses.val.length > pointLoads then query pathKey full.paths fun _ => true
        else uses.val.foldl (fun acc u => appendNew acc
          (query pathKey full.paths fun k => decide (k.path.val = u.path.val))) [] }
  | .Approve _ p | .Check _ p _ =>
    { base with patches := query patchKey full.patches fun q => decide (q.id = p) }
  | .Fork _ src n =>
    let chs := channelsNamed src (channelsNamed n [])
    { base with channels := chs, members := membersOf chs src }
  | .Merge _ n ids =>
    let chs := channelsNamed n []
    let one {α} [DecidableEq α] {κ} [LinearOrder κ] (key : α → κ) (t : List α) (P : α → U64 → Bool) :=
      if ids.val.length > pointLoads then query key t fun _ => true
      else ids.val.foldl (fun acc id => appendNew acc (query key t fun r => P r id)) []
    { base with channels := chs, members := membersOf chs n,
      patches := one patchKey full.patches fun q id => decide (q.id = id),
      deps := one depKey full.deps fun d id => decide (d.patch = id),
      approvals := one approvalKey full.approvals fun a id => decide (a.patch = id),
      checks := one checkKey full.checks fun x id => decide (x.patch = id) }
  | .Grant .. => base
  | _ => full

/-- Each table, filtered by any condition `load` uses, is already in
primary-key order in the committed state. -/
def KeyOrdered (s : St) : Prop :=
  (s.patches.Pairwise fun a b => patchKey a < patchKey b) ∧
  (∀ p, (s.deps.filter fun d => decide (d.patch = p)).Pairwise fun a b => depKey a < depKey b) ∧
  (∀ p, (s.approvals.filter fun a => decide (a.patch = p)).Pairwise fun a b => approvalKey a < approvalKey b) ∧
  (s.checks.Pairwise fun a b => checkKey a < checkKey b) ∧
  (s.channels.Pairwise fun a b => channelKey a < channelKey b) ∧
  (∀ c, (s.members.filter fun m => decide (m.channel = c)).Pairwise fun a b => memberKey a < memberKey b) ∧
  (s.paths.Pairwise fun a b => pathKey a < pathKey b) ∧
  (s.listings.Pairwise fun a b => listingKey a < listingKey b)

/-- Every stored dependency list is strictly ascending. -/
def DepsAscending (s : St) : Prop :=
  ∀ p, (s.deps.filter fun d => decide (d.patch = p)).Pairwise fun a b => a.dependency.val < b.dependency.val

/-- Reachable states are key-ordered given ascending dependency lists
(identifiers are allocated in increasing order; `Invariant.Inv`). -/
theorem key_ordered (s : Snapshot) (inv : Invariant.Inv (Snapshot.toSt s))
    (deps : DepsAscending (Snapshot.toSt s)) : KeyOrdered (Snapshot.toSt s) := by sorry

/-- The loader's rows agree with the committed tenant on every lookup. -/
theorem pg_load_narrow (c : Command) (full : St) (inv : Invariant.Inv full)
    (ord : KeyOrdered full) : Narrow c full (pgLoad c full) := by sorry

/-- **PostgreSQL loads.** On the rows the PostgreSQL loader returns, the
extracted `transition` computes what the whole committed tenant specifies. -/
theorem pg_served (a : Principal) (s p : Snapshot) (c : Command)
    (inv : Invariant.Inv (Snapshot.toSt s)) (deps : DepsAscending (Snapshot.toSt s))
    (loaded : Snapshot.toSt p = pgLoad c (Snapshot.toSt s)) (r) (h : transition a p c = ok r) :
    Refine.lift r = Model.step a (Snapshot.toSt s) c :=
  Narrow.narrow_load a s p c (loaded ▸ pg_load_narrow c _ inv (key_ordered s inv deps)) r h

end o5s_registry.Served
