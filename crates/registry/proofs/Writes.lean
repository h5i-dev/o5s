import Spec
/-!
# What a write list does to each table

`applyAll` of a write list puts, into each table, the rows of that table the
list writes, in order; the repository row becomes the last one written.
The lemmas about `upsert` folds then answer membership questions.
-/
open Aeneas Aeneas.Std o5s_registry o5s_registry.Spec H5iAppLib

namespace o5s_registry.Writes

def patchOf : Write → Option PatchRow | .PutPatch x => some x | _ => none
def depOf : Write → Option Dependency | .PutDependency x => some x | _ => none
def approvalOf : Write → Option Approval | .PutApproval x => some x | _ => none
def checkOf : Write → Option CheckReport | .PutCheck x => some x | _ => none
def channelOf : Write → Option Channel | .PutChannel x => some x | _ => none
def memberOf : Write → Option Member | .PutMember x => some x | _ => none
def pathOf : Write → Option PathKind | .PutPath x => some x | _ => none
def eventOf : Write → Option Event | .PutEvent x => some x | _ => none
def bodyOf : Write → Option Body | .PutBody x => some x | _ => none
def listingOf : Write → Option Listing | .PutListing x => some x | _ => none
def repoOf : Write → Option Repository | .SetRepository x => some x | _ => none

/-- Fold puts of one table. -/
def puts {α κ} [DecidableEq κ] (k : α → κ) (xs : List α) (l : List α) : List α := xs.foldl (fun l x => upsert k x l) l

section
variable (s : St) (ws : List Write)

theorem applyAll_patches : (applyAll s ws).patches = puts (·.id) (ws.filterMap patchOf) s.patches := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, patchOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_deps : (applyAll s ws).deps = puts (fun y => (y.patch, y.dependency)) (ws.filterMap depOf) s.deps := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, depOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_approvals :
    (applyAll s ws).approvals = puts (fun y => (y.patch, y.reviewer)) (ws.filterMap approvalOf) s.approvals := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, approvalOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_checks : (applyAll s ws).checks = puts (·.patch) (ws.filterMap checkOf) s.checks := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, checkOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_channels : (applyAll s ws).channels = puts (·.id) (ws.filterMap channelOf) s.channels := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, channelOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_members :
    (applyAll s ws).members = puts (fun y => (y.channel, y.position)) (ws.filterMap memberOf) s.members := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, memberOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_paths : (applyAll s ws).paths = puts (·.id) (ws.filterMap pathOf) s.paths := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, pathOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_events : (applyAll s ws).events = puts (·.seq) (ws.filterMap eventOf) s.events := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, eventOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_bodies : (applyAll s ws).bodies = puts (·.patch) (ws.filterMap bodyOf) s.bodies := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, bodyOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_listings : (applyAll s ws).listings = puts (·.repo) (ws.filterMap listingOf) s.listings := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih => cases w <;> simp [applyAll, applyWrite, listingOf, puts] at ih ⊢ <;> exact ih _

theorem applyAll_repo : (applyAll s ws).repo = ((ws.filterMap repoOf).getLast?).getD s.repo := by
  induction ws generalizing s with
  | nil => rfl
  | cons w ws ih =>
    cases w <;> simp only [applyAll, List.foldl_cons, applyWrite, List.filterMap_cons, repoOf] at ih ⊢ <;>
      rw [ih] <;> try rfl
    cases h : (ws.filterMap repoOf).getLast? <;> simp [List.getLast?_cons, h]

end

/-! ## Folds of `upsert` -/

section
variable {α κ : Type} [DecidableEq κ] (k : α → κ)

theorem mem_puts {xs l : List α} {z : α} (h : z ∈ puts k xs l) : z ∈ l ∨ z ∈ xs := by
  induction xs generalizing l with
  | nil => exact .inl h
  | cons x xs ih =>
    rcases ih h with h | h
    · rcases mem_upsert_of h with rfl | h
      · exact .inr (by simp)
      · exact .inl h
    · exact .inr (by simp [h])

theorem puts_keep {xs l : List α} {z : α} (hz : z ∈ l) (hk : ∀ x ∈ xs, k x ≠ k z) : z ∈ puts k xs l := by
  induction xs generalizing l with
  | nil => exact hz
  | cons x xs ih =>
    exact ih (mem_upsert_of_ne hz (fun e => hk x (by simp) e.symm)) (fun x' hx' => hk x' (by simp [hx']))

theorem puts_nodup {xs l : List α} (h : (l.map k).Nodup) : ((puts k xs l).map k).Nodup := by
  induction xs generalizing l with
  | nil => exact h
  | cons x xs ih => exact ih (nodup_upsert k x h)

theorem puts_new {xs l : List α} {x : α} (hx : x ∈ xs) (hn : (xs.map k).Nodup) : x ∈ puts k xs l := by
  induction xs generalizing l with
  | nil => simp at hx
  | cons y ys ih =>
    simp only [List.map_cons, List.nodup_cons, List.mem_map, not_exists, not_and] at hn
    show x ∈ puts k ys (upsert k y l)
    rcases List.mem_cons.1 hx with rfl | hx
    · exact puts_keep k (mem_upsert_self k _ l) (fun x' hx' e => hn.1 x' hx' e)
    · exact ih hx hn.2

/-- Every row of the result has a key from the old rows or the puts. -/
theorem key_of_mem_puts {xs l : List α} {z : α} (h : z ∈ puts k xs l) : z ∈ l ∨ z ∈ xs := mem_puts k h

end

end o5s_registry.Writes
