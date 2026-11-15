import Pico2peta.Properties

/-!
# P5: GC and repack safety

This models the physical layer as a set of packs, each a list of records, and covers the
GC operations of `crates/repo/src/gc.rs`:

* `copyInto` (repack and rescue): appends records to a fresh pack;
* `deleteChecked` (the delete phase): removes the condemned packs only after checking
  that every live hash is also present in some surviving pack. The Rust code
  re-marks, rescues, refreshes, verifies, and aborts otherwise.

The live set is any set that contains every ref target and is closed under a
children relation (the `Tracer`). Manifest chunks are children too.

Theorem `gc_preserves_live`: no live object is lost by any sequence of copies and
checked deletes.

Assumption (not modeled): the live set is computed from the refs current at
delete time. In the implementation that's the put→replace lease (see gc.rs docs).
-/

set_option linter.unusedSectionVars false

namespace Pico2peta

variable {H B : Type} [DecidableEq H]

structure Phys (H B : Type) where
  packs : Nat → Option (List (H × B))

def Phys.has (st : Phys H B) (h : H) : Prop :=
  ∃ p recs d, st.packs p = some recs ∧ (h, d) ∈ recs

/-- Every ref target is live, and live is closed under children. -/
structure LiveSet (refs : Name → Option (RefVal H)) (children : H → List H) (live : H → Prop) : Prop where
  roots : ∀ n v, refs n = some v → live v.hash
  closed : ∀ h c, live h → c ∈ children h → live c

/-- Repack or rescue: write records into a fresh pack `q`. -/
def copyInto (st : Phys H B) (q : Nat) (recs : List (H × B)) : Phys H B :=
  match st.packs q with
  | some _ => st -- packs are immutable: never overwrite
  | none => ⟨update st.packs q (some recs)⟩

open Classical in
/-- Delete phase: drop `doomed` packs only if every live hash survives elsewhere. -/
noncomputable def deleteChecked (st : Phys H B) (live : H → Prop) (doomed : List Nat) : Phys H B :=
  if ∀ h, live h → ∃ p recs d, p ∉ doomed ∧ st.packs p = some recs ∧ (h, d) ∈ recs then
    ⟨fun p => if p ∈ doomed then none else st.packs p⟩
  else st

inductive GcOp where
  | copy (q : Nat) (recs : List (H × B))
  | delete (doomed : List Nat)

noncomputable def gcStep (live : H → Prop) (st : Phys H B) : GcOp (H := H) (B := B) → Phys H B
  | .copy q recs => copyInto st q recs
  | .delete doomed => deleteChecked st live doomed

theorem copy_keeps (st : Phys H B) q recs h (hh : st.has h) : (copyInto st q recs).has h := by
  obtain ⟨p, rs, d, hp, hm⟩ := hh
  unfold copyInto
  split
  · exact ⟨p, rs, d, hp, hm⟩
  · rename_i hq
    refine ⟨p, rs, d, ?_, hm⟩
    simp only [update]
    split
    · rename_i e; subst e; rw [hq] at hp; cases hp
    · exact hp

theorem delete_keeps_live (st : Phys H B) (live : H → Prop) doomed h (hl : live h)
    (hh : st.has h) : (deleteChecked st live doomed).has h := by
  unfold deleteChecked
  split
  · rename_i hall
    obtain ⟨p, rs, d, hp, hpk, hm⟩ := hall h hl
    exact ⟨p, rs, d, by simp [hp, hpk], hm⟩
  · exact hh

/-- **P5.** Every live object present before GC is present after any sequence of
repack, rescue and checked-delete steps. -/
theorem gc_preserves_live (live : H → Prop) (st : Phys H B) (ops : List (GcOp (H := H) (B := B)))
    (h : H) (hl : live h) (hh : st.has h) : (ops.foldl (gcStep live) st).has h := by
  induction ops generalizing st with
  | nil => exact hh
  | cons op ops ih =>
    apply ih
    cases op with
    | copy q recs => exact copy_keeps st q recs h hh
    | delete doomed => exact delete_keeps_live st live doomed h hl hh

/-- Corollary: ref targets (and everything reachable from them) survive GC. -/
theorem gc_preserves_refs (refs : Name → Option (RefVal H)) (children : H → List H)
    (live : H → Prop) (hlive : LiveSet refs children live) (st : Phys H B)
    (ops : List (GcOp (H := H) (B := B))) (n : Name) (v : RefVal H)
    (hv : refs n = some v) (hh : st.has v.hash) : (ops.foldl (gcStep live) st).has v.hash :=
  gc_preserves_live live st ops v.hash (hlive.roots n v hv) hh

end Pico2peta
