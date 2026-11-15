/-!
# pico2peta pure core model

Mirrors the pure decision logic of the Rust implementation:
* `crates/core` (`mobility`, `governing`)
* `crates/refs` (`RefState::decide_replace`, `decide_set_rule`, `apply`)
* the object layer's `put` / `get`

Hashes and byte strings are abstract. The hash function is a parameter.
Collision-freedom is *not* assumed by the theorems below; where it matters
(`get` returns data whose hash is the key), we prove the stored data hashes
to its key.

What is *not* modeled: crash recovery, concurrency, and backends. The
journal's single exclusive commit point linearizes `replace`/`setRule`, so
the model is a sequential state machine. That linearization (flock +
fsync) is an explicit assumption, tested in Rust by fault injection and
multi-process tests.
-/

namespace Pico2peta

/-- A ref name is a list of path segments (`a/b/c` ↦ `["a","b","c"]`). -/
abbrev Name := List String

structure RefVal (H : Type) where
  hash : H
  version : Nat
deriving DecidableEq

inductive Observed where
  | absent
  | version (v : Nat)
deriving DecidableEq

inductive Rule where
  | mutable
  | immutable
  | headRelease
deriving DecidableEq

inductive Mobility where
  | mutable
  | createOnce
  | forbidden
deriving DecidableEq

inductive Err where
  | targetNotInReadSet
  | missingObject
  | forbidden
  | ruleChangeOnNonEmpty
  /-- Malformed input (empty or duplicate-target transaction); `Error::Parse` in Rust. -/
  | invalid
deriving DecidableEq

inductive Outcome where
  | ok (version : Nat)
  | okMany (versions : List Nat)
  | conflict
  | err (e : Err)
deriving DecidableEq

def Outcome.isOk : Outcome → Bool
  | .ok _ => true
  | .okMany _ => true
  | _ => false

structure State (H B : Type) where
  objects : H → Option B
  refs : Name → Option (RefVal H)
  rules : List (Name × Rule)
  /-- Last version of deleted names (`RefState::tombstones`). -/
  tombs : Name → Option Nat

variable {H B : Type} [DecidableEq H]

def update {α β : Type} [DecidableEq α] (f : α → β) (a : α) (b : β) : α → β :=
  fun x => if x = a then b else f x

def observe (s : State H B) (n : Name) : Observed :=
  match s.refs n with
  | none => .absent
  | some v => .version v.version

/-- Longest-prefix choice (ties: the earlier entry wins). -/
def longest : List (Name × Rule) → Option (Name × Rule)
  | [] => none
  | x :: xs =>
    match longest xs with
    | none => some x
    | some y => if x.1.length < y.1.length then some y else some x

/-- The namespace entry governing `n`: the longest rule prefix of `n`. -/
def governing (rules : List (Name × Rule)) (n : Name) : Option (Name × Rule) :=
  longest (rules.filter (fun x => x.1.isPrefixOf n))

/-- How `n` may move (mirrors `pico2peta_core::mobility`). Default: mutable. -/
def mobilityOf (rules : List (Name × Rule)) (n : Name) : Mobility :=
  match governing rules n with
  | none => .mutable
  | some (_, .mutable) => .mutable
  | some (_, .immutable) => .createOnce
  | some (p, .headRelease) =>
    match n.drop p.length with
    | "heads" :: _ :: _ => .mutable
    | "releases" :: _ :: _ => .createOnce
    | _ => .forbidden

/-- Next version for a name: continues after the current value, or after its
tombstone if it was deleted, or starts at 1. -/
def nextVersion : Option (RefVal H) → Option Nat → Nat
  | some r, _ => r.version + 1
  | none, some t => t + 1
  | none, none => 1

/-- The object layer's `put`: idempotent, never overwrites. -/
def put (hashOf : B → H) (s : State H B) (d : B) : State H B :=
  match s.objects (hashOf d) with
  | some _ => s
  | none => { s with objects := update s.objects (hashOf d) (some d) }

/-- OCC replace (mirrors `RefState::decide_replace` + `apply`, plus the
`MissingObject` check). The order is input validation, then the read set
(`conflict`), then the rule (`forbidden`). -/
def replace (s : State H B) (n : Name) (rs : List (Name × Observed)) (new : H) :
    Outcome × State H B :=
  if !(rs.any fun x => decide (x.1 = n)) then (.err .targetNotInReadSet, s)
  else if (s.objects new).isNone then (.err .missingObject, s)
  else if rs.any (fun x => decide (observe s x.1 ≠ x.2)) then (.conflict, s)
  else
    match mobilityOf s.rules n, s.refs n with
    | .forbidden, _ => (.err .forbidden, s)
    | .createOnce, some _ => (.err .forbidden, s)
    | _, cur =>
      (.ok (nextVersion cur (s.tombs n)),
        { s with refs := update s.refs n (some ⟨new, nextVersion cur (s.tombs n)⟩),
                 tombs := update s.tombs n none })

/-- OCC delete (mirrors `RefState::decide_delete`). Only names whose
effective rule is mutable can be deleted. Deleting an absent name is a no-op. -/
def deleteRef (s : State H B) (n : Name) (rs : List (Name × Observed)) : Outcome × State H B :=
  if !(rs.any fun x => decide (x.1 = n)) then (.err .targetNotInReadSet, s)
  else if rs.any (fun x => decide (observe s x.1 ≠ x.2)) then (.conflict, s)
  else
    match s.refs n with
    | none => (.ok 0, s)
    | some cur =>
      match mobilityOf s.rules n with
      | .mutable =>
        (.ok cur.version, { s with refs := update s.refs n none, tombs := update s.tombs n (some cur.version) })
      | _ => (.err .forbidden, s)

/-- One decided effect of a transaction. -/
inductive Eff (H : Type) where
  | set (n : Name) (v : RefVal H)
  | del (n : Name) (v : Nat)
  | skip

def Eff.version {H : Type} : Eff H → Nat
  | .set _ v => v.version
  | .del _ v => v
  | .skip => 0

/-- Decides one entry of a transaction against the pre-state (the rules
of `replace` for `some`, of `deleteRef` for `none`). -/
def decideOne (s : State H B) (n : Name) : Option H → Except Err (Eff H)
  | some h =>
    match mobilityOf s.rules n, s.refs n with
    | .forbidden, _ => .error .forbidden
    | .createOnce, some _ => .error .forbidden
    | _, cur => .ok (.set n ⟨h, nextVersion cur (s.tombs n)⟩)
  | none =>
    match s.refs n with
    | none => .ok .skip
    | some cur =>
      match mobilityOf s.rules n with
      | .mutable => .ok (.del n cur.version)
      | _ => .error .forbidden

def decideAll (s : State H B) : List (Name × Option H) → Except Err (List (Eff H))
  | [] => .ok []
  | (n, u) :: rest =>
    match decideOne s n u with
    | .error e => .error e
    | .ok e =>
      match decideAll s rest with
      | .error e' => .error e'
      | .ok es => .ok (e :: es)

def applyEff (s : State H B) : Eff H → State H B
  | .set n v => { s with refs := update s.refs n (some v), tombs := update s.tombs n none }
  | .del n v => { s with refs := update s.refs n none, tombs := update s.tombs n (some v) }
  | .skip => s

/-- Per-entry input checks, in order: target in read set, then object exists. -/
def inputErr (s : State H B) (rs : List (Name × Observed)) : List (Name × Option H) → Option Err
  | [] => none
  | (n, u) :: rest =>
    if !(rs.any fun x => decide (x.1 = n)) then some .targetNotInReadSet
    else if (match u with | some h => (s.objects h).isNone | none => false) then some .missingObject
    else inputErr s rs rest

/-- Atomic multi-name update (mirrors `RefStore::update_many`, API v1.6;
`replace_many` is the all-`some` case). Every entry is decided against the
pre-state. Any error aborts the whole transaction. -/
def updateMany (s : State H B) (ups : List (Name × Option H)) (rs : List (Name × Observed)) :
    Outcome × State H B :=
  match inputErr s rs ups with
  | some e => (.err e, s)
  | none =>
    if ups = [] ∨ ¬ (ups.map Prod.fst).Nodup then (.err .invalid, s)
    else if rs.any (fun x => decide (observe s x.1 ≠ x.2)) then (.conflict, s)
    else
      match decideAll s ups with
      | .error e => (.err e, s)
      | .ok es => (.okMany (es.map Eff.version), es.foldl applyEff s)

open Classical in
/-- Sets a namespace rule. Refused if any ref lies under the prefix, so a
rule can never change for a name that already exists. -/
noncomputable def setRule (s : State H B) (p : Name) (r : Rule) : Outcome × State H B :=
  if s.rules.lookup p = some r then (.ok 0, s) -- idempotent (as `decide_set_rule`)
  else if ∃ n, p.isPrefixOf n = true ∧ s.refs n ≠ none then (.err .ruleChangeOnNonEmpty, s)
  else (.ok 0, { s with rules := (p, r) :: s.rules.filter (fun x => !(decide (x.1 = p))) })

inductive Op (H B : Type) where
  | put (d : B)
  | replace (n : Name) (rs : List (Name × Observed)) (new : H)
  | deleteRef (n : Name) (rs : List (Name × Observed))
  | updateMany (ups : List (Name × Option H)) (rs : List (Name × Observed))
  | setRule (p : Name) (r : Rule)

noncomputable def step (hashOf : B → H) (s : State H B) : Op H B → Outcome × State H B
  | .put d => (.ok 0, put hashOf s d)
  | .replace n rs new => replace s n rs new
  | .deleteRef n rs => deleteRef s n rs
  | .updateMany ups rs => updateMany s ups rs
  | .setRule p r => setRule s p r

noncomputable def run (hashOf : B → H) : State H B → List (Op H B) → State H B
  | s, [] => s
  | s, op :: ops => run hashOf (step hashOf s op).2 ops

end Pico2peta
