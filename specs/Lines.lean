import Checked
import Coordinates
import LinesModel
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Specifications of the extracted `lines` kernel (to prove)

Each theorem relates an extracted function in `crates/kernel/src/lines.rs` to
the model in `proofs/LinesModel.lean`, whose order-independence and
conflict theorems are already proved. When this file is proved, move it to
`proofs/` and add its theorems to `h5i-app.toml`.
-/
namespace o5s_kernel.Lines
open LinesModel Rendering

abbrev Rg := U64 × U64 × alloc.vec.Vec U8

/-- A range as the model sees it. -/
def ab (r : Rg) : R (List U8) := (r.1.val, r.2.1.val, r.2.2.val)

/-- The model input of an extracted range vector. -/
def L (rs : alloc.vec.Vec Rg) : List (R (List U8)) := rs.val.map ab

theorem ab_inj {a b : Rg} : ab a = ab b ↔ a = b := by
  constructor
  · intro h
    obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
    simp only [ab, Prod.mk.injEq] at h
    obtain ⟨h1, h2, h3⟩ := h
    have e1 : a1 = b1 := UScalar.eq_of_val_eq h1
    have e2 : a2 = b2 := UScalar.eq_of_val_eq h2
    have e3 : a3 = b3 := alloc.vec.Vec.ext _ _ h3
    subst e1 e2 e3; rfl
  · rintro rfl; rfl

theorem conflict_iff (a z b w : U64) :
    Interval.Conflict a z b w ↔ NConf a.val z.val b.val w.val := by
  have ez : a = z ↔ a.val = z.val := ⟨fun h => h ▸ rfl, UScalar.eq_of_val_eq⟩
  have ew : b = w ↔ b.val = w.val := ⟨fun h => h ▸ rfl, UScalar.eq_of_val_eq⟩
  unfold Interval.Conflict NConf
  simp only [ez, ew]
  split_ifs <;> simp only [UScalar.le_equiv, UScalar.lt_equiv]

@[step] theorem same_spec (a b : Rg) : lines.same a b ⦃ r => r = decide (a = b) ⦄ := by
  obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
  unfold lines.same
  by_cases h1 : a1 = b1
  · by_cases h2 : a2 = b2
    · subst h1 h2
      show (if a1 = a1 then (if a2 = a2 then h5i_app_std.bytes.eq a3 b3 else ok false)
        else ok false) ⦃ r => r = decide ((a1, a2, a3) = (a1, a2, b3)) ⦄
      rw [if_pos rfl, if_pos rfl]
      apply WP.spec_mono (h5i_app_std.Specs.bytes.eq_spec a3 b3)
      intro r hr
      rw [hr]
      simp [alloc.vec.Vec.eq_iff]
    · simp [h1, h2]
  · simp [h1]

@[step] theorem key_le_spec (a b : Rg) :
    lines.key_le a b ⦃ r => r = decide (KeyLe (ab a) (ab b)) ⦄ := by
  obtain ⟨a1, a2, a3⟩ := a; obtain ⟨b1, b2, b3⟩ := b
  unfold lines.key_le
  simp only [KeyLe, ab]
  by_cases h1 : a1 < b1
  · have : a1.val < b1.val := h1
    simp [h1, this]
  · have h1' : ¬ a1.val < b1.val := h1
    by_cases h2 : a1 = b1
    · subst h2
      simp [h1]
    · have : a1.val ≠ b1.val := fun e => h2 (UScalar.eq_of_val_eq e)
      simp [h1, h1', h2, this]

theorem at_L (rs : alloc.vec.Vec Rg) (j : Nat) (h : j < rs.val.length) :
    at_ (L rs) j = ab rs.val[j] := by
  simp [at_, L, getElem!_pos, h]

theorem at_get (l : List (R (List U8))) (i : Nat) (h : i < l.length) : at_ l i = l[i] := by
  rw [at_, getElem!_pos l i h]

def sortedStep (rs : List (R (List U8))) (ok : Bool) (i : Nat) : Bool :=
  ok && decide (KeyLe (at_ rs (i - 1)) (at_ rs i))

theorem sorted_loop_spec (rs : alloc.vec.Vec Rg) (h1 : 1 ≤ rs.val.length) :
    lines.sorted_loop rs true 1#usize ⦃ b =>
      b = ((List.range rs.val.length).drop 1).foldl (sortedStep (L rs)) true ⦄ := by
  unfold lines.sorted_loop
  have hl : (List.range rs.val.length).length = rs.val.length := List.length_range
  apply WP.spec_mono (loop_fold (List.range rs.val.length) (fun b : Bool => b) (sortedStep (L rs))
    (fun _ i => 1 ≤ i)
    (fun x => lines.sorted_loop.body rs x.1 x.2) ?_ true 1#usize (by simp [hl]; omega) (by simp))
  · intro b h; simpa using h
  · intro ok i hi hinv
    unfold lines.sorted_loop.body
    h5i_step
    all_goals first
      | (refine ⟨by assumption, ?_⟩
         simp only [sortedStep, Bool.true_and]
         rw [at_L rs _ (by omega), at_L rs _ (by assumption)]
         try exact decide_eq_decide.mpr Iff.rfl)
      | simp [sortedStep]

theorem fold_sorted (l : List (R (List U8))) :
    ∀ (k : Nat) (b : Bool), 1 ≤ k →
      ((List.range' k (l.length - k)).foldl (sortedStep l) b = true ↔
        b = true ∧ ∀ i, k ≤ i → i < l.length → KeyLe (at_ l (i - 1)) (at_ l i)) := by
  intro k
  induction h : l.length - k generalizing k with
  | zero =>
    intro b hk
    simp only [List.range'_zero, List.foldl_nil]
    constructor
    · intro hb; exact ⟨hb, fun i hi hi' => by omega⟩
    · exact fun x => x.1
  | succ n ih =>
    intro b hk
    rw [List.range'_succ, List.foldl_cons, ih (k + 1) (by omega) _ (by omega)]
    simp only [sortedStep, Bool.and_eq_true, decide_eq_true_eq]
    constructor
    · rintro ⟨⟨hb, hkk⟩, rest⟩
      refine ⟨hb, fun i hi hi' => ?_⟩
      rcases Nat.lt_or_eq_of_le hi with hi | rfl
      · exact rest i hi hi'
      · exact hkk
    · rintro ⟨hb, all⟩
      exact ⟨⟨hb, all k (le_refl _) (by omega)⟩, fun i hi hi' => all i (by omega) hi'⟩

theorem adjacent_iff_sorted (l : List (R (List U8))) :
    (∀ i, 1 ≤ i → i < l.length → KeyLe (at_ l (i - 1)) (at_ l i)) ↔ Sorted l := by
  unfold Sorted
  rw [List.pairwise_iff_getElem]
  constructor
  · intro h i j hi hj hij
    obtain ⟨d, rfl⟩ : ∃ d, j = i + 1 + d := ⟨j - (i + 1), by omega⟩
    induction d with
    | zero =>
      have h2 := h (i + 1) (by omega) hj
      rw [at_get l (i + 1 - 1) (by omega), at_get l (i + 1) (by omega)] at h2
      simp only [Nat.add_sub_cancel] at h2
      exact h2
    | succ d ih =>
      have h1 := ih (by omega) (by omega)
      have h2 := h (i + 1 + d + 1) (by omega) (by omega)
      rw [at_get l (i + 1 + d + 1 - 1) (by omega), at_get l (i + 1 + d + 1) (by omega)] at h2
      simp only [Nat.add_sub_cancel] at h2
      exact keyle_trans _ _ _ h1 h2
  · intro s i hi hi'
    have := s (i - 1) i (by omega) hi' (by omega)
    rw [at_get l _ (by omega), at_get l _ hi']
    exact this

/-- `sorted` checks adjacent pairs, which is the pairwise order because
`KeyLe` is transitive. -/
@[step] theorem sorted_spec (rs : alloc.vec.Vec Rg) :
    lines.sorted rs ⦃ b => b = true ↔ Sorted (L rs) ⦄ := by
  unfold lines.sorted
  have hlen : (L rs).length = rs.val.length := by simp [L]
  by_cases h0 : rs.val.length = 0
  · unfold lines.sorted_loop
    rw [loop.eq_def]
    unfold lines.sorted_loop.body
    have : ¬ (1#usize : Usize) < alloc.vec.Vec.len rs := by
      simp [alloc.vec.Vec.len, h0]
    simp only [this, ↓reduceIte, WP.spec_ok]
    simp [Sorted, L, List.eq_nil_of_length_eq_zero h0]
  · apply WP.spec_mono (sorted_loop_spec rs (by omega))
    intro b hb
    rw [hb, ← adjacent_iff_sorted, List.range_eq_range', List.drop_range',
      show rs.val.length - 1 = (L rs).length - 1 by rw [hlen], show (0 + 1) = 1 from rfl]
    have := fold_sorted (L rs) 1 true (le_refl _)
    simp only [true_and] at this
    rw [hlen] at this ⊢
    exact this

/-- The merge state as the model sees it. -/
def absM (st : lines.Merge) : St := (st.ok, st.kept.val.map (·.val))


/-- What the merge loop keeps true after `i` steps. -/
abbrev MInv (st : lines.Merge) (i : Nat) : Prop :=
  (∀ j ∈ st.kept.val, j.val < i) ∧ st.kept.val.length ≤ i

theorem getLast?_map_val (k : List Usize) :
    (k.map (·.val)).getLast? = k.getLast?.map (·.val) := by
  simp [List.getLast?_map]

@[step] theorem merge_step_spec (st : lines.Merge) (rs : alloc.vec.Vec Rg) (i : Usize)
    (hi : i.val < rs.val.length) (inv : MInv st i.val) :
    lines.merge_step st rs i ⦃ out => absM out = step (L rs) (absM st) i.val ∧
      MInv out (i.val + 1) ⦄ := by
  obtain ⟨ok, ks⟩ := st
  obtain ⟨hk, hkl⟩ := inv
  simp only at hk hkl
  have hmax : rs.val.length ≤ Usize.max := by scalar_tac
  have keep : ∀ j ∈ ks.val, j.val < i.val + 1 := fun j hj => by have := hk j hj; omega
  have hx : at_ (L rs) i.val = ab rs.val[i.val] := at_L rs i.val hi
  unfold lines.merge_step
  cases ok
  · simp only [Bool.false_eq_true, ↓reduceIte, WP.spec_ok]
    exact ⟨by simp [absM, step], keep, by show ks.val.length ≤ i.val + 1; omega⟩
  · simp only [↓reduceIte]
    step*
    all_goals have hab : at_ (L rs) i.val = (i1.val, i2.val, v.val) := by rw [hx, ← i1_post]; rfl
    all_goals first
      -- A reversed range stops the merge.
      | (have hr : i2.val < i1.val := by scalar_tac
         exact ⟨by simp [absM, step, hab, hr], keep, by show ks.val.length ≤ i.val + 1; omega⟩)
      -- The first range is kept.
      | (have hks : ks.val = [] := List.eq_nil_of_length_eq_zero (by scalar_tac)
         have hr : ¬ i2.val < i1.val := by scalar_tac
         refine ⟨?_, fun j hj => ?_, ?_⟩
         · simp [absM, step, hab, hr, hks, kept_post]
         · simp only [kept_post, hks, List.nil_append, List.mem_singleton] at hj
           subst hj; omega
         · simp [kept_post, hks])
      -- The last kept index is in bounds.
      | (rw [i6_post]
         have h5' : i5.val < ks.val.length := by scalar_tac
         have := hk _ (List.getElem_mem h5')
         scalar_tac)
      -- Otherwise compare with the last kept range.
      | (have hr : ¬ i2.val < i1.val := by scalar_tac
         have h5 : i5.val = ks.val.length - 1 := by scalar_tac
         have h5' : i5.val < ks.val.length := by scalar_tac
         have hlast : ks.val.getLast? = some i6 := by
           rw [List.getLast?_eq_getElem?, ← h5, List.getElem?_eq_getElem h5', i6_post]
         have hi6 : i6.val < rs.val.length := by
           have := hk _ (List.getElem_mem h5'); rw [i6_post]; omega
         obtain ⟨l1, l2, lv⟩ := last
         have hjl : at_ (L rs) i6.val = (l1.val, l2.val, lv.val) := by
           rw [at_L rs _ hi6, ← last_post]; rfl
         first
         -- An identical range counts once.
         | (have hbt : b = true := by assumption
            rw [b_post] at hbt
            obtain ⟨rfl, rfl, rfl⟩ : l1 = i1 ∧ l2 = i2 ∧ lv = v := by
              simpa using of_decide_eq_true hbt
            refine ⟨?_, keep, by show ks.val.length ≤ i.val + 1; omega⟩
            simp [absM, step, hab, hr, getLast?_map_val, hlast, hjl])
         | (have hb : ¬ (l1, l2, lv) = (i1, i2, v) := by
              intro e
              have hbt : ¬ b = true := by assumption
              rw [b_post] at hbt
              exact hbt (decide_eq_true e)
            have hne : (l1.val, l2.val, lv.val) ≠ (i1.val, i2.val, v.val) := fun e =>
              hb ((ab_inj (a := (l1, l2, lv)) (b := (i1, i2, v))).mp e)
            step*
            all_goals first
            -- A conflict stops the merge.
            | (have hn : NConf l1.val l2.val i1.val i2.val :=
                 (conflict_iff _ _ _ _).mp (b1_post.mp (by assumption))
               refine ⟨?_, keep, by show ks.val.length ≤ i.val + 1; omega⟩
               simp [absM, step, hab, hr, getLast?_map_val, hlast, hjl, hne, Conf, hn])
            -- A separate range is kept.
            | (have hn : ¬ NConf l1.val l2.val i1.val i2.val := fun h =>
                 (by assumption : ¬ b1 = true) (b1_post.mpr ((conflict_iff _ _ _ _).mpr h))
               refine ⟨?_, fun j hj => ?_, ?_⟩
               · simp [absM, step, hab, hr, getLast?_map_val, hlast, hjl, hne, Conf, hn, kept_post]
               · simp only [kept_post, List.mem_append, List.mem_singleton] at hj
                 rcases hj with hj | rfl
                 · exact keep _ hj
                 · omega
               · simp [kept_post]; omega)))

@[step] theorem merge_loop_spec (rs : alloc.vec.Vec Rg) :
    lines.merge_loop rs { ok := true, kept := alloc.vec.Vec.new Usize } 0#usize ⦃ st =>
      absM st = merge (L rs) ⦄ := by
  unfold lines.merge_loop
  have hl : (List.range rs.val.length).length = rs.val.length := List.length_range
  apply WP.spec_mono (loop_fold (List.range rs.val.length) absM (step (L rs))
    (fun st i => MInv st i)
    (fun x => lines.merge_loop.body rs x.1 x.2) ?_ _ 0#usize (by simp) ⟨by simp, by simp⟩)
  · intro st h
    rw [h]
    simp [absM, merge, L]
  · intro st i hi hinv
    unfold lines.merge_loop.body
    h5i_step
    exact ⟨fun j hj => by have := state1_post1 j hj; omega, state1_post2⟩

/-- The extracted merge computes the model merge exactly, on any input. -/
@[step] theorem merge_spec (rs : alloc.vec.Vec Rg) :
    lines.merge rs ⦃ out => match out with
      | none => (merge (L rs)).1 = false
      | some k => (merge (L rs)).1 = true ∧ k.val.map (·.val) = (merge (L rs)).2 ⦄ := by
  unfold lines.merge
  apply WP.spec_bind (merge_loop_spec rs)
  intro st hst
  simp only [absM, Prod.ext_iff] at hst
  beta_reduce
  split <;> simp_all

/-- What a merge of sorted ranges means, from the model theorems: success iff
no range is reversed and no two different ranges conflict; the kept ranges
are the distinct inputs, pairwise separated and in strict key order. -/
theorem kept_ab (rs : alloc.vec.Vec Rg) (k : List Usize) (hk : ∀ j ∈ k, j.val < rs.val.length) :
    (k.map fun j => rs.val[j.val]!).map ab = kept (L rs) (k.map (·.val)) := by
  simp only [kept, List.map_map]
  apply List.map_congr_left
  intro j hj
  simp only [Function.comp]
  rw [at_L rs j.val (hk j hj), getElem!_pos rs.val j.val (hk j hj)]

theorem map_ab_inj {l₁ l₂ : List Rg} (h : l₁.map ab = l₂.map ab) : l₁ = l₂ :=
  List.map_injective_iff.2 (fun _ _ e => ab_inj.1 e) h

theorem merge_correct (rs : alloc.vec.Vec Rg) (s : Sorted (L rs)) :
    lines.merge rs ⦃ out => match out with
      | none => Bad (L rs)
      | some k => ¬ Bad (L rs) ∧ (∀ x, x ∈ kept (L rs) (k.val.map (·.val)) ↔ x ∈ L rs) ∧
          (kept (L rs) (k.val.map (·.val))).Pairwise Sep ⦄ := by
  apply WP.spec_mono (merge_spec rs)
  intro out h
  cases out with
  | none => exact merge_fails _ s h
  | some k =>
    obtain ⟨h1, h2⟩ := h
    simp only
    rw [h2]
    obtain ⟨-, -, mem, ch⟩ := merge_ok _ s h1
    exact ⟨not_bad_of_ok _ s h1, mem, ch⟩

/-- Two sorted arrangements of the same ranges (for example the versions of
concurrent heads concatenated in either order, then sorted) merge to the same
edits. -/
theorem merge_order_independent (rs₁ rs₂ : alloc.vec.Vec Rg)
    (s₁ : Sorted (L rs₁)) (s₂ : Sorted (L rs₂)) (same : ∀ x, x ∈ rs₁.val ↔ x ∈ rs₂.val) :
    lines.merge rs₁ ⦃ o₁ => lines.merge rs₂ ⦃ o₂ =>
      (o₁.isSome ↔ o₂.isSome) ∧
      ∀ k₁ k₂, o₁ = some k₁ → o₂ = some k₂ →
        k₁.val.map (fun j => rs₁.val[j.val]!) = k₂.val.map (fun j => rs₂.val[j.val]!) ⦄ ⦄ := by
  have sameL : ∀ x, x ∈ L rs₁ ↔ x ∈ L rs₂ := by
    intro x
    simp only [L, List.mem_map]
    constructor
    · rintro ⟨a, ha, rfl⟩; exact ⟨a, (same a).1 ha, rfl⟩
    · rintro ⟨a, ha, rfl⟩; exact ⟨a, (same a).2 ha, rfl⟩
  obtain ⟨he, hk⟩ := LinesModel.merge_order_independent _ _ s₁ s₂ sameL
  apply WP.spec_mono (merge_spec rs₁)
  intro o₁ h₁
  apply WP.spec_mono (merge_spec rs₂)
  intro o₂ h₂
  constructor
  · cases o₁ <;> cases o₂ <;> simp_all
  · intro k₁ k₂ e₁ e₂
    subst e₁ e₂
    obtain ⟨a1, b1⟩ := h₁
    obtain ⟨a2, b2⟩ := h₂
    have bound : ∀ (rs : alloc.vec.Vec Rg) (s : Sorted (L rs)) (k : alloc.vec.Vec Usize),
        (merge (L rs)).1 = true → k.val.map (·.val) = (merge (L rs)).2 →
        ∀ j ∈ k.val, j.val < rs.val.length := by
      intro rs s k ok e j hj
      have := ((merge_good _ s).1 ok).2 j.val (by rw [← e]; exact List.mem_map_of_mem hj)
      simpa [L] using this
    apply map_ab_inj
    rw [kept_ab rs₁ k₁.val (bound rs₁ s₁ k₁ a1 b1), kept_ab rs₂ k₂.val (bound rs₂ s₂ k₂ a2 b2), b1, b2]
    exact hk a1

theorem conf_self {α} [DecidableEq α] [Inhabited α] (x : R α) (v : Valid x) : Conf x x := conf_same_key x x rfl rfl v

theorem kept_nodup {α} [DecidableEq α] [Inhabited α] (rs : List (R α)) (s : Sorted rs) (ok : (merge rs).1 = true) :
    (kept rs (merge rs).2).Nodup := by
  obtain ⟨v, -, mem, ch⟩ := merge_ok rs s ok
  exact ch.imp_of_mem fun {a b} ha hb sab =>
    sep_ne a b (v a ((mem a).1 ha)) (v b ((mem b).1 hb)) sab

/-- A merge keeps every range exactly when no range is reversed and no two
ranges conflict, an identical copy included. -/
theorem full_iff {α} [DecidableEq α] [Inhabited α] (rs : List (R α)) (s : Sorted rs) :
    ((merge rs).1 = true ∧ (merge rs).2.length = rs.length) ↔
      (∀ x ∈ rs, Valid x) ∧
      ∀ i j (hi : i < rs.length) (hj : j < rs.length), i < j → ¬ Conf rs[i] rs[j] := by
  constructor
  · rintro ⟨ok, len⟩
    obtain ⟨v, c, mem, -⟩ := merge_ok rs s ok
    refine ⟨v, fun i j hi hj hij cf => ?_⟩
    have nd := kept_nodup rs s ok
    have sub : (kept rs (merge rs).2).Subperm rs := nd.subperm fun x hx => (mem x).1 hx
    have perm := sub.perm_of_length_le (by simp [kept, len])
    have ndr : rs.Nodup := perm.nodup_iff.mp nd
    by_cases e : rs[i] = rs[j]
    · exact List.pairwise_iff_getElem.mp ndr i j hi hj hij e
    · exact c _ (List.getElem_mem hi) _ (List.getElem_mem hj) e cf
  · rintro ⟨v, nc⟩
    have nb : ¬ Bad rs := by
      rintro (⟨x, hx, nv⟩ | ⟨x, hx, y, hy, ne, cf⟩)
      · exact nv (v x hx)
      · obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hx
        obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hy
        rcases Nat.lt_trichotomy i j with h | rfl | h
        · exact nc i j hi hj h cf
        · exact ne rfl
        · exact nc j i hj hi h ((conf_symm _ _).mp cf)
    have ok : (merge rs).1 = true := by
      cases h : (merge rs).1
      · exact absurd (merge_fails rs s h) nb
      · rfl
    refine ⟨ok, ?_⟩
    obtain ⟨-, -, mem, -⟩ := merge_ok rs s ok
    have nd := kept_nodup rs s ok
    have ndr : rs.Nodup := List.pairwise_iff_getElem.mpr fun i j hi hj hij e =>
      nc i j hi hj hij (by rw [e]; exact conf_self _ (v _ (List.getElem_mem hj)))
    have l1 := (nd.subperm fun x hx => (mem x).1 hx).length_le
    have l2 := (ndr.subperm fun x hx => (mem x).2 hx).length_le
    simp only [kept, List.length_map] at l1 l2
    omega

/-- One proposal's ranges for one file: accepted exactly when sorted, no
range reversed, and no two ranges conflict (an identical copy included). This
is the check `prepare_lines` makes. -/
@[step] theorem ranges_valid_spec (rs : alloc.vec.Vec Rg) :
    lines.ranges_valid rs ⦃ b => b = true ↔
      Sorted (L rs) ∧ (∀ x ∈ L rs, Valid x) ∧
      ∀ i j (hi : i < (L rs).length) (hj : j < (L rs).length), i < j →
        ¬ Conf (L rs)[i] (L rs)[j] ⦄ := by
  unfold lines.ranges_valid
  apply WP.spec_bind (sorted_spec rs)
  intro b hb
  by_cases hs : Sorted (L rs)
  · have hbt : b = true := hb.mpr hs
    subst hbt
    simp only [↓reduceIte]
    apply WP.spec_bind (merge_spec rs)
    intro out hout
    have fi := full_iff (L rs) hs
    have hlen : (L rs).length = rs.val.length := by simp [L]
    cases out with
    | none =>
      simp only [WP.spec_ok]
      simp only at hout
      constructor
      · intro h; cases h
      · intro ⟨_, h⟩
        have := (fi.mpr h).1
        rw [hout] at this; cases this
    | some k =>
      simp only at hout
      obtain ⟨ok, hk⟩ := hout
      simp only [WP.spec_ok, decide_eq_true_eq]
      have kl : k.val.length = (merge (L rs)).2.length := by rw [← hk]; simp
      rw [← fi]
      constructor
      · intro h
        refine ⟨hs, ok, ?_⟩
        have : k.val.length = rs.val.length := by
          have := congrArg (·.val) h
          simpa [alloc.vec.Vec.len] using this
        omega
      · rintro ⟨-, -, h⟩
        apply UScalar.eq_of_val_eq
        simp [alloc.vec.Vec.len]
        omega
  · have hbf : b = false := by
      cases b
      · rfl
      · exact absurd (hb.mp rfl) hs
    subst hbf
    simp only [Bool.false_eq_true, ↓reduceIte, WP.spec_ok]
    constructor
    · intro h; cases h
    · intro h; exact absurd h.1 hs

/-- The byte chunks of kept line ranges on `boundaries`. -/
def chunkOf (bs : List Usize) (r : Rg) : Option Chunk :=
  if h : r.1.val ≤ r.2.1.val ∧ r.2.1.val < bs.length then
    some (bs[r.1.val]'(by omega), bs[r.2.1.val]'h.2, r.2.2)
  else none

def chunksStep (bs : List Usize) (acc : Option (List Chunk)) (r : Rg) : Option (List Chunk) :=
  acc.bind fun cs => (chunkOf bs r).map (cs ++ [·])

def chunksOf (bs : List Usize) (rs : List Rg) : Option (List Chunk) :=
  rs.foldl (chunksStep bs) (some [])

def cabs (out : alloc.vec.Vec Chunk) (ok : Bool) : Option (List Chunk) :=
  if ok then some out.val else none

@[step] theorem chunks_loop_spec (bs : alloc.vec.Vec Usize) (rs : alloc.vec.Vec Rg)
    (kept : alloc.vec.Vec Usize) (hk : ∀ j ∈ kept.val, j.val < rs.val.length) :
    lines.chunks_loop bs rs kept (alloc.vec.Vec.new _) true 0#usize ⦃ r =>
      cabs r.1 r.2 = (kept.val.map fun j => rs.val[j.val]!).foldl (chunksStep bs.val) (some []) ⦄ := by
  unfold lines.chunks_loop
  have hl : (kept.val.map fun j => rs.val[j.val]!).length = kept.val.length := by simp
  apply WP.spec_mono (loop_fold2 (kept.val.map fun j => rs.val[j.val]!) cabs (chunksStep bs.val)
    (fun o _ i => o.val.length ≤ i)
    (fun st => lines.chunks_loop.body bs rs kept st.1 st.2.1 st.2.2)
    ?_ (alloc.vec.Vec.new _) true 0#usize (by simp) (by simp [alloc.vec.Vec.new]))
  · intro r h
    simpa [cabs, alloc.vec.Vec.new] using h.1
  · intro o b i hi hinv
    unfold lines.chunks_loop.body
    h5i_step
    all_goals first
      -- After a failure, nothing more is emitted.
      | (exact ⟨⟨by scalar_tac, by simp [cabs, chunksStep]⟩, by omega⟩)
      | (rw [← r_post]
         obtain ⟨x1, x2, xv⟩ := r
         h5i_step
         all_goals first
           -- The boundary lookups are in bounds.
           | (exact lt_of_le_of_lt (Nat.mod_le _ _) (by omega))
           | (exact lt_of_le_of_lt (Nat.mod_le _ _) (by simp_all [UScalar.cast_val_eq]; omega))
           -- A reversed or out-of-range line stops the chunks.
           | (refine ⟨⟨by scalar_tac, ?_⟩, by omega⟩
              rw [← r_post]
              simp [cabs, chunksStep, chunkOf]
              omega)
           -- Otherwise the chunk is emitted.
           | (refine ⟨by scalar_tac, ?_⟩
              have hx2 : x2.val < bs.val.length := by
                first | omega | (simp_all [UScalar.cast_val_eq]; omega)
              have hx1 : x1.val ≤ x2.val := by omega
              have hm1 : x1.val % 2 ^ System.Platform.numBits = x1.val :=
                Nat.mod_eq_of_lt (by scalar_tac)
              have hm2 : x2.val % 2 ^ System.Platform.numBits = x2.val :=
                Nat.mod_eq_of_lt (by scalar_tac)
              rw [← r_post]
              simp only [cabs, ↓reduceIte, chunksStep, Option.bind_some, chunkOf]
              rw [dif_pos ⟨hx1, hx2⟩]
              simp only [Option.map_some, Option.some.injEq]
              rw [x_post]
              simp [hm1, hm2]
              try exact alloc.vec.Vec.ext _ _ (by assumption))
           | (trace_state; sorry))

@[step] theorem chunks_spec (bs : alloc.vec.Vec Usize) (rs : alloc.vec.Vec Rg)
    (kept : alloc.vec.Vec Usize) (hk : ∀ j ∈ kept.val, j.val < rs.val.length) :
    lines.chunks bs rs kept ⦃ out =>
      out.map (·.val) = chunksOf bs.val (kept.val.map fun j => rs.val[j.val]!) ⦄ := by
  unfold lines.chunks
  h5i_step
  all_goals simpa [cabs, chunksOf] using out_post

/-- Rendering is the checked renderer on the line coordinates of the base. -/
@[step] theorem render_spec (original : alloc.vec.Vec U8) (rs : alloc.vec.Vec Rg)
    (kept : alloc.vec.Vec Usize) (hk : ∀ j ∈ kept.val, j.val < rs.val.length)
    (room : original.val.length < Usize.max) :
    lines.render original rs kept ⦃ out =>
      ∃ bs : List Usize, bs.map (·.val) = Coordinates.model original.val ∧
        bs.Pairwise (fun u v => u.val < v.val) ∧ (∀ v ∈ bs, v.val ≤ original.val.length) ∧
        match chunksOf bs (kept.val.map fun j => rs.val[j.val]!) with
        | none => out = none
        | some cs => match out with
          | none => ¬ Checked.Good original.val.length 0 original.val.length cs
          | some v => Checked.Good original.val.length 0 original.val.length cs ∧
              v.val = Rendering.model original.val cs ⦄ := by
  unfold lines.render
  apply WP.spec_bind (Coordinates.line_boundaries_ordered original room)
  intro bs hbs
  apply WP.spec_bind (chunks_spec bs rs kept hk)
  intro o ho
  cases o with
  | none =>
    simp only [Option.map_none] at ho
    simp only [WP.spec_ok]
    refine ⟨bs.val, hbs.1, hbs.2.2, hbs.2.1, ?_⟩
    rw [← ho]
    all_goals trivial
  | some c =>
    simp only [Option.map_some] at ho
    apply WP.spec_mono (Checked.checked_render_spec original c)
    intro out hout
    refine ⟨bs.val, hbs.1, hbs.2.2, hbs.2.1, ?_⟩
    rw [← ho]
    exact hout

theorem mono_get (bs : List Usize) (h : bs.Pairwise (fun u v => u.val < v.val))
    {i j : Nat} (hij : i ≤ j) (hj : j < bs.length) :
    (bs[i]'(by omega)).val ≤ bs[j].val := by
  rcases Nat.lt_or_eq_of_le hij with hlt | rfl
  · exact le_of_lt (List.pairwise_iff_getElem.mp h i j (by omega) hj hlt)
  · exact le_refl _

/-- Ranges in order, each inside the boundaries, become chunks the checked
renderer accepts. -/
theorem fold_good (bs : List Usize) (n : Nat) (hbs : bs.Pairwise (fun u v => u.val < v.val))
    (hn : ∀ v ∈ bs, v.val ≤ n) :
    ∀ (zs : List Rg) (acc : List Chunk) (cursor budget : Nat),
      (∀ z ∈ zs, z.1.val ≤ z.2.1.val ∧ z.2.1.val < bs.length) →
      zs.Pairwise (fun x y => x.2.1.val ≤ y.1.val) →
      (∀ z ∈ zs, ∀ h : z.1.val < bs.length, cursor ≤ (bs[z.1.val]).val) →
      budget + (zs.map fun z => z.2.2.val.length).sum ≤ Usize.max →
      ∃ cs, zs.foldl (chunksStep bs) (some acc) = some (acc ++ cs) ∧
        Checked.Good n cursor budget cs := by
  intro zs
  induction zs with
  | nil =>
    intro acc cursor budget _ _ _ hb
    exact ⟨[], by simp, by simpa [Checked.Good] using hb⟩
  | cons z zs ih =>
    intro acc cursor budget hin hp hc hb
    obtain ⟨hz1, hz2⟩ := hin z (by simp)
    have hzs : ∀ z' ∈ zs, z.2.1.val ≤ z'.1.val := (List.pairwise_cons.mp hp).1
    have hstep : chunksStep bs (some acc) z =
        some (acc ++ [(bs[z.1.val]'(by omega), bs[z.2.1.val]'hz2, z.2.2)]) := by
      simp [chunksStep, chunkOf, hz1, hz2]
    simp only [List.map_cons, List.sum_cons] at hb
    obtain ⟨cs, hf, hg⟩ := ih (acc ++ [(bs[z.1.val]'(by omega), bs[z.2.1.val]'hz2, z.2.2)])
      (bs[z.2.1.val]'hz2).val (budget + z.2.2.val.length)
      (fun z' hz' => hin z' (by simp [hz']))
      (List.pairwise_cons.mp hp).2
      (fun z' hz' h => mono_get bs hbs (hzs z' hz') h)
      (by omega)
    refine ⟨(bs[z.1.val]'(by omega), bs[z.2.1.val]'hz2, z.2.2) :: cs, ?_, ?_⟩
    · rw [List.foldl_cons, hstep, hf]; simp
    · simp only [Checked.Good]
      refine ⟨hc z (by simp) (by omega), mono_get bs hbs hz1 hz2,
        hn _ (List.getElem_mem hz2), by omega, hg⟩

theorem sum_le_of_subperm {l₁ l₂ : List Rg} (h : l₁.Subperm l₂) (f : Rg → Nat) :
    (l₁.map f).sum ≤ (l₂.map f).sum := by
  obtain ⟨l, perm, sub⟩ := h
  rw [← (perm.map f).sum_eq]
  exact (sub.map f).sum_le_sum (fun _ _ => Nat.zero_le _)

/-- A successful merge of sorted ranges, all inside the base, renders: the
chunks are in order because separated ranges map to ordered byte intervals
(`Coordinates.line_boundaries_ordered`), so only output capacity can fail. -/
theorem merged_renders (original : alloc.vec.Vec U8) (rs : alloc.vec.Vec Rg)
    (s : Sorted (L rs)) (room : original.val.length < Usize.max)
    (inside : ∀ x ∈ L rs, x.2.1 < (Coordinates.model original.val).length)
    (cap : original.val.length + (rs.val.map fun r => r.2.2.val.length).sum ≤ Usize.max) :
    lines.merge rs ⦃ o => ∀ k, o = some k → lines.render original rs k ⦃ out => out.isSome ⦄ ⦄ := by
  apply WP.spec_mono (merge_spec rs)
  intro o ho k hk
  subst hk
  obtain ⟨ok, hkm⟩ := ho
  have gm := (merge_good (L rs) s).1 ok
  have bound : ∀ j ∈ k.val, j.val < rs.val.length := fun j hj => by
    have := gm.2 j.val (by rw [← hkm]; exact List.mem_map_of_mem hj)
    simpa [L] using this
  obtain ⟨v, -, mem, ch⟩ := merge_ok (L rs) s ok
  have nd := kept_nodup (L rs) s ok
  have kab := kept_ab rs k.val bound
  rw [hkm] at kab
  apply WP.spec_mono (render_spec original rs k bound room)
  intro out ⟨bs, hbs, ord, le, hm⟩
  have hlen : bs.length = (Coordinates.model original.val).length := by
    rw [← hbs, List.length_map]
  have inK : ∀ z ∈ (k.val.map fun j => rs.val[j.val]!), ab z ∈ L rs := fun z hz =>
    (mem (ab z)).1 (by rw [← kab]; exact List.mem_map_of_mem hz)
  have hin : ∀ z ∈ (k.val.map fun j => rs.val[j.val]!), z.1.val ≤ z.2.1.val ∧ z.2.1.val < bs.length :=
    fun z hz => ⟨v _ (inK z hz), by rw [hlen]; exact inside _ (inK z hz)⟩
  have hp : (k.val.map fun j => rs.val[j.val]!).Pairwise (fun x y => x.2.1.val ≤ y.1.val) := by
    rw [← kab] at ch
    exact (List.pairwise_map.mp ch).imp fun {x y} sxy => by
      unfold LinesModel.Sep at sxy; simp only [ab] at sxy; split_ifs at sxy <;> omega
  have hsub : (k.val.map fun j => rs.val[j.val]!).Subperm rs.val := by
    apply List.Nodup.subperm
    · rw [← kab] at nd; exact List.Nodup.of_map ab nd
    · intro z hz
      obtain ⟨j, hj, rfl⟩ := List.mem_map.mp hz
      rw [getElem!_pos rs.val j.val (bound j hj)]
      exact List.getElem_mem _
  obtain ⟨cs, hf, hg⟩ := fold_good bs original.val.length ord le _ [] 0 original.val.length
    hin hp (fun _ _ _ => Nat.zero_le _)
    (le_trans (Nat.add_le_add_left (sum_le_of_subperm hsub _) _) cap)
  simp only [List.nil_append] at hf
  simp only [chunksOf, hf] at hm
  cases out with
  | none => exact absurd hg hm
  | some _ => rfl

end o5s_kernel.Lines
