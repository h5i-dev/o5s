import O5sKernel
import StdSpecs
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Interval

def Conflict (a z b w : U64) : Prop :=
  if a = z then b ≤ a ∧ a ≤ w
  else if b = w then a ≤ b ∧ b ≤ z
  else a < w ∧ b < z

@[step] theorem conflict_spec (a z b w : U64) :
    interval.conflict a z b w ⦃ out => out = true ↔ Conflict a z b w ⦄ := by
  unfold interval.conflict Conflict
  split <;> split <;> simp_all
  all_goals (try split) <;> simp_all
  all_goals scalar_tac

 theorem conflict_symmetric (a z b w : U64) :
    Conflict a z b w ↔ Conflict b w a z := by
  by_cases ha : a = z <;> by_cases hb : b = w
  all_goals simp [Conflict, ha, hb, and_comm]
  all_goals scalar_tac

 theorem strict_gap_safe (a z b w : U64) (ha : a ≤ z) (hb : b ≤ w)
    (gap : z < b ∨ w < a) : ¬ Conflict a z b w := by
  by_cases hza : a = z <;> by_cases hwb : b = w
  all_goals simp only [Conflict, hza, hwb, if_true, if_false]
  all_goals scalar_tac

@[step] theorem splice_spec (bytes replacement : alloc.vec.Vec U8) (start finish : Usize)
    (ordered : start.val ≤ finish.val) (bounded : finish.val ≤ bytes.val.length)
    (room : bytes.val.length + replacement.val.length ≤ Usize.max) :
    interval.splice bytes start finish replacement ⦃ out =>
      out.val = bytes.val.take start.val ++ replacement.val ++ bytes.val.drop finish.val ⦄ := by
  unfold interval.splice
  step*
  all_goals simp_all only [List.drop_zero, List.take_length, List.length_take,
    List.length_drop, List.length_append, Nat.sub_zero]
  all_goals try scalar_tac
  all_goals simp_all [List.take_eq_self_iff]

@[step] theorem append_range_spec (initial bytes : alloc.vec.Vec U8) (start finish : Usize)
    (ordered : start.val ≤ finish.val) (bounded : finish.val ≤ bytes.val.length)
    (room : initial.val.length + (finish.val - start.val) ≤ Usize.max) :
    interval.append_range initial bytes start finish ⦃ out =>
      out.val = initial.val ++ (bytes.val.drop start.val).take (finish.val - start.val) ⦄ := by
  unfold interval.append_range interval.append_range_loop
  apply WP.spec_mono (loop_fold (bytes.val.take finish.val) (fun out : alloc.vec.Vec U8 => out.val)
    (fun out byte => out ++ [byte])
    (fun out j => start.val ≤ j ∧ out.val.length + start.val = initial.val.length + j)
    (fun st => interval.append_range_loop.body bytes finish st.1 st.2)
    ?_ initial start (by simp; omega) (by simp))
  · intro out h
    rw [h, foldl_snoc, ← List.drop_take]
  · intro out i hi hinv
    unfold interval.append_range_loop.body
    simp only [List.length_take] at hi
    h5i_step
    all_goals scalar_tac

end o5s_kernel.Interval
