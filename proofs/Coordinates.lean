import Indexed
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
namespace o5s_kernel.Coordinates
open Indexed

def abstract (s : coordinates.Scan) : List Nat × Nat := (s.offsets.val.map (·.val), s.position.val)
def scan (n : Nat) (s : List Nat × Nat) (byte : U8) : List Nat × Nat :=
  let next := s.2 + 1
  (if byte.val = 10 ∧ next < n then s.1 ++ [next] else s.1, next)
def Inv (n i : Nat) (s : coordinates.Scan) : Prop :=
  s.position.val = i ∧ s.offsets.val.length ≤ i + 1 ∧
  s.offsets.val.length ≤ max 1 n ∧ (∀ v ∈ s.offsets.val, v.val ≤ i)

@[step] theorem step_spec (s : coordinates.Scan) (byte : U8) (n : Usize) (i : Nat)
    (hi : i < n.val) (room : n.val < Usize.max) (inv : Inv n.val i s) :
    coordinates.step s byte n ⦃ out => abstract out = scan n.val (abstract s) byte ∧
      Inv n.val (i+1) out ⦄ := by
  unfold coordinates.step
  simp only [Inv] at inv
  h5i_step
  all_goals simp_all [abstract, scan, Inv, List.map_append]
  all_goals try scalar_tac
  · constructor
    · right; omega
    · intro v hv
      rcases hv with hv | hv
      · have h := inv.2.2.2 v hv; omega
      · subst v; omega
  all_goals constructor
  all_goals try omega
  all_goals intro v hv
  all_goals have h := inv.2.2.2 v hv
  all_goals omega

@[step] theorem loop_spec (bytes : alloc.vec.Vec U8) (state : coordinates.Scan)
    (room : bytes.val.length < Usize.max) (initial : Inv bytes.val.length 0 state) :
    coordinates.line_boundaries_loop bytes state 0#usize ⦃ out =>
      abstract out = bytes.val.foldl (scan bytes.val.length) (abstract state) ∧
      Inv bytes.val.length bytes.val.length out ⦄ := by
  unfold coordinates.line_boundaries_loop
  apply WP.spec_mono (fold_with_inv bytes.val abstract (scan bytes.val.length)
    (fun s i => Inv bytes.val.length i s)
    (fun st => coordinates.line_boundaries_loop.body bytes st.1 st.2)
    ?_ state 0#usize (by simp) initial)
  · intro out h; simpa using h
  · intro s i hi hinv
    unfold coordinates.line_boundaries_loop.body
    h5i_step
    all_goals try exact hinv
    all_goals try scalar_tac
    · refine ⟨by scalar_tac, state1_post, i4_post, ?_⟩
      simpa only [i4_post] using state1_post1
    · refine ⟨by omega, rfl, ?_⟩
      have eqi : i.val = bytes.val.length := by omega
      simpa only [eqi] using hinv

def model (bytes : List U8) : List Nat :=
  let offsets := (bytes.foldl (scan bytes.length) ([0],0)).1
  if bytes.isEmpty then offsets else offsets ++ [bytes.length]

@[step] theorem line_boundaries_spec (bytes : alloc.vec.Vec U8)
    (room : bytes.val.length < Usize.max) :
    coordinates.line_boundaries bytes ⦃ out =>
      out.val.map (·.val) = model bytes.val ∧
      (∀ v ∈ out.val, v.val ≤ bytes.val.length) ∧ out.val.length ≤ bytes.val.length + 1 ⦄ := by
  unfold coordinates.line_boundaries
  h5i_step
  all_goals simp_all [Inv, abstract, model, List.map_append]
  all_goals try scalar_tac
  have hn : bytes.val ≠ [] := by
    intro eq
    have hz : bytes.val.length = 0 := by rw [eq]; simp
    omega
  have hp := congrArg Prod.fst state_post
  dsimp at hp
  refine ⟨?_, ?_, ?_⟩
  · simp only [hn, ↓reduceIte]
    rw [← hp]
  · intro v hv
    rcases hv with hv | hv
    · exact state_post1.2.2.2 v hv
    · subst v; simp
  · rcases state_post1.2.2.1 with h | h <;> omega

def Ordered (n : Nat) (s : List Nat × Nat) : Prop :=
  s.1.Pairwise (· < ·) ∧ (∀ v ∈ s.1, v ≤ s.2) ∧ (∀ v ∈ s.1, v < n)

theorem scan_ordered (n : Nat) (s : List Nat × Nat) (b : U8) (h : Ordered n s) :
    Ordered n (scan n s b) := by
  by_cases hb : b.val = 10 ∧ s.2 + 1 < n
  · simp only [scan, Ordered, hb, ite_true, ite_false, List.pairwise_append, List.pairwise_singleton, List.mem_singleton,
      List.mem_append, true_and, and_true]
    refine ⟨⟨h.1, ?_⟩, ?_, ?_⟩
    · intro a ha v hv; subst v; have hp := h.2.1 a ha; omega
    · intro v hv; rcases hv with hv | hv
      · have hp := h.2.1 v hv; omega
      · subst v; omega
    · intro v hv; rcases hv with hv | hv
      · exact h.2.2 v hv
      · subst v; exact hb.2
  · simp only [scan, Ordered, hb, ite_true, ite_false]
    exact ⟨h.1, fun v hv => Nat.le_trans (h.2.1 v hv) (by omega), h.2.2⟩

theorem fold_ordered (bytes : List U8) (n : Nat) (s : List Nat × Nat) (h : Ordered n s) :
    Ordered n (bytes.foldl (scan n) s) := by
  induction bytes generalizing s with
  | nil => exact h
  | cons b tail ih => exact ih _ (scan_ordered n s b h)

theorem model_ordered (bytes : List U8) : (model bytes).Pairwise (· < ·) := by
  cases bytes with
  | nil => simp [model]
  | cons b tail =>
    have npos : 0 < (b :: tail).length := by simp
    have start : Ordered (b :: tail).length ([0],0) := by simp [Ordered, npos]
    have h := fold_ordered (b :: tail) (b :: tail).length ([0],0) start
    simp only [model, List.isEmpty_cons, Bool.false_eq_true, ↓reduceIte]
    rw [List.pairwise_append]
    exact ⟨h.1, by simp, by simpa using h.2.2⟩

@[step] theorem line_boundaries_ordered (bytes : alloc.vec.Vec U8)
    (room : bytes.val.length < Usize.max) :
    coordinates.line_boundaries bytes ⦃ out =>
      out.val.map (·.val) = model bytes.val ∧
      (∀ v ∈ out.val, v.val ≤ bytes.val.length) ∧
      out.val.Pairwise (fun u v => u.val < v.val) ⦄ := by
  apply WP.spec_mono (line_boundaries_spec bytes room)
  intro out h
  refine ⟨h.1, h.2.1, ?_⟩
  have sorted := model_ordered bytes.val
  rw [← h.1] at sorted
  simpa only [List.pairwise_map] using sorted

end o5s_kernel.Coordinates
