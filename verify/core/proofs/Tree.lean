import O5sKernel
import H5iAppLib
import Proofs
import Mathlib.Tactic.IntervalCases
import Mathlib.Data.List.Chain
open Aeneas Aeneas.Std Result o5s_kernel H5iAppLib o5s_kernel.Proofs

/-! # Tree objects (`crates/kernel/src/tree.rs`)

The spec is the encoder `encodeL` and the canonical-form predicate `Canonical`, both plain Lean. The
theorems: the extracted `decode` accepts exactly the canonical encodings and returns the entries they
encode (`decode_spec`); `encode` produces exactly them and refuses anything not canonical (`encode_spec`);
the two round-trip (`decode_encode`, `encode_decode`); and the GC tracer `children` returns exactly the
entry hashes (`children_spec`). -/

namespace o5s_kernel.TreeProofs

/-- `"O5STREE1"`. -/
def magicL : List U8 := [79#u8, 53#u8, 83#u8, 84#u8, 82#u8, 69#u8, 69#u8, 49#u8]

def lenLo (n : Nat) : U8 := U8.ofNatCore (n % 256) (by have := Nat.mod_lt n (by decide : 256 > 0); simpa using this)
def lenHi (n : Nat) : U8 := U8.ofNatCore (n / 256 % 256) (by have := Nat.mod_lt (n / 256) (by decide : 256 > 0); simpa using this)

/-- One entry: `mode | name_len u16 LE | name | hash`. -/
def encEntry (e : tree.Entry) : List U8 :=
  [e.mode, lenLo e.name.val.length, lenHi e.name.val.length] ++ e.name.val ++ e.hash.val

/-- A tree: the magic, then the entries in order. -/
def encodeL (es : List tree.Entry) : List U8 := magicL ++ es.flatMap encEntry

/-- A valid name: one path segment of 1 to 65,535 bytes, no NUL or `/`, not `.` or `..`. -/
def NameOk (n : List U8) : Prop :=
  1 ≤ n.length ∧ n.length ≤ 65535 ∧ n ≠ [46#u8] ∧ n ≠ [46#u8, 46#u8] ∧ ∀ c ∈ n, c.val ≠ 0 ∧ c.val ≠ 47

def EntryOk (e : tree.Entry) : Prop := e.mode.val ≤ 3 ∧ NameOk e.name.val

/-- Canonical entries: each valid, names strictly increasing, encoding at most `MAX_TREE` bytes. -/
def Canonical (es : List tree.Entry) : Prop :=
  (∀ e ∈ es, EntryOk e) ∧ es.IsChain (fun a b => cmpL a.name.val b.name.val = -1) ∧
    (encodeL es).length ≤ 2147483648

/-! ## Leaf functions -/

@[step] theorem magic_ok_spec (b : Slice U8) (h : 8 ≤ b.val.length) :
    tree.magic_ok b ⦃ r => (r = true ↔ b.val.take 8 = magicL) ⦄ := by
  unfold tree.magic_ok
  have e : b.val.take 8 = [b.val[0]'(by omega), b.val[1]'(by omega), b.val[2]'(by omega), b.val[3]'(by omega),
      b.val[4]'(by omega), b.val[5]'(by omega), b.val[6]'(by omega), b.val[7]'(by omega)] := by
    apply List.ext_getElem (by simp; omega)
    intro j h1 h2
    simp only [List.length_take] at h1
    have : j < 8 := by omega
    interval_cases j <;> simp
  rw [e]
  step* <;> simp only [magicL, List.cons.injEq, and_true] <;> grind

@[step] theorem no_bad_byte_spec (nm : alloc.vec.Vec U8) :
    tree.no_bad_byte nm ⦃ r => (r = true ↔ ∀ c ∈ nm.val, c.val ≠ 0 ∧ c.val ≠ 47) ⦄ := by
  unfold tree.no_bad_byte tree.no_bad_byte_loop
  apply loop_idx_spec _ (fun i => i) nm.val.length
    (fun i => ∀ j (h : j < i.val) (h2 : j < nm.val.length), nm.val[j].val ≠ 0 ∧ nm.val[j].val ≠ 47)
  · intro i hall hi
    unfold tree.no_bad_byte_loop.body
    simp only [alloc.vec.Vec.len]
    step*
    · simp only [Bool.false_eq_true, false_iff, not_forall]
      have h1 : i.val < nm.val.length := by scalar_tac
      refine ⟨nm.val[i.val], List.getElem_mem h1, ?_⟩
      intro ⟨h, _⟩; apply h; rw [← i2_post]; simp_all
    · simp only [Bool.false_eq_true, false_iff, not_forall]
      have h1 : i.val < nm.val.length := by scalar_tac
      refine ⟨nm.val[i.val], List.getElem_mem h1, ?_⟩
      intro ⟨_, h⟩; apply h; rw [← i2_post]; simp_all
    · refine ⟨fun j hj h2 => ?_, by scalar_tac, by scalar_tac⟩
      by_cases hji : j = i.val
      · subst hji; rw [← i2_post]; constructor <;> (intro h; simp_all)
      · exact hall j (by scalar_tac) h2
    · simp only [true_iff]
      intro c hc
      obtain ⟨j, hj, rfl⟩ := List.mem_iff_getElem.1 hc
      exact hall j (by scalar_tac) hj
  · intro j h; simp at h
  · simp

theorem eq_single_iff (n : List U8) (a : U8) : n = [a] ↔ n.length = 1 ∧ n[0]! = a := by
  match n with
  | [] => simp
  | [x] => simp
  | x :: y :: z => simp

theorem eq_pair_iff (n : List U8) (a b : U8) : n = [a, b] ↔ n.length = 2 ∧ n[0]! = a ∧ n[1]! = b := by
  match n with
  | [] => simp
  | [x] => simp
  | [x, y] => simp
  | x :: y :: z :: w => simp

theorem nameOk_iff (n : List U8) : NameOk n ↔
    n.length ≠ 0 ∧ n.length ≤ 65535 ∧ ¬ (n.length = 1 ∧ n[0]!.val = 46) ∧
      ¬ (n.length = 2 ∧ n[0]!.val = 46 ∧ n[1]!.val = 46) ∧ ∀ c ∈ n, c.val ≠ 0 ∧ c.val ≠ 47 := by
  have h46 : ∀ x : U8, x = 46#u8 ↔ x.val = 46 := fun x => ⟨fun h => by subst h; rfl, fun h => by scalar_tac⟩
  unfold NameOk
  simp only [ne_eq, eq_single_iff, eq_pair_iff, h46]
  constructor
  · rintro ⟨h1, h2, h3, h4, h5⟩; exact ⟨by omega, h2, h3, h4, h5⟩
  · rintro ⟨h1, h2, h3, h4, h5⟩; exact ⟨by omega, h2, h3, h4, h5⟩

@[step] theorem name_ok_spec (nm : alloc.vec.Vec U8) :
    tree.name_ok nm ⦃ r => (r = true ↔ NameOk nm.val) ⦄ := by
  unfold tree.name_ok
  simp only [alloc.vec.Vec.len, nameOk_iff]
  have hm : tree.MAX_NAME = 65535#usize := by unfold tree.MAX_NAME; rfl
  rw [hm]
  have g0 : ∀ (h : 0 < nm.val.length), nm.val[0]! = nm.val[0] := fun h => by
    simp [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem h]
  have g1 : ∀ (h : 1 < nm.val.length), nm.val[1]! = nm.val[1] := fun h => by
    simp [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem h]
  step*

@[step] theorem copy_range_spec (b : Slice U8) (start n : Usize) (h : start.val + n.val ≤ b.val.length) :
    tree.copy_range b start n ⦃ v => v.val = (b.val.drop start.val).take n.val ⦄ := by
  unfold tree.copy_range tree.copy_range_loop
  apply loop_idx_spec _ (fun x : alloc.vec.Vec U8 × Usize => x.2) n.val
    (fun x : alloc.vec.Vec U8 × Usize => x.1.val = (b.val.drop start.val).take x.2.val)
  · rintro ⟨out, i⟩ hinv hi
    simp only at hinv hi
    unfold tree.copy_range_loop.body
    have hbl := b.property
    step*
    · refine ⟨?_, by scalar_tac, by scalar_tac⟩
      simp only [out1_post, hinv]
      have hlt : start.val + i.val < b.val.length := by scalar_tac
      rw [i3_post, List.take_add_one, List.getElem?_drop, List.getElem?_eq_getElem hlt]
      simp only [Option.toList_some, List.append_cancel_left_eq, List.cons.injEq, and_true]
      rw [i2_post]; congr 1
  · simp
  · simp

@[step] theorem hash_from_spec (b : Slice U8) (at_ : Usize) (h : at_.val + 32 ≤ b.val.length) :
    tree.hash_from b at_ ⦃ r => r.val = (b.val.drop at_.val).take 32 ⦄ := by
  unfold tree.hash_from
  apply WP.spec_mono (P₀ := fun r : Std.Array U8 32#usize => ∀ j (hj : j < 32), r.val[j]! = b.val[at_.val + j]!)
  · unfold tree.hash_from_loop
    apply loop_idx_spec _ (fun x : Std.Array U8 32#usize × Usize => x.2) 32
      (fun x : Std.Array U8 32#usize × Usize => ∀ j (hj : j < x.2.val), x.1.val[j]! = b.val[at_.val + j]!)
    · rintro ⟨hh, i⟩ hinv hi
      simp only at hinv hi
      unfold tree.hash_from_loop.body
      step*
      refine ⟨fun j hj => ?_, by scalar_tac, by scalar_tac⟩
      have hl : hh.val.length = 32 := by simp
      rw [a_post, Aeneas.Std.Array.set_val_eq]
      simp only [List.getElem!_eq_getElem?_getD, List.getElem?_set]
      by_cases hji : i.val = j
      · subst hji
        simp only [if_true, hl, show i.val < 32 by scalar_tac, Option.getD_some]
        simp only [i1_post] at i2_post
        rw [i2_post, List.getElem?_eq_getElem (by scalar_tac), Option.getD_some]
      · simp only [hji, if_false]
        have := hinv j (by scalar_tac)
        simpa only [List.getElem!_eq_getElem?_getD] using this
    · intro j hj; simp at hj
    · simp
  · intro r hr
    have hl : r.val.length = 32 := by simp
    apply List.ext_getElem (by simp; omega)
    intro j h1 h2
    have := hr j (by omega)
    simp only [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem h1,
      List.getElem?_eq_getElem (show at_.val + j < b.val.length by omega), Option.getD_some] at this
    simp [this]

/-! ## The entry encoding -/

theorem lenLo_val (n : Nat) : (lenLo n).val = n % 256 := by simp [lenLo, U8.ofNatCore]
theorem lenHi_val (n : Nat) : (lenHi n).val = n / 256 % 256 := by simp [lenHi, U8.ofNatCore]

theorem hash_len (e : tree.Entry) : e.hash.val.length = 32 := by simp

theorem encEntry_length (e : tree.Entry) : (encEntry e).length = 35 + e.name.val.length := by
  simp [encEntry]; omega

/-- What a valid entry at the front of `L` says about `L`'s bytes. -/
theorem enc_facts (e : tree.Entry) (L : List U8) (hp : encEntry e <+: L) :
    35 + e.name.val.length ≤ L.length ∧ L[0]! = e.mode ∧ (L[1]!).val = e.name.val.length % 256 ∧
      (L[2]!).val = e.name.val.length / 256 % 256 ∧ (L.drop 3).take e.name.val.length = e.name.val ∧
      (L.drop (3 + e.name.val.length)).take 32 = e.hash.val := by
  obtain ⟨t, rfl⟩ := hp
  have hh := hash_len e
  refine ⟨by simp [encEntry]; omega, by simp [encEntry], by simp [encEntry, lenLo_val],
    by simp [encEntry, lenHi_val], ?_, ?_⟩
  · simp [encEntry]
  · simp only [encEntry, List.append_assoc, List.cons_append, List.nil_append]
    rw [show 3 + e.name.val.length = (e.name.val.length + 1 + 1) + 1 by omega]
    simp [hh]

/-- The converse: bytes with those fields start with the entry's encoding. -/
theorem enc_of_facts (e : tree.Entry) (L : List U8) (hl : 35 + e.name.val.length ≤ L.length)
    (h0 : L[0]! = e.mode) (h1 : (L[1]!).val = e.name.val.length % 256)
    (h2 : (L[2]!).val = e.name.val.length / 256 % 256)
    (hn : (L.drop 3).take e.name.val.length = e.name.val)
    (hh : (L.drop (3 + e.name.val.length)).take 32 = e.hash.val) : encEntry e <+: L := by
  match L, hl with
  | a :: b :: c :: rest, hl =>
    simp only [List.getElem!_cons_zero, List.getElem!_cons_succ] at h0 h1 h2
    simp only [List.drop_succ_cons, List.drop_zero] at hn
    rw [show 3 + e.name.val.length = e.name.val.length + 1 + 1 + 1 by omega] at hh
    simp only [List.drop_succ_cons] at hh
    refine ⟨rest.drop (e.name.val.length + 32), ?_⟩
    simp only [encEntry, List.cons_append, List.nil_append, List.append_assoc, List.cons.injEq]
    refine ⟨h0.symm, ?_, ?_, ?_⟩
    · apply UScalar.eq_of_val_eq; rw [lenLo_val]; exact h1.symm
    · apply UScalar.eq_of_val_eq; rw [lenHi_val]; exact h2.symm
    · conv_rhs => rw [← List.take_append_drop e.name.val.length rest,
        ← List.take_append_drop 32 (List.drop e.name.val.length rest)]
      rw [hn, hh, List.drop_drop]
  | [], hl | [_], hl | [_, _], hl => simp at hl <;> omega

theorem lenLo_lenHi_inj {n m : Nat} (hn : n ≤ 65535) (hm : m ≤ 65535) (h1 : lenLo n = lenLo m)
    (h2 : lenHi n = lenHi m) : n = m := by
  have a := congrArg UScalar.val h1
  have b := congrArg UScalar.val h2
  simp only [lenLo_val, lenHi_val] at a b
  omega

/-- A valid entry's encoding determines it. -/
theorem encEntry_inj (e e' : tree.Entry) (he : EntryOk e) (he' : EntryOk e')
    (h : encEntry e ++ t = encEntry e' ++ t') : e = e' ∧ t = t' := by
  have hh := hash_len e
  have hh' := hash_len e'
  simp only [encEntry, List.cons_append, List.nil_append, List.append_assoc, List.cons.injEq] at h
  obtain ⟨hm, hlo, hhi, hr⟩ := h
  have hn := lenLo_lenHi_inj he.2.2.1 he'.2.2.1 hlo hhi
  have ⟨hnm, hr2⟩ := List.append_inj hr hn
  have ⟨hha, ht⟩ := List.append_inj hr2 (by rw [hh, hh'])
  refine ⟨?_, ht⟩
  cases e; cases e'
  simp only at hm hnm hha ⊢
  simp only [tree.Entry.mk.injEq]
  exact ⟨hm, alloc.vec.Vec.ext _ _ hnm, Std.Array.ext _ _ hha⟩

/-- Two valid entries both at the front of `L` are the same entry. -/
theorem entry_unique (e e' : tree.Entry) (L : List U8) (he : EntryOk e) (he' : EntryOk e')
    (hp : encEntry e <+: L) (hp' : encEntry e' <+: L) : e = e' := by
  obtain ⟨t, ht⟩ := hp
  obtain ⟨t', ht'⟩ := hp'
  exact (encEntry_inj e e' he he' (ht.trans ht'.symm)).1

theorem drop_get (L : List U8) (a k : Nat) : (L.drop a)[k]! = L[a + k]! := by
  simp [List.getElem!_eq_getElem?_getD, List.getElem?_drop]

theorem get!_eq (L : List U8) (k : Nat) (h : k < L.length) : L[k]! = L[k] := by
  simp [List.getElem!_eq_getElem?_getD, List.getElem?_eq_getElem h]

/-- A valid entry at `b[a..]`, in terms of `b`'s bytes. -/
theorem entry_bytes (b : List U8) (a : Nat) (e : tree.Entry) (he : EntryOk e) (hp : encEntry e <+: b.drop a) :
    35 + e.name.val.length ≤ b.length - a ∧ ∃ (h2 : a + 2 < b.length),
      b[a] = e.mode ∧ (b[a + 1]).val + (b[a + 2]).val * 256 = e.name.val.length ∧
      (b.drop (a + 3)).take e.name.val.length = e.name.val ∧
      (b.drop (a + 3 + e.name.val.length)).take 32 = e.hash.val := by
  obtain ⟨hl, h0, h1, h2, hn, hh⟩ := enc_facts e _ hp
  simp only [List.length_drop, drop_get, List.drop_drop] at hl h0 h1 h2 hn hh
  have hb : a + 2 < b.length := by omega
  rw [get!_eq _ _ (by omega)] at h0 h1
  rw [get!_eq _ _ hb] at h2
  have := he.2.2.1
  refine ⟨hl, hb, h0, by omega, ?_, ?_⟩
  · exact hn
  · rw [show a + 3 + e.name.val.length = a + (3 + e.name.val.length) by omega]; exact hh

set_option maxHeartbeats 1000000 in
/-- `parse_entry` at `at_`: a valid entry and the offset after it, at the front of `b.drop at_`; or no
valid entry is at the front. -/
@[step] theorem parse_entry_spec (b : Slice U8) (at_ : Usize) :
    tree.parse_entry b at_ ⦃ r => match r with
      | .Ok (e, next) => EntryOk e ∧ next.val = at_.val + (encEntry e).length ∧ encEntry e <+: b.val.drop at_.val
      | .Err _ => ∀ e, EntryOk e → ¬ encEntry e <+: b.val.drop at_.val ⦄ := by
  unfold tree.parse_entry
  have hbl := b.property
  step*
  · intro e he hp
    have := (entry_bytes _ _ e he hp).1
    scalar_tac
  · intro e he hp
    have := (entry_bytes _ _ e he hp).1
    scalar_tac
  · intro e he hp
    obtain ⟨hl, _, _, hlen, _, _⟩ := entry_bytes _ _ e he hp
    simp only [i2_post] at i3_post
    simp only [i5_post] at i6_post
    simp only [i4_post, i7_post, U8.cast_Usize_val_eq, i3_post, i6_post] at n_post i8_post
    scalar_tac
  · intro e he hp
    obtain ⟨hl, _, _, hlen, _, _⟩ := entry_bytes _ _ e he hp
    simp only [i2_post] at i3_post
    simp only [i5_post] at i6_post
    simp only [i4_post, i7_post, U8.cast_Usize_val_eq, i3_post, i6_post] at n_post i8_post
    scalar_tac
  · intro e he hp
    obtain ⟨hl, _, hm, _, _, _⟩ := entry_bytes _ _ e he hp
    have := he.1
    rw [mode_post] at *
    scalar_tac
  · -- Ok: the fields read are exactly a valid entry's encoding.
    simp only [i2_post] at i3_post
    simp only [i5_post] at i6_post
    simp only [i4_post, i7_post, U8.cast_Usize_val_eq] at n_post i8_post
    have hnv : n.val = i3.val + i6.val * 256 := by omega
    have h3 : i3.val < 256 := by scalar_tac
    have h6 : i6.val < 256 := by scalar_tac
    have hlt : at_.val + 2 < b.val.length := by scalar_tac
    have hnl : «name».val.length = n.val := by rw [name_post]; simp; scalar_tac
    have hok : EntryOk { mode := mode, «name» := «name», hash := hash } :=
      ⟨by scalar_tac, b1_post.1 ‹_›⟩
    refine ⟨hok, ?_, ?_⟩
    · rw [encEntry_length]; simp only [hnl]; scalar_tac
    · apply enc_of_facts
      · simp only [List.length_drop, hnl]; scalar_tac
      · simp only [drop_get, Nat.add_zero]; rw [get!_eq _ _ (by omega), mode_post]
      · simp only [drop_get, hnl]; rw [get!_eq _ _ (by omega), ← i3_post]; omega
      · simp only [drop_get, hnl]; rw [get!_eq _ _ (by omega), ← i6_post]; omega
      · rw [List.drop_drop, hnl, name_post, i11_post]
      · rw [List.drop_drop, hnl, hash_post, i12_post, i11_post, Nat.add_assoc]
  · -- BadName: a valid entry here would have this very name.
    intro e he hp
    obtain ⟨hl, _, _, hlen, hn, _⟩ := entry_bytes _ _ e he hp
    simp only [i2_post] at i3_post
    simp only [i5_post] at i6_post
    simp only [i4_post, i7_post, U8.cast_Usize_val_eq, i3_post, i6_post] at n_post i8_post
    have hn' : n.val = e.name.val.length := by scalar_tac
    have : «name».val = e.name.val := by rw [name_post, hn', i11_post]; exact hn
    exact ‹¬b1 = true› (b1_post.2 (this ▸ he.2))

/-! ## Entry lists -/

theorem magicL_length : magicL.length = 8 := rfl

theorem flat_length_ge (es : List tree.Entry) : 35 * es.length ≤ (es.flatMap encEntry).length := by
  induction es with
  | nil => simp
  | cons e es ih => simp only [List.flatMap_cons, List.length_append, encEntry_length, List.length_cons]; omega

theorem encodeL_length_ge (es : List tree.Entry) : 8 + 35 * es.length ≤ (encodeL es).length := by
  have := flat_length_ge es; simp only [encodeL, List.length_append, magicL_length]; omega

/-- The entry encoding is prefix-free: a prefix of encodings is the encoding of a prefix. -/
theorem flat_prefix (xs ys : List tree.Entry) (hx : ∀ e ∈ xs, EntryOk e) (hy : ∀ e ∈ ys, EntryOk e)
    (h : xs.flatMap encEntry <+: ys.flatMap encEntry) : xs <+: ys := by
  induction xs generalizing ys with
  | nil => exact List.nil_prefix
  | cons x xs ih =>
    match ys with
    | [] =>
      exfalso
      have := h.length_le
      simp only [List.flatMap_cons, List.length_append, encEntry_length, List.flatMap_nil, List.length_nil] at this
      omega
    | y :: ys =>
      obtain ⟨t, ht⟩ := h
      simp only [List.flatMap_cons, List.append_assoc] at ht
      have ⟨hxy, ht2⟩ := encEntry_inj x y (hx x (by simp)) (hy y (by simp)) ht
      subst hxy
      have := ih ys (fun e he => hx e (by simp [he])) (fun e he => hy e (by simp [he])) ⟨t, ht2⟩
      exact List.cons_prefix_cons.2 ⟨rfl, this⟩

/-- In a canonical encoding read up to `at` (the entries `out`), the next bytes are the next entry. -/
theorem next_entry (b : List U8) (es out : List tree.Entry) (at_ : Nat) (hc : Canonical es)
    (hb : b = encodeL es) (ht : b.take at_ = encodeL out) (ho : ∀ e ∈ out, EntryOk e) (hlt : at_ < b.length) :
    ∃ e rest, es = out ++ e :: rest ∧ EntryOk e ∧ encEntry e <+: b.drop at_ := by
  have hp : encodeL out <+: encodeL es := by rw [← ht, ← hb]; exact List.take_prefix _ _
  simp only [encodeL] at hp
  have hp2 := (List.prefix_append_right_inj magicL).1 hp
  obtain ⟨rest, hrest⟩ := flat_prefix out es ho hc.1 hp2
  have hat : at_ = (encodeL out).length := by rw [← ht]; simp; omega
  have hd : b.drop at_ = rest.flatMap encEntry := by
    rw [hb, hat, ← hrest]; simp [encodeL, List.flatMap_append]
  match rest with
  | [] =>
    exfalso
    have : b.length = at_ := by rw [hat, hb, ← hrest]; simp [encodeL]
    omega
  | e :: rest' =>
    refine ⟨e, rest', hrest.symm, hc.1 e (by rw [← hrest]; simp), ?_⟩
    rw [hd, List.flatMap_cons]; exact List.prefix_append _ _

theorem cmpL_cases (a b : List U8) : cmpL a b = -1 ∨ cmpL a b = 0 ∨ cmpL a b = 1 := by
  induction a generalizing b with
  | nil => cases b <;> simp [cmpL]
  | cons x xs ih =>
    cases b with
    | nil => simp [cmpL]
    | cons y ys => simp only [cmpL]; split <;> [simp; split <;> [simp; exact ih ys]]

/-! ## The decoder -/

/-- What `decode` returns: the entries of a canonical encoding, or an error exactly when the bytes are
not the encoding of any canonical entry list. -/
def DecodeOK (b : List U8) : core.result.Result (alloc.vec.Vec tree.Entry) tree.TreeError → Prop
  | .Ok es => Canonical es.val ∧ b = encodeL es.val
  | .Err _ => ∀ es, Canonical es → b ≠ encodeL es

/-- The loop's state after reading the entries `out` up to offset `at_`. -/
def DecInv (b : List U8) (out : List tree.Entry) (at_ : Nat) : Prop :=
  8 ≤ at_ ∧ at_ ≤ b.length ∧ (∀ e ∈ out, EntryOk e) ∧
    out.IsChain (fun a b => cmpL a.name.val b.name.val = -1) ∧ b.take at_ = encodeL out

theorem last_rel (out : List tree.Entry) (e : tree.Entry) (rest : List tree.Entry)
    (h : (out ++ e :: rest).IsChain (fun a b => cmpL a.name.val b.name.val = -1)) (hne : out ≠ []) :
    cmpL (out.getLast hne).name.val e.name.val = -1 := by
  rw [List.isChain_split] at h
  have := (List.isChain_append.1 h.1).2.2
  exact this _ (by simp [List.getLast?_eq_some_getLast hne]) e (by simp)

set_option maxHeartbeats 1000000 in
theorem decode_loop_spec (b : Slice U8) (hlen : b.val.length ≤ 2147483648) (out : alloc.vec.Vec tree.Entry)
    (at_ : Usize) (hinv : DecInv b.val out.val at_.val) :
    tree.decode_loop b out at_ ⦃ r => DecodeOK b.val r ⦄ := by
  unfold tree.decode_loop
  apply loop_idx_spec _ (fun x : alloc.vec.Vec tree.Entry × Usize => x.2) b.val.length
    (fun x : alloc.vec.Vec tree.Entry × Usize => DecInv b.val x.1.val x.2.val)
  · rintro ⟨out, at_⟩ ⟨h8, hle, hok, hch, htk⟩ _
    simp only at h8 hle hok hch htk ⊢
    unfold tree.decode_loop.body
    have hbl := b.property
    have hge := encodeL_length_ge out.val
    have hat : (encodeL out.val).length = at_.val := by rw [← htk]; simp; omega
    step*
    · -- `parse_entry` found a valid entry `e` ending at `next`.
      obtain ⟨e, next⟩ := p
      subst_vars
      simp only at r_post
      obtain ⟨heo, hnext, hpre⟩ := r_post
      try dsimp only
      have hel := encEntry_length e
      have hnl : next.val ≤ b.val.length := by
        have := hpre.length_le; simp only [List.length_drop] at this; omega
      have hpush : out.val.length < Usize.max := by scalar_tac
      have htk' : b.val.take next.val = encodeL (out.val ++ [e]) := by
        rw [hnext, List.take_add, htk, ← List.prefix_iff_eq_take.1 hpre]
        simp [encodeL, List.flatMap_append]
      have hok' : ∀ x ∈ out.val ++ [e], EntryOk x := by
        intro x hx; simp only [List.mem_append, List.mem_singleton] at hx
        rcases hx with hx | rfl; exacts [hok x hx, heo]
      split
      · step*
        · -- Unsorted: then no canonical entry list encodes `b`.
          intro es hc hb
          obtain ⟨e', rest, hes, he', hp'⟩ := next_entry b.val es out.val at_.val hc hb htk hok (by scalar_tac)
          have := entry_unique e e' _ heo he' hpre hp'
          subst this
          have hne : out.val ≠ [] := by intro h; simp [alloc.vec.Vec.len, h] at *
          have hr := last_rel out.val e rest (hes ▸ hc.2.1) hne
          rw [List.getLast_eq_getElem] at hr
          have hl0 : 0 < out.val.length := List.length_pos_of_ne_nil hne
          have : e1 = out.val[out.val.length - 1]'(by omega) := by rw [e1_post]; congr 1
          rw [this] at i4_post
          scalar_tac
        · simp only [out1_post]
          refine ⟨⟨by omega, hnl, hok', ?_, htk'⟩, by omega, hnl⟩
          apply List.IsChain.append hch (List.isChain_singleton e)
          intro x hx y hy
          have hne : out.val ≠ [] := by intro h; simp [alloc.vec.Vec.len, h] at *
          rw [List.getLast?_eq_some_getLast hne, Option.mem_some_iff] at hx
          simp only [List.head?_cons, Option.mem_some_iff] at hy
          subst hx hy
          rw [List.getLast_eq_getElem]
          have hl0 : 0 < out.val.length := List.length_pos_of_ne_nil hne
          have : e1 = out.val[out.val.length - 1]'(by omega) := by rw [e1_post]; congr 1
          rw [this] at i4_post
          rcases cmpL_cases (out.val[out.val.length - 1]).name.val e.name.val with h | h | h <;> scalar_tac
      · step*
        have h0 : out.val = [] := by simp [alloc.vec.Vec.len] at *; scalar_tac
        simp only [out1_post]
        refine ⟨⟨by omega, hnl, hok', ?_, htk'⟩, by omega, hnl⟩
        rw [h0]; exact List.isChain_singleton e
    · -- `parse_entry` failed: no valid entry here, so no canonical list encodes `b`.
      subst_vars
      simp only at r_post
      intro es hc hb
      obtain ⟨e', rest, _, he', hp'⟩ := next_entry b.val es out.val at_.val hc hb htk hok (by scalar_tac)
      exact r_post e' he' hp'
    · -- All bytes read: `b` is the encoding of `out`.
      have : at_.val = b.val.length := by scalar_tac
      rw [this, List.take_length] at htk
      exact ⟨⟨hok, hch, by rw [← htk]; exact hlen⟩, htk⟩
  · exact hinv
  · exact hinv.2.1

theorem encodeL_take8 (es : List tree.Entry) : (encodeL es).take 8 = magicL := by
  simp [encodeL, magicL_length]

/-- **The tree decoder equals its spec on every input**: it never panics, it accepts exactly the canonical
encodings, and it returns the entries they encode. -/
@[step] theorem decode_spec (b : Slice U8) : tree.decode b ⦃ r => DecodeOK b.val r ⦄ := by
  unfold tree.decode
  simp only [tree.MAGIC_LEN, tree.MAX_TREE]
  have hbl := b.property
  step*
  · intro es hc hb
    have := encodeL_length_ge es
    rw [← hb] at this; scalar_tac
  · intro es hc hb
    have := hc.2.2
    rw [← hb] at this; scalar_tac
  · apply decode_loop_spec b (by scalar_tac)
    refine ⟨by simp, by scalar_tac, by simp, List.isChain_nil, ?_⟩
    simp only [encodeL]
    exact b1_post.1 ‹_›
  · intro es hc hb
    apply ‹¬b1 = true›
    rw [b1_post, hb]; exact encodeL_take8 es

/-! ## The canonical-form check and the encoder -/

theorem take_succ_get (es : List tree.Entry) (i : Nat) (h : i < es.length) :
    es.take (i + 1) = es.take i ++ [es[i]] := by
  rw [List.take_add_one, List.getElem?_eq_getElem h]; rfl

theorem encodeL_take_succ (es : List tree.Entry) (i : Nat) (h : i < es.length) :
    (encodeL (es.take (i + 1))).length = (encodeL (es.take i)).length + 35 + es[i].name.val.length := by
  rw [take_succ_get es i h]
  simp only [encodeL, List.flatMap_append, List.length_append, List.flatMap_cons, List.flatMap_nil,
    List.append_nil, encEntry_length]
  omega

theorem encodeL_take_le (es : List tree.Entry) (k : Nat) : (encodeL (es.take k)).length ≤ (encodeL es).length := by
  have h := congrArg (fun l => (encodeL l).length) (List.take_append_drop k es)
  simp only [encodeL, List.flatMap_append, List.length_append] at h ⊢
  omega

def ChkOK (es : List tree.Entry) : core.result.Result Usize tree.TreeError → Prop
  | .Ok n => Canonical es ∧ n.val = (encodeL es).length
  | .Err _ => ¬ Canonical es

def ChkInv (es : List tree.Entry) (size : Nat) (i : Nat) : Prop :=
  i ≤ es.length ∧ (∀ j (h : j < i) (h2 : j < es.length), EntryOk es[j]) ∧
    (∀ j (h : j + 1 < i) (h2 : j + 1 < es.length), cmpL es[j].name.val es[j + 1].name.val = -1) ∧
    size = (encodeL (es.take i)).length ∧ size ≤ 2147483648

set_option maxHeartbeats 1000000 in
theorem check_loop_spec (es : alloc.vec.Vec tree.Entry) :
    tree.check_loop es 8#usize 0#usize ⦃ r => ChkOK es.val r ⦄ := by
  unfold tree.check_loop
  apply loop_idx_spec _ (fun x : Usize × Usize => x.2) es.val.length
    (fun x : Usize × Usize => ChkInv es.val x.1.val x.2.val)
  · rintro ⟨size, i⟩ ⟨hle, hok, hch, hsz, hmax⟩ _
    simp only at hle hok hch hsz hmax ⊢
    unfold tree.check_loop.body
    simp only [tree.MAX_TREE]
    have hesl := es.property
    step*
    · -- BadMode
      intro hc; have := (hc.1 e (by rw [e_post]; exact List.getElem_mem _)).1; scalar_tac
    · -- Unsorted: es[i-1] and es[i] are adjacent.
      have hi : i.val < es.val.length := by scalar_tac
      intro hc
      have := hc.2.1.getElem (i.val - 1) (by scalar_tac)
      have hi1 : i.val - 1 + 1 = i.val := by scalar_tac
      simp only [hi1] at this
      rw [e1_post, e_post] at i3_post
      have hj : i2.val = i.val - 1 := by scalar_tac
      simp only [hj] at i3_post
      scalar_tac
    · have := b_post.1 ‹_›; have := this.2.1; scalar_tac
    · -- TooLarge
      have hi : i.val < es.val.length := by scalar_tac
      intro hc
      have h1 := encodeL_take_succ es.val i.val hi
      have h2 := encodeL_take_le es.val (i.val + 1)
      have h3 := hc.2.2
      rw [← e_post] at h1
      scalar_tac
    · -- next entry (i > 0)
      have hi : i.val < es.val.length := by scalar_tac
      have hn := b_post.1 ‹_›
      refine ⟨⟨by scalar_tac, fun j hj h2 => ?_, fun j hj h2 => ?_, ?_, by scalar_tac⟩, by scalar_tac, by scalar_tac⟩
      · by_cases hji : j = i.val
        · subst hji; rw [← e_post]; exact ⟨by scalar_tac, hn⟩
        · exact hok j (by scalar_tac) h2
      · by_cases hji : j + 1 = i.val
        · have hj : i2.val = j := by scalar_tac
          rw [e1_post, e_post] at i3_post
          simp only [hj] at i3_post
          have hc := cmpL_cases es.val[j].name.val es.val[j + 1].name.val
          simp only [hji] at hc ⊢
          scalar_tac
        · exact hch j (by scalar_tac) h2
      · have h1 := encodeL_take_succ es.val i.val hi
        rw [← e_post] at h1
        have : i6.val = i.val + 1 := by scalar_tac
        rw [this, h1]; scalar_tac
    · have := b_post.1 ‹_›; have := this.2.1; scalar_tac
    · -- TooLarge (first entry)
      have hi : i.val < es.val.length := by scalar_tac
      intro hc
      have h1 := encodeL_take_succ es.val i.val hi
      have h2 := encodeL_take_le es.val (i.val + 1)
      have h3 := hc.2.2
      rw [← e_post] at h1
      scalar_tac
    · -- first entry
      have hi : i.val < es.val.length := by scalar_tac
      have hn := b_post.1 ‹_›
      have hi0 : i.val = 0 := by scalar_tac
      refine ⟨⟨by scalar_tac, fun j hj h2 => ?_, fun j hj h2 => by scalar_tac, ?_, by scalar_tac⟩, by scalar_tac, by scalar_tac⟩
      · have : j = i.val := by scalar_tac
        subst this; rw [← e_post]; exact ⟨by scalar_tac, hn⟩
      · have h1 := encodeL_take_succ es.val i.val hi
        rw [← e_post] at h1
        have : i6.val = i.val + 1 := by scalar_tac
        rw [this, h1]; scalar_tac
    · -- BadName
      intro hc; apply ‹¬b = true›; rw [b_post]
      exact (hc.1 e (by rw [e_post]; exact List.getElem_mem _)).2
    · -- every entry checked
      have hil : i.val = es.val.length := by scalar_tac
      rw [hil, List.take_length] at hsz
      refine ⟨⟨fun e he => ?_, List.isChain_iff_getElem.2 (fun j hj => hch j (by omega) hj), by omega⟩, hsz⟩
      obtain ⟨j, hj, rfl⟩ := List.mem_iff_getElem.1 he
      exact hok j (by omega) hj
  · refine ⟨by simp, fun j h => by simp at h, fun j h => by simp at h, by simp [encodeL, magicL_length], by simp⟩
  · simp

@[step] theorem check_spec (es : alloc.vec.Vec tree.Entry) : tree.check es ⦃ r => ChkOK es.val r ⦄ := by
  unfold tree.check; simp only [tree.MAGIC_LEN]; exact check_loop_spec es

@[step] theorem extend_spec (out src : alloc.vec.Vec U8) (h : out.val.length + src.val.length ≤ Usize.max) :
    tree.extend out src ⦃ r => r.val = out.val ++ src.val ⦄ := by
  unfold tree.extend tree.extend_loop
  apply loop_idx_spec _ (fun x : alloc.vec.Vec U8 × Usize => x.2) src.val.length
    (fun x : alloc.vec.Vec U8 × Usize => x.1.val = out.val ++ src.val.take x.2.val)
  · rintro ⟨o, i⟩ hinv _
    simp only at hinv ⊢
    unfold tree.extend_loop.body
    simp only [alloc.vec.Vec.len]
    step*
    · have : i.val < src.val.length := by scalar_tac
      rw [hinv]; simp only [List.length_append, List.length_take]; scalar_tac
    · refine ⟨?_, by scalar_tac, by scalar_tac⟩
      have hi : i.val < src.val.length := by scalar_tac
      rw [out1_post, hinv, i3_post, List.take_add_one, List.getElem?_eq_getElem hi, i2_post]; simp
    · have : i.val = src.val.length := by scalar_tac
      rw [hinv, this, List.take_length]
  · simp
  · simp

@[step] theorem extend_hash_spec (out : alloc.vec.Vec U8) (hh : Std.Array U8 32#usize)
    (h : out.val.length + 32 ≤ Usize.max) : tree.extend_hash out hh ⦃ r => r.val = out.val ++ hh.val ⦄ := by
  unfold tree.extend_hash tree.extend_hash_loop
  have hl : hh.val.length = 32 := by simp
  apply loop_idx_spec _ (fun x : alloc.vec.Vec U8 × Usize => x.2) 32
    (fun x : alloc.vec.Vec U8 × Usize => x.1.val = out.val ++ hh.val.take x.2.val)
  · rintro ⟨o, i⟩ hinv _
    simp only at hinv ⊢
    unfold tree.extend_hash_loop.body
    step*
    · have : i.val < 32 := by scalar_tac
      rw [hinv]; simp only [List.length_append, List.length_take, hl]; scalar_tac
    · refine ⟨?_, by scalar_tac, by scalar_tac⟩
      have hi : i.val < hh.val.length := by scalar_tac
      rw [out1_post, hinv, i2_post, List.take_add_one, List.getElem?_eq_getElem hi, i1_post]; simp
    · have : i.val = 32 := by scalar_tac
      rw [hinv]; simp only [this]; rw [List.take_of_length_le (by rw [hl])]
  · simp
  · simp

theorem lenLo_cast (n : Usize) (x : Usize) (hx : x.val = n.val % 256) : UScalar.cast .U8 x = lenLo n.val := by
  apply UScalar.eq_of_val_eq; rw [UScalar.cast_val_eq, lenLo_val, hx]; simp

theorem lenHi_cast (n : Usize) (x : Usize) (hx : x.val = n.val / 256) : UScalar.cast .U8 x = lenHi n.val := by
  apply UScalar.eq_of_val_eq; rw [UScalar.cast_val_eq, lenHi_val, hx]; simp

set_option maxHeartbeats 1000000 in
theorem encode_loop_spec (es : alloc.vec.Vec tree.Entry) (hc : Canonical es.val) (out : alloc.vec.Vec U8)
    (hout : out.val = magicL) :
    tree.encode_loop es out 0#usize ⦃ r => r.val = encodeL es.val ⦄ := by
  unfold tree.encode_loop
  have hlen := hc.2.2
  apply loop_idx_spec _ (fun x : alloc.vec.Vec U8 × Usize => x.2) es.val.length
    (fun x : alloc.vec.Vec U8 × Usize => x.1.val = encodeL (es.val.take x.2.val))
  · rintro ⟨o, i⟩ hinv hle
    simp only at hinv hle ⊢
    unfold tree.encode_loop.body
    simp only [alloc.vec.Vec.len]
    split
    · step
      have hi : i.val < es.val.length := by scalar_tac
      have h1 := encodeL_take_succ es.val i.val hi
      have h2 := encodeL_take_le es.val (i.val + 1)
      rw [← e_post] at h1
      have hol : o.val.length + 35 + e.name.val.length ≤ 2147483648 := by rw [hinv]; omega
      have hmx : 4294967295 ≤ Usize.max := by scalar_tac
      step*
      · simp only [out1_post, List.length_append, List.length_singleton]; scalar_tac
      · simp only [out2_post, out1_post, List.length_append, List.length_singleton]; scalar_tac
      · simp only [out3_post, out2_post, out1_post, List.length_append, List.length_singleton]; scalar_tac
      · simp only [out4_post, out3_post, out2_post, out1_post, List.length_append, List.length_singleton]; scalar_tac
      · refine ⟨?_, by scalar_tac, by scalar_tac⟩
        have hn : (Usize.ofNatCore e.name.val.length (by scalar_tac)).val = e.name.val.length := by simp
        have hlo : i3 = lenLo e.name.val.length := by
          rw [i3_post]; apply UScalar.eq_of_val_eq; rw [UScalar.cast_val_eq, lenLo_val, i2_post, hn]; simp
        have hhi : i5 = lenHi e.name.val.length := by
          rw [i5_post]; apply UScalar.eq_of_val_eq; rw [UScalar.cast_val_eq, lenHi_val, i4_post, hn]; simp
        have : i6.val = i.val + 1 := by scalar_tac
        rw [this, take_succ_get es.val i.val hi, ← e_post, out5_post, out4_post, out3_post, out2_post, out1_post,
          hinv, hlo, hhi]
        simp [encodeL, encEntry, List.flatMap_append]
    · step*
      have : i.val = es.val.length := by scalar_tac
      rw [hinv, this, List.take_length]
  · simp [hout, encodeL]
  · simp

/-- **The encoder** produces exactly the canonical encoding, and refuses exactly the non-canonical entry lists. -/
@[step] theorem encode_spec (es : alloc.vec.Vec tree.Entry) :
    tree.encode es ⦃ r => match r with
      | .Ok v => Canonical es.val ∧ v.val = encodeL es.val
      | .Err _ => ¬ Canonical es.val ⦄ := by
  unfold tree.encode
  apply WP.spec_bind (check_spec es)
  intro r hr
  cases r with
  | Err e => simpa [ChkOK] using hr
  | Ok n =>
    simp only [ChkOK] at hr
    simp only
    step*
    all_goals try (simp_all; scalar_tac)
    apply WP.spec_bind (encode_loop_spec es hr.1 out7 (by simp_all [magicL]))
    intro v hv
    simp only [WP.spec_ok]
    exact ⟨hr.1, hv⟩

/-! ## The GC tracer -/

/-- **The tracer returns exactly the entry hashes**, in order, of a canonical tree; for any other bytes it
fails (and the object is a leaf). -/
@[step] theorem children_spec (b : Slice U8) :
    tree.children b ⦃ r => match r with
      | .Ok hs => ∃ es, Canonical es ∧ b.val = encodeL es ∧ hs.val = es.map (fun e => e.hash)
      | .Err _ => ∀ es, Canonical es → b.val ≠ encodeL es ⦄ := by
  unfold tree.children
  apply WP.spec_bind (decode_spec b)
  intro r hr
  cases r with
  | Err e => simpa [DecodeOK] using hr
  | Ok es =>
    simp only [DecodeOK] at hr
    simp only
    apply WP.spec_bind (Pₘ := fun out : alloc.vec.Vec (Std.Array U8 32#usize) => out.val = es.val.map (fun e => e.hash))
    · unfold tree.children_loop
      apply loop_idx_spec _ (fun x : alloc.vec.Vec (Std.Array U8 32#usize) × Usize => x.2) es.val.length
        (fun x : alloc.vec.Vec (Std.Array U8 32#usize) × Usize => x.1.val = (es.val.take x.2.val).map (fun e => e.hash))
      · rintro ⟨o, i⟩ hinv _
        simp only at hinv ⊢
        unfold tree.children_loop.body
        simp only [alloc.vec.Vec.len]
        have hesl := es.property
        step*
        · rw [hinv]; simp; scalar_tac
        · refine ⟨?_, by scalar_tac, by scalar_tac⟩
          have hi : i.val < es.val.length := by scalar_tac
          have : i2.val = i.val + 1 := by scalar_tac
          rw [out1_post, hinv, this, take_succ_get es.val i.val hi, e_post, List.map_append]; rfl
        · have : i.val = es.val.length := by scalar_tac
          rw [hinv, this, List.take_length]
      · simp
      · simp
    · intro out hout
      simp only [WP.spec_ok]
      exact ⟨es.val, hr.1, hr.2, hout⟩

/-! ## Round trips -/

theorem flat_inj (xs ys : List tree.Entry) (hx : ∀ e ∈ xs, EntryOk e) (hy : ∀ e ∈ ys, EntryOk e)
    (h : xs.flatMap encEntry = ys.flatMap encEntry) : xs = ys := by
  have h1 := flat_prefix xs ys hx hy (h ▸ List.prefix_refl _)
  have h2 := flat_prefix ys xs hy hx (h ▸ List.prefix_refl _)
  exact h1.eq_of_length (Nat.le_antisymm h1.length_le h2.length_le)

/-- Canonical entry lists have distinct encodings. -/
theorem encodeL_inj (xs ys : List tree.Entry) (hx : Canonical xs) (hy : Canonical ys)
    (h : encodeL xs = encodeL ys) : xs = ys :=
  flat_inj xs ys hx.1 hy.1 (List.append_cancel_left h)

/-- **Round trip 1**: decoding a canonical encoding returns the entries encoded. -/
theorem decode_encode (b : Slice U8) (es : List tree.Entry) (hc : Canonical es) (hb : b.val = encodeL es) :
    tree.decode b ⦃ r => ∃ v, r = .Ok v ∧ v.val = es ⦄ := by
  apply WP.spec_mono (decode_spec b)
  intro r hr
  cases r with
  | Ok v => exact ⟨v, rfl, encodeL_inj _ _ hr.1 hc (hr.2.symm.trans hb)⟩
  | Err e => exact absurd hb (hr es hc)

/-- **Round trip 2**: re-encoding what `decode` accepted gives back the same bytes (the encoding is unique). -/
theorem encode_decode (b : Slice U8) (es : alloc.vec.Vec tree.Entry) (hd : DecodeOK b.val (.Ok es)) :
    tree.encode es ⦃ r => ∃ v, r = .Ok v ∧ v.val = b.val ⦄ := by
  apply WP.spec_mono (encode_spec es)
  intro r hr
  cases r with
  | Ok v => exact ⟨v, rfl, hr.2.trans hd.2.symm⟩
  | Err e => exact absurd hd.1 hr

end o5s_kernel.TreeProofs
