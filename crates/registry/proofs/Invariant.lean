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
  member_keys : (s.members.map (fun m => (m.channel, m.position))).Nodup
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
    · exact hi.member_keys
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

theorem inv_check {s : St} (hi : Inv s) {u patch : U64} {passed : Bool} {ws r}
    (h : Model.check u s patch passed = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.check at h
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
    · exact hi.approvals_independent
    · intro c hc'
      rcases mem_upsert_of hc' with rfl | hc'
      · exact ⟨p, hpm, hpid, ha⟩
      · exact hi.checks_independent c hc'
    · exact hi.channel_keys
    · exact hi.channel_fresh
    · exact hi.channel_labels
    · exact hi.member_keys
    · exact hi.members_placed
    · exact hi.members_known
    · exact hi.closed
    · exact hi.reviewed
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

theorem inv_register {s : St} (hi : Inv s) {a : Principal} {n : alloc.vec.Vec U8} {repo : U64} {ws r}
    (h : Model.register a s.listings n repo = .Ok (ws, r)) : Inv (applyAll s ws) := by
  have hws : ws = [] ∨ ∃ l, ws = [.PutListing l] := by
    unfold Model.register at h
    repeat' split at h
    all_goals simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    all_goals obtain ⟨rfl, -⟩ := h
    all_goals simp
  rcases hws with rfl | ⟨l, rfl⟩
  · exact hi
  · rw [applyAll_eq]
    simp only [List.filterMap_cons, List.filterMap_nil, repoOf, patchOf, depOf, approvalOf, checkOf, channelOf,
      memberOf, pathOf, eventOf, bodyOf, listingOf, puts_nil, puts_single, List.getLast?_nil, Option.getD_none]
    exact ⟨hi.1, hi.2, hi.3, hi.4, hi.5, hi.6, hi.7, hi.8, hi.9, hi.10, hi.11, hi.12, hi.13, hi.14, hi.15, hi.16,
      hi.17, hi.18, hi.19, hi.20⟩

theorem inv_create {s : St} (hi : Inv s) {a : Principal} {n : alloc.vec.Vec U8} {ws r}
    (h : Model.create a s.repo n = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.create at h
  split_ifs at h with h1 h2 h3
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, -⟩ := h
  have hu := hi.unborn (by simpa using h3)
  obtain ⟨hp, hd, ha, hc, hch, hm, hpa, he, hb⟩ := hu
  rw [applyAll_eq]
  simp only [List.filterMap_cons, List.filterMap_nil, repoOf, patchOf, depOf, approvalOf, checkOf, channelOf,
    memberOf, pathOf, eventOf, bodyOf, listingOf, puts_nil, puts_single, List.getLast?_singleton, Option.getD_some,
    hp, hd, ha, hc, hch, hm, hpa, he, hb]
  constructor <;> simp [upsert, hasPatch, isMember]

@[simp] theorem filterMap_none {α β} (l : List α) : l.filterMap (fun _ => (none : Option β)) = [] := by
  induction l <;> simp_all

@[simp] theorem filterMap_some_eq_map {α β} (g : α → β) (l : List α) :
    l.filterMap (fun x => some (g x)) = l.map g := by
  induction l <;> simp_all

@[simp] theorem filterMap_some_id {α} (l : List α) : l.filterMap (fun x => some x) = l := by
  induction l <;> simp_all

theorem channel_of_member {s : St} (hi : Inv s) {m : Member} (hm : m ∈ s.members) :
    m.channel.val ≤ s.repo.channels.val := by
  obtain ⟨c, hc, hid, -⟩ := hi.members_placed m hm
  rw [← hid]; exact hi.channel_fresh c hc

theorem inv_fork {s : St} (hi : Inv s) {u : U64} {src n : alloc.vec.Vec U8} {ws r}
    (h : Model.fork u s src n = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.fork at h
  split at h
  · simp at h
  split at h
  · simp at h
  rename_i hnone
  split at h
  · simp at h
  rename_i c hc
  split at h
  · simp at h
  rename_i hm1
  split at h
  · simp at h
  rename_i hm2
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, -⟩ := h
  have hcm : c ∈ s.channels := List.mem_of_find?_eq_some hc
  have hlabel : ∀ c' ∈ s.channels, c'.label.val ≠ n.val := by
    intro c' hc' e
    have := List.find?_eq_none.1 hnone c' hc'
    simp [e] at this
  have hid := Refine.inc_val hm1
  have hinc := Refine.inc_val hm2
  set id := Model.inc s.repo.channels
  set copies := (s.members.filter (fun m => decide (m.channel = c.id))).map (fun m => { m with channel := id })
  have hfresh : ∀ c' ∈ s.channels, c'.id ≠ id := by
    intro c' hc' e; have := hi.channel_fresh c' hc'; rw [e, hid] at this; omega
  have hch : upsert (·.id) { c with id, label := n } s.channels = s.channels ++ [{ c with id, label := n }] :=
    upsert_fresh _ _ _ (fun y hy => hfresh y hy)
  -- Members of the source channel, and their copies.
  have hsrc : ∀ m ∈ s.members, m.channel = c.id → m.position.val < c.size.val := by
    intro m hm e
    obtain ⟨c', hc', hid', hpos⟩ := hi.members_placed m hm
    have : c' = c := List.inj_on_of_nodup_map hi.channel_keys hc' hcm (by rw [hid', e])
    subst this; exact hpos
  have hcopy_keys : (copies.map (fun y => (y.channel, y.position))).Nodup := by
    have hf : ((s.members.filter (fun m => decide (m.channel = c.id))).map (fun m => (m.channel, m.position))).Nodup :=
      (List.filter_sublist.map _).nodup hi.member_keys
    simp only [copies, List.map_map]
    refine List.Nodup.map_on ?_ (List.Nodup.of_map _ hf)
    intro x hx y hy e
    simp only [Function.comp, Prod.mk.injEq] at e
    simp only [List.mem_filter, decide_eq_true_eq] at hx hy
    exact List.inj_on_of_nodup_map (List.Nodup.of_map _ hf |>.map_on (fun a _ b _ h => h) |> fun _ => hf)
      (List.mem_filter.2 ⟨hx.1, by simp [hx.2]⟩) (List.mem_filter.2 ⟨hy.1, by simp [hy.2]⟩)
      (by simp [hx.2, hy.2, e.2])
  have hkeep : ∀ m ∈ s.members, m ∈ puts (fun y => (y.channel, y.position)) copies s.members := by
    intro m hm
    apply puts_keep _ hm
    intro x hx e
    simp only [copies, List.mem_map] at hx
    obtain ⟨m', -, rfl⟩ := hx
    simp only [Prod.mk.injEq] at e
    have := channel_of_member hi hm; rw [← e.1, hid] at this; omega
  have hnew : ∀ m ∈ s.members, m.channel = c.id →
      { m with channel := id } ∈ puts (fun y => (y.channel, y.position)) copies s.members := by
    intro m hm e
    exact puts_new _ (List.mem_map.2 ⟨m, List.mem_filter.2 ⟨hm, by simp [e]⟩, rfl⟩) hcopy_keys
  rw [applyAll_eq]
  simp only [List.filterMap_cons, List.filterMap_nil, List.filterMap_append, List.filterMap_map, repoOf, patchOf,
    depOf, approvalOf, checkOf, channelOf, memberOf, pathOf, eventOf, bodyOf, listingOf, puts_nil, puts_single,
    Function.comp_def, filterMap_none, filterMap_some_eq_map, List.nil_append, List.append_nil, List.cons_append,
    List.getLast?_singleton, Option.getD_some, hch]
  have hcopies : (s.members.filter (fun m => decide (m.channel = c.id))).map
      (fun x => ({ channel := id, position := x.position, patch := x.patch } : Member)) = copies := rfl
  rw [hcopies]
  set M := puts (fun y => (y.channel, y.position)) copies s.members
  have hmemM : ∀ z ∈ M, z ∈ s.members ∨ ∃ m ∈ s.members, m.channel = c.id ∧ z = { m with channel := id } := by
    intro z hz
    rcases mem_puts _ hz with hz | hz
    · exact .inl hz
    · simp only [copies, List.mem_map, List.mem_filter, decide_eq_true_eq] at hz
      obtain ⟨m, ⟨hm, he⟩, rfl⟩ := hz
      exact .inr ⟨m, hm, he, rfl⟩
  constructor
  all_goals simp only [hasPatch, isMember, List.mem_append, List.mem_singleton]
  · exact hi.patch_keys
  · exact hi.patch_fresh
  · exact hi.hashes
  · exact hi.deps_known
  · exact hi.approvals_independent
  · exact hi.checks_independent
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.channel_keys, by simp, ?_⟩
    simp only [List.mem_map, List.mem_singleton]
    rintro _ ⟨c', hc', rfl⟩ _ ⟨_, rfl, rfl⟩; exact hfresh c' hc'
  · rintro c' (hc' | rfl)
    · have := hi.channel_fresh c' hc'; rw [hid]; omega
    · exact le_refl _
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.channel_labels, by simp, ?_⟩
    simp only [List.mem_map, List.mem_singleton]
    rintro _ ⟨c', hc', rfl⟩ _ ⟨_, rfl, rfl⟩; exact hlabel c' hc'
  · exact puts_nodup _ hi.member_keys
  · intro z hz
    rcases hmemM z hz with hz | ⟨m, hm, he, rfl⟩
    · obtain ⟨c', hc', h1, h2⟩ := hi.members_placed z hz
      exact ⟨c', .inl hc', h1, h2⟩
    · exact ⟨_, .inr rfl, rfl, hsrc m hm he⟩
  · intro z hz
    rcases hmemM z hz with hz | ⟨m, hm, -, rfl⟩
    · exact hi.members_known z hz
    · exact hi.members_known m hm
  · intro z hz d hd hdp
    rcases hmemM z hz with hz | ⟨m, hm, he, rfl⟩
    · obtain ⟨m2, hm2, h1, h2⟩ := hi.closed z hz d hd hdp
      exact ⟨m2, hkeep m2 hm2, h1, h2⟩
    · obtain ⟨m2, hm2, h1, h2⟩ := hi.closed m hm d hd hdp
      exact ⟨_, hnew m2 hm2 (h1.trans he), rfl, h2⟩
  · intro z hz
    rcases hmemM z hz with hz | ⟨m, hm, -, rfl⟩
    · exact hi.reviewed z hz
    · exact hi.reviewed m hm
  · intro e he
    rcases mem_upsert_of he with rfl | he
    · exact le_refl _
    · have := hi.event_fresh e he; rw [hinc]; omega
  · exact hi.bodies_known
  · exact hi.path_keys
  · exact hi.path_fresh
  · exact hi.path_labels
  · intro hcr
    have := (hi.unborn hcr).2.2.2.2.1
    rw [this] at hcm; simp at hcm

/-- Rows with fresh, distinct keys are appended. -/
theorem puts_fresh {α κ : Type} [DecidableEq κ] (k : α → κ) (xs l : List α) (hn : (xs.map k).Nodup)
    (hf : ∀ x ∈ xs, ∀ y ∈ l, k y ≠ k x) : puts k xs l = l ++ xs := by
  induction xs generalizing l with
  | nil => simp [puts]
  | cons x xs ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and] at hn
    show puts k xs (upsert k x l) = l ++ x :: xs
    rw [upsert_fresh k x l (hf x (by simp)), ih _ hn.2]
    · simp
    · intro x' hx' y hy
      rcases List.mem_append.1 hy with hy | hy
      · exact hf x' (by simp [hx']) y hy
      · simp at hy; subst hy; exact fun e => hn.1 x' hx' e.symm

/-- What `registerPaths` adds: fresh ids above the counter, paths new to the
table and to each other. -/
structure Added (known : List PathKind) (c0 : U64) (added : List PathKind) (next : U64) : Prop where
  ids : (added.map (·.id)).Nodup
  labels : (added.map (fun k => k.path.val)).Nodup
  bounds : ∀ k ∈ added, c0.val < k.id.val ∧ k.id.val ≤ next.val
  fresh : ∀ k ∈ added, ∀ k' ∈ known, k'.path.val ≠ k.path.val
  le : c0.val ≤ next.val

theorem registerPaths_added (known : List PathKind) (c0 : U64) :
    ∀ (uses : List PathUse) (acc : List PathKind) (nx : U64) added paths,
      Added known c0 acc nx → Model.registerPaths known uses acc nx = .Ok (added, paths) → Added known c0 added paths := by
  intro uses
  induction uses with
  | nil => intro acc nx added paths ha h; simp only [Model.registerPaths, core.result.Result.Ok.injEq,
      Prod.mk.injEq] at h; obtain ⟨rfl, rfl⟩ := h; exact ha
  | cons u us ih =>
    intro acc nx added paths ha h
    simp only [Model.registerPaths] at h
    split at h
    · rename_i hnone
      split at h
      · simp at h
      rename_i hmax
      apply ih _ _ _ _ _ h
      have hinc := Refine.inc_val hmax
      -- The path is in neither table.
      have hk : known.find? (fun (k : PathKind) => decide (k.path.val = u.path.val)) = none := by
        cases hk : known.find? (fun (k : PathKind) => decide (k.path.val = u.path.val)) <;> simp_all
      have ha' : acc.find? (fun (k : PathKind) => decide (k.path.val = u.path.val)) = none := by
        rw [hk] at hnone; exact hnone
      have hkn : ∀ k' ∈ known, k'.path.val ≠ u.path.val := by
        intro k' hk' e; have := List.find?_eq_none.1 hk k' hk'; simp [e] at this
      have han : ∀ k' ∈ acc, k'.path.val ≠ u.path.val := by
        intro k' hk' e; have := List.find?_eq_none.1 ha' k' hk'; simp [e] at this
      refine ⟨?_, ?_, ?_, ?_, ?_⟩
      · rw [List.map_append, List.nodup_append]
        refine ⟨ha.ids, by simp, ?_⟩
        simp only [List.mem_map, List.mem_singleton]
        rintro _ ⟨k, hk, rfl⟩ _ ⟨_, rfl, rfl⟩ e
        have := (ha.bounds k hk).2; rw [e, hinc] at this; omega
      · rw [List.map_append, List.nodup_append]
        refine ⟨ha.labels, by simp, ?_⟩
        simp only [List.mem_map, List.mem_singleton]
        rintro _ ⟨k, hk, rfl⟩ _ ⟨_, rfl, rfl⟩ e
        exact han k hk e
      · intro k hk
        rcases List.mem_append.1 hk with hk | hk
        · have := ha.bounds k hk; rw [hinc]; omega
        · simp at hk; subst hk; simp only [hinc]; have := ha.le; omega
      · intro k hk k' hk'
        rcases List.mem_append.1 hk with hk | hk
        · exact ha.fresh k hk k' hk'
        · simp at hk; subst hk; exact hkn k' hk'
      · rw [hinc]; have := ha.le; omega
    · split at h
      · split at h <;> simp at h
      · exact ih _ _ _ _ ha h

theorem inv_propose {s : St} (hi : Inv s) (hcr : s.repo.created = true) {u : U64} {h : alloc.vec.Vec U8}
    {deps : List U64} {uses : List PathUse} {body : alloc.vec.Vec U8} {ws r}
    (hp : Model.propose u s h deps uses body = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.propose at hp
  split at hp
  · simp at hp
  split at hp
  · simp at hp
  rename_i hdeps
  split at hp
  · simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hp
    obtain ⟨rfl, -⟩ := hp
    simpa [applyAll] using hi
  rename_i hnohash
  split at hp
  · simp at hp
  rename_i added paths hreg
  split at hp
  · simp at hp
  rename_i hm1
  split at hp
  · simp at hp
  rename_i hm2
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hp
  obtain ⟨rfl, -⟩ := hp
  have hid := Refine.inc_val hm1
  have hinc := Refine.inc_val hm2
  set id := Model.inc s.repo.patches
  have ha := registerPaths_added s.paths s.repo.paths uses [] s.repo.paths added paths
    ⟨by simp, by simp, by simp, by simp, le_refl _⟩ hreg
  have hfresh : ∀ p ∈ s.patches, p.id ≠ id := by
    intro p hp e; have := hi.patch_fresh p hp; rw [e, hid] at this; omega
  have hhash : ∀ p ∈ s.patches, p.hash.val ≠ h.val := by
    intro p hp e
    have : (s.patches.find? (fun p => decide (p.hash.val = h.val))).map (·.id) = none := by simpa using hnohash
    simp only [Option.map_eq_none_iff] at this
    have := List.find?_eq_none.1 this p hp; simp [e] at this
  have hdk : ∀ d ∈ deps, ∃ p ∈ s.patches, p.id = d := by
    intro d hd
    have hall : (deps.all fun i => s.patches.any fun p => decide (p.id = i)) = true := by simpa using hdeps
    simp only [List.all_eq_true, List.any_eq_true, decide_eq_true_eq] at hall
    exact hall d hd
  have hpatches : upsert (·.id) ⟨id, h, u⟩ s.patches = s.patches ++ [⟨id, h, u⟩] :=
    upsert_fresh _ _ _ (fun y hy => hfresh y hy)
  have hpaths : puts (·.id) added s.paths = s.paths ++ added :=
    puts_fresh _ _ _ ha.ids (fun x hx y hy e => by
      have h1 := (ha.bounds x hx).1; have h2 := hi.path_fresh y hy; rw [e] at h2; omega)
  rw [applyAll_eq]
  simp only [List.filterMap_cons, List.filterMap_nil, List.filterMap_append, List.filterMap_map, repoOf, patchOf,
    depOf, approvalOf, checkOf, channelOf, memberOf, pathOf, eventOf, bodyOf, listingOf, puts_nil, puts_single,
    Function.comp_def, filterMap_none, filterMap_some_id, filterMap_some_eq_map, List.nil_append, List.append_nil,
    List.cons_append, List.getLast?_singleton, Option.getD_some, hpatches, hpaths]
  set D := puts (fun y => (y.patch, y.dependency)) (deps.map (fun d => ({ patch := id, dependency := d } : Dependency))) s.deps
  have hD : ∀ d ∈ D, d ∈ s.deps ∨ (d.patch = id ∧ d.dependency ∈ deps) := by
    intro d hd
    rcases mem_puts _ hd with hd | hd
    · exact .inl hd
    · simp only [List.mem_map] at hd; obtain ⟨x, hx, rfl⟩ := hd; exact .inr ⟨rfl, hx⟩
  have hmemb : ∀ m ∈ s.members, m.patch ≠ id := by
    intro m hm e
    obtain ⟨p, hp, hpid⟩ := hi.members_known m hm
    exact hfresh p hp (hpid.trans e)
  constructor
  all_goals simp only [hasPatch, isMember, List.mem_append, List.mem_singleton]
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.patch_keys, by simp, ?_⟩
    simp only [List.mem_map, List.mem_singleton]
    rintro _ ⟨p, hp, rfl⟩ _ ⟨_, rfl, rfl⟩; exact hfresh p hp
  · rintro p (hp | rfl)
    · have := hi.patch_fresh p hp; rw [hid]; omega
    · exact le_refl _
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.hashes, by simp, ?_⟩
    simp only [List.mem_map, List.mem_singleton]
    rintro _ ⟨p, hp, rfl⟩ _ ⟨_, rfl, rfl⟩; exact hhash p hp
  · intro d hd
    rcases hD d hd with hd | ⟨hdp, hdd⟩
    · obtain ⟨⟨p, hp, h1⟩, ⟨q, hq, h2⟩, h3⟩ := hi.deps_known d hd
      exact ⟨⟨p, .inl hp, h1⟩, ⟨q, .inl hq, h2⟩, h3⟩
    · obtain ⟨q, hq, h2⟩ := hdk _ hdd
      refine ⟨⟨_, .inr rfl, hdp.symm⟩, ⟨q, .inl hq, h2⟩, ?_⟩
      have := hi.patch_fresh q hq; rw [hdp, hid, ← h2]; omega
  · intro a ha'
    obtain ⟨p, hp, h1, h2⟩ := hi.approvals_independent a ha'
    exact ⟨p, .inl hp, h1, h2⟩
  · intro c hc
    obtain ⟨p, hp, h1, h2⟩ := hi.checks_independent c hc
    exact ⟨p, .inl hp, h1, h2⟩
  · exact hi.channel_keys
  · exact hi.channel_fresh
  · exact hi.channel_labels
  · exact hi.member_keys
  · exact hi.members_placed
  · intro m hm
    obtain ⟨p, hp, h1⟩ := hi.members_known m hm
    exact ⟨p, .inl hp, h1⟩
  · intro m hm d hd hdp
    rcases hD d hd with hd | ⟨hdp', -⟩
    · exact hi.closed m hm d hd hdp
    · exact absurd (hdp.symm.trans hdp') (hmemb m hm)
  · exact hi.reviewed
  · intro e he
    rcases mem_upsert_of he with rfl | he
    · exact le_refl _
    · have := hi.event_fresh e he; rw [hinc]; omega
  · intro b hb
    rcases mem_upsert_of hb with rfl | hb
    · exact ⟨_, .inr rfl, rfl⟩
    · obtain ⟨p, hp, h1⟩ := hi.bodies_known b hb
      exact ⟨p, .inl hp, h1⟩
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.path_keys, ha.ids, ?_⟩
    rintro _ hx _ hy e
    simp only [List.mem_map] at hx hy
    obtain ⟨k, hk, rfl⟩ := hx; obtain ⟨k', hk', rfl⟩ := hy
    have h1 := (ha.bounds k' hk').1; have h2 := hi.path_fresh k hk; rw [e] at h2; omega
  · rintro k (hk | hk)
    · have := hi.path_fresh k hk; have := ha.le; omega
    · exact (ha.bounds k hk).2
  · rw [List.map_append, List.nodup_append]
    refine ⟨hi.path_labels, ha.labels, ?_⟩
    rintro _ hx _ hy e
    simp only [List.mem_map] at hx hy
    obtain ⟨k, hk, rfl⟩ := hx; obtain ⟨k', hk', rfl⟩ := hy
    exact ha.fresh k' hk' k hk e
  · intro hc; simp [hcr] at hc

/-! ## Merge -/

/-- The batch state while no admission has failed. -/
structure FI (s : St) (c : Channel) (st : Model.Adm) : Prop where
  size_le : c.size.val ≤ st.size.val
  seq_le : s.repo.events.val ≤ st.seq.val
  kinds : ∀ w ∈ st.writes, (∃ m, w = .PutMember m) ∨ (∃ e, w = .PutEvent e)
  members : ∀ m ∈ st.writes.filterMap memberOf, m.channel = c.id ∧ c.size.val ≤ m.position.val ∧
    m.position.val < st.size.val ∧ Model.approvedB s m.patch = true ∧ hasPatch s m.patch ∧
    ∀ d ∈ Model.depsOf s m.patch, d ∈ st.members
  positions : ((st.writes.filterMap memberOf).map (·.position)).Nodup
  events : ∀ e ∈ st.writes.filterMap eventOf, s.repo.events.val < e.seq.val ∧ e.seq.val ≤ st.seq.val
  mem : ∀ x, x ∈ st.members ↔ x ∈ (s.members.filter (fun m => decide (m.channel = c.id))).map (·.patch) ∨
    ∃ m ∈ st.writes.filterMap memberOf, m.patch = x

theorem admitK_ok' {ch deps : List U64} {a c : Bool} {id : U64} {next}
    (h : Model.admitK ch deps a c id = .Ok next) :
    a = true ∧ c = true ∧ (∀ d ∈ deps, d ∈ ch) ∧ next = if id ∈ ch then ch else ch ++ [id] := by
  unfold Model.admitK at h
  split_ifs at h with h1 h2 h3 h4 <;> simp only [core.result.Result.Ok.injEq, reduceCtorEq] at h <;>
    subst h <;> exact ⟨by simpa using h1, by simpa using h2, by simpa using h3, by simp [h4]⟩

theorem admitOne_FI {s : St} {c : Channel} {u id : U64} {st : Model.Adm} (hf : FI s c st)
    (h : (Model.admitOne u s c.id id st).1 = none) : FI s c (Model.admitOne u s c.id id st).2 := by
  unfold Model.admitOne at h ⊢
  cases hp : s.patches.find? (fun p => decide (p.id = id)) with
  | none => simp [hp] at h
  | some p =>
  simp only [hp] at h ⊢
  cases hk : Model.admitK st.members (Model.depsOf s id) (Model.approvedB s id) (Model.passedB s id) id with
  | Err e => simp [hk] at h
  | Ok next =>
  simp only [hk] at h ⊢
  obtain ⟨ha, hc, hdeps, rfl⟩ := admitK_ok' hk
  have hpat : hasPatch s id := ⟨p, List.mem_of_find?_eq_some hp, by simpa using List.find?_some hp⟩
  by_cases hin : id ∈ st.members
  · simp only [hin, ↓reduceIte] at h ⊢
    by_cases hm : st.seq = Model.MAX
    · simp [hm] at h
    simp only [hm, ↓reduceIte] at h ⊢
    have hinc := Refine.inc_val hm
    refine ⟨hf.size_le, by have := hf.seq_le; simp only; omega, ?_, ?_, ?_, ?_, ?_⟩
    · intro w hw; simp only [List.mem_append, List.mem_singleton] at hw
      rcases hw with hw | rfl
      · exact hf.kinds w hw
      · exact .inr ⟨_, rfl⟩
    · simpa [List.filterMap_append, memberOf] using hf.members
    · simpa [List.filterMap_append, memberOf] using hf.positions
    · intro e he
      simp only [List.filterMap_append, List.filterMap_cons, List.filterMap_nil, eventOf, List.mem_append,
        List.mem_singleton] at he
      rcases he with he | rfl
      · have := hf.events e he; simp only; omega
      · simp only [hinc]; have := hf.seq_le; omega
    · simpa [List.filterMap_append, memberOf] using hf.mem
  · simp only [hin, ↓reduceIte] at h ⊢
    by_cases hm1 : st.size = Model.MAX
    · simp [hm1] at h
    by_cases hm2 : st.admitted = Model.MAX
    · simp [hm1, hm2] at h
    by_cases hm3 : st.seq = Model.MAX
    · simp [hm1, hm2, hm3] at h
    simp only [hm1, hm2, hm3, ↓reduceIte] at h ⊢
    have hs := Refine.inc_val hm1
    have hq := Refine.inc_val hm3
    have hmw : ((st.writes ++ [Write.PutMember ⟨c.id, st.size, id⟩]) ++
        [Write.PutEvent ⟨Model.inc st.seq, u, MERGE, some id, some c.id⟩]).filterMap memberOf =
        st.writes.filterMap memberOf ++ [⟨c.id, st.size, id⟩] := by
      simp [List.filterMap_append, memberOf]
    have hev : ((st.writes ++ [Write.PutMember ⟨c.id, st.size, id⟩]) ++
        [Write.PutEvent ⟨Model.inc st.seq, u, MERGE, some id, some c.id⟩]).filterMap eventOf =
        st.writes.filterMap eventOf ++ [⟨Model.inc st.seq, u, MERGE, some id, some c.id⟩] := by
      simp [List.filterMap_append, eventOf]
    refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
    · simp only [hs]; have := hf.size_le; omega
    · simp only [hq]; have := hf.seq_le; omega
    · intro w hw; simp only [List.mem_append, List.mem_singleton] at hw
      rcases hw with (hw | rfl) | rfl
      · exact hf.kinds w hw
      · exact .inl ⟨_, rfl⟩
      · exact .inr ⟨_, rfl⟩
    · rw [hmw]
      intro m hm
      simp only [List.mem_append, List.mem_singleton] at hm
      rcases hm with hm | rfl
      · obtain ⟨h1, h2, h3, h4, h5, h6⟩ := hf.members m hm
        refine ⟨h1, h2, by simp only [hs]; omega, h4, h5, fun d hd => List.mem_append_left _ (h6 d hd)⟩
      · refine ⟨rfl, hf.size_le, by simp only [hs]; omega, ha, hpat, fun d hd => List.mem_append_left _ (hdeps d hd)⟩
    · rw [hmw, List.map_append, List.nodup_append]
      refine ⟨hf.positions, by simp, ?_⟩
      simp only [List.mem_map, List.mem_singleton]
      rintro _ ⟨m, hm, rfl⟩ _ ⟨_, rfl, rfl⟩ e
      have := (hf.members m hm).2.2.1; simp only at e; rw [e] at this; omega
    · rw [hev]
      intro e he
      simp only [List.mem_append, List.mem_singleton] at he
      rcases he with he | rfl
      · have := hf.events e he; simp only [hq]; omega
      · simp only [hq]; have := hf.seq_le; omega
    · rw [hmw]
      intro x
      simp only [List.mem_append, List.mem_singleton, hf.mem]
      constructor
      · rintro ((h0 | ⟨m, hm, hmx⟩) | rfl)
        · exact .inl h0
        · exact .inr ⟨m, .inl hm, hmx⟩
        · exact .inr ⟨_, .inr rfl, rfl⟩
      · rintro (h0 | ⟨m, hm | rfl, hmx⟩)
        · exact .inl (.inl h0)
        · exact .inl (.inr ⟨m, hm, hmx⟩)
        · exact .inr hmx.symm

theorem fold_FI {s : St} {c : Channel} {u : U64} (ids : List U64) (x : Model.Adm × Option Error)
    (hx : x.2 = none → FI s c x.1) :
    (ids.foldl (Model.admitStep u s c.id) x).2 = none → FI s c (ids.foldl (Model.admitStep u s c.id) x).1 := by
  induction ids generalizing x with
  | nil => exact hx
  | cons id ids ih =>
    apply ih
    unfold Model.admitStep
    cases h2 : x.2 with
    | none =>
      simp only
      intro hf
      exact admitOne_FI (hx h2) hf
    | some e => simp

theorem filterMap_nil_of {α β} (f : α → Option β) (l : List α) (h : ∀ a ∈ l, f a = none) : l.filterMap f = [] :=
  List.filterMap_eq_nil_iff.2 h

theorem inv_merge {s : St} (hi : Inv s) {u : U64} {n : alloc.vec.Vec U8} {ids : List U64} {ws r}
    (hm : Model.merge u s n ids = .Ok (ws, r)) : Inv (applyAll s ws) := by
  unfold Model.merge at hm
  split at hm
  · simp at hm
  split at hm
  · simp at hm
  rename_i c hc
  dsimp only at hm
  split at hm
  · rename_i st hfold
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hm
    obtain ⟨rfl, -⟩ := hm
    set M0 := (s.members.filter (fun m => decide (m.channel = c.id))).map (·.patch)
    have hf : FI s c st := by
      have := fold_FI (u := u) ids (⟨M0, c.size, s.repo.events, 0#u64, []⟩, none) (fun _ => ⟨le_refl _, le_refl _,
        by simp, by simp, by simp, by simp, by intro x; simp [M0]⟩)
      rw [hfold] at this; exact this rfl
    have hcm : c ∈ s.channels := List.mem_of_find?_eq_some hc
    have hsrc : ∀ m ∈ s.members, m.channel = c.id → m.position.val < c.size.val := by
      intro m hm e
      obtain ⟨c', hc', hid', hpos⟩ := hi.members_placed m hm
      have : c' = c := List.inj_on_of_nodup_map hi.channel_keys hc' hcm (by rw [hid', e])
      subst this; exact hpos
    have hnone : ∀ {β : Type} (f : Write → Option β), (∀ m, f (.PutMember m) = none) → (∀ e, f (.PutEvent e) = none) →
        st.writes.filterMap f = [] := by
      intro β f h1 h2
      apply filterMap_nil_of; intro w hw
      rcases hf.kinds w hw with ⟨m, rfl⟩ | ⟨e, rfl⟩ <;> simp [h1, h2]
    have hwr : ∀ w ∈ st.writes, (∃ m, w = .PutMember m) ∨ (∃ e, w = .PutEvent e) := hf.kinds
    set W := st.writes.filterMap memberOf
    have hWkeys : (W.map (fun m => (m.channel, m.position))).Nodup := by
      have hp := hf.positions
      have : W.map (·.position) = (W.map (fun m => (m.channel, m.position))).map Prod.snd := by simp
      rw [this] at hp; exact hp.of_map _
    set M := puts (fun y => (y.channel, y.position)) W s.members
    have hkeep : ∀ m ∈ s.members, m ∈ M := by
      intro m hm
      apply puts_keep _ hm
      intro x hx e
      obtain ⟨h1, h2, -⟩ := hf.members x hx
      simp only [Prod.mk.injEq] at e
      have := hsrc m hm (e.1.symm.trans h1); rw [← e.2] at this; omega
    have hnew : ∀ x ∈ W, x ∈ M := fun x hx => puts_new _ hx hWkeys
    set cc : Channel := { c with size := st.size }
    set rr : Repository := { s.repo with events := st.seq }
    have tail : ∀ {β : Type} (f : Write → Option β), (∀ m, f (.PutMember m) = none) → (∀ e, f (.PutEvent e) = none) →
        f (.PutChannel cc) = none → f (.SetRepository rr) = none →
        (st.writes ++ [Write.PutChannel cc, Write.SetRepository rr]).filterMap f = [] := by
      intro β f h1 h2 h3 h4
      rw [List.filterMap_append, hnone f h1 h2]; simp [h3, h4]
    rw [applyAll_eq, tail patchOf (fun _ => rfl) (fun _ => rfl) rfl rfl, tail depOf (fun _ => rfl) (fun _ => rfl) rfl rfl,
      tail approvalOf (fun _ => rfl) (fun _ => rfl) rfl rfl, tail checkOf (fun _ => rfl) (fun _ => rfl) rfl rfl,
      tail pathOf (fun _ => rfl) (fun _ => rfl) rfl rfl, tail bodyOf (fun _ => rfl) (fun _ => rfl) rfl rfl,
      tail listingOf (fun _ => rfl) (fun _ => rfl) rfl rfl]
    have ech : (st.writes ++ [Write.PutChannel cc, Write.SetRepository rr]).filterMap channelOf = [cc] := by
      rw [List.filterMap_append, hnone channelOf (fun _ => rfl) (fun _ => rfl)]; rfl
    have emb : (st.writes ++ [Write.PutChannel cc, Write.SetRepository rr]).filterMap memberOf = W := by
      rw [List.filterMap_append]; simp [memberOf, W]
    have eev : (st.writes ++ [Write.PutChannel cc, Write.SetRepository rr]).filterMap eventOf =
        st.writes.filterMap eventOf := by
      rw [List.filterMap_append]; simp [eventOf]
    have erp : ((st.writes ++ [Write.PutChannel cc, Write.SetRepository rr]).filterMap repoOf).getLast?.getD s.repo = rr := by
      rw [List.filterMap_append, hnone repoOf (fun _ => rfl) (fun _ => rfl)]; rfl
    rw [ech, emb, eev, erp]
    simp only [puts_nil, puts_single]
    trace_state
    sorry
  · simp at hm

end o5s_registry.Invariant
