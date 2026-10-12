import Invariant
/-!
# Loading less than a tenant

The PostgreSQL store loads, for each command, only the rows that match the
keys the command names (`load` in `crates/pgwire/src/service.rs`). `Narrow`
states what those loads must agree on with the whole tenant: for each lookup
the command makes, the matching rows in the same relative order. Under it the
model gives the same result, so a run of `transition` on a narrow load
computes what the whole tenant specifies (`narrow_load`).
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec H5iAppLib

namespace o5s_registry.Narrow

/-- The rows of `l₁` and `l₂` that satisfy `P` are the same, in the same order. -/
def Agree {α} (P : α → Bool) (l₁ l₂ : List α) : Prop := l₁.filter P = l₂.filter P

theorem find?_agree {α} {P : α → Bool} {l₁ l₂ : List α} (h : Agree P l₁ l₂) : l₁.find? P = l₂.find? P := by
  rw [← List.head?_filter, ← List.head?_filter, h]

theorem any_agree {α} {P : α → Bool} {l₁ l₂ : List α} (h : Agree P l₁ l₂) : l₁.any P = l₂.any P := by
  have e : ∀ l : List α, l.any P = (l.filter P).any (fun _ => true) := by intro l; simp
  rw [e, e l₂, h]

theorem filter_agree {α} {P : α → Bool} {l₁ l₂ : List α} (h : Agree P l₁ l₂) : l₁.filter P = l₂.filter P := h

/-- Repository commands check the caller's role, so they read the role table. -/
def usesRoles : Command → Bool
  | .Register .. | .Create .. => false
  | _ => true

/-- What a narrow load `part` must share with the whole tenant `full` for command `c`. -/
def Narrow (c : Command) (full part : St) : Prop :=
  part.repo = full.repo ∧ (usesRoles c = true → part.roles = full.roles) ∧
  match c with
  | .Register n repo => Agree (fun l : Listing => decide (l.label.val = n.val)) part.listings full.listings ∧
      Agree (fun l : Listing => decide (l.repo = repo)) part.listings full.listings
  | .Fork _ src n => Agree (fun c : Channel => decide (c.label.val = n.val)) part.channels full.channels ∧
      Agree (fun c : Channel => decide (c.label.val = src.val)) part.channels full.channels ∧
      ∀ c ∈ full.channels, c.label.val = src.val →
        Agree (fun m : Member => decide (m.channel = c.id)) part.members full.members
  | .Propose _ h deps uses _ => Agree (fun p : PatchRow => decide (p.hash.val = h.val)) part.patches full.patches ∧
      (∀ d ∈ deps.val, Agree (fun p : PatchRow => decide (p.id = d)) part.patches full.patches) ∧
      ∀ u ∈ uses.val, Agree (fun k : PathKind => decide (k.path.val = u.path.val)) part.paths full.paths
  | .Approve _ p => Agree (fun q : PatchRow => decide (q.id = p)) part.patches full.patches
  | .Check _ p _ => Agree (fun q : PatchRow => decide (q.id = p)) part.patches full.patches
  | .Merge _ n ids => Agree (fun c : Channel => decide (c.label.val = n.val)) part.channels full.channels ∧
      (∀ c ∈ full.channels, c.label.val = n.val →
        Agree (fun m : Member => decide (m.channel = c.id)) part.members full.members) ∧
      ∀ id ∈ ids.val, Agree (fun q : PatchRow => decide (q.id = id)) part.patches full.patches ∧
        Agree (fun d : Dependency => decide (d.patch = id)) part.deps full.deps ∧
        Agree (fun a : Approval => decide (a.patch = id)) part.approvals full.approvals ∧
        Agree (fun x : CheckReport => decide (x.patch = id)) part.checks full.checks
  | _ => True

theorem registerPaths_agree (k₁ k₂ : List PathKind) :
    ∀ (uses : List PathUse) (acc : List PathKind) (nx : U64),
      (∀ u ∈ uses, Agree (fun k : PathKind => decide (k.path.val = u.path.val)) k₁ k₂) →
      Model.registerPaths k₁ uses acc nx = Model.registerPaths k₂ uses acc nx := by
  intro uses
  induction uses with
  | nil => intros; rfl
  | cons u us ih =>
    intro acc nx h
    simp only [Model.registerPaths]
    rw [find?_agree (h u (by simp))]
    split <;> (try split) <;> (try split) <;> (try split) <;> first | rfl | exact ih _ _ (fun u' hu' => h u' (by simp [hu']))

theorem all_congr' {α} {f g : α → Bool} (l : List α) (h : ∀ x ∈ l, f x = g x) : l.all f = l.all g := by
  induction l with
  | nil => rfl
  | cons x xs ih => simp only [List.all_cons, h x (by simp), ih (fun y hy => h y (by simp [hy]))]

theorem admitOne_agree {part full : St} {id : U64}
    (h1 : Agree (fun q : PatchRow => decide (q.id = id)) part.patches full.patches)
    (h2 : Agree (fun d : Dependency => decide (d.patch = id)) part.deps full.deps)
    (h3 : Agree (fun a : Approval => decide (a.patch = id)) part.approvals full.approvals)
    (h4 : Agree (fun x : CheckReport => decide (x.patch = id)) part.checks full.checks) (u ch : U64) (st : Model.Adm) :
    Model.admitOne u part ch id st = Model.admitOne u full ch id st := by
  unfold Model.admitOne Model.depsOf Model.approvedB Model.passedB
  rw [find?_agree h1, filter_agree h2, any_agree h3, find?_agree h4]

theorem fold_agree {β} (f g : β → U64 → β) (ids : List U64) (x : β) (h : ∀ id ∈ ids, ∀ y, f y id = g y id) :
    ids.foldl f x = ids.foldl g x := by
  induction ids generalizing x with
  | nil => rfl
  | cons id ids ih => simp only [List.foldl_cons, h id (by simp)]; exact ih _ (fun i hi => h i (by simp [hi]))

theorem fork_agree {full part : St} (hr : part.repo = full.repo) {u : U64} {src n : alloc.vec.Vec U8}
    (h1 : Agree (fun c : Channel => decide (c.label.val = n.val)) part.channels full.channels)
    (h2 : Agree (fun c : Channel => decide (c.label.val = src.val)) part.channels full.channels)
    (h3 : ∀ c ∈ full.channels, c.label.val = src.val →
      Agree (fun m : Member => decide (m.channel = c.id)) part.members full.members) :
    Model.fork u part src n = Model.fork u full src n := by
  unfold Model.fork Model.findChannel
  rw [find?_agree h1, find?_agree h2, hr]
  cases hc : List.find? (fun c : Channel => decide (c.label.val = src.val)) full.channels with
  | none => rfl
  | some c' =>
    have hcm := List.mem_of_find?_eq_some hc
    have hl : c'.label.val = src.val := by simpa using List.find?_some hc
    simp only
    rw [filter_agree (h3 c' hcm hl)]

theorem propose_agree {full part : St} (hr : part.repo = full.repo) {u : U64} {h : alloc.vec.Vec U8}
    {deps : List U64} {uses : List PathUse} {body : alloc.vec.Vec U8}
    (h1 : Agree (fun p : PatchRow => decide (p.hash.val = h.val)) part.patches full.patches)
    (h2 : ∀ d ∈ deps, Agree (fun p : PatchRow => decide (p.id = d)) part.patches full.patches)
    (h3 : ∀ u ∈ uses, Agree (fun k : PathKind => decide (k.path.val = u.path.val)) part.paths full.paths) :
    Model.propose u part h deps uses body = Model.propose u full h deps uses body := by
  unfold Model.propose
  rw [find?_agree h1, all_congr' _ (fun d hd => any_agree (h2 d hd)), hr,
    registerPaths_agree part.paths full.paths uses [] full.repo.paths h3]

theorem merge_agree {full part : St} (hr : part.repo = full.repo) {u : U64} {n : alloc.vec.Vec U8} {ids : List U64}
    (h1 : Agree (fun c : Channel => decide (c.label.val = n.val)) part.channels full.channels)
    (h2 : ∀ c ∈ full.channels, c.label.val = n.val →
      Agree (fun m : Member => decide (m.channel = c.id)) part.members full.members)
    (h3 : ∀ id ∈ ids, Agree (fun q : PatchRow => decide (q.id = id)) part.patches full.patches ∧
        Agree (fun d : Dependency => decide (d.patch = id)) part.deps full.deps ∧
        Agree (fun a : Approval => decide (a.patch = id)) part.approvals full.approvals ∧
        Agree (fun x : CheckReport => decide (x.patch = id)) part.checks full.checks) :
    Model.merge u part n ids = Model.merge u full n ids := by
  unfold Model.merge Model.findChannel
  rw [find?_agree h1, hr]
  cases hc : List.find? (fun c : Channel => decide (c.label.val = n.val)) full.channels with
  | none => rfl
  | some c' =>
    have hcm := List.mem_of_find?_eq_some hc
    have hl : c'.label.val = n.val := by simpa using List.find?_some hc
    simp only
    rw [filter_agree (h2 c' hcm hl)]
    rw [fold_agree (Model.admitStep u part c'.id) (Model.admitStep u full c'.id) ids _ (by
      intro id hid y
      obtain ⟨g1, g2, g3, g4⟩ := h3 id hid
      unfold Model.admitStep
      rw [admitOne_agree g1 g2 g3 g4])]

/-- On a narrow load the model computes what it computes on the whole tenant. -/
theorem narrow_model (a : Principal) (full part : St) (c : Command) (hn : Narrow c full part) :
    Model.step a part c = Model.step a full c := by
  obtain ⟨hr, hroles, hn⟩ := hn
  have hg : ∀ n need (k k' : Model.Out), part.roles = full.roles → k = k' →
      Model.guarded a part n need k = Model.guarded a full n need k' := by
    intro n need k k' hro e
    subst e
    unfold Model.guarded Model.authorize Model.readable
    rw [hr, hro]
  cases c <;> simp only [Model.step, usesRoles] at hn hroles ⊢
  case Register => unfold Model.register; rw [find?_agree hn.1, any_agree hn.2]
  case Create => rw [hr]
  case Fork => exact hg _ _ _ _ (hroles trivial) (fork_agree hr hn.1 hn.2.1 hn.2.2)
  case Propose => exact hg _ _ _ _ (hroles trivial) (propose_agree hr hn.1 hn.2.1 hn.2.2)
  case Approve => exact hg _ _ _ _ (hroles trivial) (by unfold Model.approve; rw [find?_agree hn, hr])
  case Check => exact hg _ _ _ _ (hroles trivial) (by unfold Model.check; rw [find?_agree hn, hr])
  case Merge => exact hg _ _ _ _ (hroles trivial) (merge_agree hr hn.1 hn.2.1 hn.2.2)
  case Grant => exact hg _ _ _ _ (hroles trivial) (by simp only [Model.grant, hr, hroles trivial])

/-- **Narrow loads.** A run of the extracted `transition` on rows that agree
with the whole tenant on every lookup the command makes computes what the
whole tenant specifies. -/
theorem narrow_load (a : Principal) (s p : Snapshot) (c : Command)
    (hn : Narrow c (Snapshot.toSt s) (Snapshot.toSt p)) (r) (h : transition a p c = ok r) :
    Refine.lift r = Model.step a (Snapshot.toSt s) c := by
  rw [Refine.transition_refines _ _ _ _ h]
  exact narrow_model a _ _ c hn

end o5s_registry.Narrow
