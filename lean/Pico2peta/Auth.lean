import Pico2peta.Properties

/-!
# P4: authorization

This mirrors `crates/server/src/auth.rs` (`Authorizer::allows`) and the guards in
`crates/server/src/api.rs`:
* access levels are ordered `read < write < admin`, and a grant on namespace `p` covers
  every name under `p` (segment-aligned, so the empty namespace covers everything);
* `resolve n` needs read on `n`;
* `replace n rs _` needs write on `n` **and** read on every name in `rs` (a read set
  is information about those names);
* `setRule p _` needs admin on `p`.

Theorems: an unauthorized principal can't move a name (`unauthorized_cannot_move`,
`unauthorized_cannot_move_run`) and can't learn a name's value (`resolve_denied`).
-/

set_option linter.unusedSectionVars false

namespace Pico2peta

inductive Access where
  | read
  | write
  | admin
deriving DecidableEq

def Access.level : Access → Nat
  | .read => 0
  | .write => 1
  | .admin => 2

structure Grant where
  /-- `none` = the `*` wildcard (any authenticated principal). -/
  principal : Option String
  namespace_ : Name
  access : Access

structure Principal where
  name : String
  authenticated : Bool

def Grant.applies (g : Grant) (p : Principal) : Bool :=
  p.authenticated && (match g.principal with | none => true | some n => n == p.name)

def allows (pol : List Grant) (p : Principal) (path : Name) (a : Access) : Bool :=
  pol.any fun g => g.applies p && decide (a.level ≤ g.access.level) && g.namespace_.isPrefixOf path

variable {H B : Type} [DecidableEq H]

/-- The server's guarded step. A denied request leaves the state unchanged. -/
noncomputable def authStep (hashOf : B → H) (pol : List Grant) (p : Principal) (s : State H B) :
    Op H B → Option (Outcome × State H B)
  | .put d => some (step hashOf s (.put d))
  | .replace n rs new =>
    if allows pol p n .write && rs.all (fun x => allows pol p x.1 .read) then
      some (step hashOf s (.replace n rs new))
    else none
  | .setRule q r => if allows pol p q .admin then some (step hashOf s (.setRule q r)) else none

/-- What `resolve` returns to a principal: `none` means denied. -/
def authResolve (pol : List Grant) (p : Principal) (s : State H B) (n : Name) :
    Option (Option (RefVal H)) :=
  if allows pol p n .read then some (s.refs n) else none

theorem resolve_denied (pol : List Grant) (p : Principal) (s : State H B) (n : Name)
    (h : allows pol p n .read = false) : authResolve pol p s n = none := by
  simp [authResolve, h]

theorem replace_other_refs (s : State H B) m rs new n (hmn : m ≠ n) :
    (replace s m rs new).2.refs n = s.refs n := by
  unfold replace
  split
  · rfl
  split
  · rfl
  split
  · rfl
  split
  · rfl
  · rfl
  · simp [update, Ne.symm hmn]

theorem setRule_refs (s : State H B) q r : (setRule s q r).2.refs = s.refs := by
  unfold setRule; split
  · rfl
  · split <;> rfl

theorem put_refs (hashOf : B → H) (s : State H B) d : (put hashOf s d).refs = s.refs := by
  unfold put; split <;> rfl

/-- **P4.** A principal without write access to `n` can't change `n`, whatever request
it sends. -/
theorem unauthorized_cannot_move (hashOf : B → H) (pol : List Grant) (p : Principal)
    (s : State H B) (op : Op H B) (n : Name) (hw : allows pol p n .write = false)
    (r : Outcome × State H B) (hr : authStep hashOf pol p s op = some r) :
    r.2.refs n = s.refs n := by
  cases op with
  | put d =>
    simp only [authStep, Option.some.injEq] at hr
    subst hr; simp [step, put_refs]
  | setRule q ru =>
    simp only [authStep] at hr
    split at hr
    · simp only [Option.some.injEq] at hr; subst hr; simp [step, setRule_refs]
    · cases hr
  | replace m rs new =>
    simp only [authStep] at hr
    split at hr
    · rename_i hall
      simp only [Option.some.injEq] at hr
      subst hr
      have hmn : m ≠ n := by
        intro e; subst e
        simp [hw] at hall
      simp [step, replace_other_refs s m rs new n hmn]
    · cases hr

/-- Run a sequence of requests from one principal; denied requests are skipped. -/
noncomputable def authRun (hashOf : B → H) (pol : List Grant) (p : Principal) :
    State H B → List (Op H B) → State H B
  | s, [] => s
  | s, op :: ops =>
    match authStep hashOf pol p s op with
    | some r => authRun hashOf pol p r.2 ops
    | none => authRun hashOf pol p s ops

theorem unauthorized_cannot_move_run (hashOf : B → H) (pol : List Grant) (p : Principal)
    (s : State H B) (ops : List (Op H B)) (n : Name) (hw : allows pol p n .write = false) :
    (authRun hashOf pol p s ops).refs n = s.refs n := by
  induction ops generalizing s with
  | nil => rfl
  | cons op ops ih =>
    simp only [authRun]
    split
    · rename_i r hr
      rw [ih, unauthorized_cannot_move hashOf pol p s op n hw r hr]
    · exact ih s

end Pico2peta
