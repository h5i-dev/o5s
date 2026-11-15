import Pico2peta.Model
import Pico2peta.Fronts

/-!
# Executable model (for differential testing against Rust)

The spec's `setRule` quantifies over all names. Here `setRuleC` decides the same
thing over a finite list of the names that were ever set. `setRuleC_eq` proves the
two agree whenever that list covers every existing ref, which `ExecState`
maintains. `replace` and `put` are the spec definitions themselves.

`p2p-model` reads one operation per line on stdin and prints one outcome per line.
Hashes and data are both strings (`hashOf := id`).
```
P <hash>                                   put
R <target> <new> <name> <obs> [<name> <obs>]...   replace; obs = "-" (absent) or version
D <target> <name> <obs> [<name> <obs>]...         delete
U <k> (<name> <hash|->){k} <name> <obs> ...       update_many (`-` deletes)
S <prefix> <m|i|h>                         set rule (mutable/immutable/head-release)
```
Outputs: `ok <v>`, `conflict`, or `err <code>`, using the Rust `Error::code()` strings.
-/

namespace Pico2peta

variable {H B : Type} [DecidableEq H]

def setRuleC (s : State H B) (keys : List Name) (p : Name) (r : Rule) : Outcome × State H B :=
  if s.rules.lookup p = some r then (.ok 0, s)
  else if keys.any (fun n => p.isPrefixOf n && (s.refs n).isSome) then (.err .ruleChangeOnNonEmpty, s)
  else (.ok 0, { s with rules := (p, r) :: s.rules.filter (fun x => !(decide (x.1 = p))) })

theorem setRuleC_eq (s : State H B) (keys : List Name) (p : Name) (r : Rule)
    (hk : ∀ n, s.refs n ≠ none → n ∈ keys) : setRuleC s keys p r = setRule s p r := by
  unfold setRuleC setRule
  split
  · rfl
  · have : (keys.any (fun n => p.isPrefixOf n && (s.refs n).isSome)) = true ↔
        ∃ n, p.isPrefixOf n = true ∧ s.refs n ≠ none := by
      constructor
      · intro h
        obtain ⟨n, _, hn⟩ := List.any_eq_true.mp h
        simp only [Bool.and_eq_true, Option.isSome_iff_ne_none] at hn
        exact ⟨n, hn.1, hn.2⟩
      · rintro ⟨n, hp, hn⟩
        exact List.any_eq_true.mpr ⟨n, hk n hn, by simp [hp, Option.isSome_iff_ne_none, hn]⟩
    by_cases h : ∃ n, p.isPrefixOf n = true ∧ s.refs n ≠ none
    · rw [if_pos (this.mpr h), if_pos h]
    · rw [if_neg (fun h' => h (this.mp h')), if_neg h]

structure ExecState where
  s : State String String
  keys : List Name

def ExecState.empty : ExecState := ⟨⟨fun _ => none, fun _ => none, [], fun _ => none⟩, []⟩

def errCode : Err → String
  | .targetNotInReadSet => "target_not_in_read_set"
  | .missingObject => "missing_object"
  | .forbidden => "forbidden"
  | .ruleChangeOnNonEmpty => "rule_change_on_non_empty"
  | .invalid => "parse"

def showOutcome : Outcome → String
  | .ok v => s!"ok {v}"
  | .okMany vs => "ok " ++ ",".intercalate (vs.map toString)
  | .conflict => "conflict"
  | .err e => s!"err {errCode e}"

def parseName (t : String) : Name := (t.splitOn "/")

def parseObs (t : String) : Option Observed :=
  if t == "-" then some .absent else t.toNat?.map Observed.version

partial def parseReadSet : List String → Option (List (Name × Observed))
  | [] => some []
  | n :: o :: rest => do
    let ob ← parseObs o
    let tl ← parseReadSet rest
    pure ((parseName n, ob) :: tl)
  | _ => none

def execLine (st : ExecState) (line : String) : ExecState × String :=
  match line.splitOn " " with
  | ["P", h] => ({ st with s := put id st.s h }, "ok 0")
  | "R" :: t :: new :: rest =>
    match parseReadSet rest with
    | none => (st, "parse_error")
    | some rs =>
      let (o, s') := replace st.s (parseName t) rs new
      let keys := if o.isOk then parseName t :: st.keys else st.keys
      ({ s := s', keys := keys }, showOutcome o)
  | "D" :: t :: rest =>
    match parseReadSet rest with
    | none => (st, "parse_error")
    | some rs =>
      let (o, s') := deleteRef st.s (parseName t) rs
      ({ st with s := s' }, showOutcome o)
  | "U" :: k :: rest =>
    match k.toNat? with
    | none => (st, "parse_error")
    | some k =>
      let entries := (rest.take (2 * k))
      let rec pairs : List String → List (Name × Option String)
        | n :: v :: tl => (parseName n, if v == "-" then none else some v) :: pairs tl
        | _ => []
      match parseReadSet (rest.drop (2 * k)) with
      | none => (st, "parse_error")
      | some rs =>
        let ups := pairs entries
        let (o, s') := updateMany st.s ups rs
        let keys := if o.isOk then ups.map Prod.fst ++ st.keys else st.keys
        ({ s := s', keys := keys }, showOutcome o)
  | ["M", src, dst] =>
    let (o, s') := promote st.s (parseName src) (parseName dst)
    let keys := if o.isOk then parseName dst :: st.keys else st.keys
    ({ s := s', keys := keys }, showOutcome o)
  | ["S", p, r] =>
    let rule := if r == "i" then Rule.immutable else if r == "h" then Rule.headRelease else Rule.mutable
    let (o, s') := setRuleC st.s st.keys (parseName p) rule
    ({ st with s := s' }, showOutcome o)
  | _ => (st, "parse_error")

end Pico2peta

open Pico2peta in
partial def loop (st : ExecState) (h : IO.FS.Stream) (out : IO.FS.Stream) : IO Unit := do
  let line ← h.getLine
  if line.isEmpty then return
  let (st', res) := execLine st line.trimRight
  out.putStrLn res
  out.flush
  loop st' h out

def main : IO Unit := do
  loop Pico2peta.ExecState.empty (← IO.getStdin) (← IO.getStdout)
