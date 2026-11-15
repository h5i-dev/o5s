import Pico2peta.Properties
import Pico2peta.Auth

/-!
# Fronts' owned prefixes and promotion (Front API, ROADMAP P5)

Mirrors `crates/repo/src/front.rs` (`FrontRegistry::owner`, `LocalRepo::guard`)
and `crates/attest` (`promote`):

* A front claims name prefixes; the longest claim owning a name decides.
  A write to an owned name is refused unless it comes through that front's
  handle (`who = some f`).
* `promote src dst` moves `dst` to the object `src` names, as one
  `updateMany` whose read set pins both `src` and `dst`.
-/

namespace Pico2peta

variable {H B : Type} [DecidableEq H]

/-- The front owning `n`: the longest claimed prefix of `n`. -/
def owner (claims : List (Name × String)) (n : Name) : Option String :=
  ((claims.filter (fun c => c.1.isPrefixOf n)).foldl
      (fun best c => match best with
        | none => some c
        | some b => if b.1.length < c.1.length then some c else some b) none).map Prod.snd

/-- May `who` (a front id, or `none` for the plain handle) write `n`? -/
def mayWrite (claims : List (Name × String)) (who : Option String) (n : Name) : Bool :=
  match owner claims n with
  | none => true
  | some f => decide (some f = who)

/-- The guarded step of `LocalRepo` / `FrontHandle`: a write to a name owned
by another front is refused (state unchanged). -/
noncomputable def frontStep (hashOf : B → H) (claims : List (Name × String)) (who : Option String)
    (s : State H B) : Op H B → Option (Outcome × State H B)
  | .put d => some (step hashOf s (.put d))
  | .replace n rs new => if mayWrite claims who n then some (step hashOf s (.replace n rs new)) else none
  | .deleteRef n rs => if mayWrite claims who n then some (step hashOf s (.deleteRef n rs)) else none
  | .updateMany ups rs =>
    if ups.all (fun x => mayWrite claims who x.1) then some (step hashOf s (.updateMany ups rs)) else none
  | .setRule q r => some (step hashOf s (.setRule q r))

/-- A name owned by a front other than `who` doesn't move, whatever `who` sends. -/
theorem non_owner_cannot_move (hashOf : B → H) (claims : List (Name × String)) (who : Option String)
    (s : State H B) (op : Op H B) (n : Name) (hw : mayWrite claims who n = false)
    (r : Outcome × State H B) (hr : frontStep hashOf claims who s op = some r) :
    r.2.refs n = s.refs n := by
  cases op with
  | put d =>
    simp only [frontStep, Option.some.injEq] at hr
    subst hr; simp [step, put_refs]
  | setRule q ru =>
    simp only [frontStep, Option.some.injEq] at hr
    subst hr; simp [step, setRule_refs]
  | updateMany ups rs =>
    simp only [frontStep] at hr
    split at hr
    · rename_i hall
      simp only [Option.some.injEq] at hr
      subst hr
      have hn : ∀ u, (n, u) ∉ ups := by
        intro u hu
        have := List.all_eq_true.mp hall (n, u) hu
        simp [hw] at this
      simp [step, updateMany_other s ups rs n hn]
    · cases hr
  | deleteRef m rs =>
    simp only [frontStep] at hr
    split at hr
    · rename_i hm
      simp only [Option.some.injEq] at hr
      subst hr
      have hmn : m ≠ n := by
        intro e; subst e; simp [hw] at hm
      simp [step, deleteRef_other s m rs n hmn]
    · cases hr
  | replace m rs new =>
    simp only [frontStep] at hr
    split at hr
    · rename_i hm
      simp only [Option.some.injEq] at hr
      subst hr
      have hmn : m ≠ n := by
        intro e; subst e; simp [hw] at hm
      simp [step, replace_other_refs s m rs new n hmn]
    · cases hr

/-- A sequence of requests through one handle; refused requests are skipped. -/
noncomputable def frontRun (hashOf : B → H) (claims : List (Name × String)) (who : Option String) :
    State H B → List (Op H B) → State H B
  | s, [] => s
  | s, op :: ops =>
    match frontStep hashOf claims who s op with
    | some r => frontRun hashOf claims who r.2 ops
    | none => frontRun hashOf claims who s ops

theorem non_owner_cannot_move_run (hashOf : B → H) (claims : List (Name × String)) (who : Option String)
    (s : State H B) (ops : List (Op H B)) (n : Name) (hw : mayWrite claims who n = false) :
    (frontRun hashOf claims who s ops).refs n = s.refs n := by
  induction ops generalizing s with
  | nil => rfl
  | cons op ops ih =>
    simp only [frontRun]
    split
    · rename_i r hr
      rw [ih, non_owner_cannot_move hashOf claims who s op n hw r hr]
    · exact ih s

/-- `promote src dst` (as `crates/attest::promote`): if `src` names an
object, move `dst` to it in one `updateMany` that pins `src`'s version and
`dst`'s current state. -/
def promote (s : State H B) (src dst : Name) : Outcome × State H B :=
  match s.refs src with
  | none => (.err .invalid, s)
  | some rv => updateMany s [(dst, some rv.hash)] [(src, .version rv.version), (dst, observe s dst)]

/-- Promotion moves no name other than `dst`. -/
theorem promote_only_moves_dst (s : State H B) (src dst n : Name) (hn : n ≠ dst) :
    (promote s src dst).2.refs n = s.refs n := by
  unfold promote
  split
  · rfl
  · apply updateMany_other
    intro u hu
    simp at hu
    exact hn hu.1

omit [DecidableEq H] in
/-- If the read set is stale (here: `src` moved since it was observed), the
whole transaction leaves the state unchanged. This is what makes a
promotion of an object `src` no longer names impossible. (`split` resolves
the conflict test with `hst` from the context.) -/
theorem updateMany_stale_noop (s : State H B) ups (rs : List (Name × Observed))
    (hst : rs.any (fun x => decide (observe s x.1 ≠ x.2)) = true) : (updateMany s ups rs).2 = s := by
  unfold updateMany
  split
  · rfl
  · split
    · rfl
    · rfl

end Pico2peta
