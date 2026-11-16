import Pico2petaNetKernel
import H5iAppLib
import ParseSpec
/-!
# Git LFS pointers: the extracted `parse_lfs_pointer` computes `ptrS`

`crates/net-kernel/src/parse.rs`. `ptrS` reads a pointer file the way the Git LFS spec does: at
most 512 bytes; the first line is `version https://git-lfs.github.com/spec/v1`; then every line
(`\n`-terminated, a trailing `\r` dropped, the last one possibly unterminated) is looked at, and
the last valid `oid sha256:<64 hex>` and the last valid `size <strict decimal u64>` win (other
lines, extensions included, say nothing). Both must be present.
-/
open Aeneas Aeneas.Std Result pico2peta_net_kernel H5iAppLib

namespace pico2peta_net_kernel.Spec.Lfs

open pico2peta_net_kernel.Spec (B g nib hexDec decSpec allHex)

/-! ## The spec -/

/-- `b[s..e]`. -/
def seg (b : List Nat) (s e : Nat) : List Nat := (b.drop s).take (e - s)

/-- `b[s..e]` begins with `p` (and `e ≤ |b|`, as at every call). -/
def startsAt (b : List Nat) (s e : Nat) (p : List Nat) : Bool :=
  decide (s ≤ e ∧ p.length ≤ e - s ∧ ∀ k, k < p.length → g b (s + k) = g p k)

/-- Every byte of `b[s..e]` is a hex digit (either case). -/
def hexIn (b : List Nat) (s e : Nat) : Bool :=
  decide (∀ k, s ≤ k → k < e → nib false (g b k) < 16)

/-- The end of the line `[s, i)` without a trailing `\r`. -/
def trimCr (b : List Nat) (s i : Nat) : Nat :=
  if s < i ∧ g b (i - 1) = 13 then i - 1 else i

def oidP : List Nat := [111, 105, 100, 32, 115, 104, 97, 50, 53, 54, 58]
def sizeP : List Nat := [115, 105, 122, 101, 32]

/-- The scan: the current line's start, and the last valid oid (its hex range) and size. -/
structure St where
  start : Nat
  oid : Option (Nat × Nat)
  size : Option Nat
deriving DecidableEq

/-- One line `b[s..e]`. -/
def lineS (st : St) (b : List Nat) (s e : Nat) : St :=
  if startsAt b s e oidP then
    if e - (s + 11) = 64 ∧ hexIn b (s + 11) e then { st with oid := some (s + 11, e) } else st
  else if startsAt b s e sizeP then
    match decSpec (seg b (s + 5) e) with
    | some n => { st with size := some n }
    | none => st
  else st

/-- Position `i`: at a newline or the end, the line `[start, i)` is read. -/
def scanS (st : St) (b : List Nat) (i : Nat) : St :=
  if i = b.length ∨ g b i = 10 then
    let r := if st.start < i ∨ i < b.length then lineS st b st.start (trimCr b st.start i) else st
    { r with start := i + 1 }
  else st

/-- Positions `i ..= |b|`. -/
def loopS (b : List Nat) (st : St) (i : Nat) : St :=
  if i ≤ b.length then loopS b (scanS st b i) (i + 1) else st
termination_by b.length + 1 - i

/-- `version https://git-lfs.github.com/spec/v1` -/
def versionLine : List Nat :=
  [118, 101, 114, 115, 105, 111, 110, 32, 104, 116, 116, 112, 115, 58, 47, 47, 103, 105, 116, 45,
   108, 102, 115, 46, 103, 105, 116, 104, 117, 98, 46, 99, 111, 109, 47, 115, 112, 101, 99, 47,
   118, 49]

/-- **The pointer spec**: `(sha256, size)` of a valid pointer file. -/
def ptrS (b : List Nat) : Option (List Nat × Nat) :=
  if 512 < b.length then none
  else if ¬ (42 < b.length ∧ startsAt b 0 b.length versionLine ∧ g b 42 = 10) then none
  else
    let st := loopS b ⟨43, none, none⟩ 43
    match st.oid, st.size with
    | some (s, e), some n =>
      (if (seg b s e).length = 64 then hexDec false (seg b s e) else none).map (·, n)
    | _, _ => none

/-! ## Proofs about the extracted code -/

theorem g_lt (l : List U8) (k : Nat) (h : k < l.length) : g (B l) k = (l[k]'h).val := by
  simp [g, List.getD_eq_getElem?_getD, List.getElem?_map, List.getElem?_eq_getElem h]

theorem seg_succ (b : List Nat) (s j : Nat) (h1 : s ≤ j) (h2 : j < b.length) :
    seg b s (j + 1) = seg b s j ++ [b[j]] := by
  unfold seg
  rw [show j + 1 - s = (j - s) + 1 by omega, List.take_succ, List.getElem?_drop,
    show s + (j - s) = j by omega, List.getElem?_eq_getElem h2]
  simp

@[step]
theorem trim_cr_spec (b : Slice U8) (s i : Usize) (hi : i.val ≤ b.length) :
    parse.trim_cr b s i ⦃ r => r.val = trimCr (B b.val) s.val i.val ∧ r.val ≤ i.val ⦄ := by
  unfold parse.trim_cr
  step*
  all_goals refine ⟨?_, by scalar_tac⟩
  · unfold trimCr
    have e : (b.val[i.val - 1]'(by scalar_tac)) = i2 := by simp only [i2_post, i1_post]
    rw [if_pos ⟨by scalar_tac, by rw [g_lt b.val (i.val - 1) (by scalar_tac), e, ‹i2 = 13#u8›]; rfl⟩]
    scalar_tac
  · unfold trimCr
    have e : (b.val[i.val - 1]'(by scalar_tac)) = i2 := by simp only [i2_post, i1_post]
    rw [if_neg]; rintro ⟨_, h⟩
    rw [g_lt b.val (i.val - 1) (by scalar_tac), e] at h
    exact ‹¬i2 = 13#u8› (UScalar.eq_of_val_eq (by rw [h]; rfl))
  · unfold trimCr; rw [if_neg (by rintro ⟨h, _⟩; scalar_tac)]

theorem starts_at_loop_spec (b : Slice U8) (s : Usize) (p : Slice U8) (i : Usize)
    (hb : s.val + p.length ≤ b.length) (hi : i.val ≤ p.length)
    (ht : ∀ k, k < i.val → g (B b.val) (s.val + k) = g (B p.val) k) :
    parse.starts_at_loop b s p i ⦃ r =>
      r = decide (∀ k, k < p.length → g (B b.val) (s.val + k) = g (B p.val) k) ⦄ := by
  unfold parse.starts_at_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => p.length - j.val)
    (inv := fun j => j.val ≤ p.length ∧ ∀ k, k < j.val → g (B b.val) (s.val + k) = g (B p.val) k)
  · intro j ⟨h1, h2⟩
    unfold parse.starts_at_loop.body
    step*
    · symm; rw [decide_eq_false_iff_not]; intro h
      have e := h j.val (by scalar_tac)
      rw [g_lt b.val (s.val + j.val) (by scalar_tac), g_lt p.val j.val (by scalar_tac)] at e
      have : i3 = i4 := by
        apply UScalar.eq_of_val_eq; simp only [i3_post, i4_post, i2_post]; exact e
      simp_all
    · have hall : ∀ k, k < i5.val → g (B b.val) (s.val + k) = g (B p.val) k := by
        intro k hk
        by_cases hkj : k < j.val
        · exact h2 k hkj
        · have hk' : k = j.val := by scalar_tac
          subst hk'
          rw [g_lt b.val (s.val + j.val) (by scalar_tac), g_lt p.val j.val (by scalar_tac)]
          have : i3 = i4 := UScalar.eq_of_val_eq (by simpa using ‹¬(i3 != i4) = true›)
          have e3 : i3.val = (b.val[s.val + j.val]'(by scalar_tac)).val := by
            simp only [i3_post, i2_post]
          rw [← e3, this, i4_post]
      and_intros <;> first | exact hall | scalar_tac
    all_goals (symm; rw [decide_eq_true_iff]; intro k hk; exact h2 k (by scalar_tac))
  · exact ⟨hi, ht⟩

@[step]
theorem starts_at_spec (b : Slice U8) (s e : Usize) (p : Slice U8) (he : e.val ≤ b.length) :
    parse.starts_at b s e p ⦃ r => r = startsAt (B b.val) s.val e.val (B p.val) ⦄ := by
  unfold parse.starts_at
  have hp : (B p.val).length = p.length := by simp
  step*
  · unfold startsAt; rw [eq_comm, decide_eq_false_iff_not]; rintro ⟨h, _⟩; scalar_tac
  · unfold startsAt; rw [eq_comm, decide_eq_false_iff_not]; rintro ⟨_, h, _⟩; rw [hp] at h; scalar_tac
  · apply WP.spec_mono (starts_at_loop_spec b s p 0#usize (by scalar_tac) (by simp)
      (by intro k hk; simp at hk))
    intro r hr
    rw [hr]; unfold startsAt; rw [hp]
    simp only [decide_eq_decide]
    exact ⟨fun h => ⟨by scalar_tac, by scalar_tac, h⟩, fun h => h.2.2⟩

theorem all_hex_loop_spec (b : Slice U8) (e i : Usize) (s : Nat) (he : e.val ≤ b.length) (hsi : s ≤ i.val)
    (ht : ∀ k, s ≤ k → k < i.val → nib false (g (B b.val) k) < 16) :
    parse.all_hex_loop b e i ⦃ r => r = hexIn (B b.val) s e.val ⦄ := by
  unfold parse.all_hex_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => e.val - j.val)
    (inv := fun j => s ≤ j.val ∧ ∀ k, s ≤ k → k < j.val → nib false (g (B b.val) k) < 16)
  · intro j ⟨h1, h2⟩
    unfold parse.all_hex_loop.body
    step*
    all_goals first
      | (symm; unfold hexIn; rw [decide_eq_false_iff_not]; intro h
         have hn := h j.val h1 (by scalar_tac)
         rw [g_lt b.val j.val (by scalar_tac), ← i1_post, ← i2_post] at hn
         scalar_tac)
      | (have hall : ∀ k, s ≤ k → k < i3.val → nib false (g (B b.val) k) < 16 := by
           intro k hk1 hk2
           by_cases hkj : k < j.val
           · exact h2 k hk1 hkj
           · have hk' : k = j.val := by scalar_tac
             subst hk'
             rw [g_lt b.val j.val (by scalar_tac), ← i1_post, ← i2_post]
             scalar_tac
         and_intros <;> first | exact hall | scalar_tac)
      | (symm; unfold hexIn; rw [decide_eq_true_iff]; intro k hk1 hk2; exact h2 k hk1 (by scalar_tac))
  · exact ⟨hsi, ht⟩

@[step]
theorem all_hex_spec (b : Slice U8) (s e : Usize) (he : e.val ≤ b.length) :
    parse.all_hex b s e ⦃ r => r = hexIn (B b.val) s.val e.val ⦄ := by
  unfold parse.all_hex
  exact all_hex_loop_spec b e s s.val he (le_refl _) (by intro k h1 h2; omega)

theorem slice_of_loop_spec (b : Slice U8) (s e : Nat) (en : Usize) (out : alloc.vec.Vec U8) (i : Usize)
    (hen : en.val = e) (he : e ≤ b.length) (hsi : s ≤ i.val) (hie : i.val ≤ max s e)
    (ho : B out.val = seg (B b.val) s i.val) :
    parse.slice_of_loop b en out i ⦃ r => B r.val = seg (B b.val) s e ⦄ := by
  unfold parse.slice_of_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => e - x.2.val)
    (inv := fun x => s ≤ x.2.val ∧ x.2.val ≤ max s e ∧ B x.1.val = seg (B b.val) s x.2.val)
  · rintro ⟨o, j⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    unfold parse.slice_of_loop.body
    have hlen : o.val.length ≤ j.val - s := by
      have := congrArg List.length h3
      simp [seg] at this; scalar_tac
    have := Slice.length_ineq b
    step*
    all_goals first
      | (scalar_tac; done)
      | (have hseg : B out1.val = seg (B b.val) s i3.val := by
           rw [out1_post, i3_post, seg_succ (B b.val) s j.val h1 (by simp; scalar_tac), ← h3]
           simp [B, i2_post]
         and_intros <;> first | exact hseg | scalar_tac)
      | (rcases (by scalar_tac : j.val = e ∨ (j.val = s ∧ e ≤ s)) with h | ⟨h, h'⟩
         · rw [h3, h]
         · rw [h3, h]; simp [seg, show e - s = 0 by omega])
      | (exfalso; scalar_tac)
  · exact ⟨hsi, hie, ho⟩

@[step]
theorem slice_of_spec (b : Slice U8) (s e : Usize) (he : e.val ≤ b.length) :
    parse.slice_of b s e ⦃ r => B r.val = seg (B b.val) s.val e.val ⦄ := by
  unfold parse.slice_of
  exact slice_of_loop_spec b s.val e.val e (alloc.vec.Vec.new U8) s rfl he (le_refl _)
    (le_max_left _ _) (by simp [seg])

/-- The Rust scan state, abstracted. -/
def abs (sc : parse.LfsScan) : St :=
  ⟨sc.start.val, if sc.has_oid then some (sc.oid_s.val, sc.oid_e.val) else none,
   if sc.has_size then some sc.size.val else none⟩

/-- A recorded oid range lies inside the body. -/
def Ok (n : Nat) (sc : parse.LfsScan) : Prop := sc.has_oid = true → sc.oid_e.val ≤ n

theorem startsAt_le (b : List Nat) (s e : Nat) (p : List Nat) (h : startsAt b s e p = true) :
    s + p.length ≤ e := by
  unfold startsAt at h; simp only [decide_eq_true_eq] at h; omega

@[step]
theorem lfs_line_spec (sc : parse.LfsScan) (body : Slice U8) (s e : Usize) (he : e.val ≤ body.length) :
    parse.lfs_line sc body s e ⦃ r => abs r = lineS (abs sc) (B body.val) s.val e.val ∧ r.start = sc.start ∧
      (Ok body.length sc → Ok body.length r) ⦄ := by
  unfold parse.lfs_line
  have hmax := Slice.length_ineq body
  step*
  all_goals (
    have hop : B oid_p.val = oidP := by rw [oid_p_post]; rfl
    have hsp : B size_p.val = sizeP := by rw [size_p_post]; rfl
    have hol : oid_p.val.length = 11 := by simpa [oidP] using congrArg List.length hop
    have hsl : size_p.val.length = 5 := by simpa [sizeP] using congrArg List.length hsp
    rw [hop] at b_post
    try rw [hsp] at b1_post)
  · have := startsAt_le _ _ _ _ (by rw [← b_post]; assumption); simp only [oidP] at this; scalar_tac
  · have := startsAt_le _ _ _ _ (by rw [← b_post]; assumption); simp only [oidP] at this; scalar_tac
  · have hb : startsAt (B body.val) s.val e.val oidP = true := by rw [← b_post]; assumption
    have hv : vs.val = s.val + 11 := by scalar_tac
    have hx : hexIn (B body.val) (s.val + 11) e.val = true := by rw [← hv, ← b1_post]; assumption
    have h64 : e.val - (s.val + 11) = 64 := by
      have := congrArg UScalar.val ‹i1 = 64#usize›; simp at this; scalar_tac
    refine ⟨?_, fun _ => fun _ => he⟩
    unfold lineS; rw [if_pos hb, if_pos ⟨h64, hx⟩]; simp [abs, hv]
  · have hb : startsAt (B body.val) s.val e.val oidP = true := by rw [← b_post]; assumption
    have hv : vs.val = s.val + 11 := by scalar_tac
    refine ⟨?_, fun h => h⟩
    unfold lineS; rw [if_pos hb, if_neg]
    rintro ⟨_, hx⟩
    rw [← hv, ← b1_post] at hx; exact ‹¬b1 = true› hx
  · have hb : startsAt (B body.val) s.val e.val oidP = true := by rw [← b_post]; assumption
    refine ⟨?_, fun h => h⟩
    unfold lineS; rw [if_pos hb, if_neg]
    rintro ⟨h64, _⟩
    exact ‹¬i1 = 64#usize› (UScalar.eq_of_val_eq (by simp; scalar_tac))
  · have := startsAt_le _ _ _ _ (by rw [← b1_post]; assumption); simp only [sizeP] at this; scalar_tac
  all_goals (
    have hb : startsAt (B body.val) s.val e.val oidP = false := by
      rw [← b_post]; simpa using ‹¬b = true›)
  · have hs : startsAt (B body.val) s.val e.val sizeP = true := by rw [← b1_post]; assumption
    have hd : decSpec (seg (B body.val) (s.val + 5) e.val) = o.map (·.val) := by
      rw [o_post, vec_deref_val, v_post, show i1.val = s.val + 5 by scalar_tac]
    refine ⟨?_, fun h => h⟩
    unfold lineS; simp only [hb, hs, hd, ‹o = none›]; rfl
  · have hs : startsAt (B body.val) s.val e.val sizeP = true := by rw [← b1_post]; assumption
    have hd : decSpec (seg (B body.val) (s.val + 5) e.val) = o.map (·.val) := by
      rw [o_post, vec_deref_val, v_post, show i1.val = s.val + 5 by scalar_tac]
    refine ⟨?_, fun h => h⟩
    unfold lineS; simp [hb, hs, hd, ‹o = some n›, abs]
  · have hs : startsAt (B body.val) s.val e.val sizeP = false := by
      rw [← b1_post]; simpa using ‹¬b1 = true›
    refine ⟨?_, fun h => h⟩
    unfold lineS; simp [hb, hs]

@[step]
theorem lfs_scan_spec (sc : parse.LfsScan) (body : Slice U8) (i : Usize) (hi : i.val ≤ body.length)
    (hb : body.length ≤ 512) :
    parse.lfs_scan sc body i ⦃ r => abs r = scanS (abs sc) (B body.val) i.val ∧
      (Ok body.length sc → Ok body.length r) ⦄ := by
  unfold parse.lfs_scan
  step*
  all_goals (try (have h10 : g (B body.val) i.val = 10 := (by rw [g_lt body.val i.val (by scalar_tac), ← i2_post, ‹i2 = 10#u8›]; rfl)))
  all_goals (try (split <;> (try split)))
  all_goals step*
  · refine ⟨?_, ?_⟩
    · unfold scanS
      rw [if_pos (by first | exact Or.inr h10 | (left; simp only [B, List.length_map]; scalar_tac))]
      dsimp only
      rw [if_pos (by simp only [abs, B, List.length_map]; scalar_tac)]
      simp only [show (abs sc).start = sc.start.val from rfl]
      rw [← x_post, ← r_post]
      simp only [abs, i2_post]; (try simp only [St.mk.injEq]); (try scalar_tac)
    · intro h; have := r_post2 h; unfold Ok at *; exact this
  · refine ⟨?_, ?_⟩
    · unfold scanS
      rw [if_pos (Or.inl (by simp only [B, List.length_map]; scalar_tac))]
      dsimp only
      rw [if_neg (by simp only [abs, B, List.length_map]; scalar_tac)]
      simp only [abs, i2_post]; (try simp only [St.mk.injEq]); (try scalar_tac)
    · intro h; unfold Ok at *; exact h
  · refine ⟨?_, ?_⟩
    · unfold scanS
      rw [if_pos (by first | exact Or.inr h10 | (left; simp only [B, List.length_map]; scalar_tac))]
      dsimp only
      rw [if_pos (by simp only [abs, B, List.length_map]; scalar_tac)]
      simp only [show (abs sc).start = sc.start.val from rfl]
      rw [← x_post, ← r_post]
      simp only [abs, i2_post]; (try simp only [St.mk.injEq]); (try scalar_tac)
    · intro h; have := r_post2 h; unfold Ok at *; exact this
  · refine ⟨?_, ?_⟩
    · unfold scanS
      rw [if_pos (by first | exact Or.inr h10 | (left; simp only [B, List.length_map]; scalar_tac))]
      dsimp only
      rw [if_pos (by simp only [abs, B, List.length_map]; scalar_tac)]
      simp only [show (abs sc).start = sc.start.val from rfl]
      rw [← x_post, ← r_post]
      simp only [abs, i2_post]; (try simp only [St.mk.injEq]); (try scalar_tac)
    · intro h; have := r_post2 h; unfold Ok at *; exact this
  · refine ⟨?_, fun h => h⟩
    unfold scanS
    rw [if_neg]
    rintro (h | h)
    · exact ‹¬i = body.len› (UScalar.eq_of_val_eq (by simp only [B, List.length_map] at h; scalar_tac))
    · rw [g_lt body.val i.val (by scalar_tac)] at h
      exact ‹¬i2 = 10#u8› (UScalar.eq_of_val_eq (by simp only [i2_post]; simpa using h))

theorem loopS_eq (b : List Nat) (st : St) (i : Nat) :
    loopS b st i = if i ≤ b.length then loopS b (scanS st b i) (i + 1) else st := by
  rw [loopS]

@[step]
theorem parse_lfs_pointer_loop_spec (body : Slice U8) (sc : parse.LfsScan) (i : Usize)
    (hb : body.length ≤ 512) (hi : i.val ≤ body.length + 1) (hok : Ok body.length sc) :
    parse.parse_lfs_pointer_loop body sc i ⦃ r =>
      abs r = loopS (B body.val) (abs sc) i.val ∧ Ok body.length r ⦄ := by
  unfold parse.parse_lfs_pointer_loop
  apply loop.spec_decr_nat (measure := fun (x : parse.LfsScan × Usize) => body.length + 1 - x.2.val)
    (inv := fun x => x.2.val ≤ body.length + 1 ∧ Ok body.length x.1 ∧
      loopS (B body.val) (abs x.1) x.2.val = loopS (B body.val) (abs sc) i.val)
  · rintro ⟨o, j⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    unfold parse.parse_lfs_pointer_loop.body
    have hlen : (B body.val).length = body.length := by simp
    step*
    · have hl : loopS (B body.val) (abs sc1) i2.val = loopS (B body.val) (abs sc) i.val := by
        rw [← h3, loopS_eq (B body.val) (abs o) j.val, if_pos (by rw [hlen]; scalar_tac), ← sc1_post,
          i2_post]
      have hk : Ok body.length sc1 := ‹Ok body.length o → Ok body.length sc1› h2
      and_intros <;> first | exact hl | exact hk | scalar_tac
    · have hl : abs o = loopS (B body.val) (abs sc) i.val := by
        rw [← h3, loopS_eq, if_neg (by rw [hlen]; scalar_tac)]
      and_intros <;> first | exact hl | exact h2
  · exact ⟨hi, hok, rfl⟩

@[step]
theorem lfs_version_ok_spec (body : Slice U8) :
    parse.lfs_version_ok body ⦃ r => r = decide (42 < body.length ∧
      startsAt (B body.val) 0 body.length versionLine = true ∧ g (B body.val) 42 = 10) ⦄ := by
  unfold parse.lfs_version_ok
  have h42 : parse.LFS_VERSION_LEN.val = 42 := by unfold parse.LFS_VERSION_LEN; rfl
  have hvl : B parse.LFS_VERSION_LINE.val = versionLine := by unfold parse.LFS_VERSION_LINE; rfl
  step*
  · rw [hvl] at b_post
    have hb : startsAt (B body.val) 0 body.len.val versionLine = true := by rw [← b_post]; assumption
    simp only [h42] at i2_post
    have hg : g (B body.val) 42 = i2.val := by rw [g_lt body.val 42 (by scalar_tac), i2_post]
    rw [decide_eq_decide, hg]
    constructor
    · intro h; refine ⟨by scalar_tac, by simpa using hb, by rw [h]; rfl⟩
    · rintro ⟨_, _, h⟩; exact UScalar.eq_of_val_eq (by rw [h]; rfl)

/-- **`parse_lfs_pointer` computes `ptrS`** on every input (so it never panics). -/
theorem parse_lfs_pointer_spec (body : Slice U8) :
    parse.parse_lfs_pointer body ⦃ r => r.map (fun p => (B p.1.val, p.2.val)) = ptrS (B body.val) ⦄ := by
  unfold parse.parse_lfs_pointer
  have h512 : parse.LFS_MAX_POINTER.val = 512 := by unfold parse.LFS_MAX_POINTER; rfl
  have h42 : parse.LFS_VERSION_LEN.val = 42 := by unfold parse.LFS_VERSION_LEN; rfl
  have hlen : (B body.val).length = body.length := by simp
  step*
  all_goals (try (have hi1 : i1.val = 43 := by scalar_tac))
  all_goals (try (have hsc : loopS (B body.val) ⟨43, none, none⟩ 43 = abs sc := (by rw [sc_post, hi1]; simp [abs, hi1])))
  all_goals (try (have hver : 42 < body.length ∧ startsAt (B body.val) 0 body.length versionLine = true ∧ g (B body.val) 42 = 10 := (by have := ‹b = true›; rw [b_post] at this; simpa using this)))
  all_goals (try (have hd : (if (seg (B body.val) sc.oid_s.val sc.oid_e.val).length = 64 then hexDec false (seg (B body.val) sc.oid_s.val sc.oid_e.val) else none) = o.map (fun v => B v.val) := (by rw [o_post, ← v_post]; simp [vec_deref_val])))
  · unfold ptrS; rw [hlen, if_pos (by scalar_tac)]; rfl
  · intro h; simp at h
  · exact ‹Ok body.length sc› ‹_›
  all_goals first
    | (unfold ptrS
       rw [hlen, if_neg (by scalar_tac), if_neg (by simp only [hver]; simp)]
       dsimp only
       rw [hsc]
       simp only [abs, ‹sc.has_oid = true›, ‹sc.has_size = true›, if_true]
       rw [hd]
       simp_all)
    | (unfold ptrS
       rw [hlen, if_neg (by scalar_tac), if_neg (by simp only [hver]; simp)]
       dsimp only
       rw [hsc]
       simp_all [abs])
    | (unfold ptrS
       rw [hlen, if_neg (by scalar_tac), if_pos]
       · rfl
       have := ‹¬b = true›; rw [b_post] at this; simpa using this)

/-- An accepted pointer is at most 512 bytes and begins with the version line and a newline. -/
theorem ptr_accepted (b : List Nat) (h : List Nat) (n : Nat) (hp : ptrS b = some (h, n)) :
    b.length ≤ 512 ∧ startsAt b 0 b.length versionLine = true ∧ g b 42 = 10 := by
  unfold ptrS at hp
  split_ifs at hp with h1 h2
  exact ⟨by omega, h2.2.1, h2.2.2⟩

end pico2peta_net_kernel.Spec.Lfs
