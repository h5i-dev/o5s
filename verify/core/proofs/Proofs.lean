import Pico2petaKernel
import H5iAppLib
open Aeneas Aeneas.Std Result pico2peta_kernel H5iAppLib

namespace pico2peta_kernel.Proofs

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
