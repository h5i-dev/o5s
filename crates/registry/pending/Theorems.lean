import Storage
/-!
# What the registry guarantees (statements)

The properties the server relies on, stated about the extracted
`transition`. Proofs are pending (`sorry`); these theorems are deliberately
absent from the `h5i-app.toml` gate until they are proved.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec H5iAppLib

namespace o5s_registry.Theorems

/-! ## The registry invariant -/

def hasPatch (s : St) (id : U64) : Prop := ∃ p ∈ s.patches, p.id = id

def memberOf (s : St) (c p : U64) : Prop := ∃ m ∈ s.members, m.channel = c ∧ m.patch = p

/-- Facts that hold in every state a repository tenant can reach. -/
structure Inv (s : St) : Prop where
  patch_keys : (s.patches.map (·.id)).Nodup
  /-- Handles never exceed the counter, so a new handle never overwrites a patch. -/
  patch_fresh : ∀ p ∈ s.patches, p.id.val ≤ s.repo.patches.val
  /-- One patch per content identity. -/
  hashes : (s.patches.map (·.hash)).Nodup
  /-- Dependencies name earlier, registered patches. -/
  deps_known : ∀ d ∈ s.deps, hasPatch s d.patch ∧ hasPatch s d.dependency ∧ d.dependency.val < d.patch.val
  /-- Reviews and check reports come from someone other than the author. -/
  approvals_independent : ∀ a ∈ s.approvals, ∃ p ∈ s.patches, p.id = a.patch ∧ p.author ≠ a.reviewer
  checks_independent : ∀ c ∈ s.checks, ∃ p ∈ s.patches, p.id = c.patch ∧ p.author ≠ c.reporter
  channel_fresh : ∀ c ∈ s.channels, c.id.val ≤ s.repo.channels.val
  channel_labels : (s.channels.map (·.label)).Nodup
  /-- Every member sits below its channel's size, so admission appends. -/
  members_placed : ∀ m ∈ s.members, ∃ c ∈ s.channels, c.id = m.channel ∧ m.position.val < c.size.val
  members_known : ∀ m ∈ s.members, hasPatch s m.patch
  /-- Channels are dependency closed. -/
  closed : ∀ m ∈ s.members, ∀ d ∈ s.deps, d.patch = m.patch → memberOf s m.channel d.dependency
  /-- Admitted patches were approved. -/
  reviewed : ∀ m ∈ s.members, ∃ a ∈ s.approvals, a.patch = m.patch
  event_fresh : ∀ e ∈ s.events, e.seq.val ≤ s.repo.events.val
  bodies_known : ∀ b ∈ s.bodies, hasPatch s b.patch
  path_fresh : ∀ k ∈ s.paths, k.id.val ≤ s.repo.paths.val
  path_labels : (s.paths.map (·.path)).Nodup
  /-- Nothing exists in a repository tenant before it is created. -/
  unborn : s.repo.created = false → s.patches = [] ∧ s.channels = [] ∧ s.members = [] ∧ s.events = []

def Succeeds (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ r, transition a s c = .ok (.Ok (ws, r))

theorem inv_preserved (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (hi : Inv (Snapshot.toSt s)) (h : Succeeds a s c ws) : Inv (applyAll (Snapshot.toSt s) ws.val) := by
  sorry

/-! ## Loading less than a tenant -/

/-- Repository commands read no events, bodies or listings. -/
theorem frame (a : Principal) (s : Snapshot) (c : Command) (e : alloc.vec.Vec Event)
    (b : alloc.vec.Vec Body) (l : alloc.vec.Vec Listing)
    (hc : ∀ n r, c ≠ .Register n r) :
    transition a { s with events := e, bodies := b, listings := l } c = transition a s c := by
  sorry

/-- What the PostgreSQL store's `load_for` keeps of a tenant's rows for `c`
(`crates/server/src/pg.rs`): rows matching the keys and columns `c` names. -/
def Narrow (c : Command) (full part : St) : Prop :=
  part.repo = full.repo ∧ part.events = [] ∧ part.bodies = [] ∧
  (∀ p ∈ part.patches, p ∈ full.patches) ∧
  match c with
  | .Approve _ id | .Check _ id _ => ∀ p ∈ full.patches, p.id = id → p ∈ part.patches
  | .Propose _ h ds ps _ => (∀ p ∈ full.patches, (p.hash = h ∨ p.id ∈ ds.val) → p ∈ part.patches) ∧
      ∀ k ∈ full.paths, (∃ u ∈ ps.val, u.path = k.path) → k ∈ part.paths
  | .Merge _ ch ids => (∀ k ∈ full.channels, k.label = ch → k ∈ part.channels) ∧
      (∀ m ∈ full.members, (∃ k ∈ full.channels, k.label = ch ∧ k.id = m.channel) → m ∈ part.members) ∧
      (∀ p ∈ full.patches, p.id ∈ ids.val → p ∈ part.patches) ∧
      (∀ d ∈ full.deps, d.patch ∈ ids.val → d ∈ part.deps) ∧
      (∀ x ∈ full.approvals, x.patch ∈ ids.val → x ∈ part.approvals) ∧
      (∀ x ∈ full.checks, x.patch ∈ ids.val → x ∈ part.checks)
  | _ => True

/-- On a narrow load the transition writes and replies as on the whole
tenant, given keyed tables without duplicates. -/
theorem narrow_load (a : Principal) (s p : Snapshot) (c : Command)
    (hi : Inv (Snapshot.toSt s)) (hn : Narrow c (Snapshot.toSt s) (Snapshot.toSt p)) :
    transition a p c = transition a s c := by
  sorry

/-! ## Isolation -/

/-- A repository command succeeds only on the caller's own created repository. -/
theorem isolation (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (h : Succeeds a s c ws) (hc : ∀ n r, c ≠ .Register n r) (hc' : ∀ n, c ≠ .Create n) :
    s.repository.created = true ∧ s.repository.org = a.org := by
  sorry

/-- Directory and repository rows never share a tenant scope. -/
theorem scopes_separate (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (h : Succeeds a s c ws) :
    (a.scope.val < REPOSITORY_SCOPE.val → ∀ w ∈ ws.val, ∃ l, w = .PutListing l) ∧
      (REPOSITORY_SCOPE.val ≤ a.scope.val → ∀ w ∈ ws.val, ∀ l, w ≠ .PutListing l) := by
  sorry

/-! ## History -/

/-- Registered patches never change or disappear. -/
theorem patches_immutable (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (hi : Inv (Snapshot.toSt s)) (h : Succeeds a s c ws) :
    ∀ p ∈ (Snapshot.toSt s).patches, p ∈ (applyAll (Snapshot.toSt s) ws.val).patches := by
  sorry

/-- Channel membership only grows. -/
theorem members_append_only (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (hi : Inv (Snapshot.toSt s)) (h : Succeeds a s c ws) :
    ∀ m ∈ (Snapshot.toSt s).members, m ∈ (applyAll (Snapshot.toSt s) ws.val).members := by
  sorry

/-- A patch newly admitted by a merge had an approval and a passing check
report in the state the merge read. -/
theorem merge_gate (a : Principal) (s : Snapshot) (r ch : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64)
    (ws : alloc.vec.Vec Write) (h : Succeeds a s (.Merge r ch ids) ws) :
    ∀ m, Write.PutMember m ∈ ws.val →
      (∃ x ∈ s.approvals.val, x.patch = m.patch) ∧ (∃ x ∈ s.checks.val, x.patch = m.patch ∧ x.passed = true) := by
  sorry

end o5s_registry.Theorems
