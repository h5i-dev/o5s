import Guarantees
import Writes
/-!
# The registry invariant

`Inv` holds of every tenant the registry can reach. `inv_step` proves each
successful command keeps it, about the model; `inv_preserved` transfers that
to the extracted `transition`.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Writes H5iAppLib

namespace o5s_registry.Invariant

def hasPatch (s : St) (id : U64) : Prop := ∃ p ∈ s.patches, p.id = id

def isMember (s : St) (c p : U64) : Prop := ∃ m ∈ s.members, m.channel = c ∧ m.patch = p

/-- Facts that hold in every state a tenant can reach. -/
structure Inv (s : St) : Prop where
  patch_keys : (s.patches.map (·.id)).Nodup
  /-- Handles never exceed the counter, so a new handle never overwrites a patch. -/
  patch_fresh : ∀ p ∈ s.patches, p.id.val ≤ s.repo.patches.val
  /-- One patch per content identity. -/
  hashes : (s.patches.map (fun p => p.hash.val)).Nodup
  /-- Dependencies name registered patches, and earlier ones. -/
  deps_known : ∀ d ∈ s.deps, hasPatch s d.patch ∧ hasPatch s d.dependency ∧ d.dependency.val < d.patch.val
  /-- Reviews and check reports come from someone other than the author. -/
  approvals_independent : ∀ a ∈ s.approvals, ∃ p ∈ s.patches, p.id = a.patch ∧ p.author ≠ a.reviewer
  checks_independent : ∀ c ∈ s.checks, ∃ p ∈ s.patches, p.id = c.patch ∧ p.author ≠ c.reporter
  channel_keys : (s.channels.map (·.id)).Nodup
  channel_fresh : ∀ c ∈ s.channels, c.id.val ≤ s.repo.channels.val
  channel_labels : (s.channels.map (fun c => c.label.val)).Nodup
  /-- Every member sits below its channel's size, so admission appends. -/
  members_placed : ∀ m ∈ s.members, ∃ c ∈ s.channels, c.id = m.channel ∧ m.position.val < c.size.val
  members_known : ∀ m ∈ s.members, hasPatch s m.patch
  /-- Channels are dependency closed. -/
  closed : ∀ m ∈ s.members, ∀ d ∈ s.deps, d.patch = m.patch → isMember s m.channel d.dependency
  /-- Admitted patches were approved. -/
  reviewed : ∀ m ∈ s.members, ∃ a ∈ s.approvals, a.patch = m.patch
  event_fresh : ∀ e ∈ s.events, e.seq.val ≤ s.repo.events.val
  bodies_known : ∀ b ∈ s.bodies, hasPatch s b.patch
  path_keys : (s.paths.map (·.id)).Nodup
  path_fresh : ∀ k ∈ s.paths, k.id.val ≤ s.repo.paths.val
  path_labels : (s.paths.map (fun k => k.path.val)).Nodup
  /-- Nothing exists in a repository before it is created. -/
  unborn : s.repo.created = false → s.patches = [] ∧ s.deps = [] ∧ s.approvals = [] ∧ s.checks = [] ∧
    s.channels = [] ∧ s.members = [] ∧ s.paths = [] ∧ s.events = [] ∧ s.bodies = []

theorem applyAll_eq (s : St) (ws : List Write) : applyAll s ws =
    ⟨((ws.filterMap repoOf).getLast?).getD s.repo, puts (·.id) (ws.filterMap patchOf) s.patches,
      puts (fun y => (y.patch, y.dependency)) (ws.filterMap depOf) s.deps,
      puts (fun y => (y.patch, y.reviewer)) (ws.filterMap approvalOf) s.approvals,
      puts (·.patch) (ws.filterMap checkOf) s.checks, puts (·.id) (ws.filterMap channelOf) s.channels,
      puts (fun y => (y.channel, y.position)) (ws.filterMap memberOf) s.members,
      puts (·.id) (ws.filterMap pathOf) s.paths, puts (·.seq) (ws.filterMap eventOf) s.events,
      puts (·.patch) (ws.filterMap bodyOf) s.bodies, puts (·.repo) (ws.filterMap listingOf) s.listings⟩ := by
  rw [← applyAll_repo, ← applyAll_patches, ← applyAll_deps, ← applyAll_approvals, ← applyAll_checks,
    ← applyAll_channels, ← applyAll_members, ← applyAll_paths, ← applyAll_events, ← applyAll_bodies,
    ← applyAll_listings]

@[simp] theorem puts_nil {α κ} [DecidableEq κ] (k : α → κ) (l : List α) : puts k [] l = l := rfl
@[simp] theorem puts_single {α κ} [DecidableEq κ] (k : α → κ) (x : α) (l : List α) : puts k [x] l = upsert k x l := rfl

/-- Facts kept when only the repository row's event counter grows and other
tables are untouched, except approvals, checks and events. -/
theorem key_kept_puts {α κ : Type} [DecidableEq κ] (k : α → κ) (xs l : List α) {y : α} (hy : y ∈ l) :
    ∃ z ∈ puts k xs l, k z = k y := by
  induction xs generalizing l y with
  | nil => exact ⟨y, hy, rfl⟩
  | cons x xs ih =>
    obtain ⟨z, hz, hk⟩ := key_kept k x hy
    obtain ⟨w, hw, hk'⟩ := ih (l := upsert k x l) hz
    exact ⟨w, hw, hk'.trans hk⟩

theorem inv_approve {s : St} (hi : Inv s) {u patch : U64} {ws r}
    (h : Model.approve u s patch = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.approve at h
  split at h
  · simp at h
  · rename_i p hp
    split_ifs at h with ha hm
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    have hpm : p ∈ s.patches := List.mem_of_find?_eq_some hp
    have hpid : p.id = patch := by simpa using List.find?_some hp
    have hinc := Refine.inc_val hm
    rw [applyAll_eq]
    simp only [List.filterMap_cons, List.filterMap_nil, repoOf, patchOf, depOf, approvalOf, checkOf, channelOf,
      memberOf, pathOf, eventOf, bodyOf, listingOf, puts_nil, puts_single, List.getLast?_singleton,
      Option.getD_some]
    constructor
    all_goals simp only [hasPatch, isMember]
    · exact hi.patch_keys
    · exact hi.patch_fresh
    · exact hi.hashes
    · exact hi.deps_known
    · intro a ha'
      rcases mem_upsert_of ha' with rfl | ha'
      · exact ⟨p, hpm, hpid, ha⟩
      · exact hi.approvals_independent a ha'
    · exact hi.checks_independent
    · exact hi.channel_keys
    · exact hi.channel_fresh
    · exact hi.channel_labels
    · exact hi.members_placed
    · exact hi.members_known
    · exact hi.closed
    · intro m hm'
      obtain ⟨a, ha', hap⟩ := hi.reviewed m hm'
      obtain ⟨z, hz, hk⟩ := key_kept (fun y : Approval => (y.patch, y.reviewer)) ⟨patch, u⟩ ha'
      exact ⟨z, hz, by simp only [Prod.mk.injEq] at hk; rw [hk.1, hap]⟩
    · intro e he
      rcases mem_upsert_of he with rfl | he
      · exact le_refl _
      · have := hi.event_fresh e he; rw [hinc]; omega
    · exact hi.bodies_known
    · exact hi.path_keys
    · exact hi.path_fresh
    · exact hi.path_labels
    · intro hc
      have := (hi.unborn hc).1
      rw [this] at hpm; simp at hpm

end o5s_registry.Invariant
