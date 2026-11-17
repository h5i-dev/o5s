import O5sKernel
import H5iAppLib
open Aeneas Aeneas.Std Result o5s_kernel H5iAppLib

namespace o5s_kernel.PathName

/-! # File paths as names (`crates/kernel/src/pathname.rs`)

The encoding is a bijection onto what `decode` accepts, and its output is always a valid name:
* `encode_valid`: whatever `encode` returns passes `names::validate`;
* `decode_encode`: `decode (encode p) = p`;
* `decode_canonical`: whatever `decode` accepts is the encoding of its result;
* `encode_injective`: distinct paths never share a name.
-/

/-- The bytes kept literally: `[A-Za-z0-9._-]`. -/
def plainP (b : Nat) : Prop :=
  (97 ≤ b ∧ b ≤ 122) ∨ (65 ≤ b ∧ b ≤ 90) ∨ b = 95 ∨ (48 ≤ b ∧ b ≤ 57) ∨ b = 46 ∨ b = 45

instance (b : Nat) : Decidable (plainP b) := by unfold plainP; infer_instance

/-- The uppercase hex digit of `v < 16`. -/
def hexd (v : Nat) : Nat := if v < 10 then 48 + v else 55 + v

/-- The value of an uppercase hex digit, 16 otherwise. -/
def hexv (c : Nat) : Nat := if 48 ≤ c ∧ c ≤ 57 then c - 48 else if 65 ≤ c ∧ c ≤ 70 then c - 55 else 16

theorem hexv_hexd (v : Nat) (h : v < 16) : hexv (hexd v) = v := by
  unfold hexv hexd; split_ifs <;> omega

@[step] theorem plain_spec (b : U8) : pathname.plain b ⦃ r => (r = true ↔ plainP b.val) ⦄ := by
  unfold pathname.plain plainP
  step*
  all_goals (simp_all; try scalar_tac)

@[step] theorem literal_spec (c : U8) :
    pathname.literal c ⦃ r => (r = true ↔ (c.val = 47 ∨ plainP c.val)) ⦄ := by
  unfold pathname.literal
  step*

@[step] theorem hex_digit_spec (v : U8) (h : v.val < 16) :
    pathname.hex_digit v ⦃ r => r.val = hexd v.val ⦄ := by
  unfold pathname.hex_digit hexd
  step*

@[step] theorem hex_value_spec (c : U8) : pathname.hex_value c ⦃ r => r.val = hexv c.val ⦄ := by
  unfold pathname.hex_value hexv
  step*

@[step] theorem starts_segment_spec (p : Slice U8) (i : Usize) (h : i.val < p.val.length) :
    pathname.starts_segment p i ⦃ r => (i.val = 0 → r = true) ⦄ := by
  unfold pathname.starts_segment
  step*

@[step] theorem ends_segment_spec (p : Slice U8) (i : Usize) (h : i.val < p.val.length) :
    pathname.ends_segment p i ⦃ _ => True ⦄ := by
  unfold pathname.ends_segment
  step*

@[step] theorem dot_segment_spec (p : Slice U8) (i : Usize) (h : i.val < p.val.length) :
    pathname.dot_segment p i ⦃ _ => True ⦄ := by
  unfold pathname.dot_segment
  step*

@[step] theorem empty_segment_at_spec (p : Slice U8) (i : Usize) (h : i.val < p.val.length) :
    pathname.empty_segment_at p i ⦃ _ => True ⦄ := by
  unfold pathname.empty_segment_at
  step*

/-- A byte kept literally is a plain byte (whatever the dot-segment rule decided). -/
@[step] theorem keep_spec (p : Slice U8) (i : Usize) (h : i.val < p.val.length) :
    pathname.keep p i ⦃ r => (r = true → plainP (p.val[i.val]'h).val) ⦄ := by
  unfold pathname.keep
  step*

/-! ## The decoder as a model -/

/-- One step of the decoder (`decode_raw`'s loop body) on (state, high digit, decoded bytes). -/
def dstep : Option (Nat × Nat × List Nat) → Nat → Option (Nat × Nat × List Nat)
  | none, _ => none
  | some (st, hi, out), c =>
    if st = 0 then
      if c = 43 then some (1, hi, out)
      else if c = 47 ∨ plainP c then some (0, hi, out ++ [c]) else none
    else if 16 ≤ hexv c then none
    else if st = 1 then some (2, hexv c, out)
    else some (0, hi, out ++ [hi * 16 + hexv c])

/-- The decoder over a whole input, from a state. -/
def dfold (s : Option (Nat × Nat × List Nat)) (l : List Nat) : Option (Nat × Nat × List Nat) :=
  l.foldl dstep s

theorem dfold_append (s : Option (Nat × Nat × List Nat)) (a b : List Nat) :
    dfold s (a ++ b) = dfold (dfold s a) b := by
  simp [dfold, List.foldl_append]

theorem not_plain_plus : ¬ plainP 43 := by unfold plainP; omega

/-- A literal byte (a `/` or a plain byte) decodes to itself. -/
theorem dfold_lit (h0 : Nat) (acc : List Nat) (b : Nat) (hb : b = 47 ∨ plainP b) :
    dfold (some (0, h0, acc)) [b] = some (0, h0, acc ++ [b]) := by
  have h43 : b ≠ 43 := by
    rintro rfl; rcases hb with h | h
    · omega
    · exact not_plain_plus h
  simp [dfold, dstep, h43, hb]

theorem dstep_plus (h0 : Nat) (acc : List Nat) : dstep (some (0, h0, acc)) 43 = some (1, h0, acc) := by
  simp [dstep]

theorem dstep_hi (h0 : Nat) (acc : List Nat) (c : Nat) (hc : hexv c < 16) :
    dstep (some (1, h0, acc)) c = some (2, hexv c, acc) := by
  have : ¬ 16 ≤ hexv c := by omega
  simp [dstep, this]

theorem dstep_lo (h0 : Nat) (acc : List Nat) (c : Nat) (hc : hexv c < 16) :
    dstep (some (2, h0, acc)) c = some (0, h0, acc ++ [h0 * 16 + hexv c]) := by
  have : ¬ 16 ≤ hexv c := by omega
  simp [dstep, this]

/-- An escape `+HH` of a byte decodes to that byte. -/
theorem dfold_esc (h0 : Nat) (acc : List Nat) (b : Nat) (hb : b < 256) :
    dfold (some (0, h0, acc)) [43, hexd (b / 16), hexd (b % 16)] = some (0, b / 16, acc ++ [b]) := by
  have h1 : hexv (hexd (b / 16)) = b / 16 := hexv_hexd _ (by omega)
  have h2 : hexv (hexd (b % 16)) = b % 16 := hexv_hexd _ (by omega)
  have h3 : b / 16 * 16 + b % 16 = b := by omega
  simp only [dfold, List.foldl_cons, List.foldl_nil]
  rw [dstep_plus, dstep_hi _ _ _ (by rw [h1]; omega), h1, dstep_lo _ _ _ (by rw [h2]; omega), h2, h3]


/-! ## `names::validate` never panics -/

@[step] theorem segment_byte_total (c : U8) : names.segment_byte c ⦃ _ => True ⦄ := by
  unfold names.segment_byte
  step*

theorem validate_loop_spec (s : Slice U8) (hs : s.val.length ≤ 1024) (start i : Usize)
    (h1 : start.val ≤ i.val) (h2 : i.val ≤ s.val.length + 1) :
    names.validate_loop s start i ⦃ _ => True ⦄ := by
  unfold names.validate_loop
  apply loop_idx_spec _ (fun x => x.2) (s.val.length + 1)
    (fun x => x.1.val ≤ x.2.val ∧ x.2.val ≤ s.val.length + 1) (fun _ => True)
  · rintro ⟨st, i⟩ ⟨ha, hb⟩ _
    unfold names.validate_loop.body
    step*
  · exact ⟨h1, h2⟩
  · exact h2

theorem validate_total (s : Slice U8) : names.validate s ⦃ _ => True ⦄ := by
  unfold names.validate
  step*
  apply validate_loop_spec
  · simp only [names.MAX_NAME] at *; scalar_tac
  · scalar_tac
  · scalar_tac


/-- `validate`'s result, as an equation (it never panics). -/
@[step] theorem validate_ok (s : Slice U8) : names.validate s ⦃ r => names.validate s = ok r ⦄ := by
  obtain ⟨r, h, -⟩ := WP.spec_imp_exists (validate_total s)
  rw [h]; simp [WP.spec_ok]

/-- The escape `+HH` of `b`, appended. -/
@[step] theorem push_escape_spec (out : alloc.vec.Vec U8) (b : U8) (h : out.val.length + 3 ≤ Usize.max) :
    pathname.push_escape out b ⦃ r => r.val.length = out.val.length + 3 ∧
      r.val.map (·.val) = out.val.map (·.val) ++ [43, hexd (b.val / 16), hexd (b.val % 16)] ⦄ := by
  unfold pathname.push_escape
  step*
  all_goals (simp_all [List.length_append]; try omega)

/-! ## `encode`'s loop: the output so far decodes to the input so far -/

/-- The decoded value of a byte list (as `Nat`s). -/
abbrev vals (l : List U8) : List Nat := l.map (·.val)

def EncInv (p : Slice U8) (x : alloc.vec.Vec U8 × Usize) : Prop :=
  x.2.val ≤ p.val.length ∧ x.1.val.length ≤ 3 * x.2.val ∧
  ∃ h, dfold (some (0, 0, [])) (vals x.1.val) = some (0, h, vals (p.val.take x.2.val))

def EncPost (p : Slice U8) (r : core.result.Result (alloc.vec.Vec U8) pathname.PathError) : Prop :=
  match r with
  | .Ok v => (∃ h, dfold (some (0, 0, [])) (vals v.val) = some (0, h, vals p.val)) ∧
      names.validate (alloc.vec.Vec.deref v) = ok (.Ok ())
  | .Err _ => True

theorem take_succ_vals (p : List U8) (i : Nat) (h : i < p.length) :
    vals (p.take (i + 1)) = vals (p.take i) ++ [(p[i]'h).val] := by
  unfold vals
  rw [List.take_succ_eq_append_getElem h, List.map_append]
  rfl

theorem encode_loop_spec (p : Slice U8) (hp : p.val.length ≤ 1024) (out : alloc.vec.Vec U8) (i : Usize)
    (hI : EncInv p (out, i)) : pathname.encode_loop p out i ⦃ EncPost p ⦄ := by
  unfold pathname.encode_loop
  apply loop_idx_spec _ (fun x => x.2) p.val.length (EncInv p) (EncPost p)
  · rintro ⟨out, i⟩ ⟨hle, hlen, h0, hdec⟩ _
    simp only at hle hlen hdec
    unfold pathname.encode_loop.body
    step*
    · simp [EncPost]
    · -- a separator: `/` is kept
      refine ⟨⟨by scalar_tac, ?_, h0, ?_⟩, by scalar_tac, by scalar_tac⟩
      · simp only [out1_post, List.length_append, List.length_singleton]; scalar_tac
      · have hi : i.val < p.val.length := by scalar_tac
        have h47 : b = 47#u8 := by assumption
        rw [out1_post, i2_post, take_succ_vals _ _ hi, ← b_post, h47]
        simp only [vals, List.map_append, List.map_cons, List.map_nil] at hdec ⊢
        rw [dfold_append, hdec, dfold_lit _ _ _ (Or.inl (by simp))]
    · have hi : i.val < p.val.length := by scalar_tac
      have hb : b.val < 256 := by scalar_tac
      split
      · -- kept literally: a plain byte
        step*
        refine ⟨⟨by scalar_tac, ?_, h0, ?_⟩, by scalar_tac, by scalar_tac⟩
        · simp only [out1_post, List.length_append, List.length_singleton]; scalar_tac
        · rw [out1_post, i2_post, take_succ_vals _ _ hi, ← b_post]
          simp only [vals, List.map_append, List.map_cons, List.map_nil] at hdec ⊢
          have hp' := b1_post (by assumption)
          rw [← b_post] at hp'
          rw [dfold_append, hdec, dfold_lit _ _ _ (Or.inr hp')]
      · -- escaped: `+HH`
        step*
        refine ⟨⟨by scalar_tac, by scalar_tac, b.val / 16, ?_⟩, by scalar_tac, by scalar_tac⟩
        rw [i2_post, take_succ_vals _ _ hi, ← b_post]
        simp only [vals] at hdec ⊢
        rw [out1_post1, dfold_append, hdec, dfold_esc _ _ _ hb]
    · simp [EncPost]
    · refine ⟨⟨h0, ?_⟩, ?_⟩
      · have : i.val = p.val.length := by scalar_tac
        rw [this, List.take_length] at hdec; exact hdec
      · simp_all
    · simp [EncPost]
  · exact hI
  · exact hI.1

/-- Everything `encode` returns decodes to the path and is a valid name. -/
@[step] theorem encode_spec (p : Slice U8) :
    pathname.encode p ⦃ r => pathname.encode p = ok r ∧ EncPost p r ⦄ := by
  have hs : pathname.encode p ⦃ EncPost p ⦄ := by
    unfold pathname.encode
    step*
    · simp [EncPost]
    · simp [EncPost]
    · apply encode_loop_spec
      · simp only [names.MAX_NAME] at *; scalar_tac
      · refine ⟨by simp, by simp, 0, ?_⟩
        simp [dfold, vals]
  obtain ⟨r, h, hr⟩ := WP.spec_imp_exists hs
  rw [h]; simp [WP.spec_ok, hr]

/-! ## `decode_raw`'s loop computes the model -/

theorem dfold_none (l : List Nat) : dfold none l = none := by
  induction l with
  | nil => rfl
  | cons c l ih => simpa [dfold, dstep] using ih

def DecInv (n : Slice U8) (x : alloc.vec.Vec U8 × U8 × U8 × Usize) : Prop :=
  x.2.2.2.val ≤ n.val.length ∧ x.1.val.length ≤ x.2.2.2.val ∧ x.2.2.1.val < 16 ∧ x.2.1.val ≤ 2 ∧
  dfold (some (0, 0, [])) (vals (n.val.take x.2.2.2.val)) = some (x.2.1.val, x.2.2.1.val, vals x.1.val)

def DecPost (n : Slice U8) (r : core.result.Result (alloc.vec.Vec U8) pathname.PathError) : Prop :=
  match r with
  | .Ok v => ∃ h, dfold (some (0, 0, [])) (vals n.val) = some (0, h, vals v.val)
  | .Err _ => ∀ h o, dfold (some (0, 0, [])) (vals n.val) ≠ some (0, h, o)

theorem dfold_take_none (n : List U8) (i : Nat) (h : dfold (some (0, 0, [])) (vals (n.take i)) = none) :
    ∀ h o, dfold (some (0, 0, [])) (vals n) ≠ some (0, h, o) := by
  intro h' o
  have : vals n = vals (n.take i) ++ vals (n.drop i) := by simp [vals, ← List.map_append]
  rw [this, dfold_append, h, dfold_none]; simp

theorem dec_next (n : Slice U8) (i : Usize) (hi : i.val < n.val.length) (st : Option (Nat × Nat × List Nat))
    (hdec : dfold (some (0, 0, [])) (vals (n.val.take i.val)) = st) :
    dfold (some (0, 0, [])) (vals (n.val.take (i.val + 1))) = dstep st (n.val[i.val]'hi).val := by
  rw [take_succ_vals _ _ hi, dfold_append, hdec]; rfl

theorem decode_raw_loop_spec (n : Slice U8) (out : alloc.vec.Vec U8) (st hi : U8) (i : Usize)
    (hI : DecInv n (out, st, hi, i)) : pathname.decode_raw_loop n out st hi i ⦃ DecPost n ⦄ := by
  unfold pathname.decode_raw_loop
  apply loop_idx_spec _ (fun x => x.2.2.2) n.val.length (DecInv n) (DecPost n)
  · rintro ⟨out, st, hi, i⟩ ⟨hle, hlen, hhi, hst, hdec⟩ _
    simp only at hle hlen hhi hst hdec
    unfold pathname.decode_raw_loop.body
    step*
    · -- `+`: an escape starts
      have hi' : i.val < n.val.length := by scalar_tac
      refine ⟨⟨by scalar_tac, by scalar_tac, hhi, by simp, ?_⟩, by scalar_tac, by scalar_tac⟩
      rw [i2_post, dec_next n i hi' _ hdec, ← c_post]
      have h0 : st.val = 0 := by simp [‹st = 0#u8›]
      have h43 : c.val = 43 := by simp [‹c = 43#u8›]
      simp [dstep, h0, h43]
    · -- a literal byte
      have hi' : i.val < n.val.length := by scalar_tac
      refine ⟨⟨by scalar_tac, by simp [out1_post]; scalar_tac, hhi, by simp, ?_⟩, by scalar_tac, by scalar_tac⟩
      rw [i2_post, dec_next n i hi' _ hdec, ← c_post, out1_post]
      have h0 : st.val = 0 := by simp [‹st = 0#u8›]
      have h43 : c.val ≠ 43 := by intro h; apply ‹¬ c = 43#u8›; scalar_tac
      have hl : c.val = 47 ∨ plainP c.val := b_post.1 ‹b = true›
      simp [dstep, h0, h43, hl, vals]
    · -- a byte that is neither literal nor `+`
      have hi' : i.val < n.val.length := by scalar_tac
      apply dfold_take_none n.val (i.val + 1)
      rw [dec_next n i hi' _ hdec, ← c_post]
      have h0 : st.val = 0 := by simp [‹st = 0#u8›]
      have h43 : c.val ≠ 43 := by intro h; apply ‹¬ c = 43#u8›; scalar_tac
      have hl : ¬ (c.val = 47 ∨ plainP c.val) := fun h => ‹¬ b = true› (b_post.2 h)
      simp [dstep, h0, h43, hl]
    · -- not a hex digit inside an escape
      have hi' : i.val < n.val.length := by scalar_tac
      apply dfold_take_none n.val (i.val + 1)
      rw [dec_next n i hi' _ hdec, ← c_post]
      have h0 : st.val ≠ 0 := by intro h; apply ‹¬ st = 0#u8›; scalar_tac
      have hv : 16 ≤ hexv c.val := by rw [← v_post]; scalar_tac
      simp [dstep, h0, hv]
    · have hi' : i.val < n.val.length := by scalar_tac
      have h0 : st.val ≠ 0 := by intro h; apply ‹¬ st = 0#u8›; scalar_tac
      have hv : hexv c.val < 16 := by rw [← v_post]; scalar_tac
      split
      · -- the first hex digit
        step*
        refine ⟨⟨by scalar_tac, by scalar_tac, by scalar_tac, by simp, ?_⟩, by scalar_tac, by scalar_tac⟩
        rw [i2_post, dec_next n i hi' _ hdec, ← c_post]
        have h1 : st.val = 1 := by simp [‹st = 1#u8›]
        have hv' : ¬ 16 ≤ hexv c.val := by omega
        simp [dstep, h1, hv', v_post]
      · -- the second: the byte is complete
        step*
        refine ⟨⟨by scalar_tac, by simp [x_post]; scalar_tac, hhi, by simp, ?_⟩, by scalar_tac, by scalar_tac⟩
        rw [i2_post, dec_next n i hi' _ hdec, ← c_post, x_post]
        have h1 : st.val ≠ 1 := by intro h; apply ‹¬ st = 1#u8›; scalar_tac
        have hv' : ¬ 16 ≤ hexv c.val := by omega
        simp only [dstep, h0, h1, hv', vals, if_false, List.map_append, List.map_cons, List.map_nil]
        simp_all
    · -- the input ends inside an escape
      intro h o
      have : i.val = n.val.length := by scalar_tac
      rw [this, List.take_length] at hdec
      rw [hdec]
      have hne := ‹(st != 0#u8) = true›
      have h0 : st.val ≠ 0 := by simp at hne; intro h; apply hne; scalar_tac
      simp [h0]
    · have : i.val = n.val.length := by scalar_tac
      rw [this, List.take_length] at hdec
      refine ⟨hi.val, ?_⟩
      have h0 : st.val = 0 := by simp_all
      rw [hdec, h0]
  · exact hI
  · exact hI.1

@[step] theorem decode_raw_spec (n : Slice U8) : pathname.decode_raw n ⦃ DecPost n ⦄ := by
  unfold pathname.decode_raw
  apply decode_raw_loop_spec
  refine ⟨by simp, by simp, by simp, by simp, ?_⟩
  simp [dfold, vals]

/-! ## Byte-string equality -/

@[step] theorem bytes_eq_spec (a b : Slice U8) :
    pathname.bytes_eq a b ⦃ r => (r = true ↔ a.val = b.val) ⦄ := by
  unfold pathname.bytes_eq
  step*
  have hl : a.val.length = b.val.length := by
    have := ‹¬ (a.len != b.len) = true›; simp at this; scalar_tac
  unfold pathname.bytes_eq_loop
  apply loop_idx_spec _ (fun i => i) a.val.length
    (fun i => ∀ j (hj : j < i.val) (h1 : j < a.val.length) (h2 : j < b.val.length), a.val[j] = b.val[j])
  · intro i hall _
    unfold pathname.bytes_eq_loop.body
    step*
    refine iff_of_true trivial ?_
    apply List.ext_getElem hl
    intro j h1 h2; exact hall j (by scalar_tac) h1 h2
  · intro j hj; simp at hj
  · simp

/-! ## The theorems -/

theorem vals_inj {a b : List U8} (h : vals a = vals b) : a = b := by
  apply List.map_injective_iff.2 _ h
  intro x y hxy; scalar_tac

/-- **The encoding of a path is always a valid name** (`names::validate` accepts it). -/
theorem encode_valid (p : Slice U8) (n : alloc.vec.Vec U8) (h : pathname.encode p = ok (.Ok n)) :
    names.validate (alloc.vec.Vec.deref n) = ok (.Ok ()) := by
  have := encode_spec p; rw [h] at this
  exact ((WP.spec_ok _).1 this).2.2

theorem decoded_eq (n : alloc.vec.Vec U8) (src : Slice U8) (q : alloc.vec.Vec U8) (h0 : Nat)
    (hd : dfold (some (0, 0, [])) (vals n.val) = some (0, h0, vals src.val))
    (hr : DecPost (alloc.vec.Vec.deref n) (.Ok q)) : q.val = src.val := by
  obtain ⟨h1, hq⟩ := hr
  have hq2 : dfold (some (0, 0, [])) (vals n.val) = some (0, h1, vals q.val) := by
    simpa [alloc.vec.Vec.deref] using hq
  rw [hd] at hq2; simp at hq2
  exact vals_inj hq2.2.symm

theorem decoded_not_err (n : alloc.vec.Vec U8) (src : Slice U8) (h0 : Nat) (e : pathname.PathError)
    (hd : dfold (some (0, 0, [])) (vals n.val) = some (0, h0, vals src.val))
    (hr : DecPost (alloc.vec.Vec.deref n) (.Err e)) : False :=
  hr h0 (vals src.val) (by simpa [alloc.vec.Vec.deref] using hd)

/-- **Decoding inverts encoding:** `decode (encode p) = p`. -/
theorem decode_encode (src : Slice U8) (n : alloc.vec.Vec U8) (h : pathname.encode src = ok (.Ok n)) :
    pathname.decode (alloc.vec.Vec.deref n) ⦃ r => ∃ q, r = .Ok q ∧ q.val = src.val ⦄ := by
  have hs := encode_spec src; rw [h] at hs
  obtain ⟨-, ⟨h0, hd⟩, -⟩ := (WP.spec_ok _).1 hs
  -- the name is not empty (the empty path does not encode)
  have hne : n.val ≠ [] := by
    intro hn
    rw [hn] at hd
    simp [dfold, vals] at hd
    have hl : src.len = 0#usize := by
      have : src.len.val = 0 := by simp [Slice.len, hd.2]
      scalar_tac
    unfold pathname.encode at h; simp [hl] at h
  unfold pathname.decode
  step*
  · exfalso; apply hne
    rename_i hz
    have := congrArg (fun x : Usize => x.val) hz
    simpa [alloc.vec.Vec.deref, Slice.len] using this
  · refine ⟨_, rfl, ?_⟩
    rw [‹r = _›] at r_post; exact decoded_eq n src _ h0 hd r_post
  · -- not canonical: impossible, re-encoding gives `n` back
    exfalso
    rw [‹r = _›] at r_post
    have hqp := decoded_eq n src _ h0 hd r_post
    have hsl : alloc.vec.Vec.deref p = src := Slice.ext _ _ (by simpa [alloc.vec.Vec.deref] using hqp)
    rw [hsl, h] at r1_post
    have hr1 : r1 = .Ok n := (Result.ok_injective r1_post).symm
    simp_all
  · -- re-encoding fails: impossible
    exfalso
    rw [‹r = _›] at r_post
    have hqp := decoded_eq n src _ h0 hd r_post
    have hsl : alloc.vec.Vec.deref p = src := Slice.ext _ _ (by simpa [alloc.vec.Vec.deref] using hqp)
    rw [hsl, h] at r1_post
    have hr1 : r1 = .Ok n := (Result.ok_injective r1_post).symm
    simp_all
  · exfalso; rw [‹r = _›] at r_post; exact decoded_not_err n src h0 _ hd r_post

/-- **Only canonical names decode:** whatever `decode` accepts is exactly the encoding of its result
(no lowercase hex, no needless escape, no literal `.`/`..` segment). -/
theorem decode_canonical (n : Slice U8) :
    pathname.decode n ⦃ r => ∀ q, r = .Ok q →
      ∃ e, pathname.encode (alloc.vec.Vec.deref q) = ok (.Ok e) ∧ e.val = n.val ⦄ := by
  unfold pathname.decode
  step*
  intro q hq
  injection hq with hq; subst hq
  refine ⟨e, by rw [r1_post, ‹r1 = _›], ?_⟩
  have := b_post.1 ‹b = true›
  simpa [alloc.vec.Vec.deref] using this

/-- **Encoding is injective:** two paths with the same name are the same path. -/
theorem encode_injective (p1 p2 : Slice U8) (n1 n2 : alloc.vec.Vec U8)
    (h1 : pathname.encode p1 = ok (.Ok n1)) (h2 : pathname.encode p2 = ok (.Ok n2))
    (hn : n1.val = n2.val) : p1.val = p2.val := by
  have hd : alloc.vec.Vec.deref n1 = alloc.vec.Vec.deref n2 :=
    Slice.ext _ _ (by simpa [alloc.vec.Vec.deref] using hn)
  obtain ⟨r1, hr1, q1, hq1, hv1⟩ := WP.spec_imp_exists (decode_encode p1 n1 h1)
  obtain ⟨r2, hr2, q2, hq2, hv2⟩ := WP.spec_imp_exists (decode_encode p2 n2 h2)
  rw [hd, hr2] at hr1
  have : r2 = r1 := Result.ok_injective hr1
  subst this
  rw [hq1] at hq2
  cases hq2
  rw [← hv1, hv2]

end o5s_kernel.PathName
