import Pico2petaNetKernel
import H5iAppLib.Basic
/-!
# pico2peta's wire parsers: the extracted code computes these specs

Each theorem below is about `crates/net-kernel/src/parse.rs` as Charon and Aeneas extract it
(`generated/Pico2petaNetKernel.lean`). A `⦃ … ⦄` spec holds on every input, so it also says the
function never panics (no out-of-bounds index, no arithmetic overflow).
-/
open Aeneas Aeneas.Std Result pico2peta_net_kernel H5iAppLib

namespace pico2peta_net_kernel.Spec

/-- Byte values of a slice or vector. -/
abbrev B (l : List U8) : List Nat := l.map (·.val)

/-- Value of a hex digit, or 16. With `lower`, only `0-9a-f`. -/
def nib (lower : Bool) (c : Nat) : Nat :=
  if 48 ≤ c ∧ c ≤ 57 then c - 48
  else if 97 ≤ c ∧ c ≤ 102 then c - 87
  else if lower = false ∧ 65 ≤ c ∧ c ≤ 70 then c - 55
  else 16

/-- Strict hex: pairs of digits, each pair one byte. Odd length or a non-digit: `none`. -/
def hexDec (lower : Bool) : List Nat → Option (List Nat)
  | [] => some []
  | [_] => none
  | a :: b :: r =>
    if nib lower a < 16 ∧ nib lower b < 16 then (hexDec lower r).map ((nib lower a * 16 + nib lower b) :: ·)
    else none

/-- Big-endian value of hex digits (either case). -/
def hexNum (l : List Nat) : Nat := l.foldl (fun a c => a * 16 + nib false c) 0

/-- All hex digits (either case). -/
def allHex (l : List Nat) : Bool := l.all (fun c => decide (nib false c < 16))

/-- A pkt-line length: exactly four hex digits, big-endian. -/
def pktLen (l : List Nat) : Option Nat :=
  if l.length = 4 ∧ allHex l = true then some (hexNum l) else none

theorem nib_lt_or (lower : Bool) (c : Nat) : nib lower c < 16 ∨ nib lower c = 16 := by
  unfold nib; split_ifs <;> omega

/-! ## Proofs about the extracted code -/

@[step]
theorem hex_nibble_spec (c : U8) (lower : Bool) :
    parse.hex_nibble c lower ⦃ v => v.val = nib lower c.val ⦄ := by
  unfold parse.hex_nibble
  step*
  all_goals (unfold nib; cases lower <;> simp_all <;> split_ifs <;> scalar_tac)

theorem hexDec_drop (lower : Bool) (L : List Nat) (k : Nat) (h : 2 * k + 2 ≤ L.length) :
    hexDec lower (L.drop (2 * k)) =
      if nib lower L[2 * k] < 16 ∧ nib lower L[2 * k + 1] < 16 then
        (hexDec lower (L.drop (2 * k + 2))).map ((nib lower L[2 * k] * 16 + nib lower L[2 * k + 1]) :: ·)
      else none := by
  rw [List.drop_eq_getElem_cons (by omega), List.drop_eq_getElem_cons (by omega)]
  simp only [hexDec]
  have : 2 * k + 1 + 1 = 2 * k + 2 := by omega
  rw [this]
  rfl

theorem hexDec_odd (lower : Bool) : ∀ L : List Nat, L.length % 2 = 1 → hexDec lower L = none
  | [], h => by simp at h
  | [_], _ => rfl
  | _ :: _ :: r, h => by
    simp only [hexDec]
    split <;> simp [hexDec_odd lower r (by simp at h; omega)]

@[step]
theorem decode_hex_loop_spec (s : Slice U8) (n : Usize) (lower : Bool)
    (out : alloc.vec.Vec U8) (i : Usize)
    (hn : s.length = 2 * n.val) (hi : i.val ≤ n.val) (ho : out.length ≤ i.val) :
    parse.decode_hex_loop s n lower out i ⦃ r =>
      r.map (fun v => B v.val) = (hexDec lower ((B s.val).drop (2 * i.val))).map (B out.val ++ ·) ⦄ := by
  unfold parse.decode_hex_loop
  apply loop.spec_decr_nat (measure := fun (x : alloc.vec.Vec U8 × Usize) => n.val - x.2.val)
    (inv := fun x => i.val ≤ x.2.val ∧ x.2.val ≤ n.val ∧ x.1.length ≤ x.2.val ∧
      (hexDec lower ((B s.val).drop (2 * i.val))).map (B out.val ++ ·) =
        (hexDec lower ((B s.val).drop (2 * x.2.val))).map (B x.1.val ++ ·))
  · rintro ⟨o, j⟩ ⟨h1, h2, h3, h4⟩
    simp only at h1 h2 h3 h4 ⊢
    unfold parse.decode_hex_loop.body
    step*
    all_goals (try have hd := hexDec_drop lower (B s.val) j.val (by simp; scalar_tac))
    all_goals (try simp only [B, List.getElem_map] at hd)
    · rw [h4, hd]
      rw [if_neg (by intro h; simp_all; scalar_tac)]; simp
    · rw [h4, hd]
      rw [if_neg (by intro h; simp_all; scalar_tac)]; simp
    · refine ⟨by scalar_tac, by scalar_tac, by simp [out1_post]; scalar_tac, ?_⟩
      rw [h4, hd]
      rw [if_pos (by simp_all; constructor <;> scalar_tac)]
      have e1 : 2 * (j.val + 1) = 2 * j.val + 2 := by ring
      simp only [Option.map_map, Function.comp_def, out1_post, B, List.map_append, List.map_cons,
        List.map_nil, List.append_assoc, List.cons_append, List.nil_append, e1, i7_post]
      simp_all
      scalar_tac
    · have : (B s.val).drop (2 * j.val) = [] := by
        apply List.drop_eq_nil_of_le; simp; scalar_tac
      rw [h4, this]; simp [hexDec]
  · simp [hi, ho]

@[step]
theorem decode_hex_spec (s : Slice U8) (n : Usize) (lower : Bool) :
    parse.decode_hex s n lower ⦃ r => r.map (fun v => B v.val) =
      if s.length = 2 * n.val then hexDec lower (B s.val) else none ⦄ := by
  unfold parse.decode_hex
  step*
  all_goals (try (rw [if_neg (by scalar_tac)]; rfl))
  · rw [r_post, if_pos (by scalar_tac)]; simp

theorem hexNum_snoc (l : List Nat) (c : Nat) : hexNum (l ++ [c]) = hexNum l * 16 + nib false c := by
  simp [hexNum, List.foldl_append]

theorem hexNum_lt (l : List Nat) (h : allHex l = true) : hexNum l < 16 ^ l.length := by
  induction l using List.reverseRecOn with
  | nil => simp [hexNum]
  | append_singleton l c ih =>
    simp [allHex, List.all_append] at h
    have h1 := ih (by simp [allHex]; exact h.1)
    rw [hexNum_snoc]; simp [pow_succ]
    have := h.2; omega

@[step]
theorem pkt_len_loop_spec (b : Slice U8) (v : U16) (i : Usize)
    (hb : b.length = 4) (hi : i.val ≤ 4)
    (hv : v.val = hexNum ((B b.val).take i.val)) (ha : allHex ((B b.val).take i.val) = true) :
    parse.pkt_len_loop b v i ⦃ r => r.map (·.val) =
      if allHex (B b.val) = true then some (hexNum (B b.val)) else none ⦄ := by
  unfold parse.pkt_len_loop
  apply loop.spec_decr_nat (measure := fun (x : U16 × Usize) => 4 - x.2.val)
    (inv := fun x => x.2.val ≤ 4 ∧ x.1.val = hexNum ((B b.val).take x.2.val) ∧
      allHex ((B b.val).take x.2.val) = true)
  · rintro ⟨w, j⟩ ⟨h1, h2, h3⟩
    simp only at h1 h2 h3 ⊢
    have hlen : (B b.val).length = 4 := by simp; scalar_tac
    have tk : ∀ k (hk : k < 4), (B b.val).take (k + 1) = (B b.val).take k ++ [(B b.val)[k]'(by rw [hlen]; exact hk)] := by
      intro k hk; rw [List.take_succ_eq_append_getElem (by rw [hlen]; exact hk)]
    have hlt := hexNum_lt _ h3
    have hl : ((B b.val).take j.val).length = j.val := by simp; scalar_tac
    unfold parse.pkt_len_loop.body
    by_cases hj4 : j.val < 4
    · have hj : j.val < (B b.val).length := by rw [hlen]; exact hj4
      have hpow : 16 ^ j.val ≤ 16 ^ 3 := Nat.pow_le_pow_right (by norm_num) (by omega)
      have h43 : (16:Nat) ^ 3 = 4096 := by norm_num
      have hw : w.val < 4096 := by rw [h2]; rw [hl] at hlt; omega
      step*
      · -- a non-digit at j: the whole input is not all hex
        rw [if_neg]; · simp
        intro hall
        simp only [allHex, List.all_eq_true, decide_eq_true_eq] at hall
        have := hall ((B b.val)[j.val]'hj) (List.getElem_mem _)
        simp only [B, List.getElem_map] at this
        scalar_tac
      · have hd : nib false ((B b.val)[j.val]'hj) < 16 := by simp only [B, List.getElem_map]; scalar_tac
        refine ⟨by scalar_tac, ?_, ?_, by scalar_tac⟩
        · rw [i4_post, tk j.val hj4, hexNum_snoc]
          simp only [B, List.getElem_map] at hd ⊢
          scalar_tac
        · rw [i4_post, tk j.val hj4]; simp [allHex, List.all_append] at h3 ⊢
          refine ⟨h3, ?_⟩; simp only [B, List.getElem_map] at hd; simpa using hd
    · have : j.val = 4 := by omega
      step*
      rw [this, List.take_of_length_le (by omega)] at h2 h3
      rw [if_pos h3]; simp [h2]
  · exact ⟨hi, hv, ha⟩

@[step]
theorem pkt_len_spec (b : Slice U8) :
    parse.pkt_len b ⦃ r => r.map (·.val) = pktLen (B b.val) ⦄ := by
  unfold parse.pkt_len
  step*
  all_goals first
    | (simp [hexNum]; done)
    | (simp [allHex]; done)
    | (unfold pktLen; rw [if_neg (by rintro ⟨h, _⟩; simp at h; scalar_tac)]; rfl)
    | (rw [r_post, pktLen]
       have : (B b.val).length = 4 := by simp; scalar_tac
       by_cases h : allHex (B b.val) = true <;> simp [h, this])

end pico2peta_net_kernel.Spec
