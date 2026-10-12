import Mathlib.Data.List.Sort
import Mathlib.Data.List.Dedup
/-!
# Line-range merging: the model

A range `(s, e, c)` replaces lines `s..e` of an immutable base with `c`. Two
ranges *conflict* (`NConf`, the same relation as `interval::conflict`) when
they overlap, when an insertion touches the other range, or when they are the
same coordinates. `merge` is the model of `lines::merge`: one pass over ranges
sorted by `(s, e)` that keeps a range unless it equals the last kept one, and
fails on a reversed range or a conflict with the last kept one.

Results:
- `merge_ok`: on sorted input, success means every range is valid, distinct
  ranges never conflict, and the kept ranges are exactly the distinct inputs in
  strictly increasing, separated order.
- `merge_fails`: failure exhibits a reversed range or two distinct conflicting
  ones, so it is never spurious.
- `merge_order_independent`: any two sorted arrangements of the same set of
  ranges give the same answer and the same kept list.
-/
namespace o5s_kernel.LinesModel

variable {α : Type} [DecidableEq α] [Inhabited α]
set_option linter.unusedSectionVars false
set_option linter.unusedVariables false

abbrev R (α : Type) := Nat × Nat × α

def NConf (a z b w : Nat) : Prop :=
  if a = z then b ≤ a ∧ a ≤ w
  else if b = w then a ≤ b ∧ b ≤ z
  else a < w ∧ b < z

instance (a z b w : Nat) : Decidable (NConf a z b w) := by unfold NConf; infer_instance

def Conf (x y : R α) : Prop := NConf x.1 x.2.1 y.1 y.2.1
instance (x y : R α) : Decidable (Conf x y) := by unfold Conf; infer_instance

def Valid (x : R α) : Prop := x.1 ≤ x.2.1
def KeyLe (x y : R α) : Prop := x.1 < y.1 ∨ (x.1 = y.1 ∧ x.2.1 ≤ y.2.1)
instance (x y : R α) : Decidable (KeyLe x y) := by unfold KeyLe; infer_instance
/-- Separation: `x` ends before `y` starts, strictly if either is an insertion. -/
def Sep (x y : R α) : Prop :=
  if x.1 = x.2.1 ∨ y.1 = y.2.1 then x.2.1 < y.1 else x.2.1 ≤ y.1

theorem conf_symm (x y : R α) : Conf x y ↔ Conf y x := by
  simp only [Conf, NConf]; split_ifs <;> omega

/-- Equal coordinates always conflict, whatever the contents. -/
theorem conf_same_key (x y : R α) (h1 : x.1 = y.1) (h2 : x.2.1 = y.2.1) (vx : Valid x) :
    Conf x y := by
  simp only [Conf, NConf, Valid] at *; split_ifs <;> omega

theorem sep_of_sorted (x y : R α) (vx : Valid x) (vy : Valid y) (k : KeyLe x y)
    (n : ¬ Conf x y) : Sep x y := by
  simp only [Conf, NConf, Valid, KeyLe, Sep] at *; split_ifs at * <;> omega

theorem sep_no_conf (x y : R α) (vx : Valid x) (vy : Valid y) (s : Sep x y) :
    ¬ Conf x y := by
  simp only [Conf, NConf, Valid, Sep] at *; split_ifs at * <;> omega

theorem sep_trans (x y z : R α) (vx : Valid x) (vy : Valid y) (vz : Valid z)
    (a : Sep x y) (b : Sep y z) : Sep x z := by
  simp only [Valid, Sep] at *; split_ifs at * <;> omega

theorem sep_key (x y : R α) (vx : Valid x) (vy : Valid y) (s : Sep x y) :
    x.1 < y.1 ∨ (x.1 = y.1 ∧ x.2.1 < y.2.1) := by
  simp only [Valid, Sep] at *; split_ifs at s <;> omega

theorem sep_ne (x y : R α) (vx : Valid x) (vy : Valid y) (s : Sep x y) : x ≠ y := by
  rintro rfl
  rcases sep_key x x vx vx s with h | h <;> omega

theorem keyle_trans (x y z : R α) (a : KeyLe x y) (b : KeyLe y z) : KeyLe x z := by
  unfold KeyLe at *; omega

/-- `rs[i]!`, kept folded so that `simp` leaves it alone. -/
def at_ (rs : List (R α)) (i : Nat) : R α := rs[i]!

/-- The merge state: still accepting, and the indices kept so far. -/
abbrev St := Bool × List Nat

def step (rs : List (R α)) (st : St) (i : Nat) : St :=
  if st.1 = false then st
  else
    let x := (at_ rs i)
    if x.2.1 < x.1 then (false, st.2)
    else match st.2.getLast? with
      | none => (true, st.2 ++ [i])
      | some j =>
        if (at_ rs j) = x then st
        else if Conf (at_ rs j) x then (false, st.2)
        else (true, st.2 ++ [i])

def merge (rs : List (R α)) : St := (List.range rs.length).foldl (step rs) (true, [])

/-- The kept ranges of a merge. -/
def kept (rs : List (R α)) (k : List Nat) : List (R α) := k.map (at_ rs)

def Sorted (rs : List (R α)) : Prop := rs.Pairwise KeyLe

/-- Invariant after the first `i` indices, while still accepting. -/
structure Good (rs : List (R α)) (i : Nat) (k : List Nat) : Prop where
  bound : ∀ j ∈ k, j < i
  valid : ∀ j < i, j < rs.length → Valid (at_ rs j)
  covered : ∀ j < i, j < rs.length → (at_ rs j) ∈ kept rs k
  chain : (kept rs k).Pairwise Sep
  last : ∀ j, k.getLast? = some j → ∀ m < i, m < rs.length → KeyLe (at_ rs m) (at_ rs j)

/-- Failure witness. -/
def Bad (rs : List (R α)) : Prop :=
  (∃ x ∈ rs, ¬ Valid x) ∨ (∃ x ∈ rs, ∃ y ∈ rs, x ≠ y ∧ Conf x y)

theorem getElem!_mem {rs : List (R α)} {i : Nat} (h : i < rs.length) : (at_ rs i) ∈ rs := by
  rw [at_, getElem!_pos rs i h]; exact List.getElem_mem h

theorem kept_valid {rs : List (R α)} {i : Nat} {k : List Nat} (g : Good rs i k)
    (hk : ∀ j ∈ k, j < rs.length) : ∀ y ∈ kept rs k, Valid y := by
  intro y hy
  simp only [kept, List.mem_map] at hy
  obtain ⟨j, hj, rfl⟩ := hy
  exact g.valid j (g.bound j hj) (hk j hj)

theorem sorted_get {rs : List (R α)} (s : Sorted rs) {a b : Nat} (hab : a ≤ b)
    (hb : b < rs.length) : KeyLe (at_ rs a) (at_ rs b) := by
  rcases Nat.lt_or_eq_of_le hab with h | rfl
  · rw [at_, at_, getElem!_pos rs a (by omega), getElem!_pos rs b hb]
    exact List.pairwise_iff_getElem.mp s a b (by omega) hb h
  · unfold KeyLe; right; exact ⟨rfl, le_refl _⟩

theorem step_good (rs : List (R α)) (s : Sorted rs) (i : Nat) (hi : i < rs.length)
    (k : List Nat) (g : Good rs i k) (hk : ∀ j ∈ k, j < rs.length) :
    let st' := step rs (true, k) i
    (st'.1 = true → Good rs (i+1) st'.2 ∧ ∀ j ∈ st'.2, j < rs.length) ∧
    (st'.1 = false → Bad rs) := by
  intro st'
  have xin := getElem!_mem hi
  by_cases hv : (at_ rs i).2.1 < (at_ rs i).1
  · have : st' = (false, k) := by simp [st', step, hv]
    rw [this]
    refine ⟨by simp, fun _ => Or.inl ⟨_, xin, ?_⟩⟩
    unfold Valid; omega
  have vx : Valid (at_ rs i) := by unfold Valid; omega
  -- Adding `i` keeps the invariant when `rs[i]` is separated from all kept ranges.
  have add : ∀ (hsep : ∀ y ∈ kept rs k, Sep y (at_ rs i))
      (hlast : ∀ m < i + 1, m < rs.length → KeyLe (at_ rs m) (at_ rs i)),
      Good rs (i+1) (k ++ [i]) ∧ ∀ j ∈ k ++ [i], j < rs.length := by
    intro hsep hlast
    refine ⟨⟨?_, ?_, ?_, ?_, ?_⟩, ?_⟩
    · intro j hj; simp at hj; rcases hj with hj | rfl
      · have := g.bound j hj; omega
      · omega
    · intro j hj hjl
      rcases Nat.lt_succ_iff_lt_or_eq.mp hj with hj | rfl
      · exact g.valid j hj hjl
      · exact vx
    · intro j hj hjl
      simp only [kept, List.map_append, List.mem_append, List.map_cons, List.map_nil,
        List.mem_singleton]
      rcases Nat.lt_succ_iff_lt_or_eq.mp hj with hj | rfl
      · exact Or.inl (g.covered j hj hjl)
      · exact Or.inr rfl
    · simp only [kept, List.map_append, List.map_cons, List.map_nil]
      rw [List.pairwise_append]
      exact ⟨g.chain, by simp, fun a ha b hb => by simp at hb; subst hb; exact hsep a ha⟩
    · intro j hj m hm hml
      simp at hj; subst hj; exact hlast m hm hml
    · intro j hj; simp at hj; rcases hj with hj | rfl
      · exact hk j hj
      · exact hi
  have hlast : ∀ m < i + 1, m < rs.length → KeyLe (at_ rs m) (at_ rs i) :=
    fun m hm _ => sorted_get s (by omega) hi
  cases hl : k.getLast? with
  | none =>
    have k0 : k = [] := List.getLast?_eq_none_iff.mp hl
    subst k0
    have : st' = (true, [i]) := by simp [st', step, hv]
    rw [this]
    refine ⟨fun _ => ?_, by simp⟩
    simpa using add (by simp [kept]) hlast
  | some j =>
    have jk : j ∈ k := List.mem_of_getLast? hl
    have jl := hk j jk
    have jin := getElem!_mem jl
    have vj : Valid (at_ rs j) := g.valid j (g.bound j jk) jl
    by_cases he : (at_ rs j) = (at_ rs i)
    · have : st' = (true, k) := by simp [st', step, hv, hl, he]
      rw [this]
      refine ⟨fun _ => ⟨⟨fun j hj => by have := g.bound j hj; omega, ?_, ?_, g.chain, ?_⟩, hk⟩,
        by simp⟩
      · intro m hm hml
        rcases Nat.lt_succ_iff_lt_or_eq.mp hm with hm | rfl
        · exact g.valid m hm hml
        · exact vx
      · intro m hm hml
        rcases Nat.lt_succ_iff_lt_or_eq.mp hm with hm | rfl
        · exact g.covered m hm hml
        · rw [← he]; simp only [kept, List.mem_map]; exact ⟨j, jk, rfl⟩
      · intro j' hj' m hm hml
        rw [hl] at hj'; cases hj'
        rcases Nat.lt_succ_iff_lt_or_eq.mp hm with hm | rfl
        · exact g.last j hl m hm hml
        · rw [he]; unfold KeyLe; right; exact ⟨rfl, le_refl _⟩
    · by_cases hc : Conf (at_ rs j) (at_ rs i)
      · have : st' = (false, k) := by simp [st', step, hv, hl, he, hc]
        rw [this]
        exact ⟨by simp, fun _ => Or.inr ⟨_, jin, _, xin, he, hc⟩⟩
      · have : st' = (true, k ++ [i]) := by simp [st', step, hv, hl, he, hc]
        rw [this]
        refine ⟨fun _ => add ?_ hlast, by simp⟩
        -- `rs[i]` is separated from the last kept range, hence from all of them.
        have sj : Sep (at_ rs j) (at_ rs i) :=
          sep_of_sorted _ _ vj vx (sorted_get s (by have := g.bound j jk; omega) hi) hc
        have kv := kept_valid g hk
        intro y hy
        by_cases hyj : y = (at_ rs j)
        · rw [hyj]; exact sj
        · -- `y` comes before the last kept range in the chain.
          have hk' : kept rs k = (kept rs k).dropLast ++ [(at_ rs j)] := by
            have hk0 : k.dropLast ++ [j] = k := List.dropLast_append_getLast? _ (by simp [hl])
            simp only [kept]
            rw [← List.map_dropLast]
            conv_lhs => rw [← hk0]
            simp
          have hc' := g.chain
          rw [hk', List.pairwise_append] at hc'
          have hy' : y ∈ (kept rs k).dropLast := by
            rw [hk'] at hy; simp at hy; tauto
          have syj : Sep y (at_ rs j) := hc'.2.2 y hy' _ (by simp)
          exact sep_trans _ _ _ (kv y hy) vj vx syj sj

theorem bad_step (rs : List (R α)) (st : St) (i : Nat) (h : st.1 = false) :
    step rs st i = st := by simp [step, h]

/-- The fold, generalised over a starting index and state. -/
theorem fold_spec (rs : List (R α)) (s : Sorted rs) :
    ∀ (n i : Nat) (st : St), i + n = rs.length →
      (st.1 = true → Good rs i st.2 ∧ ∀ j ∈ st.2, j < rs.length) →
      (st.1 = false → Bad rs) →
      let r := (List.range' i n).foldl (step rs) st
      (r.1 = true → Good rs rs.length r.2 ∧ ∀ j ∈ r.2, j < rs.length) ∧
      (r.1 = false → Bad rs) := by
  intro n
  induction n with
  | zero =>
    intro i st hn ht hf
    simp only [List.range'_zero, List.foldl_nil]
    obtain rfl : i = rs.length := by omega
    exact ⟨ht, hf⟩
  | succ n ih =>
    intro i st hn ht hf
    simp only [List.range'_succ, List.foldl_cons]
    apply ih (i+1) _ (by omega)
    · cases hb : st.1
      · rw [bad_step rs st i hb]; simp [hb]
      · obtain ⟨g, hk⟩ := ht hb
        have e : st = (true, st.2) := by ext <;> simp [hb]
        rw [e]; exact (step_good rs s i (by omega) st.2 g hk).1
    · cases hb : st.1
      · rw [bad_step rs st i hb]; exact fun _ => hf hb
      · obtain ⟨g, hk⟩ := ht hb
        have e : st = (true, st.2) := by ext <;> simp [hb]
        rw [e]; exact (step_good rs s i (by omega) st.2 g hk).2

theorem merge_good (rs : List (R α)) (s : Sorted rs) :
    ((merge rs).1 = true → Good rs rs.length (merge rs).2 ∧ ∀ j ∈ (merge rs).2, j < rs.length) ∧
    ((merge rs).1 = false → Bad rs) := by
  have := fold_spec rs s rs.length 0 (true, []) (by simp)
    (fun _ => ⟨⟨by simp, by simp, by simp, by simp [kept], by simp⟩, by simp⟩) (by simp)
  simpa [merge, List.range_eq_range'] using this

/-- Success: every range is valid, distinct ranges never conflict, and the
kept ranges are the inputs without repetition, pairwise separated. -/
theorem merge_ok (rs : List (R α)) (s : Sorted rs) (h : (merge rs).1 = true) :
    (∀ x ∈ rs, Valid x) ∧ (∀ x ∈ rs, ∀ y ∈ rs, x ≠ y → ¬ Conf x y) ∧
    (∀ x, x ∈ kept rs (merge rs).2 ↔ x ∈ rs) ∧ (kept rs (merge rs).2).Pairwise Sep := by
  obtain ⟨g, hk⟩ := (merge_good rs s).1 h
  have kv := kept_valid g hk
  have mem : ∀ x, x ∈ kept rs (merge rs).2 ↔ x ∈ rs := by
    intro x; constructor
    · intro hx; simp only [kept, List.mem_map] at hx
      obtain ⟨j, hj, rfl⟩ := hx; exact getElem!_mem (hk j hj)
    · intro hx
      obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hx
      have e : rs[j] = at_ rs j := by rw [at_, getElem!_pos rs j hj]
      rw [e]; exact g.covered j hj hj
  have valid : ∀ x ∈ rs, Valid x := fun x hx => kv x ((mem x).2 hx)
  refine ⟨valid, ?_, mem, g.chain⟩
  intro x hx y hy hne
  have hx' := (mem x).2 hx
  have hy' := (mem y).2 hy
  -- Both are kept; the chain is pairwise separated in one direction or the other.
  obtain ⟨a, ha, rfl⟩ := List.getElem_of_mem hx'
  obtain ⟨b, hb, rfl⟩ := List.getElem_of_mem hy'
  have hab : a ≠ b := by rintro rfl; exact hne rfl
  rcases Nat.lt_or_gt_of_ne hab with l | l
  · exact sep_no_conf _ _ (kv _ hx') (kv _ hy') (List.pairwise_iff_getElem.mp g.chain a b ha hb l)
  · intro c
    rw [conf_symm] at c
    exact sep_no_conf _ _ (kv _ hy') (kv _ hx')
      (List.pairwise_iff_getElem.mp g.chain b a hb ha l) c

/-- Failure is never spurious. -/
theorem merge_fails (rs : List (R α)) (s : Sorted rs) (h : (merge rs).1 = false) : Bad rs :=
  (merge_good rs s).2 h

theorem not_bad_of_ok (rs : List (R α)) (s : Sorted rs) (h : (merge rs).1 = true) : ¬ Bad rs := by
  obtain ⟨v, c, -, -⟩ := merge_ok rs s h
  rintro (⟨x, hx, nv⟩ | ⟨x, hx, y, hy, ne, cf⟩)
  · exact nv (v x hx)
  · exact c x hx y hy ne cf

/-- Strict key order, which separated valid ranges follow. -/
def KeyLt (x y : R α) : Prop := x.1 < y.1 ∨ (x.1 = y.1 ∧ x.2.1 < y.2.1)

theorem kept_keylt (rs : List (R α)) (s : Sorted rs) (h : (merge rs).1 = true) :
    (kept rs (merge rs).2).Pairwise KeyLt := by
  obtain ⟨v, -, mem, ch⟩ := merge_ok rs s h
  exact ch.imp_of_mem fun {a b} ha hb sab =>
    sep_key a b (v a ((mem a).1 ha)) (v b ((mem b).1 hb)) sab

theorem keylt_irrefl (x : R α) : ¬ KeyLt x x := fun h => by unfold KeyLt at h; omega

/-- Two lists strictly sorted by key with the same members are equal. -/
theorem eq_of_keylt : ∀ (l₁ l₂ : List (R α)), l₁.Pairwise KeyLt → l₂.Pairwise KeyLt →
    (∀ x, x ∈ l₁ ↔ x ∈ l₂) → l₁ = l₂
  | [], [], _, _, _ => rfl
  | [], b :: _, _, _, m => absurd ((m b).2 (by simp)) (by simp)
  | a :: _, [], _, _, m => absurd ((m a).1 (by simp)) (by simp)
  | a :: as, b :: bs, h₁, h₂, m => by
    have p₁ := List.pairwise_cons.1 h₁
    have p₂ := List.pairwise_cons.1 h₂
    have hab : a = b := by
      by_contra ne
      have ha : a ∈ bs := by
        have := (m a).1 (by simp); simp only [List.mem_cons] at this
        exact this.resolve_left ne
      have hb : b ∈ as := by
        have := (m b).2 (by simp); simp only [List.mem_cons] at this
        exact this.resolve_left (Ne.symm ne)
      have x := p₂.1 a ha
      have y := p₁.1 b hb
      unfold KeyLt at x y; omega
    subst hab
    congr 1
    apply eq_of_keylt as bs p₁.2 p₂.2
    intro x
    have mx := m x
    simp only [List.mem_cons] at mx
    constructor
    · intro hx
      rcases mx.1 (Or.inr hx) with rfl | h
      · exact absurd (p₁.1 x hx) (keylt_irrefl x)
      · exact h
    · intro hx
      rcases mx.2 (Or.inr hx) with rfl | h
      · exact absurd (p₂.1 x hx) (keylt_irrefl x)
      · exact h

/-- Order independence: two sorted arrangements of the same set of ranges
succeed together, and then keep the same ranges in the same order. -/
theorem merge_order_independent (rs₁ rs₂ : List (R α)) (s₁ : Sorted rs₁) (s₂ : Sorted rs₂)
    (same : ∀ x, x ∈ rs₁ ↔ x ∈ rs₂) :
    (merge rs₁).1 = (merge rs₂).1 ∧
    ((merge rs₁).1 = true → kept rs₁ (merge rs₁).2 = kept rs₂ (merge rs₂).2) := by
  have bad : Bad rs₁ ↔ Bad rs₂ := by unfold Bad; simp only [same]
  have ok_iff : ∀ (rs : List (R α)) (s : Sorted rs), (merge rs).1 = true ↔ ¬ Bad rs := fun rs s =>
    ⟨not_bad_of_ok rs s, fun nb => by
      cases h : (merge rs).1
      · exact absurd (merge_fails rs s h) nb
      · rfl⟩
  have e : (merge rs₁).1 = (merge rs₂).1 := by
    rw [Bool.eq_iff_iff, ok_iff rs₁ s₁, ok_iff rs₂ s₂, bad]
  refine ⟨e, fun h => ?_⟩
  have h₂ : (merge rs₂).1 = true := e ▸ h
  apply eq_of_keylt _ _ (kept_keylt rs₁ s₁ h) (kept_keylt rs₂ s₂ h₂)
  intro x
  rw [(merge_ok rs₁ s₁ h).2.2.1, (merge_ok rs₂ s₂ h₂).2.2.1, same]

end o5s_kernel.LinesModel
