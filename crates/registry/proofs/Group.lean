import Served
/-!
# Group commit with the repository row in memory

A grouped PostgreSQL transaction (`attempt_group` in `crates/pgwire/src/lib.rs`)
runs several commands of one repository in order. Every command writes the
repository row (its counters), and every command used to read it back. Now
the transaction keeps that row in memory: a later command loads its other
rows from the database, which holds every earlier command's other writes,
takes the repository row from memory, and the row is written once, last.

`group_load` shows the rows a command then runs on are exactly what `load`
returns from the state the earlier commands left, so `pg_served` applies to
each command (`group_served`). `defer_repo` shows that writing every other
row in order and the repository row last stores that same state.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Writes H5iAppLib

namespace o5s_registry.Group
open Served

/-- `load` reads the repository row whole, into its own field: loading from
a state whose repository row is replaced loads that row and the same tables. -/
theorem pgLoad_repo (c : Command) (s : St) (r : Repository) :
    pgLoad c { s with repo := r } = { pgLoad c s with repo := r } := by
  cases c <;> rfl

/-- The database holds the tables the earlier commands left (`s` but for its
repository row); the repository row comes from memory. -/
theorem group_load (c : Command) (s : St) (r : Repository) :
    { pgLoad c { s with repo := r } with repo := s.repo } = pgLoad c s := by
  rw [pgLoad_repo]
  cases c <;> rfl

/-- Each command of a group computes what the whole tenant specifies. -/
theorem group_served (a : Principal) (s p : Snapshot) (c : Command) (stale : Repository)
    (reach : Invariant.Reachable (Snapshot.toSt s)) (deps : DepsAscending (Snapshot.toSt s))
    (loaded : Snapshot.toSt p =
      { pgLoad c { Snapshot.toSt s with repo := stale } with repo := (Snapshot.toSt s).repo })
    (r) (h : transition a p c = ok r) :
    Refine.lift r = Model.step a (Snapshot.toSt s) c := by
  rw [group_load] at loaded
  exact pg_served a s p c reach deps loaded r h

/-- Whether a write is to a table rather than the repository row. -/
def isTable : Write → Bool
  | .SetRepository _ => false
  | _ => true

/-- Writes other than the repository row. -/
def tableWrites (ws : List Write) : List Write := ws.filter isTable

/-- The last repository row a list writes, if any. -/
def lastRepo (ws : List Write) : List Write :=
  ((ws.filterMap repoOf).getLast?).toList.map Write.SetRepository

theorem filterMap_tableWrites {β} (f : Write → Option β) (hf : ∀ r, f (.SetRepository r) = none)
    (ws : List Write) : (tableWrites ws).filterMap f = ws.filterMap f := by
  induction ws with
  | nil => rfl
  | cons w ws ih =>
    unfold tableWrites at ih ⊢
    rw [List.filter_cons]
    cases w <;> simp only [isTable, ↓reduceIte, Bool.false_eq_true, List.filterMap_cons, hf, ih]

theorem filterMap_lastRepo {β} (f : Write → Option β) (hf : ∀ r, f (.SetRepository r) = none)
    (ws : List Write) : (lastRepo ws).filterMap f = [] := by
  unfold lastRepo
  cases (ws.filterMap repoOf).getLast? <;> simp [hf]

theorem repos_tableWrites (ws : List Write) : (tableWrites ws).filterMap repoOf = [] := by
  induction ws with
  | nil => rfl
  | cons w ws ih =>
    unfold tableWrites at ih ⊢
    rw [List.filter_cons]
    cases w <;> simp only [isTable, ↓reduceIte, Bool.false_eq_true] <;>
      first | exact ih | (rw [List.filterMap_cons_none rfl]; exact ih)

/-- Storing every other write in order and the last repository row at the end
gives the state the writes give in order. -/
theorem defer_repo (s : St) (ws : List Write) :
    applyAll s (tableWrites ws ++ lastRepo ws) = applyAll s ws := by
  have t := fun {β} (f : Write → Option β) (hf : ∀ r, f (.SetRepository r) = none) =>
    (show (tableWrites ws ++ lastRepo ws).filterMap f = ws.filterMap f by
      rw [List.filterMap_append, filterMap_tableWrites f hf, filterMap_lastRepo f hf, List.append_nil])
  rw [Invariant.applyAll_eq, Invariant.applyAll_eq]
  have hr : (tableWrites ws ++ lastRepo ws).filterMap repoOf = ((ws.filterMap repoOf).getLast?).toList := by
    rw [List.filterMap_append, repos_tableWrites, List.nil_append]
    unfold lastRepo
    cases (ws.filterMap repoOf).getLast? <;> simp [repoOf]
  rw [hr]
  rw [t patchOf (by simp [patchOf]), t depOf (by simp [depOf]), t approvalOf (by simp [approvalOf]),
    t checkOf (by simp [checkOf]), t channelOf (by simp [channelOf]), t memberOf (by simp [memberOf]),
    t pathOf (by simp [pathOf]), t eventOf (by simp [eventOf]), t bodyOf (by simp [bodyOf]),
    t listingOf (by simp [listingOf]), t roleOf (by simp [roleOf])]
  congr 1
  cases h : (ws.filterMap repoOf).getLast? <;> simp

end o5s_registry.Group
