import Rendering
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Checked
open Rendering
abbrev State := Bool × Nat × Nat
def abs (s : interval.Validation) : State := (s.valid, s.cursor.val, s.capacity.val)
def advance (n : Nat) (s : State) (c : Chunk) : State :=
  if c.2.2.val.length ≤ Usize.max - s.2.2 then
    (s.1 && decide (s.2.1 ≤ c.1.val ∧ c.1.val ≤ c.2.1.val ∧ c.2.1.val ≤ n),
      c.2.1.val, s.2.2 + c.2.2.val.length)
  else (false, c.2.1.val, s.2.2)

@[step] theorem validation_step_spec (s : interval.Validation) (n a b r : Usize) :
    interval.validation_step s n a b r ⦃ out =>
      abs out = (if r.val ≤ Usize.max - s.capacity.val then
        (s.valid && decide (s.cursor.val ≤ a.val ∧ a.val ≤ b.val ∧ b.val ≤ n.val),
          b.val, s.capacity.val + r.val)
        else (false,b.val,s.capacity.val)) ⦄ := by
  unfold interval.validation_step
  h5i_step
  all_goals simp_all [abs]

@[step] theorem validation_loop_spec (n : Usize) (chunks : alloc.vec.Vec Chunk) :
    interval.valid_chunks_loop n chunks
      { valid := true, cursor := 0#usize, capacity := n } 0#usize ⦃ out =>
      abs out = chunks.val.foldl (advance n.val) (true,0,n.val) ⦄ := by
  unfold interval.valid_chunks_loop
  apply loop_fold chunks.val abs (advance n.val) (fun _ _ => True)
    (fun st => interval.valid_chunks_loop.body n chunks st.1 st.2)
    ?_ _ _ (by simp) trivial
  intro s i hi hinv
  unfold interval.valid_chunks_loop.body
  h5i_step
  refine ⟨by scalar_tac, ?_⟩
  simp [advance, abs, ← i2_post]

@[step] theorem valid_chunks_spec (n : Usize) (chunks : alloc.vec.Vec Chunk) :
    interval.valid_chunks n chunks ⦃ out =>
      out = (chunks.val.foldl (advance n.val) (true,0,n.val)).1 ⦄ := by
  unfold interval.valid_chunks
  h5i_step
  simpa [abs] using congrArg Prod.fst state_post

def Good (n cursor budget : Nat) : List Chunk → Prop
  | [] => budget ≤ Usize.max
  | c :: cs => cursor ≤ c.1.val ∧ c.1.val ≤ c.2.1.val ∧ c.2.1.val ≤ n ∧
      c.2.2.val.length ≤ Usize.max - budget ∧
      Good n c.2.1.val (budget + c.2.2.val.length) cs

theorem fold_accepts (n : Nat) (cs : List Chunk) (s : State) (hb : s.2.2 ≤ Usize.max) :
    (cs.foldl (advance n) s).1 = true ↔ s.1 = true ∧ Good n s.2.1 s.2.2 cs := by
  induction cs generalizing s with
  | nil => simp [Good, hb]
  | cons c cs ih =>
    simp only [List.foldl_cons]
    by_cases hr : c.2.2.val.length ≤ Usize.max - s.2.2
    · have room : (advance n s c).2.2 ≤ Usize.max := by simp [advance, hr]; omega
      rw [ih _ room]
      simp [advance, hr, Good, Bool.and_eq_true]
    · have room : (advance n s c).2.2 ≤ Usize.max := by simpa [advance, hr] using hb
      rw [ih _ room]
      simp [advance, hr, Good]


theorem good_budget (n cursor budget : Nat) (cs : List Chunk)
    (h : Good n cursor budget cs) : budget + weight cs ≤ Usize.max := by
  induction cs generalizing cursor budget with
  | nil => simpa [Good, weight] using h
  | cons c cs ih =>
    have ht := ih c.2.1.val (budget + c.2.2.val.length) h.2.2.2.2
    simp [weight] at *
    omega

theorem good_bounds (n cursor budget : Nat) (cs : List Chunk)
    (h : Good n cursor budget cs) :
    ∀ i (hi : i < cs.length), cs[i].1.val ≤ cs[i].2.1.val ∧ cs[i].2.1.val ≤ n := by
  induction cs generalizing cursor budget with
  | nil => simp
  | cons c cs ih =>
    intro i hi
    cases i with
    | zero => exact ⟨h.2.1, h.2.2.1⟩
    | succ i => simpa using ih c.2.1.val (budget+c.2.2.val.length) h.2.2.2.2 i (by simpa using hi)

theorem good_next (n cursor budget : Nat) (cs : List Chunk)
    (h : Good n cursor budget cs) :
    ∀ i (hi : i + 1 < cs.length), cs[i].2.1.val ≤ cs[i+1].1.val := by
  induction cs generalizing cursor budget with
  | nil => simp
  | cons c cs ih =>
    intro i hi
    cases i with
    | zero =>
      cases cs with
      | nil => simp at hi
      | cons d ds => simpa using h.2.2.2.2.1
    | succ i => simpa using ih c.2.1.val (budget+c.2.2.val.length) h.2.2.2.2 i (by simpa using hi)


@[step] theorem valid_chunks_safe (n : Usize) (chunks : alloc.vec.Vec Chunk) :
    interval.valid_chunks n chunks ⦃ out =>
      out = true ↔ Good n.val 0 n.val chunks.val ⦄ := by
  apply WP.spec_mono (valid_chunks_spec n chunks)
  intro out h
  rw [h, fold_accepts n.val chunks.val (true,0,n.val) (by scalar_tac)]
  simp

@[step] theorem checked_render_spec (bytes : alloc.vec.Vec U8) (chunks : alloc.vec.Vec Chunk) :
    interval.checked_render bytes chunks ⦃ out =>
      match out with
      | none => ¬ Good bytes.val.length 0 bytes.val.length chunks.val
      | some v => Good bytes.val.length 0 bytes.val.length chunks.val ∧
          v.val = model bytes.val chunks.val ⦄ := by
  unfold interval.checked_render
  h5i_step
  all_goals
    have hg : Good bytes.val.length 0 bytes.val.length chunks.val := by simp_all
    have wf : WellFormed bytes.val chunks.val := ⟨good_bounds _ _ _ _ hg, good_next _ _ _ _ hg⟩
    have room := good_budget _ _ _ _ hg
    h5i_step

end o5s_kernel.Checked
