import Helpers
/-!
# What each command writes

One lemma per kernel command: if the extracted function returns `Ok`, its
write set is exactly the listed writes, and the facts the command checked
hold of the rows it read. `Theorems.lean` uses only these lemmas.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema o5s_registry.Helpers H5iAppLib

namespace o5s_registry.Commands

attribute [simp] H5iAppLib.u64_val_eq

/-- The repository row with one more event, as `bump_events` builds it. -/
def bumped (r : Repository) (events : U64) : Repository := { r with events }

/-- A counter that is not `u64::MAX` can be incremented. -/
theorem lt_max {x : U64} (h : ¬x = core.num.U64.MAX) : x.val < 2 ^ 64 - 1 := by
  have hm := H5iAppLib.u64_max_val
  have : x.val ≠ core.num.U64.MAX.val := fun e => h (by scalar_tac)
  scalar_tac

@[step] theorem bump_events_spec (r : Repository) (h : r.events.val < 2 ^ 64 - 1) :
    bump_events r ⦃ r' => ∃ e : U64, e.val = r.events.val + 1 ∧ r' = bumped r e ⦄ := by
  unfold bump_events bumped
  step*
  exact ⟨i, i_post, by simp [v_post]⟩

theorem register_spec (a : Principal) (ls : alloc.vec.Vec Listing) (n : alloc.vec.Vec U8) (repo : U64) :
    register a ls n repo ⦃ r => ∀ ws rep, r = .Ok (ws, rep) →
      a.scope = a.org ∧ a.org.val < REPOSITORY_SCOPE.val ∧ REPOSITORY_SCOPE.val ≤ repo.val ∧
      (ws.val = [] ∨
        (ws.val = [.PutListing ⟨repo, n⟩] ∧ (∀ l ∈ ls.val, l.label.val ≠ n.val) ∧ ∀ l ∈ ls.val, l.repo ≠ repo)) ⦄ := by
  unfold register
  step*
  intro ws' rep h
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, rfl⟩ := h
  have hnone : ls.val.find? (fun l => decide (l.label.val = n.val)) = none := by simp_all
  have hlabel : ∀ l ∈ ls.val, l.label.val ≠ n.val := by
    intro l hl e
    have := List.find?_eq_none.1 hnone l hl
    simp_all
  refine ⟨by simp_all, by scalar_tac, by scalar_tac, .inr ⟨by simp_all, hlabel, fun l hl e => ?_⟩⟩
  simp_all

/-- The bytes of the label `main`. -/
def mainLabel : List Nat := [109, 97, 105, 110]

theorem create_spec (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) :
    create a r n ⦃ res => ∀ ws rep, res = .Ok (ws, rep) →
      REPOSITORY_SCOPE.val ≤ a.scope.val ∧ r.created = false ∧
      ∃ main : alloc.vec.Vec U8, main.val.map (·.val) = mainLabel ∧
        ws.val = [.SetRepository ⟨a.org, n, true, 0#u64, 1#u64, 0#u64, 1#u64⟩,
          .PutChannel ⟨1#u64, main, 0#u64⟩, .PutEvent ⟨1#u64, a.user, CREATE, none, none⟩] ⦄ := by
  unfold create event
  step*
  all_goals (try (simp_all; scalar_tac))
  intro ws' rep h
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, rfl⟩ := h
  exact ⟨by scalar_tac, main3, by simp [main3_post, main2_post, main1_post, main_post, mainLabel], by simp [ws2_post, ws1_post, ws_post, v_post]⟩

theorem approve_spec (u : U64) (s : Snapshot) (patch : U64) :
    approve u s patch ⦃ res => ∀ ws rep, res = .Ok (ws, rep) →
      ∃ p ∈ s.patches.val, p.id = patch ∧ p.author ≠ u ∧
      ∃ e : U64, e.val = s.repository.events.val + 1 ∧
        ws.val = [.PutApproval ⟨patch, u⟩, .SetRepository (bumped s.repository e), .PutEvent ⟨e, u, APPROVE, some patch, none⟩] ⦄ := by
  unfold approve event
  step*
  all_goals (try (apply lt_max; assumption))
  intro ws' rep h
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, rfl⟩ := h
  subst o_post
  have hp : s.patches.val.find? (fun p => decide (p.id = patch)) = some p := ‹_›
  have hir : i = r := (H5iAppLib.u64_val_eq _ _).1 (by rw [i_post, r_post])
  refine ⟨p, List.mem_of_find?_eq_some hp, by simpa using List.find?_some hp, ‹_›, r, r_post, ?_⟩
  simp [ws2_post, ws1_post, ws_post, x_post, hir]

theorem check_spec (u : U64) (s : Snapshot) (patch : U64) (passed : Bool) :
    check u s patch passed ⦃ res => ∀ ws rep, res = .Ok (ws, rep) →
      ∃ p ∈ s.patches.val, p.id = patch ∧ p.author ≠ u ∧
      ∃ e : U64, e.val = s.repository.events.val + 1 ∧
        ws.val = [.PutCheck ⟨patch, passed, u⟩, .SetRepository (bumped s.repository e),
          .PutEvent ⟨e, u, CHECK, some patch, none⟩] ⦄ := by
  unfold check event
  step*
  all_goals (try (apply lt_max; assumption))
  intro ws' rep h
  simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
  obtain ⟨rfl, rfl⟩ := h
  subst o_post
  have hp : s.patches.val.find? (fun p => decide (p.id = patch)) = some p := ‹_›
  have hir : i = r := (H5iAppLib.u64_val_eq _ _).1 (by rw [i_post, r_post])
  refine ⟨p, List.mem_of_find?_eq_some hp, by simpa using List.find?_some hp, ‹_›, r, r_post, ?_⟩
  simp [ws2_post, ws1_post, ws_post, x_post, hir]

end o5s_registry.Commands
