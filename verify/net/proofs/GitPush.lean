import O5sNetKernel
import H5iAppLib
/-!
# Git push acceptance: the extracted decision computes this spec

`crates/net-kernel/src/gitpush.rs`. `decide_push_spec`: for every input, the decisions are
`cmdSpec` of each command, and with `--atomic` and any refusal, every accepted one becomes
`AtomicFailed`. The corollaries state what an accepted command guarantees.
-/
open Aeneas Aeneas.Std Result H5iAppLib o5s_net_kernel o5s_net_kernel.gitpush

namespace o5s_net_kernel.Spec.GitPush

/-- One command's decision. -/
def cmdSpec (c : PushCmd) : core.result.Result Accept Ng :=
  if c.ref_ok = false then .Err .UnsupportedRef
  else if c.delete = true then
    match c.old with
    | none => .Ok .Delete
    | some _ => if c.cur = c.old then .Ok .Delete else .Err .StaleOld
  else if c.new_present = false then .Err .MissingNew
  else if c.is_branch = true ∧ c.new_is_commit = false then .Err .BranchNotCommit
  else if c.cur = c.old then .Ok .Update else .Err .StaleOld

def isErrS : core.result.Result Accept Ng → Bool
  | .Ok _ => false
  | .Err _ => true

def refuseS : core.result.Result Accept Ng → core.result.Result Accept Ng
  | .Ok _ => .Err .AtomicFailed
  | .Err e => .Err e

/-- All decisions of a push. -/
def pushSpec (cs : List PushCmd) (atomic : Bool) : List (core.result.Result Accept Ng) :=
  if atomic = true ∧ (cs.map cmdSpec).any isErrS = true then (cs.map cmdSpec).map refuseS
  else cs.map cmdSpec

@[step]
theorem same_spec (a b : Option (alloc.vec.Vec U8)) : same a b ⦃ r => r = decide (a = b) ⦄ := by
  unfold same
  split <;> split <;> step* <;> simp_all

@[step]
theorem decide_cmd_spec (c : PushCmd) : decide_cmd c ⦃ r => r = cmdSpec c ⦄ := by
  unfold decide_cmd
  step* <;> simp_all [cmdSpec]

@[step]
theorem is_err_spec (d : core.result.Result Accept Ng) : is_err d ⦃ b => b = isErrS d ⦄ := by
  unfold is_err; split <;> simp [isErrS]

@[step]
theorem refuse_spec (d : core.result.Result Accept Ng) : refuse d ⦃ r => r = refuseS d ⦄ := by
  unfold refuse; split <;> simp [refuseS]

theorem foldl_pair (l : List PushCmd) (o : List (core.result.Result Accept Ng)) (b : Bool) :
    l.foldl (fun (acc : List (core.result.Result Accept Ng) × Bool) c =>
      (acc.1 ++ [cmdSpec c], acc.2 || isErrS (cmdSpec c))) (o, b) =
      (o ++ l.map cmdSpec, b || (l.map cmdSpec).any isErrS) := by
  induction l generalizing o b with
  | nil => simp
  | cons c r ih => simp only [List.foldl_cons, ih, List.map_cons, List.any_cons]; simp [Bool.or_assoc]

@[step]
theorem decide_all_spec (cmds : alloc.vec.Vec PushCmd) :
    decide_all cmds ⦃ r => r.1.val = cmds.val.map cmdSpec ∧ r.2 = (cmds.val.map cmdSpec).any isErrS ⦄ := by
  unfold decide_all decide_all_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_fold (alloc.vec.Vec.deref cmds)
    (fun (acc : alloc.vec.Vec (core.result.Result Accept Ng) × Bool) => (acc.1.val, acc.2))
    (fun acc c => (acc.1 ++ [cmdSpec c], acc.2 || isErrS (cmdSpec c)))
    (fun acc i => acc.1.val.length ≤ i) _ ?_ 0 _ (by simp) (by simp))
  · intro y hy
    rw [foldl_pair] at hy
    simp only [List.drop_zero, alloc.vec.Vec.new, List.nil_append, Bool.false_or, Prod.mk.injEq,
      vec_deref_val] at hy
    exact hy
  · rintro i ⟨o, b⟩ hi hinv
    unfold decide_all_loop.body
    h5i_iter [IterFoldStep]

@[step]
theorem refuse_all_spec (ds : alloc.vec.Vec (core.result.Result Accept Ng)) :
    refuse_all ds ⦃ r => r.val = ds.val.map refuseS ⦄ := by
  unfold refuse_all refuse_all_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_filter_map (alloc.vec.Vec.deref ds) (fun _ => true) refuseS _ ?_)
  · intro y hy; simpa [vec_deref_val] using hy
  · intro i acc hi hacc
    unfold refuse_all_loop.body
    h5i_iter [IterFoldStep]

theorem decide_push_spec (cmds : alloc.vec.Vec PushCmd) (atomic : Bool) :
    decide_push cmds atomic ⦃ r => r.val = pushSpec cmds.val atomic ⦄ := by
  unfold decide_push
  step*
  all_goals simp_all [pushSpec]

/-! ## What an accepted command guarantees -/

theorem accepted_update (c : PushCmd) (h : cmdSpec c = .Ok .Update) :
    c.ref_ok = true ∧ c.delete = false ∧ c.new_present = true ∧
      (c.is_branch = true → c.new_is_commit = true) ∧ c.cur = c.old := by
  unfold cmdSpec at h
  split_ifs at h <;> simp_all
  all_goals (split at h <;> simp_all)

theorem accepted_delete (c : PushCmd) (h : cmdSpec c = .Ok .Delete) :
    c.ref_ok = true ∧ c.delete = true ∧ (c.old = none ∨ c.cur = c.old) := by
  unfold cmdSpec at h
  split_ifs at h <;> simp_all
  all_goals (split at h <;> simp_all)

/-- `--atomic`: either every command is accepted, or none is. -/
theorem atomic_all_or_nothing (cs : List PushCmd) :
    (∀ d ∈ pushSpec cs true, isErrS d = false) ∨ (∀ d ∈ pushSpec cs true, isErrS d = true) := by
  unfold pushSpec
  by_cases h : (cs.map cmdSpec).any isErrS = true
  · right; simp only [h, and_self, if_true]
    intro d hd; simp only [List.mem_map] at hd
    obtain ⟨x, _, rfl⟩ := hd; cases x <;> rfl
  · left; simp only [h, and_false, if_false]
    intro d hd
    simp only [List.any_eq_true, not_exists, not_and, Bool.not_eq_true] at h
    exact h d hd

end o5s_net_kernel.Spec.GitPush
