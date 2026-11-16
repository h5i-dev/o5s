import O5sKernel
import H5iAppLib
open Aeneas Aeneas.Std Result o5s_kernel H5iAppLib

namespace o5s_kernel.Proofs

theorem prefix_iff_get {α} (p s : List α) :
    p <+: s ↔ p.length ≤ s.length ∧ ∀ j (h1 : j < p.length) (h2 : j < s.length), s[j] = p[j] := by
  constructor
  · rintro ⟨t, rfl⟩
    refine ⟨by simp, fun j h1 h2 => ?_⟩
    simp [List.getElem_append_left h1]
  · rintro ⟨hl, h⟩
    rw [List.prefix_iff_eq_take]
    apply List.ext_getElem (by simp; omega)
    intro j h1 h2
    simp only [List.getElem_take]
    exact (h j h1 (by omega)).symm

@[step] theorem starts_with_spec (s p : alloc.vec.Vec U8) :
    names.starts_with s p ⦃ b => (b = true ↔ p.val <+: s.val) ⦄ := by
  unfold names.starts_with names.starts_with_loop
  simp only [alloc.vec.Vec.len]
  split
  · rename_i hl
    simp only [WP.spec_ok, Bool.false_eq_true, false_iff]
    intro h; have := h.length_le
    simp only [gt_iff_lt] at hl
    scalar_tac
  · rename_i hl
    apply loop_idx_spec _ (fun i => i) p.length
      (fun i => p.length ≤ s.length ∧ ∀ j (h1 : j < i.val) (h2 : j < s.val.length) (h3 : j < p.val.length),
        s.val[j] = p.val[j])
    · intro i ⟨hps, hall⟩ hi
      unfold names.starts_with_loop.body
      simp only [alloc.vec.Vec.len]
      step*
      · simp only [Bool.false_eq_true, false_iff]
        intro hp
        have hil : i.val < p.length := by scalar_tac
        have := (prefix_iff_get _ _).1 hp |>.2 i.val (by simpa [alloc.vec.Vec.length] using hil)
          (by simp [alloc.vec.Vec.length] at hps; scalar_tac)
        simp_all
      · simp only [true_iff]
        have hip : i.val = p.length := by scalar_tac
        rw [prefix_iff_get]
        refine ⟨by simpa [alloc.vec.Vec.length] using hps, fun j h1 h2 => ?_⟩
        exact hall j (by simp [alloc.vec.Vec.length] at hip; omega) h2 h1
    · refine ⟨?_, fun j h1 => by simp at h1⟩
      simp only [gt_iff_lt] at hl; scalar_tac
    · simp

/-- Segment-aligned prefix: `s` is `p` or lies under `p/`; `[]` covers everything. -/
def isUnderL (s p : List U8) : Prop :=
  p = [] ∨ (p <+: s ∧ (s.length = p.length ∨ s[p.length]? = some 47#u8))

@[step] theorem is_under_spec (s p : alloc.vec.Vec U8) :
    names.is_under s p ⦃ b => (b = true ↔ isUnderL s.val p.val) ⦄ := by
  unfold names.is_under
  simp only [alloc.vec.Vec.len]
  step*
  · simp only [true_iff, isUnderL]; left
    have : p.val.length = 0 := by scalar_tac
    exact List.length_eq_zero_iff.mp this
  · simp only [true_iff, isUnderL]; right
    refine ⟨b_post.1 ‹_›, Or.inl ?_⟩; scalar_tac
  · have := (b_post.1 ‹_›).length_le
    have : s.val.length ≠ p.val.length := by intro h; apply ‹¬_ = _›; scalar_tac
    scalar_tac
  · have hpl : p.val.length < s.val.length := by
      have := (b_post.1 ‹_›).length_le
      have : s.val.length ≠ p.val.length := by intro h; apply ‹¬_ = _›; scalar_tac
      omega
    have hne : p.val ≠ [] := by intro h; apply ‹¬_ = 0#usize›; simp [h]; rfl
    simp only [isUnderL, hne, false_or, decide_eq_true_eq]
    rw [i4_post]
    simp only [Usize.ofNatCore_val_eq, List.getElem?_eq_getElem hpl, Option.some.injEq]
    constructor
    · intro h; exact ⟨b_post.1 ‹_›, Or.inr h⟩
    · rintro ⟨_, h | h⟩
      · omega
      · exact h
  · simp only [Bool.false_eq_true, false_iff, isUnderL, not_or]
    have hne : p.val ≠ [] := by intro h; apply ‹¬_ = 0#usize›; simp [h]; rfl
    refine ⟨hne, fun ⟨h, _⟩ => ‹¬b = true› (b_post.2 h)⟩

/-- Lexicographic order of byte strings, as -1, 0 or 1 (the order of `BTreeMap<Name, _>`). -/
def cmpL : List U8 → List U8 → Int
  | [], [] => 0
  | [], _ :: _ => -1
  | _ :: _, [] => 1
  | x :: xs, y :: ys => if x.val < y.val then -1 else if y.val < x.val then 1 else cmpL xs ys

theorem cmpL_drop (a b : List U8) (i : Nat) (ha : i ≤ a.length) (hb : i ≤ b.length)
    (h : ∀ j (_ : j < i) (h2 : j < a.length) (h3 : j < b.length), a[j] = b[j]) :
    cmpL a b = cmpL (a.drop i) (b.drop i) := by
  induction i generalizing a b with
  | zero => simp
  | succ k ih =>
    match a, b with
    | [], _ => simp at ha
    | _, [] => simp at hb
    | x :: xs, y :: ys =>
      have hxy : x = y := h 0 (by omega) (by simp) (by simp)
      subst hxy
      simp only [List.drop_succ_cons]
      rw [← ih xs ys (by simp at ha; omega) (by simp at hb; omega)
        (fun j h1 h2 h3 => by
          have := h (j + 1) (by omega) (by simp; omega) (by simp; omega)
          rwa [List.getElem_cons_succ, List.getElem_cons_succ] at this)]
      simp [cmpL]

theorem cmpL_refl (a : List U8) : cmpL a a = 0 := by
  induction a with
  | nil => rfl
  | cons x xs ih => simp [cmpL, ih]

@[step] theorem cmp_loop_spec (a b : alloc.vec.Vec U8) (n : Usize) (hn : n.val = min a.length b.length) :
    names.cmp_loop a b n 0#usize ⦃ r => r.val = cmpL a.val b.val ⦄ := by
  unfold names.cmp_loop
  apply loop_idx_spec _ (fun i => i) n.val
    (fun i => ∀ j (h1 : j < i.val) (h2 : j < a.val.length) (h3 : j < b.val.length), a.val[j] = b.val[j])
  · intro i hall hi
    unfold names.cmp_loop.body
    simp only [alloc.vec.Vec.len]
    have hia : i.val ≤ a.val.length := by simp [alloc.vec.Vec.length] at hn; omega
    have hib : i.val ≤ b.val.length := by simp [alloc.vec.Vec.length] at hn; omega
    have hdrop := cmpL_drop a.val b.val i.val hia hib hall
    step*
    · have h1 : i.val < a.val.length := by scalar_tac
      have h2 : i.val < b.val.length := by scalar_tac
      rw [hdrop, List.drop_eq_getElem_cons h1, List.drop_eq_getElem_cons h2]
      simp only [cmpL]; rw [← i1_post, ← i2_post]
      split
      · rfl
      · scalar_tac
    · have h1 : i.val < a.val.length := by scalar_tac
      have h2 : i.val < b.val.length := by scalar_tac
      rw [hdrop, List.drop_eq_getElem_cons h1, List.drop_eq_getElem_cons h2]
      simp only [cmpL]; rw [← i1_post, ← i2_post]
      split
      · scalar_tac
      · split
        · rfl
        · scalar_tac
    · have hi' : i.val = a.val.length := by scalar_tac
      rw [hdrop, hi', List.drop_length]
      have : a.val.length < b.val.length := by scalar_tac
      rw [List.drop_eq_getElem_cons this]; rfl
    · have hi' : i.val = b.val.length := by scalar_tac
      rw [hdrop, hi', List.drop_length]
      have : b.val.length < a.val.length := by scalar_tac
      rw [List.drop_eq_getElem_cons this]; rfl
    · have ha : i.val = a.val.length := by scalar_tac
      have hb : i.val = b.val.length := by scalar_tac
      rw [hdrop]; nth_rewrite 1 [ha]; rw [hb, List.drop_length, List.drop_length]; rfl
  · intro j h; simp at h
  · simp

@[step] theorem cmp_spec (a b : alloc.vec.Vec U8) :
    names.cmp a b ⦃ r => r.val = cmpL a.val b.val ⦄ := by
  unfold names.cmp
  simp only [alloc.vec.Vec.len]
  apply WP.spec_bind (Pₘ := fun n : Usize => n.val = min a.length b.length)
  · split <;> (rw [WP.spec_ok]; simp only [Usize.ofNatCore_val_eq]; scalar_tac)
  · intro n hn; exact cmp_loop_spec a b n hn

/-- `j` is the governing rule's index among `rules` for `nm` (looking at the first `i` rules):
it covers `nm`, no covering rule is longer, and earlier ones are strictly shorter. `none`: no
rule covers `nm`. -/
def GovernsUpTo (rules : List (alloc.vec.Vec U8 × Rule)) (nm : List U8) (i : Nat) : Option Usize → Prop
  | none => ∀ k (h : k < rules.length), k < i → ¬ isUnderL nm rules[k].1.val
  | some j => ∃ hj : j.val < rules.length, j.val < i ∧ isUnderL nm rules[j.val].1.val ∧
      ∀ k (h : k < rules.length), k < i → isUnderL nm rules[k].1.val →
        rules[k].1.val.length ≤ rules[j.val].1.val.length ∧
        (k < j.val → rules[k].1.val.length < rules[j.val].1.val.length)

/-- The loop's state: what the first `i` rules govern, and the governing prefix's length. -/
def GovInv (rules : List (alloc.vec.Vec U8 × Rule)) (nm : List U8) (best : Option Usize) (bl i : Usize) : Prop :=
  GovernsUpTo rules nm i.val best ∧ i.val ≤ rules.length ∧
    (∀ j : Usize, best = some j → ∃ hj : j.val < rules.length, bl.val = rules[j.val].1.val.length)

@[step] theorem pick_spec (best : Option Usize) (bl i plen : Usize) (covers : Bool) :
    names.pick best bl i plen covers ⦃ r =>
      r = (if covers = true then
             (match best with
              | none => (some i, plen)
              | some _ => if bl.val < plen.val then (some i, plen) else (best, bl))
           else (best, bl)) ⦄ := by
  unfold names.pick
  cases covers <;> cases best <;> simp <;> split <;> simp_all

theorem governing_loop_spec (rules : alloc.vec.Vec (alloc.vec.Vec U8 × Rule)) (nm : alloc.vec.Vec U8) :
    names.governing_loop rules nm none 0#usize 0#usize ⦃ best => GovernsUpTo rules.val nm.val rules.val.length best ⦄ := by
  unfold names.governing_loop
  apply loop_idx_spec _ (fun x => x.2.2) rules.val.length (fun x => GovInv rules.val nm.val x.1 x.2.1 x.2.2)
  · rintro ⟨best, bl, i⟩ hinv _
    obtain ⟨hg, hi, hbl⟩ := hinv
    unfold names.governing_loop.body
    simp only [alloc.vec.Vec.len]
    step*
    have hil : i.val < rules.val.length := by scalar_tac
    have hv : v = (rules.val[i.val]'hil).1 := by
      have := congrArg Prod.fst v_post; simp only at this; rw [this]
    have hvl : (Usize.ofNatCore v.val.length (by scalar_tac)).val = v.val.length := by simp
    simp only at hg hbl
    refine ⟨⟨?_, by scalar_tac, ?_⟩, by scalar_tac, by scalar_tac⟩
    · -- What the first i+1 rules govern.
      rw [i3_post]
      by_cases hc : covers = true
      · have hcov : isUnderL nm.val rules.val[i.val].1.val := hv ▸ covers_post.1 hc
        simp only [hc, if_true] at best1_post
        cases best with
        | none =>
          simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
          refine ⟨hil, by omega, hcov, fun k hk hk1 hck => ?_⟩
          by_cases hki : k < i.val
          · exact absurd hck (hg k hk hki)
          · have : k = i.val := by omega
            subst this; exact ⟨le_refl _, fun h => absurd h (lt_irrefl _)⟩
        | some j0 =>
          obtain ⟨hj0, hj0i, hj0c, hj0m⟩ := hg
          obtain ⟨_, hblj⟩ := hbl j0 rfl
          simp only at best1_post
          split at best1_post
          · rename_i hlt
            simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
            (try rw [hvl] at hlt); (try rw [hv] at hlt)
            refine ⟨hil, by omega, hcov, fun k hk hk1 hck => ?_⟩
            by_cases hki : k < i.val
            · have := (hj0m k hk hki hck).1; exact ⟨by omega, fun _ => by omega⟩
            · have : k = i.val := by omega
              subst this; exact ⟨le_refl _, fun h => absurd h (lt_irrefl _)⟩
          · rename_i hnlt
            simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
            (try rw [hvl] at hnlt); (try rw [hv] at hnlt)
            refine ⟨hj0, by omega, hj0c, fun k hk hk1 hck => ?_⟩
            by_cases hki : k < i.val
            · exact hj0m k hk hki hck
            · have : k = i.val := by omega
              subst this; exact ⟨by omega, fun h => by omega⟩
      · have hncov : ¬ isUnderL nm.val rules.val[i.val].1.val := fun h => hc (covers_post.2 (hv ▸ h))
        simp only [hc, if_false, Bool.false_eq_true, Prod.mk.injEq] at best1_post
        obtain ⟨rfl, -⟩ := best1_post
        cases best1 with
        | none =>
          intro k hk hk1
          by_cases hki : k < i.val
          · exact hg k hk hki
          · have : k = i.val := by omega
            subst this; exact hncov
        | some j0 =>
          obtain ⟨hj0, hj0i, hj0c, hj0m⟩ := hg
          refine ⟨hj0, by omega, hj0c, fun k hk hk1 hck => ?_⟩
          by_cases hki : k < i.val
          · exact hj0m k hk hki hck
          · have : k = i.val := by omega
            subst this; exact absurd hck hncov
    · -- The best length is the governing prefix's length.
      intro j hj
      by_cases hc : covers = true
      · simp only [hc, if_true] at best1_post
        cases best with
        | none =>
          simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
          subst h1; subst h2; cases hj
          exact ⟨hil, by rw [hvl, hv]⟩
        | some j0 =>
          simp only at best1_post
          split at best1_post
          · simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
            subst h1; subst h2; cases hj
            exact ⟨hil, by rw [hvl, hv]⟩
          · simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
            subst h1; subst h2; exact hbl j hj
      · simp only [hc, if_false, Bool.false_eq_true, Prod.mk.injEq] at best1_post
        obtain ⟨h1, h2⟩ := best1_post
        subst h1; subst h2; exact hbl j hj
  · refine ⟨fun k _ h => by simp at h, by simp, fun j h => by simp at h⟩
  · simp

@[step] theorem governing_spec (rules : alloc.vec.Vec (alloc.vec.Vec U8 × Rule)) (nm : alloc.vec.Vec U8) :
    names.governing rules nm ⦃ r => GovernsUpTo rules.val nm.val rules.val.length r.2 ∧
      (r.2 = none → r.1 = Rule.Mutable) ∧
      (∀ j : Usize, r.2 = some j → ∃ hj : j.val < rules.val.length, r.1 = rules.val[j.val].2) ⦄ := by
  unfold names.governing
  apply WP.spec_bind (Pₘ := fun best => GovernsUpTo rules.val nm.val rules.val.length best)
  · exact governing_loop_spec rules nm
  · intro best hb
    cases best with
    | none => simp only [WP.spec_ok]; exact ⟨hb, fun _ => trivial, fun j h => by simp at h⟩
    | some j =>
      have hb' := hb
      obtain ⟨hj, _⟩ := hb
      simp only
      step*

/-- `s` continues past `from + lit` with exactly `lit` at `from`. -/
def restHasL (s : List U8) (f : Nat) (lit : List U8) : Prop :=
  f + lit.length < s.length ∧ ∀ j (h1 : j < lit.length) (h2 : f + j < s.length), s[f + j] = lit[j]

@[step] theorem rest_has_spec (s : alloc.vec.Vec U8) (f : Usize) (lit : Slice U8) :
    names.rest_has s f lit ⦃ b => (b = true ↔ restHasL s.val f.val lit.val) ⦄ := by
  unfold names.rest_has names.rest_has_loop
  simp only [alloc.vec.Vec.len, Slice.len]
  step*
  · exact iff_of_false (by simp) (fun ⟨h, _⟩ => by scalar_tac)
  · exact iff_of_false (by simp) (fun ⟨h, _⟩ => by scalar_tac)
  · apply loop_idx_spec _ (fun i => i) lit.val.length
      (fun i => f.val + lit.val.length < s.val.length ∧
        ∀ j (h1 : j < i.val) (h2 : j < lit.val.length) (h3 : f.val + j < s.val.length), s.val[f.val + j] = lit.val[j])
    · intro i ⟨hlen, hall⟩ hi
      unfold names.rest_has_loop.body
      simp only [Slice.len]
      step*
      · refine iff_of_false (by simp) (fun ⟨_, hall'⟩ => ?_)
        have := hall' i.val (by scalar_tac) (by scalar_tac)
        simp only [i2_post] at i3_post
        simp_all
      · refine iff_of_true trivial ⟨hlen, fun j h1 h2 => hall j (by scalar_tac) h1 h2⟩
    · exact ⟨by scalar_tac, fun j h => by simp at h⟩
    · simp

def headsL : List U8 := [104#u8, 101#u8, 97#u8, 100#u8, 115#u8, 47#u8]
def releasesL : List U8 := [114#u8, 101#u8, 108#u8, 101#u8, 97#u8, 115#u8, 101#u8, 115#u8, 47#u8]

open Classical in
/-- How a name may move under a rule whose namespace is `p` (the spec of `names::mobility`). -/
noncomputable def mobilityL (rule : Rule) (p nm : List U8) : Mobility :=
  match rule with
  | .Mutable => .Mutable
  | .Immutable => .CreateOnce
  | .HeadRelease =>
    if p ≠ [] ∧ nm.length ≤ p.length then .Forbidden
    else
      let f := if p = [] then 0 else p.length + 1
      if restHasL nm f headsL then .Mutable
      else if restHasL nm f releasesL then .CreateOnce
      else .Forbidden

@[step] theorem lift_to_slice_spec {α : Type} {n : Usize} (a : Array α n) :
    lift (Array.to_slice a) ⦃ sl => sl.val = a.val ⦄ := by
  simp [lift, WP.spec_ok, Array.val_to_slice]

theorem heads_val : names.HEADS.val = headsL := by unfold names.HEADS; rfl
theorem releases_val : names.RELEASES.val = releasesL := by unfold names.RELEASES; rfl

@[step] theorem mobility_spec (rule : Rule) (p nm : alloc.vec.Vec U8) :
    names.mobility rule p nm ⦃ m => m = mobilityL rule p.val nm.val ⦄ := by
  unfold names.mobility
  cases rule
  · simp [mobilityL]
  · simp [mobilityL]
  · simp only [alloc.vec.Vec.len]
    split
    · rename_i hp
      split
      · rename_i hge
        simp only [WP.spec_ok, mobilityL]
        rw [if_pos ⟨by intro h; simp [h] at hp, by scalar_tac⟩]
      · rename_i hlt
        apply WP.spec_bind (Pₘ := fun f : Usize => f.val = p.val.length + 1)
        · split
          · rename_i h0; exfalso; scalar_tac
          · step*
        · intro f hf
          step*
          all_goals
          have hpne : p.val ≠ [] := by intro h; simp [h] at hp
          have hcond : ¬ (p.val ≠ [] ∧ nm.val.length ≤ p.val.length) := fun ⟨_, h⟩ => hlt (by scalar_tac)
          have hfv : (if p.val = [] then 0 else p.val.length + 1) = f.val := by rw [if_neg hpne, hf]
          simp only [mobilityL, if_neg hcond, hfv]
          rw [s_post, heads_val] at b_post
          try rw [s1_post, releases_val] at b1_post
          split_ifs <;> simp_all
    · rename_i hp
      apply WP.spec_bind (Pₘ := fun f : Usize => f.val = 0)
      · split
        · simp
        · rename_i h0; exfalso; apply hp; scalar_tac
      · intro f hf
        step*
        all_goals
        have hpe : p.val = [] := by
          have : p.val.length = 0 := by scalar_tac
          exact List.length_eq_zero_iff.mp this
        have hcond : ¬ (p.val ≠ [] ∧ nm.val.length ≤ p.val.length) := fun ⟨h, _⟩ => h hpe
        have hfv : (if p.val = [] then 0 else p.val.length + 1) = f.val := by rw [if_pos hpe, hf]
        simp only [mobilityL, if_neg hcond, hfv]
        rw [s_post, heads_val] at b_post
        try rw [s1_post, releases_val] at b1_post
        split_ifs <;> simp_all

/-- The governing rule of `nm` and how it lets `nm` move. -/
def MobilityOf (rules : List (alloc.vec.Vec U8 × Rule)) (nm : List U8) (r : Rule × Mobility) : Prop :=
  ∃ at_ : Option Usize, GovernsUpTo rules nm rules.length at_ ∧
    (at_ = none → r.1 = Rule.Mutable ∧ r.2 = mobilityL Rule.Mutable [] nm) ∧
    (∀ j : Usize, at_ = some j → ∃ hj : j.val < rules.length,
      r.1 = rules[j.val].2 ∧ r.2 = mobilityL rules[j.val].2 rules[j.val].1.val nm)

@[step] theorem mobility_of_spec (rules : alloc.vec.Vec (alloc.vec.Vec U8 × Rule)) (nm : alloc.vec.Vec U8) :
    names.mobility_of rules nm ⦃ r => MobilityOf rules.val nm.val r ⦄ := by
  unfold names.mobility_of
  step*
  · have hr := rule_post1 ‹_›
    subst hr
    refine ⟨none, by simpa [‹«at» = none›] using rule_post, fun _ => ⟨rfl, by simpa using m_post⟩,
      fun j h => by simp at h⟩
  · obtain ⟨hj, hr⟩ := rule_post2 j ‹_›
    have hv : v = ((rules.val)[j.val]'hj).1 := by
      have := congrArg Prod.fst v_post; simp only at this; rw [this]
    refine ⟨some j, by simpa [‹«at» = some j›] using rule_post, fun h => by simp at h, fun j' h => ?_⟩
    simp only [Option.some.injEq] at h; subst h
    exact ⟨hj, hr, by rw [m_post, hr, hv]⟩

theorem cmpL_eq_zero (a b : List U8) : cmpL a b = 0 ↔ a = b := by
  induction a generalizing b with
  | nil => cases b <;> simp [cmpL]
  | cons x xs ih =>
    cases b with
    | nil => simp [cmpL]
    | cons y ys =>
      simp only [cmpL]
      by_cases h1 : x.val < y.val
      · simp only [h1, if_true]
        constructor
        · intro h; omega
        · intro h; injection h with hx; subst hx; omega
      · by_cases h2 : y.val < x.val
        · simp only [h1, h2, if_true, if_false]
          constructor
          · intro h; omega
          · intro h; injection h with hx; subst hx; omega
        · have : x = y := by scalar_tac
          subst this; simp [h1, ih]

@[step] theorem observed_eq_spec (a b : Observed) :
    Observed.Insts.CoreCmpPartialEqObserved.eq a b ⦃ r => (r = true ↔ a = b) ⦄ := by
  unfold Observed.Insts.CoreCmpPartialEqObserved.eq
  cases a <;> cases b <;>
    simp [read_discriminant, Observed.read_discriminant, lift, WP.spec_ok, core.cmp.impls.PartialEqU64.eq]

@[step] theorem observed_ne_spec (a b : Observed) :
    core.cmp.PartialEq.ne.trait_default Observed.Insts.CoreCmpPartialEqObserved a b ⦃ r => (r = true ↔ a ≠ b) ⦄ := by
  unfold core.cmp.PartialEq.ne.trait_default
  apply WP.spec_bind (Pₘ := fun r => (r = true ↔ a = b))
  · exact observed_eq_spec a b
  · intro r hr; simp only [WP.spec_ok]; cases r <;> simp_all

@[step] theorem same_as_last_spec (out : alloc.vec.Vec (alloc.vec.Vec U8 × Observed)) (nm : alloc.vec.Vec U8) (cur : Observed) :
    refs.same_as_last out nm cur ⦃ b => (b = true ↔ ∃ h : out.val ≠ [], (out.val.getLast h).1.val = nm.val ∧ (out.val.getLast h).2 = cur) ⦄ := by
  unfold refs.same_as_last
  step*
  · have : out.val = [] := List.length_eq_zero_iff.mp (by scalar_tac)
    simp [this]
  · have hne : out.val ≠ [] := by intro h; simp [h] at i2_post1
    have hlast : out.val.getLast hne = (out.val)[i2.val]'(by scalar_tac) := by
      rw [List.getLast_eq_getElem]; congr 1; scalar_tac
    have hv : (v, o) = out.val.getLast hne := by rw [hlast, v_post]
    have hcmp : v.val = nm.val := (cmpL_eq_zero _ _).1 (by rw [← i3_post]; simp_all)
    rw [b_post]
    constructor
    · intro h; refine ⟨hne, ?_, ?_⟩
      · rw [← hv]; exact hcmp
      · rw [← hv]; exact h
    · rintro ⟨h, _, h2⟩
      rw [show out.val.getLast h = out.val.getLast hne from rfl, ← hv] at h2; exact h2
  · have hne : out.val ≠ [] := by intro h; simp [h] at i2_post1
    have hlast : out.val.getLast hne = (out.val)[i2.val]'(by scalar_tac) := by
      rw [List.getLast_eq_getElem]; congr 1; scalar_tac
    have hv : (v, o) = out.val.getLast hne := by rw [hlast, v_post]
    refine iff_of_false (by simp) (fun ⟨h, h1, _⟩ => ?_)
    have : cmpL v.val nm.val = 0 := (cmpL_eq_zero _ _).2 (by rw [← hv] at h1; simpa using h1)
    apply ‹¬i3 = 0#i8›; scalar_tac

open Classical in
/-- One step of `stale_reads`: record a stale entry unless it repeats the last one recorded. -/
noncomputable def staleStep (out : List (alloc.vec.Vec U8 × Observed)) (e : refs.Read) :
    List (alloc.vec.Vec U8 × Observed) :=
  if e.observed ≠ e.current then
    if ∃ h : out ≠ [], (out.getLast h).1.val = e.name.val ∧ (out.getLast h).2 = e.current then out
    else out ++ [(e.name, e.current)]
  else out

@[step] theorem stale_reads_spec (reads : alloc.vec.Vec refs.Read) :
    refs.stale_reads reads ⦃ out => out.val = reads.val.foldl staleStep [] ⦄ := by
  unfold refs.stale_reads refs.stale_reads_loop
  step*
  rw [iter_post]
  have h := iter_fold (alloc.vec.Vec.deref reads) (fun o : alloc.vec.Vec (alloc.vec.Vec U8 × Observed) => o.val)
    staleStep (fun o i => o.val.length ≤ i) (fun x => refs.stale_reads_loop.body x.1 x.2)
    (fun i acc hi hacc => by
      show refs.stale_reads_loop.body _ _ ⦃ _ ⦄
      unfold refs.stale_reads_loop.body
      rw [slice_iter_next]
      split
      · rename_i hlt
        step*
        · simp only [IterFoldStep]
          refine ⟨hlt, trivial, ?_, by omega⟩
          simp only [staleStep]; simp_all
        · simp only [IterFoldStep]
          refine ⟨hlt, trivial, ?_, by simp_all⟩
          simp only [staleStep]; simp_all
        · simp only [IterFoldStep]
          refine ⟨hlt, trivial, ?_, by omega⟩
          simp only [staleStep]; simp_all
      · step*
        simp only [IterFoldStep]; exact ⟨by omega, trivial⟩) 0 (alloc.vec.Vec.new _) (by simp) (by simp)
  apply WP.spec_mono h
  intro out hout; rw [hout]; simp [vec_deref_val]

@[step] theorem reads_sorted_spec (v : alloc.vec.Vec refs.Read) : refs.reads_sorted v ⦃ _ => True ⦄ := by
  unfold refs.reads_sorted refs.reads_sorted_loop
  apply loop_idx_spec _ (fun i => i) (max 1 v.val.length) (fun i => 1 ≤ i.val) (fun _ => True)
  · intro i hi _
    unfold refs.reads_sorted_loop.body
    step*
    all_goals (have : i.val < v.val.length := by scalar_tac
               refine ⟨by omega, by omega, by omega⟩)
  · simp
  · simp

@[step] theorem first_duplicate_spec (v : alloc.vec.Vec refs.Target) : refs.first_duplicate v ⦃ _ => True ⦄ := by
  unfold refs.first_duplicate refs.first_duplicate_loop
  apply loop_idx_spec _ (fun i => i) (max 1 v.val.length) (fun i => 1 ≤ i.val) (fun _ => True)
  · intro i hi _
    unfold refs.first_duplicate_loop.body
    step*
    all_goals (have : i.val < v.val.length := by scalar_tac
               refine ⟨by omega, by omega, by omega⟩)
  · simp
  · simp

theorem first_unread_inner_spec (reads : alloc.vec.Vec refs.Read) (targets : alloc.vec.Vec refs.Target)
    (r t : Usize) (hr : r.val ≤ reads.val.length) (ht : t.val < targets.val.length) :
    refs.first_unread_loop0_loop0 reads targets r t ⦃ r' => r.val ≤ r'.val ∧ r'.val ≤ reads.val.length ⦄ := by
  unfold refs.first_unread_loop0_loop0
  apply loop_idx_spec _ (fun i : Usize => i) reads.val.length (fun i : Usize => r.val ≤ i.val)
    (fun r' : Usize => r.val ≤ r'.val ∧ r'.val ≤ reads.val.length)
  · intro i hi hle
    unfold refs.first_unread_loop0_loop0.body
    step*
  · exact le_refl _
  · exact hr

@[step] theorem first_unread_spec (reads : alloc.vec.Vec refs.Read) (targets : alloc.vec.Vec refs.Target) :
    refs.first_unread reads targets ⦃ _ => True ⦄ := by
  unfold refs.first_unread refs.first_unread_loop0
  apply loop_idx_spec _ (fun x => x.2) targets.val.length (fun x => x.1.val ≤ reads.val.length) (fun _ => True)
  · rintro ⟨r, t⟩ hr ht
    unfold refs.first_unread_loop0.body
    simp only [alloc.vec.Vec.len]
    split
    · rename_i hlt
      apply WP.spec_bind (Pₘ := fun r' : Usize => r.val ≤ r'.val ∧ r'.val ≤ reads.val.length)
      · exact first_unread_inner_spec reads targets r t hr (by scalar_tac)
      · intro r' ⟨_, hr'⟩
        step*
    · simp
  · simp
  · simp

/-- The version a name was last at: its current version, else its tombstone, else 0. -/
def lastV (cur : Option RefVal) (tomb : Option U64) : Nat :=
  match cur with
  | some v => v.version.val
  | none => match tomb with
    | some tv => tv.val
    | none => 0

def lastVersion (t : refs.Target) : Nat := lastV t.current t.tombstone

/-- What one target's decision may be, given its governing rule and mobility `rm`. -/
def TargetOK (rm : Rule × Mobility) (t : refs.Target) (i : Usize) : core.result.Result refs.Effect refs.Refusal → Prop
  | .Ok e =>
    (∀ h, t.new = some h →
      (rm.2 = .Mutable ∨ (rm.2 = .CreateOnce ∧ t.current = none)) ∧
      ∃ v : U64, e = .Set ⟨h, v⟩ ∧ v.val = lastVersion t + 1) ∧
    (t.new = none →
      (t.current = none ∧ e = .Noop) ∨
      (rm.2 = .Mutable ∧ ∃ c, t.current = some c ∧ e = .Delete c.version))
  | .Err r =>
    (∃ w, r = .Forbidden i rm.1 w ∧
       (rm.2 = .Forbidden ∨ (rm.2 = .CreateOnce ∧ t.current ≠ none))) ∨
    (r = .VersionExhausted i ∧ lastVersion t = U64.max)

@[step] theorem last_version_spec (cur : Option RefVal) (tomb : Option U64) :
    refs.last_version cur tomb ⦃ v => v.val = lastV cur tomb ⦄ := by
  unfold refs.last_version
  cases cur <;> (try cases tomb) <;> simp [lastV]

@[step] theorem decide_target_spec (t : refs.Target) (i : Usize) :
    refs.decide_target t i ⦃ r => ∃ rm, MobilityOf t.rules.val t.name.val rm ∧ TargetOK rm t i r ⦄ := by
  unfold refs.decide_target
  step*
  all_goals try (refine ⟨_, ‹MobilityOf _ _ _›, ?_⟩; simp_all [TargetOK, lastVersion])
  all_goals first
    | (rw [← last_post]; simp [U64.max_eq, U64.rMax]; done)
    | (cases hc : t.current <;> simp_all; done)
    | (have hv : last.val ≠ U64.max := by
         intro hv; apply ‹¬last = core.num.U64.MAX›
         apply UScalar.eq_of_val_eq; rw [hv]; simp [core.num.U64.MAX, U64.max_eq, U64.rMax]
       scalar_tac)

theorem staleStep_ne_nil (acc : List (alloc.vec.Vec U8 × Observed)) (e : refs.Read) (h : acc ≠ []) :
    staleStep acc e ≠ [] := by
  unfold staleStep; split_ifs <;> simp_all

theorem foldl_stale_nil (l : List refs.Read) (acc : List (alloc.vec.Vec U8 × Observed))
    (h : l.foldl staleStep acc = []) : acc = [] ∧ ∀ e ∈ l, e.observed = e.current := by
  induction l generalizing acc with
  | nil => simpa using h
  | cons e es ih =>
    simp only [List.foldl_cons] at h
    obtain ⟨h1, h2⟩ := ih _ h
    have hacc : acc = [] := by
      by_contra hne; exact staleStep_ne_nil acc e hne h1
    subst hacc
    refine ⟨rfl, fun x hx => ?_⟩
    rcases List.mem_cons.1 hx with rfl | hx
    · by_contra hne
      unfold staleStep at h1; simp [hne] at h1
    · exact h2 x hx

/-- Every effect of a committed batch is what its target's rule allows. -/
def EffectsOK (targets : List refs.Target) (es : List refs.Effect) (n : Nat) : Prop :=
  es.length = n ∧ ∀ k (hk : k < n) (ht : k < targets.length) (he : k < es.length),
    ∃ rm, MobilityOf targets[k].rules.val targets[k].name.val rm ∧
      ∃ ki : Usize, ki.val = k ∧ TargetOK rm targets[k] ki (.Ok es[k])

/-- What the decision loop can return: a commit with every effect allowed, or a refusal. -/
def LoopOK (targets : List refs.Target) : core.result.Result refs.Decision refs.Refusal → Prop
  | .Ok (.Commit es) => EffectsOK targets es.val targets.length
  | .Ok (.Conflict _) => False
  | .Err _ => True

theorem decide_loop_spec (v : alloc.vec.Vec refs.Target) (n : Usize) (hn : n.val = v.val.length) :
    refs.decide_loop v n (alloc.vec.Vec.new refs.Effect) 0#usize ⦃ r => LoopOK v.val r ⦄ := by
  unfold refs.decide_loop
  apply loop_idx_spec _ (fun x => x.2) n.val (fun x => EffectsOK v.val x.1.val x.2.val) (fun r => LoopOK v.val r)
  · rintro ⟨es, i⟩ hinv hi
    obtain ⟨hlen, hall⟩ := hinv
    unfold refs.decide_loop.body
    simp only at hlen hall hi
    step*
    · refine ⟨⟨by simp [effects1_post, hlen, i1_post], fun k hk ht he => ?_⟩, by scalar_tac, by scalar_tac⟩
      have hes : effects1.val = es.val ++ [e] := effects1_post
      by_cases hki : k < i.val
      · obtain ⟨rm, hm, ki, hki', hok⟩ := hall k hki ht (by omega)
        refine ⟨rm, hm, ki, hki', ?_⟩
        have : effects1.val[k] = es.val[k] := by
          simp only [hes]; rw [List.getElem_append_left (by omega)]
        rw [this]; exact hok
      · have hk' : k = i.val := by scalar_tac
        subst hk'
        have ht' : t = v.val[i.val] := t_post
        have : effects1.val[i.val] = e := by
          simp only [hes]; rw [List.getElem_append_right (by omega)]; simp [hlen]
        refine ⟨r, ?_, i, rfl, ?_⟩
        · rw [← ht']; exact r_post
        · have hx := ‹_ = core.result.Result.Ok e›
          rw [this, ← ht']; rw [hx] at r_post1; exact r_post1
    · simp [LoopOK]
    · have : i.val = n.val := by scalar_tac
      simp only [LoopOK, EffectsOK]
      refine ⟨by rw [hlen, this, hn], fun k hk ht he => ?_⟩
      exact hall k (by omega) ht he
  · exact ⟨by simp, fun k hk => by simp at hk⟩
  · simp

/-- What `decide` returns. -/
def DecideOK (view : refs.View) : core.result.Result refs.Decision refs.Refusal → Prop
  | .Ok (.Commit es) => (∀ rd ∈ view.reads.val, rd.observed = rd.current) ∧
      EffectsOK view.targets.val es.val view.targets.val.length
  | .Ok (.Conflict st) => st.val ≠ [] ∧ st.val = view.reads.val.foldl staleStep []
  | .Err _ => True

/-- `decide` never fails (no panic, no overflow) and its result satisfies `DecideOK`. -/
@[step] theorem decide_spec (view : refs.View) : refs.decide view ⦃ r => DecideOK view r ⦄ := by
  unfold refs.decide
  step*
  all_goals try (simp [DecideOK]; done)
  · -- Conflict: the stale list is what `stale_reads` computes, and not empty.
    simp only [DecideOK]
    refine ⟨fun h => ?_, stale_post⟩
    have h0 : stale.val.length = 0 := by rw [h]; rfl
    have hgt := ‹stale.len > 0#usize›
    clear * - hgt h0
    simp only [alloc.vec.Vec.len] at hgt
    scalar_tac
  · -- Commit: no read was stale, and every effect is allowed.
    have hst : stale.val = [] := List.length_eq_zero_iff.mp (by scalar_tac)
    rw [stale_post] at hst
    have hall := (foldl_stale_nil _ _ hst).2
    apply WP.spec_mono (decide_loop_spec view.targets view.targets.len (by simp))
    intro r hr
    cases r with
    | Ok d =>
      cases d with
      | Commit es => exact ⟨hall, hr⟩
      | Conflict _ => exact absurd hr (by simp [LoopOK])
    | Err _ => simp [DecideOK]

/-! ## P2 and P3 on the extracted `decide` -/

theorem governs_unique (rules : List (alloc.vec.Vec U8 × Rule)) (nm : List U8) (a b : Option Usize)
    (ha : GovernsUpTo rules nm rules.length a) (hb : GovernsUpTo rules nm rules.length b) : a = b := by
  cases a with
  | none =>
    cases b with
    | none => rfl
    | some j => obtain ⟨hj, _, hc, _⟩ := hb; exact absurd hc (ha j.val hj hj)
  | some i =>
    cases b with
    | none => obtain ⟨hi, _, hc, _⟩ := ha; exact absurd hc (hb i.val hi hi)
    | some j =>
      obtain ⟨hi, _, hci, hmi⟩ := ha
      obtain ⟨hj, _, hcj, hmj⟩ := hb
      have h1 := hmi j.val hj hj hcj
      have h2 := hmj i.val hi hi hci
      have : i.val = j.val := by
        rcases Nat.lt_trichotomy i.val j.val with h | h | h
        · have := h2.2 h; omega
        · exact h
        · have := h1.2 h; omega
      congr 1; exact UScalar.eq_of_val_eq this

theorem mobilityOf_unique (rules : List (alloc.vec.Vec U8 × Rule)) (nm : List U8) (r s : Rule × Mobility)
    (hr : MobilityOf rules nm r) (hs : MobilityOf rules nm s) : r = s := by
  obtain ⟨a, ha, hna, hsa⟩ := hr
  obtain ⟨b, hb, hnb, hsb⟩ := hs
  have := governs_unique rules nm a b ha hb
  subst this
  cases a with
  | none =>
    obtain ⟨h1, h2⟩ := hna rfl; obtain ⟨h3, h4⟩ := hnb rfl
    exact Prod.ext (h1.trans h3.symm) (h2.trans h4.symm)
  | some j =>
    obtain ⟨_, h1, h2⟩ := hsa j rfl; obtain ⟨_, h3, h4⟩ := hsb j rfl
    exact Prod.ext (h1.trans h3.symm) (h2.trans h4.symm)

/-- **P2 (no undetected overwrite).** A committed batch validated its whole read set: every name
the writer depended on was still in the state it observed. -/
theorem commit_validated (view : refs.View) (es : alloc.vec.Vec refs.Effect)
    (h : refs.decide view = ok (.Ok (.Commit es))) :
    ∀ rd ∈ view.reads.val, rd.observed = rd.current := by
  have := decide_spec view
  rw [h] at this
  exact (WP.spec_ok _).1 this |>.1

/-- **P3 (published names never move).** In a committed batch, no target that already exists is
written or deleted when its governing rule makes it create-once, and no target at all is written
when the rule forbids it. -/
theorem commit_respects_rules (view : refs.View) (es : alloc.vec.Vec refs.Effect)
    (h : refs.decide view = ok (.Ok (.Commit es)))
    (k : Nat) (hk : k < view.targets.val.length) (rm : Rule × Mobility)
    (hm : MobilityOf view.targets.val[k].rules.val view.targets.val[k].name.val rm) :
    (rm.2 = .CreateOnce → view.targets.val[k].current = none) ∧
    (rm.2 = .Forbidden → view.targets.val[k].new = none ∧ view.targets.val[k].current = none) := by
  have := decide_spec view
  rw [h] at this
  obtain ⟨_, hlen, hall⟩ := (WP.spec_ok _).1 this
  obtain ⟨rm', hm', ki, _, hok⟩ := hall k hk hk (by omega)
  have := mobilityOf_unique _ _ _ _ hm hm'
  subst this
  obtain ⟨hset, hdel⟩ := hok
  constructor
  · intro hc
    cases hn : view.targets.val[k].new with
    | some hh =>
      obtain ⟨hmob, _⟩ := hset hh hn
      rcases hmob with hmob | ⟨_, hcur⟩
      · rw [hc] at hmob; cases hmob
      · exact hcur
    | none =>
      rcases hdel hn with ⟨hcur, _⟩ | ⟨hmob, _⟩
      · exact hcur
      · rw [hc] at hmob; cases hmob
  · intro hf
    cases hn : view.targets.val[k].new with
    | some hh =>
      obtain ⟨hmob, _⟩ := hset hh hn
      rcases hmob with hmob | ⟨hmob, _⟩ <;> (rw [hf] at hmob; cases hmob)
    | none =>
      rcases hdel hn with ⟨hcur, _⟩ | ⟨hmob, _⟩
      · exact ⟨rfl, hcur⟩
      · rw [hf] at hmob; cases hmob

/-- **Versions (P2's other half).** A committed write sets the version to the last version plus
one, so a reader's observed version identifies the state it saw. -/
theorem commit_versions (view : refs.View) (es : alloc.vec.Vec refs.Effect)
    (h : refs.decide view = ok (.Ok (.Commit es)))
    (k : Nat) (hk : k < view.targets.val.length) (hh : Std.Array U8 32#usize)
    (hn : view.targets.val[k].new = some hh) :
    ∃ (he : k < es.val.length) (v : U64), es.val[k] = .Set ⟨hh, v⟩ ∧ v.val = lastVersion view.targets.val[k] + 1 := by
  have := decide_spec view
  rw [h] at this
  obtain ⟨_, hlen, hall⟩ := (WP.spec_ok _).1 this
  obtain ⟨rm', _, ki, _, hset, _⟩ := hall k hk hk (by omega)
  obtain ⟨_, v, he, hv⟩ := hset hh hn
  exact ⟨by omega, v, he, hv⟩

/-! ## Front ownership (P6) -/

/-- `j` is the governing rule's index among `rules` for `nm` (looking at the first `i` rules):
it covers `nm`, no covering rule is longer, and earlier ones are strictly shorter. `none`: no
rule covers `nm`. -/
def OwnsUpTo (rules : List (alloc.vec.Vec U8 × U32)) (nm : List U8) (i : Nat) : Option Usize → Prop
  | none => ∀ k (h : k < rules.length), k < i → ¬ (fun p => p <+: nm) rules[k].1.val
  | some j => ∃ hj : j.val < rules.length, j.val < i ∧ (fun p => p <+: nm) rules[j.val].1.val ∧
      ∀ k (h : k < rules.length), k < i → (fun p => p <+: nm) rules[k].1.val →
        rules[k].1.val.length ≤ rules[j.val].1.val.length ∧
        (k < j.val → rules[k].1.val.length < rules[j.val].1.val.length)

/-- The loop's state: what the first `i` rules govern, and the governing prefix's length. -/
def OwnInv (rules : List (alloc.vec.Vec U8 × U32)) (nm : List U8) (best : Option Usize) (bl i : Usize) : Prop :=
  OwnsUpTo rules nm i.val best ∧ i.val ≤ rules.length ∧
    (∀ j : Usize, best = some j → ∃ hj : j.val < rules.length, bl.val = rules[j.val].1.val.length)

@[step] theorem pick_owner_spec (best : Option Usize) (bl i plen : Usize) (covers : Bool) :
    fronts.pick_owner best bl i plen covers ⦃ r =>
      r = (if covers = true then
             (match best with
              | none => (some i, plen)
              | some _ => if bl.val < plen.val then (some i, plen) else (best, bl))
           else (best, bl)) ⦄ := by
  unfold fronts.pick_owner
  cases covers <;> cases best <;> simp <;> split <;> simp_all

theorem owner_loop_spec (rules : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (nm : alloc.vec.Vec U8) :
    fronts.owner_loop rules nm none 0#usize 0#usize ⦃ best => OwnsUpTo rules.val nm.val rules.val.length best ⦄ := by
  unfold fronts.owner_loop
  apply loop_idx_spec _ (fun x => x.2.2) rules.val.length (fun x => OwnInv rules.val nm.val x.1 x.2.1 x.2.2)
  · rintro ⟨best, bl, i⟩ hinv _
    obtain ⟨hg, hi, hbl⟩ := hinv
    unfold fronts.owner_loop.body
    simp only [alloc.vec.Vec.len]
    step*
    have hil : i.val < rules.val.length := by scalar_tac
    have hv : v = (rules.val[i.val]'hil).1 := by
      have := congrArg Prod.fst v_post; simp only at this; rw [this]
    have hvl : (Usize.ofNatCore v.val.length (by scalar_tac)).val = v.val.length := by simp
    simp only at hg hbl
    refine ⟨⟨?_, by scalar_tac, ?_⟩, by scalar_tac, by scalar_tac⟩
    · -- What the first i+1 rules govern.
      rw [i3_post]
      by_cases hc : covers = true
      · have hcov : (fun p => p <+: nm.val) rules.val[i.val].1.val := hv ▸ covers_post.1 hc
        simp only [hc, if_true] at best1_post
        cases best with
        | none =>
          simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
          refine ⟨hil, by omega, hcov, fun k hk hk1 hck => ?_⟩
          by_cases hki : k < i.val
          · exact absurd hck (hg k hk hki)
          · have : k = i.val := by omega
            subst this; exact ⟨le_refl _, fun h => absurd h (lt_irrefl _)⟩
        | some j0 =>
          obtain ⟨hj0, hj0i, hj0c, hj0m⟩ := hg
          obtain ⟨_, hblj⟩ := hbl j0 rfl
          simp only at best1_post
          split at best1_post
          · rename_i hlt
            simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
            (try rw [hvl] at hlt); (try rw [hv] at hlt)
            refine ⟨hil, by omega, hcov, fun k hk hk1 hck => ?_⟩
            by_cases hki : k < i.val
            · have := (hj0m k hk hki hck).1; exact ⟨by omega, fun _ => by omega⟩
            · have : k = i.val := by omega
              subst this; exact ⟨le_refl _, fun h => absurd h (lt_irrefl _)⟩
          · rename_i hnlt
            simp only [Prod.mk.injEq] at best1_post; obtain ⟨rfl, -⟩ := best1_post
            (try rw [hvl] at hnlt); (try rw [hv] at hnlt)
            refine ⟨hj0, by omega, hj0c, fun k hk hk1 hck => ?_⟩
            by_cases hki : k < i.val
            · exact hj0m k hk hki hck
            · have : k = i.val := by omega
              subst this; exact ⟨by omega, fun h => by omega⟩
      · have hncov : ¬ (fun p => p <+: nm.val) rules.val[i.val].1.val := fun h => hc (covers_post.2 (hv ▸ h))
        simp only [hc, if_false, Bool.false_eq_true, Prod.mk.injEq] at best1_post
        obtain ⟨rfl, -⟩ := best1_post
        cases best1 with
        | none =>
          intro k hk hk1
          by_cases hki : k < i.val
          · exact hg k hk hki
          · have : k = i.val := by omega
            subst this; exact hncov
        | some j0 =>
          obtain ⟨hj0, hj0i, hj0c, hj0m⟩ := hg
          refine ⟨hj0, by omega, hj0c, fun k hk hk1 hck => ?_⟩
          by_cases hki : k < i.val
          · exact hj0m k hk hki hck
          · have : k = i.val := by omega
            subst this; exact absurd hck hncov
    · -- The best length is the governing prefix's length.
      intro j hj
      by_cases hc : covers = true
      · simp only [hc, if_true] at best1_post
        cases best with
        | none =>
          simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
          subst h1; subst h2; cases hj
          exact ⟨hil, by rw [hvl, hv]⟩
        | some j0 =>
          simp only at best1_post
          split at best1_post
          · simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
            subst h1; subst h2; cases hj
            exact ⟨hil, by rw [hvl, hv]⟩
          · simp only [Prod.mk.injEq] at best1_post; obtain ⟨h1, h2⟩ := best1_post
            subst h1; subst h2; exact hbl j hj
      · simp only [hc, if_false, Bool.false_eq_true, Prod.mk.injEq] at best1_post
        obtain ⟨h1, h2⟩ := best1_post
        subst h1; subst h2; exact hbl j hj
  · refine ⟨fun k _ h => by simp at h, by simp, fun j h => by simp at h⟩
  · simp


/-- `o` is the owner of `nm`: the front of its longest claimed prefix (first on ties), or none. -/
def OwnerOf (claims : List (alloc.vec.Vec U8 × U32)) (nm : List U8) (o : Option U32) : Prop :=
  ∃ at_ : Option Usize, OwnsUpTo claims nm claims.length at_ ∧
    (at_ = none → o = none) ∧ (∀ j : Usize, at_ = some j → ∃ hj : j.val < claims.length, o = some claims[j.val].2)

@[step] theorem owner_spec (claims : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (nm : alloc.vec.Vec U8) :
    fronts.owner claims nm ⦃ o => OwnerOf claims.val nm.val o ⦄ := by
  unfold fronts.owner
  apply WP.spec_bind (Pₘ := fun best => OwnsUpTo claims.val nm.val claims.val.length best)
  · exact owner_loop_spec claims nm
  · intro best hb
    cases best with
    | none => simp only [WP.spec_ok]; exact ⟨none, hb, fun _ => rfl, fun j h => by simp at h⟩
    | some j =>
      have hb' := hb
      obtain ⟨hj, _⟩ := hb
      step*
      refine ⟨some j, hb', fun h => by simp at h, fun j' h => ?_⟩
      simp only [Option.some.injEq] at h; subst h
      refine ⟨hj, ?_⟩
      have := congrArg Prod.snd ‹(_, _) = _›; simp only at this; rw [this]

theorem owns_unique (claims : List (alloc.vec.Vec U8 × U32)) (nm : List U8) (a b : Option Usize)
    (ha : OwnsUpTo claims nm claims.length a) (hb : OwnsUpTo claims nm claims.length b) : a = b := by
  cases a with
  | none =>
    cases b with
    | none => rfl
    | some j => obtain ⟨hj, _, hc, _⟩ := hb; exact absurd hc (ha j.val hj hj)
  | some i =>
    cases b with
    | none => obtain ⟨hi, _, hc, _⟩ := ha; exact absurd hc (hb i.val hi hi)
    | some j =>
      obtain ⟨hi, _, hci, hmi⟩ := ha
      obtain ⟨hj, _, hcj, hmj⟩ := hb
      have h1 := hmi j.val hj hj hcj
      have h2 := hmj i.val hi hi hci
      have : i.val = j.val := by
        rcases Nat.lt_trichotomy i.val j.val with h | h | h
        · have := h2.2 h; omega
        · exact h
        · have := h1.2 h; omega
      congr 1; exact UScalar.eq_of_val_eq this

theorem ownerOf_unique (claims : List (alloc.vec.Vec U8 × U32)) (nm : List U8) (o o' : Option U32)
    (h : OwnerOf claims nm o) (h' : OwnerOf claims nm o') : o = o' := by
  obtain ⟨a, ha, hna, hsa⟩ := h
  obtain ⟨b, hb, hnb, hsb⟩ := h'
  have := owns_unique claims nm a b ha hb
  subst this
  cases a with
  | none => rw [hna rfl, hnb rfl]
  | some j => obtain ⟨_, h1⟩ := hsa j rfl; obtain ⟨_, h2⟩ := hsb j rfl; rw [h1, h2]

@[step] theorem may_write_spec (claims : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (nm : alloc.vec.Vec U8) (who : Option U32) :
    fronts.may_write claims nm who ⦃ b => ∃ o, OwnerOf claims.val nm.val o ∧ (b = true ↔ (o = none ∨ o = who)) ⦄ := by
  unfold fronts.may_write
  step*
  all_goals (refine ⟨_, ‹OwnerOf _ _ _›, ?_⟩; simp_all)

/-- **P6 (front ownership).** A write the guard accepts is by the name's owner, or the name has none:
no front, and no core caller, writes a name another front owns. -/
theorem guard_owner (claims : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (nm : alloc.vec.Vec U8) (who : Option U32)
    (h : fronts.may_write claims nm who = ok true) (o : Option U32) (ho : OwnerOf claims.val nm.val o) :
    o = none ∨ o = who := by
  have := may_write_spec claims nm who
  rw [h] at this
  obtain ⟨o', ho', hiff⟩ := (WP.spec_ok _).1 this
  rw [ownerOf_unique _ _ _ _ ho ho']
  exact hiff.1 rfl

/-! ## GC verify-before-delete (P5) -/

/-- `p` names one of the doomed packs. -/
def DoomedL (doomed : List (alloc.vec.Vec U8)) (p : List U8) : Prop := ∃ d ∈ doomed, d.val = p

@[step] theorem is_doomed_spec (doomed : alloc.vec.Vec (alloc.vec.Vec U8)) (p : alloc.vec.Vec U8) :
    gc.is_doomed doomed p ⦃ b => (b = true ↔ DoomedL doomed.val p.val) ⦄ := by
  unfold gc.is_doomed gc.is_doomed_loop
  apply loop_idx_spec _ (fun i => i) doomed.val.length
    (fun i => ∀ k (hk : k < i.val) (h2 : k < doomed.val.length), doomed.val[k].val ≠ p.val)
  · intro i hall hi
    unfold gc.is_doomed_loop.body
    step*
    · have hil : i.val < doomed.val.length := by scalar_tac
      refine iff_of_true trivial ⟨doomed.val[i.val]'hil, List.getElem_mem hil, ?_⟩
      have := (cmpL_eq_zero _ _).1 (by rw [← i2_post]; simp_all)
      simp_all
    · refine ⟨fun k hk h2 => ?_, by scalar_tac, by scalar_tac⟩
      by_cases hki : k < i.val
      · exact hall k hki h2
      · have : k = i.val := by scalar_tac
        subst this
        intro heq
        apply ‹¬i2 = 0#i8›
        have : cmpL v.val p.val = 0 := (cmpL_eq_zero _ _).2 (by simp_all)
        scalar_tac
    · refine iff_of_false (by simp) (fun ⟨d, hd, hdp⟩ => ?_)
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hd
      exact hall k (by scalar_tac) hk hdp
  · intro k hk; simp at hk
  · simp

/-- Verify-before-delete holds: no live object is served by a doomed pack. -/
def SafeToDelete (doomed : List (alloc.vec.Vec U8)) (serving : List (Option (alloc.vec.Vec U8))) : Prop :=
  ∀ x ∈ serving, ∀ p, x = some p → ¬ DoomedL doomed p.val

@[step] theorem delete_ok_spec (doomed : alloc.vec.Vec (alloc.vec.Vec U8)) (serving : alloc.vec.Vec (Option (alloc.vec.Vec U8))) :
    gc.delete_ok doomed serving ⦃ b => (b = true ↔ SafeToDelete doomed.val serving.val) ⦄ := by
  unfold gc.delete_ok gc.delete_ok_loop
  apply loop_idx_spec _ (fun i => i) serving.val.length
    (fun i => ∀ k (hk : k < i.val) (h2 : k < serving.val.length), ∀ p, serving.val[k] = some p → ¬ DoomedL doomed.val p.val)
  · intro i hall hi
    unfold gc.delete_ok_loop.body
    step*
    · refine iff_of_false (by simp) (fun hs => ?_)
      have hil : i.val < serving.val.length := by scalar_tac
      exact hs _ (List.getElem_mem hil) p (by rw [← o_post]; assumption) (b_post.1 ‹_›)
    · refine iff_of_true trivial (fun x hx p hp => ?_)
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
      exact hall k (by scalar_tac) hk p hp
  · intro k hk; simp at hk
  · simp

/-- **P5 (GC never deletes a live object's only copy).** If the delete phase's check passes, no
live object is served by a pack it deletes: each is served from a surviving pack (or not from a
pack at all), so deleting the condemned packs keeps every live object readable. -/
theorem delete_keeps_live (doomed : alloc.vec.Vec (alloc.vec.Vec U8)) (serving : alloc.vec.Vec (Option (alloc.vec.Vec U8)))
    (h : gc.delete_ok doomed serving = ok true) :
    ∀ x ∈ serving.val, ∀ p, x = some p → ¬ ∃ d ∈ doomed.val, d.val = p.val := by
  have := delete_ok_spec doomed serving
  rw [h] at this
  exact ((WP.spec_ok _).1 this).1 rfl

/-! ## Promotion (P7) -/

@[step] theorem has_key_spec (keys : alloc.vec.Vec (alloc.vec.Vec U8)) (key : alloc.vec.Vec U8) :
    promote.has_key keys key ⦃ b => (b = true ↔ DoomedL keys.val key.val) ⦄ := by
  unfold promote.has_key promote.has_key_loop
  apply loop_idx_spec _ (fun i => i) keys.val.length
    (fun i => ∀ k (hk : k < i.val) (h2 : k < keys.val.length), keys.val[k].val ≠ key.val)
  · intro i hall hi
    unfold promote.has_key_loop.body
    step*
    · have hil : i.val < keys.val.length := by scalar_tac
      refine iff_of_true trivial ⟨keys.val[i.val]'hil, List.getElem_mem hil, ?_⟩
      have := (cmpL_eq_zero _ _).1 (by rw [← i2_post]; simp_all)
      simp_all
    · refine ⟨fun k hk h2 => ?_, by scalar_tac, by scalar_tac⟩
      by_cases hki : k < i.val
      · exact hall k hki h2
      · have : k = i.val := by scalar_tac
        subst this
        intro heq
        apply ‹¬i2 = 0#i8›
        have : cmpL v.val key.val = 0 := (cmpL_eq_zero _ _).2 (by simp_all)
        scalar_tac
    · refine iff_of_false (by simp) (fun ⟨d, hd, hdp⟩ => ?_)
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hd
      exact hall k (by scalar_tac) hk hdp
  · intro k hk; simp at hk
  · simp


/-- Attestation `a` counts under the policy. -/
def CountsL (pol : promote.Policy) (a : promote.Attestation) : Prop :=
  a.about_object = true ∧ (pol.predicate_type = none ∨ ∃ t, pol.predicate_type = some t ∧ t.val = a.predicate_type.val)

@[step] theorem counts_spec (pol : promote.Policy) (a : promote.Attestation) :
    promote.counts pol a ⦃ b => (b = true ↔ CountsL pol a) ⦄ := by
  unfold promote.counts
  step*
  · simp_all [CountsL]
  · have hp : pol.predicate_type = some t := ‹_›
    simp only [CountsL, ‹a.about_object = true›, true_and, hp, reduceCtorEq, false_or, Option.some.injEq,
      exists_eq_left', decide_eq_true_eq]
    constructor
    · intro h; exact (cmpL_eq_zero _ _).1 (by rw [← i_post, h]; rfl)
    · intro h; have := (cmpL_eq_zero _ _).2 h; scalar_tac
  · simp_all [CountsL]

/-- Key `k` signed some attestation that counts under the policy. -/
def SignedL (pol : promote.Policy) (atts : List promote.Attestation) (k : List U8) : Prop :=
  ∃ a ∈ atts, CountsL pol a ∧ DoomedL a.verified_keys.val k

@[step] theorem signed_by_spec (pol : promote.Policy) (atts : alloc.vec.Vec promote.Attestation) (key : alloc.vec.Vec U8) :
    promote.signed_by pol atts key ⦃ b => (b = true ↔ SignedL pol atts.val key.val) ⦄ := by
  unfold promote.signed_by promote.signed_by_loop
  apply loop_idx_spec _ (fun i => i) atts.val.length
    (fun i => ∀ k (hk : k < i.val) (h2 : k < atts.val.length), ¬ (CountsL pol atts.val[k] ∧ DoomedL atts.val[k].verified_keys.val key.val))
  · intro i hall hi
    unfold promote.signed_by_loop.body
    step*
    · have hil : i.val < atts.val.length := by scalar_tac
      refine iff_of_true trivial ⟨atts.val[i.val]'hil, List.getElem_mem hil, ?_, ?_⟩
      · rw [← a_post]; exact b_post.1 ‹_›
      · rw [← a_post]; exact b1_post.1 ‹_›
    · refine iff_of_false (by simp) (fun ⟨x, hx, hc, hd⟩ => ?_)
      obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hx
      exact hall k (by scalar_tac) hk ⟨hc, hd⟩
  · intro k hk; simp at hk
  · simp

@[step] theorem key_missing_spec (pol : promote.Policy) (atts : alloc.vec.Vec promote.Attestation) (i : Usize) :
    promote.key_missing pol atts i ⦃ r =>
      (r = none ↔ (i.val < pol.required_keys.val.length → SignedL pol atts.val (pol.required_keys.val[i.val]!).val)) ⦄ := by
  unfold promote.key_missing
  step*
  · have hil : i.val < pol.required_keys.val.length := by scalar_tac
    have hg : (pol.required_keys.val[i.val]!) = pol.required_keys.val[i.val]'hil := by
      simp [getElem!_pos, hil]
    refine iff_of_true trivial (fun _ => ?_)
    rw [hg, ← v_post]; exact b_post.1 ‹_›
  · have hil : i.val < pol.required_keys.val.length := by scalar_tac
    have hg : (pol.required_keys.val[i.val]!) = pol.required_keys.val[i.val]'hil := by
      simp [getElem!_pos, hil]
    refine iff_of_false (by simp) (fun h => ‹¬b = true› (b_post.2 ?_))
    have := h hil; rw [hg, ← v_post] at this; exact this

/-- Every required key signed a counting attestation. -/
def AllSigned (pol : promote.Policy) (atts : List promote.Attestation) : Prop :=
  ∀ k (hk : k < pol.required_keys.val.length), SignedL pol atts (pol.required_keys.val[k]'hk).val

@[step] theorem keys_missing_spec (pol : promote.Policy) (atts : alloc.vec.Vec promote.Attestation) :
    promote.keys_missing pol atts ⦃ ms => (ms.val = [] ↔ AllSigned pol atts.val) ⦄ := by
  unfold promote.keys_missing promote.keys_missing_loop
  apply loop_idx_spec _ (fun x => x.2) pol.required_keys.val.length
    (fun x => x.1.val.length ≤ x.2.val ∧
      (x.1.val = [] ↔ ∀ k (hk : k < x.2.val) (h2 : k < pol.required_keys.val.length),
        SignedL pol atts.val (pol.required_keys.val[k]'h2).val))
  · rintro ⟨ms, i⟩ ⟨hlen, hiff⟩ hi
    simp only at hlen hiff hi
    unfold promote.keys_missing_loop.body
    simp only [alloc.vec.Vec.len]
    split
    · rename_i hlt
      have hil : i.val < pol.required_keys.val.length := by scalar_tac
      have hg : (pol.required_keys.val[i.val]!) = pol.required_keys.val[i.val]'hil := by simp [getElem!_pos, hil]
      apply WP.spec_bind (Pₘ := fun o => (o = none ↔ SignedL pol atts.val (pol.required_keys.val[i.val]'hil).val))
      · apply WP.spec_mono (key_missing_spec pol atts i)
        intro o ho; rw [ho, hg]; exact ⟨fun h => h hil, fun h _ => h⟩
      · intro o ho
        cases o with
        | none =>
          step*
        | some m =>
          step*
          refine ⟨by simp_all, ?_, by scalar_tac, by scalar_tac⟩
          refine iff_of_false (by simp_all) (fun h => ?_)
          have := h i.val (by scalar_tac) hil
          have := ho.2 this; simp at this
    · simp only [WP.spec_ok]
      have : i.val = pol.required_keys.val.length := by scalar_tac
      rw [hiff]
      constructor
      · intro h k hk; exact h k (by omega) hk
      · intro h k _ h2; exact h k h2
  · exact ⟨by simp, by simp⟩
  · simp

@[step] theorem prov_missing_spec (r h : Bool) :
    promote.prov_missing r h ⦃ b => (b = true ↔ (r = true ∧ h = false)) ⦄ := by
  unfold promote.prov_missing; cases r <;> cases h <;> simp

@[step] theorem observed_spec (v : Option RefVal) :
    promote.observed v ⦃ o => o = (match v with | some d => Observed.Version d.version | none => Observed.Absent) ⦄ := by
  unfold promote.observed; cases v <;> simp

/-- What `decide_promote` returns: an accepted plan meets every requirement and targets `src`'s
current object at its current version; a refusal names at least one unmet requirement. -/
def PromoteOK (pol : promote.Policy) (src dst : Option RefVal) (has_producer : Bool) (atts : List promote.Attestation) :
    core.result.Result promote.Plan promote.Refused → Prop
  | .Ok plan => ∃ s, src = some s ∧ plan.object = s.hash ∧ plan.src_observed = .Version s.version ∧
      plan.dst_observed = (match dst with | some d => .Version d.version | none => .Absent) ∧
      (pol.require_provenance = true → has_producer = true) ∧ AllSigned pol atts
  | .Err r => r.no_source = true ∨ r.no_provenance = true ∨ r.keys.val ≠ []

@[step] theorem decide_promote_spec (pol : promote.Policy) (src dst : Option RefVal) (has_producer : Bool)
    (atts : alloc.vec.Vec promote.Attestation) :
    promote.decide_promote pol src dst has_producer atts ⦃ r => PromoteOK pol src dst has_producer atts.val r ⦄ := by
  unfold promote.decide_promote
  step*
  · simp [PromoteOK]
  · simp_all [PromoteOK]
  · simp only [PromoteOK]; right; right
    intro h
    have : keys.val.length = 0 := by rw [h]; rfl
    have hgt := ‹keys.len > 0#usize›
    clear * - hgt this
    simp only [alloc.vec.Vec.len] at hgt
    scalar_tac
  · have hk : keys.val = [] := by
      have hle := ‹¬keys.len > 0#usize›
      have : keys.val.length = 0 := by
        clear * - hle
        simp only [alloc.vec.Vec.len] at hle
        scalar_tac
      exact List.length_eq_zero_iff.mp this
    refine ⟨s, rfl, rfl, rfl, ?_, ?_, keys_post.1 hk⟩
    · rw [o_post]
    · intro hr
      cases hp : has_producer
      · exact absurd (no_provenance_post.2 ⟨hr, hp⟩) ‹_›
      · rfl

/-- **P7 (promotion), the decision.** An accepted promotion meets every policy requirement and moves
`dst` to `src`'s current object, pinning `src` at its current version. (The plan's only target is
`dst`: `Plan` has no other.) -/
theorem promote_plan (pol : promote.Policy) (src dst : Option RefVal) (hp : Bool)
    (atts : alloc.vec.Vec promote.Attestation) (plan : promote.Plan)
    (h : promote.decide_promote pol src dst hp atts = ok (.Ok plan)) :
    ∃ s, src = some s ∧ plan.object = s.hash ∧ plan.src_observed = .Version s.version ∧
      (pol.require_provenance = true → hp = true) ∧ AllSigned pol atts.val := by
  have := decide_promote_spec pol src dst hp atts
  rw [h] at this
  obtain ⟨s, h1, h2, h3, _, h5, h6⟩ := (WP.spec_ok _).1 this
  exact ⟨s, h1, h2, h3, h5, h6⟩

/-- **P7, the commit: never from a stale source.** When the promotion's batch (whose read set pins
`src` at the plan's observation) commits, `src` was still at the version the policy was checked on. -/
theorem promote_src_fresh (view : refs.View) (es : alloc.vec.Vec refs.Effect) (plan : promote.Plan)
    (rd : refs.Read) (hrd : rd ∈ view.reads.val) (hobs : rd.observed = plan.src_observed)
    (hc : refs.decide view = ok (.Ok (.Commit es))) :
    rd.current = plan.src_observed := by
  rw [← hobs]; exact (commit_validated view es hc rd hrd).symm

/-! ## GC repack victims -/

@[step] theorem is_victim_spec (t : gc.Thresholds) (total live size : U64) :
    gc.is_victim t total live size ⦃ b => (b = true ↔
      (t.repack_all = true ∨ size.val < t.small_pack_bytes.val ∨ total.val = 0 ∨ total.val < live.val ∨
        t.min_garbage_permille.val * total.val ≤ (total.val - live.val) * 1000)) ⦄ := by
  unfold gc.is_victim
  step*

@[step] theorem is_mine_spec (who : Option U32) (f : U32) : fronts.is_mine who f ⦃ _ => True ⦄ := by
  unfold fronts.is_mine; cases who <;> simp

@[step] theorem rule_spares_spec (claims : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (p : alloc.vec.Vec U8)
    (who : Option U32) (i : Usize) (hi : i.val < claims.val.length) :
    fronts.rule_spares claims p who i ⦃ _ => True ⦄ := by
  unfold fronts.rule_spares
  step*

/-- **P6 for rules.** A namespace rule is set only on a prefix its setter may write: no front, and
no core caller, sets a rule on a prefix another front owns. -/
theorem may_set_rule_spec (claims : alloc.vec.Vec (alloc.vec.Vec U8 × U32)) (p : alloc.vec.Vec U8) (who : Option U32) :
    fronts.may_set_rule claims p who ⦃ b => b = true → ∃ o, OwnerOf claims.val p.val o ∧ (o = none ∨ o = who) ⦄ := by
  unfold fronts.may_set_rule fronts.may_set_rule_loop
  step*
  refine WP.spec_mono (P₀ := fun _ => True) ?_ ?_
  · apply loop_idx_spec _ (fun i => i) claims.val.length (fun _ => True) (fun _ => True)
    · intro i _ hi
      unfold fronts.may_set_rule_loop.body
      step*
    · trivial
    · simp
  · intro r _ _; exact ⟨_, b_post, b_post1.1 ‹_›⟩

/-! ## Retention -/

@[step] theorem is_digit_spec (c : U8) : retention.is_digit c ⦃ _ => True ⦄ := by
  unfold retention.is_digit; step*

@[step] theorem flush_run_spec (out : alloc.vec.Vec (Bool × alloc.vec.Vec U8)) (cur : alloc.vec.Vec U8) (cd d : Bool)
    (h : out.val.length < Usize.max) :
    retention.flush_run out cur cd d ⦃ r =>
      (r.1.val.length = out.val.length ∧ r.2 = cur) ∨
      (r.1.val.length = out.val.length + 1 ∧ r.2.val = [] ∧ cur.val ≠ []) ⦄ := by
  unfold retention.flush_run
  step*
  all_goals first
    | (left; simp_all; done)
    | (right; refine ⟨by simp_all, by simp, ?_⟩; intro hc; simp_all)
    | (simp_all; done)

abbrev RunsState := alloc.vec.Vec (Bool × alloc.vec.Vec U8) × alloc.vec.Vec U8 × Bool × Usize
abbrev RunsOut := alloc.vec.Vec (Bool × alloc.vec.Vec U8) × alloc.vec.Vec U8 × Bool

theorem runs_loop_spec (s : alloc.vec.Vec U8) :
    retention.runs_loop s (alloc.vec.Vec.new _) (alloc.vec.Vec.new U8) false 0#usize ⦃ r =>
      r.1.val.length + (if r.2.1.val = [] then 0 else 1) ≤ s.val.length ⦄ := by
  unfold retention.runs_loop
  apply loop_idx_spec _ (fun x : RunsState => x.2.2.2) s.val.length
    (fun x : RunsState => x.1.val.length + (if x.2.1.val = [] then 0 else 1) ≤ x.2.2.2.val ∧ x.2.1.val.length ≤ x.2.2.2.val)
    (fun r : RunsOut => r.1.val.length + (if r.2.1.val = [] then 0 else 1) ≤ s.val.length)
  · rintro ⟨out, cur, cd, i⟩ ⟨h1, h2⟩ hi
    simp only at h1 h2 hi
    unfold retention.runs_loop.body
    have hs := s.property
    step*
    · have : i.val < s.val.length := by scalar_tac
      rcases out1_post with ⟨_, rfl⟩ | ⟨_, h0, _⟩
      · omega
      · rw [List.length_eq_zero_iff.mpr h0]; scalar_tac
    · have hlt : i.val < s.val.length := by scalar_tac
      simp only [cur2_post, List.append_eq_nil_iff, List.cons_ne_nil, and_false, if_false,
        List.length_append, List.length_singleton]
      rcases out1_post with ⟨ho, rfl⟩ | ⟨ho, h0, hc⟩
      · by_cases hc : cur1.val = []
        · simp only [hc, if_true] at h1
          rw [hc]; simp; omega
        · simp only [hc, if_false] at h1; omega
      · simp only [hc, if_false] at h1
        rw [h0]; simp; omega
  · simp
  · simp

@[step] theorem runs_spec (s : alloc.vec.Vec U8) : retention.runs s ⦃ _ => True ⦄ := by
  unfold retention.runs
  have hs := s.property
  apply WP.spec_bind (Pₘ := fun r : RunsOut => r.1.val.length + (if r.2.1.val = [] then 0 else 1) ≤ s.val.length)
  · exact runs_loop_spec s
  · rintro ⟨out, cur, cd⟩ h
    simp only at h
    step*
    have : cur.val ≠ [] := List.ne_nil_of_length_pos (by scalar_tac)
    simp only [this, if_false] at h; scalar_tac

@[step] theorem skip_zeros_spec (s : alloc.vec.Vec U8) : retention.skip_zeros s ⦃ n => n.val ≤ s.val.length ⦄ := by
  unfold retention.skip_zeros retention.skip_zeros_loop
  apply loop_idx_spec _ (fun i => i) s.val.length (fun _ => True) (fun n : Usize => n.val ≤ s.val.length)
  · intro i _ hi
    unfold retention.skip_zeros_loop.body
    step*
  · trivial
  · simp

@[step] theorem cmp_from_spec (a : alloc.vec.Vec U8) (ai : Usize) (b : alloc.vec.Vec U8) (bi n : Usize)
    (ha : ai.val + n.val ≤ a.val.length) (hb : bi.val + n.val ≤ b.val.length) :
    retention.cmp_from a ai b bi n ⦃ _ => True ⦄ := by
  have := a.property; have := b.property
  unfold retention.cmp_from retention.cmp_from_loop
  apply loop_idx_spec _ (fun k => k) n.val (fun _ => True) (fun _ => True)
  · intro k _ hk
    unfold retention.cmp_from_loop.body
    step*
  · trivial
  · simp

@[step] theorem run_cmp_spec (p q : Bool × alloc.vec.Vec U8) : retention.run_cmp p q ⦃ _ => True ⦄ := by
  unfold retention.run_cmp
  obtain ⟨b1, v1⟩ := p; obtain ⟨b2, v2⟩ := q
  simp only
  step*

theorem version_cmp_loop_spec (a b : alloc.vec.Vec U8) (ra rb : alloc.vec.Vec (Bool × alloc.vec.Vec U8)) (n : Usize)
    (hn : n.val ≤ ra.val.length ∧ n.val ≤ rb.val.length) :
    retention.version_cmp_loop a b ra rb n 0#usize ⦃ _ => True ⦄ := by
  unfold retention.version_cmp_loop
  apply loop_idx_spec _ (fun i => i) n.val (fun _ => True) (fun _ => True)
  · intro i _ hi
    unfold retention.version_cmp_loop.body
    step*
  · trivial
  · simp

@[step] theorem version_cmp_spec (a b : alloc.vec.Vec U8) : retention.version_cmp a b ⦃ _ => True ⦄ := by
  unfold retention.version_cmp
  step*
  apply WP.spec_bind (Pₘ := fun n : Usize => n.val ≤ ra.val.length ∧ n.val ≤ rb.val.length)
  · split <;> (rw [WP.spec_ok]; simp only [alloc.vec.Vec.len, Usize.ofNatCore_val_eq]; scalar_tac)
  · intro n hn; exact version_cmp_loop_spec a b ra rb n hn

/-- What retention may do with entry `k` of a series whose first `old` entries are old. -/
def ActionOK (k old : Nat) (onlyRegen : Bool) (e : retention.Entry) (a : retention.Action) : Prop :=
  (old ≤ k → a = .Keep) ∧
  (a = .Delete → k < old ∧ (onlyRegen = true → e.regenerable = true) ∧
     ∃ rm, MobilityOf e.rules.val e.name.val rm ∧ rm.2 = .Mutable)

@[step] theorem action_spec (k old : Usize) (onlyRegen : Bool) (e : retention.Entry) :
    retention.action k old onlyRegen e ⦃ a => ActionOK k.val old.val onlyRegen e a ⦄ := by
  unfold retention.action
  step*
  all_goals (simp only [ActionOK]; refine ⟨fun h => ?_, fun h => ?_⟩)
  all_goals first
    | (simp_all; done)
    | (scalar_tac)
    | (refine ⟨by scalar_tac, by simp_all, _, ‹MobilityOf _ _ _›, by simp_all⟩)
    | (exfalso; simp_all; scalar_tac)

/-- Retention's plan for a series: one action per entry, each allowed (`ActionOK`), with the first
`|entries| - keep` entries old. -/
def PlanOK (keep : Nat) (onlyRegen : Bool) (entries : List retention.Entry) (acts : List retention.Action) : Prop :=
  acts.length = entries.length ∧ ∀ j (hj : j < entries.length) (ha : j < acts.length),
    ActionOK j (entries.length - keep) onlyRegen entries[j] acts[j]

theorem actions_loop_spec (onlyRegen : Bool) (entries : alloc.vec.Vec retention.Entry) (old : Usize) :
    retention.plan_loop0_loop0 onlyRegen entries old (alloc.vec.Vec.new _) 0#usize ⦃ out =>
      out.val.length = entries.val.length ∧ ∀ j (hj : j < entries.val.length) (ha : j < out.val.length),
        ActionOK j old.val onlyRegen entries.val[j] out.val[j] ⦄ := by
  unfold retention.plan_loop0_loop0
  have := entries.property
  apply loop_idx_spec _ (fun x : alloc.vec.Vec retention.Action × Usize => x.2) entries.val.length
    (fun x : alloc.vec.Vec retention.Action × Usize => x.1.val.length = x.2.val ∧
      ∀ j (hj : j < entries.val.length) (ha : j < x.1.val.length), ActionOK j old.val onlyRegen entries.val[j] x.1.val[j])
  · rintro ⟨out, k⟩ ⟨hlen, hall⟩ hk
    simp only at hlen hall hk
    unfold retention.plan_loop0_loop0.body
    step*
    have hout : out1.val = out.val ++ [a] := out1_post
    refine ⟨by simp_all, fun j hj ha => ?_, by scalar_tac, by scalar_tac⟩
    by_cases hjk : j < k.val
    · have : out1.val[j] = out.val[j]'(by omega) := by
        simp only [hout]; rw [List.getElem_append_left (by omega)]
      rw [this]; exact hall j hj (by omega)
    · have hj' : j = k.val := by simp [hout] at ha; omega
      subst hj'
      have : out1.val[k.val] = a := by
        simp only [hout]; rw [List.getElem_append_right (by omega)]; simp [hlen]
      rw [this]
      have he : e = entries.val[k.val] := e_post
      rw [← he]; exact a_post
  · exact ⟨by simp, fun j _ ha => by simp at ha⟩
  · simp

/-- **Retention.** If the series is sorted, the plan has one action per entry; the newest `keep` are
kept, and an entry is deleted only if it is old, regenerable when the profile requires it, and its
governing rule lets it be deleted. -/
@[step] theorem plan_spec (keep : Usize) (onlyRegen : Bool) (entries : alloc.vec.Vec retention.Entry) :
    retention.plan keep onlyRegen entries ⦃ r => ∀ acts, r = .Ok acts → PlanOK keep.val onlyRegen entries.val acts.val ⦄ := by
  unfold retention.plan retention.plan_loop0
  apply loop_idx_spec _ (fun i : Usize => i) (max 1 entries.val.length) (fun i : Usize => 1 ≤ i.val)
    (fun r : core.result.Result (alloc.vec.Vec retention.Action) Unit =>
      ∀ acts : alloc.vec.Vec retention.Action, r = .Ok acts → PlanOK keep.val onlyRegen entries.val acts.val)
  · intro i hi _
    unfold retention.plan_loop0.body
    simp only [alloc.vec.Vec.len]
    split
    · step*
      all_goals (have : i.val < entries.val.length := by scalar_tac
                 refine ⟨by omega, by omega, by omega⟩)
    · apply WP.spec_bind (Pₘ := fun old : Usize => old.val = entries.val.length - keep.val)
      · split
        · step*
        · rw [WP.spec_ok]; scalar_tac
      · intro old hold
        apply WP.spec_bind (Pₘ := fun out : alloc.vec.Vec retention.Action =>
          out.val.length = entries.val.length ∧ ∀ j (hj : j < entries.val.length) (ha : j < out.val.length),
            ActionOK j old.val onlyRegen entries.val[j] out.val[j])
        · exact actions_loop_spec onlyRegen entries old
        · rintro out ⟨hl, ha⟩
          rw [WP.spec_ok]
          intro acts h; simp only [core.result.Result.Ok.injEq] at h; subst h
          exact ⟨hl, fun j hj hja => by rw [← hold]; exact ha j hj hja⟩
  · simp
  · simp

/-! ## Objects (P1) -/

theorem arr_ext (a b : Std.Array U8 32#usize) (h : a.val = b.val) : a = b := Aeneas.Std.Array.ext a b h

@[step] theorem accept_read_spec (key digest : Std.Array U8 32#usize) :
    objects.accept_read key digest ⦃ b => (b = true ↔ key = digest) ⦄ := by
  unfold objects.accept_read objects.accept_read_loop
  have hk : key.val.length = 32 := by simp
  have hd : digest.val.length = 32 := by simp
  apply loop_idx_spec _ (fun i => i) 32
    (fun i => ∀ j (hj : j < i.val) (h1 : j < key.val.length) (h2 : j < digest.val.length), key.val[j] = digest.val[j])
  · intro i hall hi
    unfold objects.accept_read_loop.body
    step*
    refine iff_of_true trivial ?_
    apply arr_ext
    apply List.ext_getElem (by rw [hk, hd])
    intro j h1 h2; exact hall j (by scalar_tac) h1 h2
  · intro j hj; simp at hj
  · simp

/-- **P1 (a read returns only what was stored under the key).** A verified read accepts bytes only
if their digest is the key; with BLAKE3's collision resistance (assumed), the bytes are the stored
content. -/
theorem read_accepts_only_key (key digest : Std.Array U8 32#usize)
    (h : objects.accept_read key digest = ok true) : key = digest := by
  have := accept_read_spec key digest
  rw [h] at this
  exact ((WP.spec_ok _).1 this).1 rfl

/-- **P1 (content is never overwritten).** A put of an object that is already present writes
nothing; only an absent object is appended. -/
theorem put_never_overwrites (present : Bool) :
    objects.put_action present = ok (if present then .Skip else .Append) := by
  unfold objects.put_action; cases present <;> rfl

/-! ## Decoders: the pack record header -/

/-- The little-endian u32 at `b[at..at+4]`. -/
def le32L (b : List U8) (at_ : Nat) : Nat :=
  b[at_]!.val + b[at_ + 1]!.val * 256 + b[at_ + 2]!.val * 65536 + b[at_ + 3]!.val * 16777216

@[step] theorem le32_spec (b : Slice U8) (at_ : Usize) (h : at_.val + 4 ≤ b.val.length) :
    codec.le32 b at_ ⦃ r => r.val = le32L b.val at_.val ⦄ := by
  unfold codec.le32
  step*
  simp only [le32L]
  rw [getElem!_pos b.val at_.val (by omega), getElem!_pos b.val (at_.val + 1) (by omega),
    getElem!_pos b.val (at_.val + 2) (by omega), getElem!_pos b.val (at_.val + 3) (by omega)]
  simp_all [UScalar.cast_val_eq]
  have h0 : ((b.val)[at_.val]'(by omega)).val < 256 := by scalar_tac
  have h1 : ((b.val)[at_.val + 1]'(by omega)).val < 256 := by scalar_tac
  have h2 : ((b.val)[at_.val + 2]'(by omega)).val < 256 := by scalar_tac
  have h3 : ((b.val)[at_.val + 3]'(by omega)).val < 256 := by scalar_tac
  omega

/-- The little-endian u64 at `b[at..at+8]`. -/
def le64L (b : List U8) (at_ : Nat) : Nat := le32L b at_ + le32L b (at_ + 4) * 4294967296

theorem le32L_lt (b : Slice U8) (at_ : Nat) (h : at_ + 4 ≤ b.val.length) : le32L b.val at_ < 4294967296 := by
  simp only [le32L]
  rw [getElem!_pos b.val at_ (by omega), getElem!_pos b.val (at_ + 1) (by omega),
    getElem!_pos b.val (at_ + 2) (by omega), getElem!_pos b.val (at_ + 3) (by omega)]
  have h0 : ((b.val)[at_]'(by omega)).val < 256 := by scalar_tac
  have h1 : ((b.val)[at_ + 1]'(by omega)).val < 256 := by scalar_tac
  have h2 : ((b.val)[at_ + 2]'(by omega)).val < 256 := by scalar_tac
  have h3 : ((b.val)[at_ + 3]'(by omega)).val < 256 := by scalar_tac
  omega

@[step] theorem le64_spec (b : Slice U8) (at_ : Usize) (h : at_.val + 8 ≤ b.val.length) :
    codec.le64 b at_ ⦃ r => r.val = le64L b.val at_.val ⦄ := by
  unfold codec.le64
  have hl0 := le32L_lt b at_.val (by omega)
  have hl1 := le32L_lt b (at_.val + 4) (by omega)
  step*
  all_goals simp_all [le64L, UScalar.cast_val_eq]
  all_goals omega

@[step] theorem hash_at_spec (b : Slice U8) (hb : 40 ≤ b.val.length) :
    codec.hash_at b ⦃ h => ∀ j (hj : j < 32), h.val[j]! = b.val[8 + j]! ⦄ := by
  unfold codec.hash_at codec.hash_at_loop
  apply loop_idx_spec _ (fun x : Std.Array U8 32#usize × Usize => x.2) 32
    (fun x : Std.Array U8 32#usize × Usize => ∀ j (hj : j < x.2.val), x.1.val[j]! = b.val[8 + j]!)
  · rintro ⟨h, i⟩ hinv hi
    simp only at hinv hi
    unfold codec.hash_at_loop.body
    step*
    refine ⟨fun j hj => ?_, by scalar_tac, by scalar_tac⟩
    have hl : h.val.length = 32 := by simp
    rw [a_post, Aeneas.Std.Array.set_val_eq]
    simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set]
    by_cases hji : i.val = j
    · subst hji
      simp only [if_true, hl, show i.val < 32 by scalar_tac, Option.getD_some]
      have hi32 : i.val < 32 := by scalar_tac
      simp only [i1_post] at i2_post
      rw [i2_post, List.getElem?_eq_getElem (by omega), Option.getD_some]
    · simp only [hji, if_false]
      have := hinv j (by scalar_tac)
      simpa only [List.getElem!_eq_getElem?_getD] using this
  · intro j hj; simp at hj
  · simp

/-- What `parse_header` returns for the bytes `b`: the first failing check, or the header with every
field decoded little endian from its offset and every bound checked. -/
def HeaderOK (b : List U8) : core.result.Result codec.Header codec.HeaderError → Prop
  | .Ok h => 52 ≤ b.length ∧ le32L b 0 = 1381184847 ∧ h.kind = b[4]! ∧ h.codec = b[5]! ∧
      h.kind.val ≤ 5 ∧ h.codec.val ≤ 2 ∧ h.raw_len.val = le64L b 40 ∧ h.stored_len.val = le32L b 48 ∧
      (∀ j (hj : j < 32), h.hash.val[j]! = b[8 + j]!) ∧
      le32L b 48 ≤ 2148532224 ∧ (h.kind.val = 0 → le64L b 40 ≤ 2147483648) ∧
      ((h.kind.val = 2 ∨ h.kind.val = 3 ∨ h.kind.val = 5) → le64L b 40 ≤ 67108864)
  | .Err .Short => b.length < 52
  | .Err .BadMagic => 52 ≤ b.length ∧ le32L b 0 ≠ 1381184847
  | .Err .BadKind => 52 ≤ b.length ∧ le32L b 0 = 1381184847 ∧ 5 < b[4]!.val
  | .Err .BadCodec => 52 ≤ b.length ∧ le32L b 0 = 1381184847 ∧ b[4]!.val ≤ 5 ∧ 2 < b[5]!.val
  | .Err .TooLong => 52 ≤ b.length ∧ le32L b 0 = 1381184847 ∧ b[4]!.val ≤ 5 ∧ b[5]!.val ≤ 2 ∧
      (2148532224 < le32L b 48 ∨ (b[4]!.val = 0 ∧ 2147483648 < le64L b 40) ∨
       ((b[4]!.val = 2 ∨ b[4]!.val = 3 ∨ b[4]!.val = 5) ∧ 67108864 < le64L b 40))

@[step] theorem is_frame_spec (k : U8) :
    codec.is_frame k ⦃ r => (r = true ↔ (k.val = 2 ∨ k.val = 3 ∨ k.val = 5)) ⦄ := by
  unfold codec.is_frame; step*; all_goals (simp_all; try scalar_tac)

@[step] theorem too_long_spec (k : U8) (raw : U64) (st : U32) :
    codec.too_long k raw st ⦃ r => (r = true ↔ (2148532224 < st.val ∨ (k.val = 0 ∧ 2147483648 < raw.val) ∨
      ((k.val = 2 ∨ k.val = 3 ∨ k.val = 5) ∧ 67108864 < raw.val))) ⦄ := by
  unfold codec.too_long
  simp only [codec.MAX_STORED, codec.MAX_BLOB, codec.MAX_FRAME]
  step*
  all_goals (simp_all; try scalar_tac)

/-- The record-header decoder is total and equals its spec on every input. -/
@[step] theorem parse_header_spec (b : Slice U8) :
    codec.parse_header b ⦃ r => HeaderOK b.val r ⦄ := by
  unfold codec.parse_header
  simp only [codec.REC_HEADER, codec.REC_MAGIC]
  step*
  all_goals first
    | (scalar_tac; done)
    | (simp only [HeaderOK]; scalar_tac; done)
    | (simp only [HeaderOK]; simp_all [getElem!_pos]; done)
    | (have hl : 52 ≤ b.val.length := by scalar_tac
       have hm : le32L b.val 0 = 1381184847 := by
         have : i1.val = 1381184847 := by simpa using ‹¬(i1 != 1381184847#u32) = true›
         rw [← i1_post, this]
       have h4 : b.val[4]! = kind := by rw [getElem!_pos b.val 4 (by omega), kind_post]
       have h5 : b.val[5]! = codec_code := by rw [getElem!_pos b.val 5 (by omega), codec_code_post]
       simp only [HeaderOK, h4, h5, hm, hl, true_and]
       simp_all
       all_goals (try scalar_tac))

/-! ## Ref-journal framing -/

/-- **The journal never truncates valid history.** A record whose CRC fails while more data
follows it is corruption (an error), not a torn tail; only the last record, a short header, or a
zero-filled tail is treated as torn. A record is accepted only with a valid CRC, and its end is
within the data. -/
@[step] theorem journal_step_spec (rest : Usize) (zero : Bool) (len maxr : U32) (crc : Bool) :
    codec.journal_step rest zero len maxr crc ⦃ s =>
      (s = .Torn ↔ (rest.val < 8 ∨ zero = true ∨
         (len.val ≤ maxr.val ∧ (rest.val < 8 + len.val ∨ (crc = false ∧ 8 + len.val = rest.val))))) ∧
      (s = .Checksum ↔ (8 ≤ rest.val ∧ zero = false ∧ len.val ≤ maxr.val ∧ 8 + len.val < rest.val ∧ crc = false)) ∧
      (∀ e, s = .Record e → crc = true ∧ e.val = 8 + len.val ∧ e.val ≤ rest.val) ⦄ := by
  unfold codec.journal_step
  step*
  all_goals (simp_all; try omega)
