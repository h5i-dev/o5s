import DocKernel
import Interval
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Kahn layering in the extracted `materialize`

`materialize` orders the live atoms with Kahn's algorithm: the boundary rows
give each live atom its successors, `indegree` counts the remaining
predecessors, and each round (`emit`) layers the ready atoms and queues the
successors whose count drops to zero.

The proofs here are about the extracted loops and are stated over an abstract
graph `KG`: a liveness table, successor lists, and depth and blocked
predicates satisfying `KG.Ax` (the facts Kahn's algorithm relies on).
`loop2_spec` concludes that the layers are exactly the depth layers, every
unblocked live atom is layered, and the atoms left over are exactly the
blocked ones. The document instance of `KG` takes depth and blocking from
`DocModel` (`DocDepth.lean` proves the step characterisation).
-/
namespace o5s_kernel.DocKahn

/-- The graph `materialize` layers: vertices `0..n`, a liveness table, the
boundary successors of each live vertex, and the depth and blocked predicates
the layering is measured against. -/
structure KG where
  n : Nat
  live : List Bool
  bs : Nat → List Nat
  D : Nat → Nat → Prop
  B : Nat → Prop

namespace KG
variable (g : KG)

def liveF (v : Nat) : Bool := g.live[v]?.getD false
def L (v : Nat) : Prop := v < g.n ∧ g.liveF v = true
def S (u v : Nat) : Prop := g.L u ∧ v ∈ g.bs u

/-- The facts about depth and blocking that Kahn's algorithm relies on. -/
structure Ax : Prop where
  succ : ∀ u v, v ∈ g.bs u → g.L u ∧ g.L v
  nodup : ∀ u, (g.bs u).Nodup
  d0 : ∀ v, g.L v → ¬ g.B v → (g.D v 0 ↔ ∀ u, ¬ g.S u v)
  dsucc : ∀ v r, g.L v → ¬ g.B v →
    (g.D v (r+1) ↔ (∀ u, g.S u v → ∃ k ≤ r, g.D u k) ∧ ∃ u, g.S u v ∧ g.D u r)
  bpred : ∀ u v, g.S u v → ¬ g.B v → ¬ g.B u
  bunb : ∀ v, g.L v → (∀ u, g.S u v → ¬ g.B u) → ¬ g.B v
  dex : ∀ v, g.L v → ¬ g.B v → ∃ k, g.D v k
  duniq : ∀ v k m, g.D v k → g.D v m → k = m
  dbelow : ∀ v k j, g.L v → ¬ g.B v → g.D v k → j ≤ k → ∃ w, g.L w ∧ ¬ g.B w ∧ g.D w j

/-- Emitted before round `r`. -/
def Em (r v : Nat) : Prop := g.L v ∧ ¬ g.B v ∧ ∃ k < r, g.D v k

/-- Remaining predecessors of `w` under the remaining set `R`. -/
def cnt (R : Nat → Bool) (w : Nat) : Nat :=
  ((Finset.range g.n).filter (fun u => R u = true ∧ w ∈ g.bs u)).card

variable {g}

theorem pred_lt (ax : g.Ax) {u v a b : Nat} (h : g.S u v) (hv : g.L v) (hb : ¬ g.B v)
    (ha : g.D u a) (hbd : g.D v b) : a < b := by
  cases b with
  | zero => exact absurd h (((ax.d0 v hv hb).mp hbd) u)
  | succ j =>
    obtain ⟨k, hk, hd⟩ := ((ax.dsucc v j hv hb).mp hbd).1 u h
    have := ax.duniq u a k ha hd
    omega

theorem cnt_zero {R : Nat → Bool} {w : Nat} :
    g.cnt R w = 0 ↔ ∀ u < g.n, R u = true → w ∉ g.bs u := by
  unfold cnt
  rw [Finset.card_eq_zero, Finset.filter_eq_empty_iff]
  simp only [Finset.mem_range, not_and]

/-- Removing a remaining vertex `s` lowers the counts of its successors by one. -/
theorem cnt_erase {R : Nat → Bool} {s w : Nat} (hs : s < g.n) (hr : R s = true) :
    g.cnt (fun u => if u = s then false else R u) w + (if w ∈ g.bs s then 1 else 0) = g.cnt R w := by
  unfold cnt
  split_ifs with hw
  · have e : (Finset.range g.n).filter (fun u => R u = true ∧ w ∈ g.bs u) =
        insert s ((Finset.range g.n).filter
          (fun u => (if u = s then false else R u) = true ∧ w ∈ g.bs u)) := by
      ext u
      simp only [Finset.mem_filter, Finset.mem_range, Finset.mem_insert]
      by_cases hu : u = s
      · subst hu; simp [hs, hr, hw]
      · simp [hu]
    rw [e, Finset.card_insert_of_notMem (by simp)]
  · simp only [Nat.add_zero]
    congr 1
    ext u
    simp only [Finset.mem_filter, Finset.mem_range]
    by_cases hu : u = s
    · subst hu; simp [hw]
    · simp [hu]

theorem cnt_mono {R R' : Nat → Bool} (h : ∀ u, R' u = true → R u = true) (w : Nat) :
    g.cnt R' w ≤ g.cnt R w := by
  unfold cnt
  apply Finset.card_le_card
  intro u; simp only [Finset.mem_filter]
  exact fun ⟨a, b, c⟩ => ⟨a, h u b, c⟩

/-- The vertices that become ready after round `r`. -/
theorem next_iff (ax : g.Ax) {r : Nat} {R : Nat → Bool}
    (hR : ∀ v < g.n, R v = true ↔ g.L v ∧ ¬ g.Em (r+1) v)
    (ready : List Nat) (hready : ∀ v, v ∈ ready ↔ g.L v ∧ ¬ g.B v ∧ g.D v r) (w : Nat) :
    (g.cnt R w = 0 ∧ ∃ u ∈ ready, w ∈ g.bs u) ↔ g.L w ∧ ¬ g.B w ∧ g.D w (r+1) := by
  constructor
  · rintro ⟨h0, u, hu, hw⟩
    obtain ⟨hlu, hbu, hdu⟩ := (hready u).mp hu
    have hS : g.S u w := ⟨hlu, hw⟩
    have hlw := (ax.succ u w hw).2
    have hpred : ∀ u', g.S u' w → g.Em (r+1) u' := by
      intro u' hs'
      by_contra hne
      have hr' := (hR u' hs'.1.1).mpr ⟨hs'.1, hne⟩
      exact (cnt_zero.mp h0) u' hs'.1.1 hr' hs'.2
    have hbw : ¬ g.B w := ax.bunb w hlw (fun u' hs' => (hpred u' hs').2.1)
    refine ⟨hlw, hbw, (ax.dsucc w r hlw hbw).mpr ⟨?_, u, hS, hdu⟩⟩
    intro u' hs'
    obtain ⟨-, -, k, hk, hd⟩ := hpred u' hs'
    exact ⟨k, by omega, hd⟩
  · rintro ⟨hlw, hbw, hdw⟩
    obtain ⟨hall, u, hS, hdu⟩ := (ax.dsucc w r hlw hbw).mp hdw
    refine ⟨cnt_zero.mpr ?_, u, (hready u).mpr ⟨hS.1, ax.bpred u w hS hbw, hdu⟩, hS.2⟩
    intro u' hu' hr' hm
    have hs' : g.S u' w := ⟨(ax.succ u' w hm).1, hm⟩
    obtain ⟨k, hk, hd⟩ := hall u' hs'
    exact ((hR u' hu').mp hr').2 ⟨hs'.1, ax.bpred u' w hs' hbw, k, by omega, hd⟩

end KG

def valAt (l : List Usize) (w : Nat) : Nat := (l[w]?.map (·.val)).getD 0
def ValRel (v' v : List Usize) (l : List Nat) : Prop :=
  ∀ w, valAt v' w = valAt v w - if w ∈ l then 1 else 0
def NxRel (nx next : List Usize) (l : List Nat) (v : List Usize) : Prop :=
  nx.map (·.val) = next.map (·.val) ++ l.filter (fun w => valAt v w = 1)

/-! ## The outer loop of `emit` -/

def remF (l : List Bool) (u : Nat) : Bool := l[u]?.getD false

theorem remF_set (l : List Bool) (s u : Nat) :
    remF (l.set s false) u = if u = s then false else remF l u := by
  unfold remF
  rw [List.getElem?_set]
  by_cases h : s = u
  · subst h; by_cases h2 : s < l.length <;> simp [h2]
  · simp [h, Ne.symm h]

/-- The remaining set after the first `j` frontier vertices are emitted. -/
def Rj (R0 : Nat → Bool) (F : List Nat) (j : Nat) (u : Nat) : Bool :=
  if u ∈ F.take j then false else R0 u

theorem Rj_succ (R0 : Nat → Bool) (F : List Nat) {j : Nat} (hj : j < F.length) (u : Nat) :
    Rj R0 F (j+1) u = if u = F[j] then false else Rj R0 F j u := by
  unfold Rj
  rw [List.take_add_one, List.getElem?_eq_getElem hj]
  simp only [Option.toList_some, List.mem_append, List.mem_singleton]
  by_cases h1 : u = F[j] <;> by_cases h2 : u ∈ F.take j <;> simp [h1, h2]

def atomB (atoms : List (alloc.vec.Vec U8)) (u : Nat) : List U8 := (atoms[u]?.map (·.val)).getD []

namespace KG
variable (g : KG)

/-- The state of `emit`'s outer loop after `j` frontier vertices. -/
structure OInv (atoms : List (alloc.vec.Vec U8)) (R0 : Nat → Bool) (F : List Nat) (left0 : Nat)
    (bytes0 : List U8) (j : Nat) (indeg : List Usize) (rem : List Bool) (left : Nat)
    (bytes : List U8) (next : List Usize) : Prop where
  jle : j ≤ F.length
  rlen : rem.length = g.n
  rem : ∀ u, remF rem u = Rj R0 F j u
  left : left + j = left0
  bytes : bytes = bytes0 ++ (F.take j).flatMap (atomB atoms)
  ilen : indeg.length = g.n
  deg : ∀ w < g.n, valAt indeg w = g.cnt (Rj R0 F j) w
  nnd : (next.map (·.val)).Nodup
  nmem : ∀ w, w ∈ next.map (·.val) ↔ g.cnt (Rj R0 F j) w = 0 ∧ ∃ u ∈ F.take j, w ∈ g.bs u

variable {g}

theorem Rj_cur {R0 : Nat → Bool} {F : List Nat} {j : Nat} (nd : F.Nodup) (hj : j < F.length)
    (hR : R0 F[j] = true) : Rj R0 F j F[j] = true := by
  unfold Rj
  have : F[j] ∉ F.take j := by
    intro hm
    obtain ⟨i, hi, he⟩ := List.getElem_of_mem hm
    simp only [List.getElem_take, List.length_take] at he hi
    have := (List.Nodup.getElem_inj_iff nd).mp he
    omega
  simp [this, hR]

theorem cnt_pos {R : Nat → Bool} {s w : Nat} (hs : s < g.n) (hr : R s = true) (hw : w ∈ g.bs s) :
    1 ≤ g.cnt R w := by
  unfold cnt
  apply Finset.card_pos.mpr
  exact ⟨s, Finset.mem_filter.mpr ⟨Finset.mem_range.mpr hs, hr, hw⟩⟩

theorem oinv_step (ax : g.Ax) {atoms : List (alloc.vec.Vec U8)} {R0 : Nat → Bool} {F : List Nat}
    {left0 : Nat} {bytes0 : List U8} {j : Nat} {indeg : List Usize} {rem : List Bool} {left : Nat}
    {bytes : List U8} {next : List Usize}
    (h : g.OInv atoms R0 F left0 bytes0 j indeg rem left bytes next)
    (nd : F.Nodup) (hF : ∀ s ∈ F, s < g.n ∧ R0 s = true) (hj : j < F.length)
    (hleft : F.length ≤ left0) (indeg' next' : List Usize) (hl' : indeg'.length = indeg.length)
    (hv : ValRel indeg' indeg (g.bs F[j])) (hn : NxRel next' next (g.bs F[j]) indeg) :
    g.OInv atoms R0 F left0 bytes0 (j+1) indeg' (rem.set F[j] false) (left - 1)
      (bytes ++ atomB atoms F[j]) next' := by
  obtain ⟨hsn, hR0⟩ := hF _ (List.getElem_mem hj)
  have hcur := Rj_cur nd hj hR0
  have hc : ∀ w, g.cnt (Rj R0 F (j+1)) w + (if w ∈ g.bs F[j] then 1 else 0) = g.cnt (Rj R0 F j) w := by
    intro w
    have e : Rj R0 F (j+1) = fun u => if u = F[j] then false else Rj R0 F j u := funext (Rj_succ R0 F hj)
    rw [e]; exact cnt_erase hsn hcur
  refine ⟨by omega, by simp [h.rlen], ?_, ?_, ?_, by rw [hl', h.ilen], ?_, ?_, ?_⟩
  · intro u; rw [remF_set, h.rem, Rj_succ R0 F hj]
  · have := h.left; have := h.jle; omega
  · rw [h.bytes, List.take_succ_eq_append_getElem hj, List.flatMap_append]; simp
  · intro w hw
    have := hc w
    rw [hv w, h.deg w hw]
    split_ifs at this ⊢ <;> omega
  · have hwn : ∀ w ∈ g.bs F[j], w < g.n := fun w hw => (ax.succ _ _ hw).2.1
    unfold NxRel at hn
    rw [hn]
    refine List.nodup_append.mpr ⟨h.nnd, (ax.nodup _).filter _, ?_⟩
    intro a ha b hb e
    subst e
    have h0 := ((h.nmem a).mp ha).1
    obtain ⟨hb1, hb2⟩ := List.mem_filter.mp hb
    have := h.deg a (hwn a hb1)
    simp only [decide_eq_true_eq] at hb2
    omega
  · intro w
    unfold NxRel at hn
    have hwn : ∀ w ∈ g.bs F[j], w < g.n := fun w hw => (ax.succ _ _ hw).2.1
    have hcw := hc w
    have htk : ∀ u, u ∈ F.take (j+1) ↔ u ∈ F.take j ∨ u = F[j] := by
      intro u; rw [List.take_succ_eq_append_getElem hj, List.mem_append, List.mem_singleton]
    rw [hn, List.mem_append, h.nmem, List.mem_filter]
    simp only [decide_eq_true_eq]
    constructor
    · rintro (⟨h0, u, hu, hw⟩ | ⟨hw, h1⟩)
      · refine ⟨by split_ifs at hcw <;> omega, u, (htk u).mpr (.inl hu), hw⟩
      · have := h.deg w (hwn w hw)
        rw [if_pos hw] at hcw
        exact ⟨by omega, F[j], (htk _).mpr (.inr rfl), hw⟩
    · rintro ⟨h0, u, hu, hw⟩
      by_cases hws : w ∈ g.bs F[j]
      · right
        have := h.deg w (hwn w hws)
        rw [if_pos hws] at hcw
        exact ⟨hws, by omega⟩
      · left
        rw [if_neg hws] at hcw
        rcases (htk u).mp hu with hu | hu
        · exact ⟨by omega, u, hu, hw⟩
        · subst hu; exact absurd hw hws

end KG


def rowN (bv : List (alloc.vec.Vec Usize)) (s : Nat) : List Nat :=
  (bv[s]?.map fun r => r.val.map (·.val)).getD []

theorem take_succ_drop {α} (l : List α) {t k : Nat} (htk : t ≤ k) (hk : k < l.length) :
    (l.take (k+1)).drop t = (l.take k).drop t ++ [l[k]] := by
  rw [List.take_succ, List.drop_append_of_le_length (by simp; omega)]
  simp [List.getElem?_eq_getElem hk]

theorem not_mem_take_drop {α} {l : List α} {t k : Nat} (htk : t ≤ k) (hk : k < l.length)
    (nd : (l.drop t).Nodup) : l[k] ∉ (l.take k).drop t := by
  have e : l.drop t = (l.take k).drop t ++ l.drop k := by
    rw [← List.drop_append_of_le_length (by simp; omega), List.take_append_drop]
  rw [e] at nd
  intro hm
  exact (List.disjoint_of_nodup_append nd) hm (List.mem_iff_getElem.mpr ⟨0, by simp; omega, by simp⟩)

theorem valAt_set (l : List Usize) (i w : Nat) (x : Usize) :
    valAt (l.set i x) w = if i = w ∧ w < l.length then x.val else valAt l w := by
  unfold valAt
  rw [List.getElem?_set]
  by_cases h : i = w
  · subst h; by_cases h2 : i < l.length <;> simp [h2]
  · simp [h]

/-- The successors `t..k` of a row. -/
def segL (rv : List Usize) (t k : Nat) : List Nat := ((rv.map (·.val)).take k).drop t

theorem segL_succ (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) :
    segL rv t (k+1) = segL rv t k ++ [(rv[k]).val] := by
  unfold segL
  rw [take_succ_drop _ htk (by simpa using hk)]
  simp

theorem segL_not_mem (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length)
    (nd : ((rv.map (·.val)).drop t).Nodup) : (rv[k]).val ∉ segL rv t k := by
  have := not_mem_take_drop htk (by simpa using hk) nd
  simpa [segL] using this

theorem segL_full (rv : List Usize) {t k : Nat} (hk : rv.length ≤ k) :
    segL rv t k = (rv.map (·.val)).drop t := by
  unfold segL; rw [List.take_of_length_le (by simpa using hk)]

theorem segL_length (rv : List Usize) (t k : Nat) : (segL rv t k).length ≤ k - t := by
  unfold segL; simp; omega

theorem mem_drop_of (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) :
    (rv[k]).val ∈ (rv.map (·.val)).drop t :=
  List.mem_iff_getElem.mpr ⟨k - t, by simp only [List.length_drop, List.length_map]; omega,
    by simp only [List.getElem_drop, List.getElem_map]; congr 2; omega⟩

theorem w_bound (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v : List Usize)
    (hb : ∀ w ∈ (rv.map (·.val)).drop t, w < v.length ∧ 1 ≤ valAt v w) :
    (rv[k]).val < v.length ∧ 1 ≤ valAt v (rv[k]).val :=
  hb _ (mem_drop_of rv htk hk)

theorem w_cur (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : ValRel v' v (segL rv t k)) :
    valAt v' (rv[k]).val = valAt v (rv[k]).val := by
  rw [hval, if_neg (segL_not_mem rv htk hk nd), Nat.sub_zero]

theorem w_pos (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : ValRel v' v (segL rv t k))
    (hb : ∀ w ∈ (rv.map (·.val)).drop t, w < v.length ∧ 1 ≤ valAt v w)
    (h3 : (rv[k]).val < v'.length) : 1 ≤ (v'[(rv[k]).val]'h3).val := by
  have e := w_cur rv htk hk v' v nd hval
  have := (w_bound rv htk hk v hb).2
  rw [← e] at this
  simpa [valAt, List.getElem?_eq_getElem h3] using this

theorem valrel_step (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : ValRel v' v (segL rv t k))
    (h3 : (rv[k]).val < v'.length) (x : Usize) (hx : x.val = (v'[(rv[k]).val]'h3).val - 1) :
    ValRel (v'.set (rv[k]).val x) v (segL rv t (k+1)) := by
  intro w
  rw [valAt_set, segL_succ rv htk hk]
  have hnot := segL_not_mem rv htk hk nd
  have e := w_cur rv htk hk v' v nd hval
  by_cases hw : (rv[k]).val = w
  · subst hw
    have : valAt v' (rv[k]).val = (v'[(rv[k]).val]'h3).val := by
      simp [valAt, List.getElem?_eq_getElem h3]
    simp only [h3, and_self, if_true, List.mem_append, List.mem_singleton, or_true, hx]
    omega
  · have hw' : w ≠ (rv[k]).val := fun e => hw e.symm
    simp only [hw, false_and, if_false, List.mem_append, List.mem_singleton, hw', or_false]
    exact hval w

theorem nxrel_step (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length)
    (nx next v : List Usize) (h : NxRel nx next (segL rv t k) v) (b : Bool)
    (hbv : b = true ↔ valAt v (rv[k]).val = 1) :
    NxRel (if b then nx ++ [rv[k]] else nx) next (segL rv t (k+1)) v := by
  unfold NxRel at *
  rw [segL_succ rv htk hk, List.filter_append]
  cases b <;> simp_all

theorem nx_push (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v nx next : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : ValRel v' v (segL rv t k))
    (hnx : NxRel nx next (segL rv t k) v)
    (hb : ∀ w ∈ (rv.map (·.val)).drop t, w < v.length ∧ 1 ≤ valAt v w)
    (h3 : (rv[k]).val < v'.length) (x : Usize) (hx : x.val = (v'[(rv[k]).val]'h3).val - 1)
    (hz : 0#usize = x) :
    NxRel (nx ++ [rv[k]]) next (segL rv t (k+1)) v := by
  have e := w_cur rv htk hk v' v nd hval
  have hp := w_pos rv htk hk v' v nd hval hb h3
  subst hz
  have := nxrel_step rv htk hk nx next v hnx true (by
    simp only [true_iff]; rw [← e]; simp only [valAt, List.getElem?_eq_getElem h3, Option.map_some,
      Option.getD_some]; simp at hx; omega)
  simpa using this

theorem nx_skip (rv : List Usize) {t k : Nat} (htk : t ≤ k) (hk : k < rv.length) (v' v nx next : List Usize)
    (nd : ((rv.map (·.val)).drop t).Nodup) (hval : ValRel v' v (segL rv t k))
    (hnx : NxRel nx next (segL rv t k) v)
    (hb : ∀ w ∈ (rv.map (·.val)).drop t, w < v.length ∧ 1 ≤ valAt v w)
    (h3 : (rv[k]).val < v'.length) (hz : ¬ (v'[(rv[k]).val]'h3).val - 1 = 0) :
    NxRel nx next (segL rv t (k+1)) v := by
  have e := w_cur rv htk hk v' v nd hval
  have hp := w_pos rv htk hk v' v nd hval hb h3
  have := nxrel_step rv htk hk nx next v hnx false (by
    simp only [Bool.false_eq_true, false_iff]; rw [← e]; simp only [valAt, List.getElem?_eq_getElem h3,
      Option.map_some, Option.getD_some]; omega)
  simpa using this

/-- The inner loop of `emit`: one decrement per successor of `s`, and the
successors whose count reaches zero are queued in order. -/
theorem emit_inner_spec (bv : alloc.vec.Vec (alloc.vec.Vec Usize)) (s : Usize)
    (v next : alloc.vec.Vec Usize) (t : Usize) (hs : s.val < bv.val.length)
    (ht : t.val ≤ (bv.val[s.val]).val.length)
    (nd : (((bv.val[s.val]).val.map (·.val)).drop t.val).Nodup)
    (hb : ∀ w ∈ ((bv.val[s.val]).val.map (·.val)).drop t.val, w < v.val.length ∧ 1 ≤ valAt v.val w)
    (room : next.val.length + (bv.val[s.val]).val.length ≤ Usize.max) :
    document.emit_loop0_loop0 v bv next s t ⦃ out =>
      out.1.val.length = v.val.length ∧
      ValRel out.1.val v.val (((bv.val[s.val]).val.map (·.val)).drop t.val) ∧
      NxRel out.2.val next.val (((bv.val[s.val]).val.map (·.val)).drop t.val) v.val ⦄ := by
  unfold document.emit_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Usize × alloc.vec.Vec Usize × Usize =>
      (bv.val[s.val]).val.length - st.2.2.val)
    (inv := fun st => t.val ≤ st.2.2.val ∧ st.2.2.val ≤ (bv.val[s.val]).val.length ∧
      st.1.val.length = v.val.length ∧
      ValRel st.1.val v.val (segL (bv.val[s.val]).val t.val st.2.2.val) ∧
      NxRel st.2.1.val next.val (segL (bv.val[s.val]).val t.val st.2.2.val) v.val)
  · rintro ⟨v', nx, k⟩ ⟨htk, hkl, hlen, hval, hnx⟩
    simp only at htk hkl hlen hval hnx
    have hnxl : nx.val.length ≤ next.val.length + (k.val - t.val) := by
      have := congrArg List.length hnx
      simp only [List.length_map, List.length_append] at this
      have h2 := segL_length (bv.val[s.val]).val t.val k.val
      have h3 := List.length_filter_le (fun w => decide (valAt v.val w = 1)) (segL (bv.val[s.val]).val t.val k.val)
      omega
    unfold document.emit_loop0_loop0.body
    h5i_step
    all_goals first
      | exact (w_bound _ htk (by scalar_tac) _ hb).1
      | (rw [← hlen]; exact (w_bound _ htk (by scalar_tac) _ hb).1)
      | exact w_pos _ htk (by scalar_tac) _ _ nd hval hb _
      | (simp only [List.length_set] at *; scalar_tac)
      | (rw [segL_full _ (by scalar_tac)] at hval hnx; exact ⟨hval, hnx⟩)
      | (subst v1_post v2_post i1_post
         have hk' : k.val < (bv.val[s.val]).val.length := by scalar_tac
         have h3 : ((bv.val[s.val]).val[k.val]).val < v'.val.length := by
           rw [hlen]; exact (w_bound _ htk hk' _ hb).1
         exact ⟨by omega, hk', valrel_step _ htk hk' _ _ nd hval h3 _ i2_post,
           nx_push _ htk hk' _ _ _ _ nd hval hnx hb h3 _ i2_post i3_post, by scalar_tac⟩)
      | (subst v1_post v2_post i1_post
         have hk' : k.val < (bv.val[s.val]).val.length := by scalar_tac
         have h3 : ((bv.val[s.val]).val[k.val]).val < v'.val.length := by
           rw [hlen]; exact (w_bound _ htk hk' _ hb).1
         exact ⟨by omega, hk', valrel_step _ htk hk' _ _ nd hval h3 _ i2_post,
           nx_skip _ htk hk' _ _ _ _ nd hval hnx hb h3 (by assumption), by scalar_tac⟩)
  · refine ⟨le_refl _, ht, rfl, ?_, ?_⟩
    · intro w; simp [segL]
    · simp [NxRel, segL]


/-! ## `emit` -/

theorem len_le_of_nodup {l : List Nat} {n : Nat} (nd : l.Nodup) (h : ∀ x ∈ l, x < n) : l.length ≤ n := by
  rw [← List.toFinset_card_of_nodup nd]
  have : l.toFinset ⊆ Finset.range n := fun x hx => Finset.mem_range.mpr (h x (List.mem_toFinset.mp hx))
  simpa using Finset.card_le_card this

theorem sum_take_le (l : List Nat) (j : Nat) : (l.take j).sum ≤ l.sum := by
  conv_rhs => rw [← List.take_append_drop j l]
  rw [List.sum_append]; omega

theorem flat_len (atoms : List (alloc.vec.Vec U8)) (F : List Nat) {j : Nat} (hj : j < F.length) :
    ((F.take j).flatMap (atomB atoms)).length + (atomB atoms F[j]).length ≤
      (F.map fun u => (atomB atoms u).length).sum := by
  have h := sum_take_le (F.map fun u => (atomB atoms u).length) (j+1)
  rw [List.take_succ_eq_append_getElem (by simpa using hj), List.sum_append] at h
  simp only [List.getElem_map, List.sum_singleton] at h
  rw [List.length_flatMap, List.map_take]
  exact h

theorem atomB_eq (atoms : List (alloc.vec.Vec U8)) {s : Nat} (h : s < atoms.length) :
    atomB atoms s = (atoms[s]).val := by
  simp [atomB, List.getElem?_eq_getElem h]

namespace KG
variable {g : KG}

/-- The rows of `boundaries` are the successor lists. -/
def Rows (g : KG) (bv : List (alloc.vec.Vec Usize)) : Prop :=
  bv.length = g.n ∧ ∀ u (h : u < bv.length), g.bs u = (bv[u]).val.map (·.val)

theorem emit_outer_spec (ax : g.Ax) (doc : document.Doc) (bv : alloc.vec.Vec (alloc.vec.Vec Usize))
    (frontier : alloc.vec.Vec Usize) (R0 : Nat → Bool) (left0 : Nat) (bytes0 : List U8)
    (hrows : g.Rows bv.val) (hal : doc.atoms.val.length = g.n) (hnm : g.n < Usize.max)
    (nd : (frontier.val.map (·.val)).Nodup)
    (hF : ∀ s ∈ frontier.val.map (·.val), s < g.n ∧ R0 s = true)
    (hleft : frontier.val.length ≤ left0)
    (hroom : bytes0.length + ((frontier.val.map (·.val)).map (fun u => (atomB doc.atoms.val u).length)).sum
      ≤ Usize.max)
    (v : alloc.vec.Vec Usize) (v1 : alloc.vec.Vec Bool) (i : Usize) (v2 : alloc.vec.Vec U8)
    (next : alloc.vec.Vec Usize) (j : Usize)
    (h : g.OInv doc.atoms.val R0 (frontier.val.map (·.val)) left0 bytes0 j.val v.val v1.val i.val
      v2.val next.val) :
    document.emit_loop0 v v1 i v2 doc bv frontier next j ⦃ out =>
      g.OInv doc.atoms.val R0 (frontier.val.map (·.val)) left0 bytes0 frontier.val.length
        out.1.val out.2.1.val out.2.2.1.val out.2.2.2.1.val out.2.2.2.2.val ⦄ := by
  have hFl : (frontier.val.map (·.val)).length = frontier.val.length := by simp
  unfold document.emit_loop0
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Usize × alloc.vec.Vec Bool × Usize × alloc.vec.Vec U8 ×
      alloc.vec.Vec Usize × Usize => frontier.val.length - st.2.2.2.2.2.val)
    (inv := fun st => g.OInv doc.atoms.val R0 (frontier.val.map (·.val)) left0 bytes0 st.2.2.2.2.2.val
      st.1.val st.2.1.val st.2.2.1.val st.2.2.2.1.val st.2.2.2.2.1.val)
  · rintro ⟨w, w1, k, w2, nx, j'⟩ hI
    simp only at hI
    have hjl := hI.jle
    rw [hFl] at hjl
    -- facts about the current frontier vertex
    have hs : ∀ (hj : j'.val < frontier.val.length),
        (frontier.val[j'.val]).val < g.n ∧ R0 (frontier.val[j'.val]).val = true := by
      intro hj; have := hF _ (List.mem_map.mpr ⟨_, List.getElem_mem hj, rfl⟩); exact this
    have hbsl : ∀ u (hu : u < g.n), (g.bs u).length ≤ g.n := fun u hu =>
      len_le_of_nodup (ax.nodup u) (fun x hx => (ax.succ u x hx).2.1)
    have hnxl : nx.val.length ≤ g.n := by
      have := len_le_of_nodup hI.nnd (fun x hx => by
        obtain ⟨-, u, -, hu⟩ := (hI.nmem x).mp hx
        exact (ax.succ u x hu).2.1)
      simpa using this
    have hR : ∀ (hj : j'.val < frontier.val.length), Rj R0 (frontier.val.map (·.val)) j'.val
        (frontier.val[j'.val]).val = true := by
      intro hj
      have := Rj_cur nd (by simpa using hj) (by simpa using (hs hj).2)
      simpa using this

    have hroomj : ∀ (hj : j'.val < frontier.val.length),
        w2.val.length + (atomB doc.atoms.val (frontier.val[j'.val]).val).length ≤ Usize.max := by
      intro hj
      have := flat_len doc.atoms.val (frontier.val.map (·.val)) (j := j'.val) (by simpa using hj)
      rw [hI.bytes, List.length_append]
      simp only [List.getElem_map] at this
      omega
    unfold document.emit_loop0.body
    h5i_step
    all_goals first
      | (rw [hI.rlen]; exact (hF _ (List.getElem_mem _)).1)
      | (rw [hal]; exact (hF _ (List.getElem_mem _)).1)
      | (have := hI.left; scalar_tac)
      | (have h1 := hroomj (by scalar_tac)
         rw [atomB_eq _ (by rw [hal]; exact (hs (by scalar_tac)).1)] at h1
         simpa using h1)
      | (have hj : j'.val < frontier.val.length := by scalar_tac
         have hsn : (frontier.val[j'.val]).val < g.n ∧ R0 (frontier.val[j'.val]).val = true := by
           first | exact hF _ (List.getElem_mem _) | exact hF _ (List.mem_map.mpr ⟨_, List.getElem_mem _, rfl⟩)
         have hbl : (frontier.val[j'.val]).val < bv.val.length := by rw [hrows.1]; exact hsn.1
         have hrw := hrows.2 _ hbl
         apply WP.spec_bind (emit_inner_spec bv (frontier.val[j'.val]) w nx 0#usize hbl (by simp)
           (by rw [show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, ← hrw]; exact ax.nodup _)
           (by
             intro x hx
             rw [show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, ← hrw] at hx
             have hxn := (ax.succ _ _ hx).2.1
             refine ⟨by rw [hI.ilen]; exact hxn, ?_⟩
             rw [hI.deg x hxn]
             exact cnt_pos hsn.1 (hR hj) hx)
           (by
             have hd : ((nx.val.map (·.val)) ++ g.bs (frontier.val[j'.val]).val).Nodup :=
               List.nodup_append.mpr ⟨hI.nnd, ax.nodup _, fun a ha b hb e => by
                 subst e
                 have h0 := ((hI.nmem a).mp ha).1
                 have := cnt_pos hsn.1 (hR hj) hb
                 omega⟩
             have := len_le_of_nodup hd (fun x hx => by
               rcases List.mem_append.mp hx with h | h
               · obtain ⟨-, u, -, hu⟩ := (hI.nmem x).mp h; exact (ax.succ u x hu).2.1
               · exact (ax.succ _ x h).2.1)
             rw [hrw] at this; simp at this; omega))
         rintro ⟨v6, next1⟩ ⟨hl6, hv6, hn6⟩
         rw [show ((0#usize : Usize).val) = 0 from rfl, List.drop_zero, ← hrw] at hv6 hn6
         have hF2 : ∀ s ∈ frontier.val.map (·.val), s < g.n ∧ R0 s = true := by
           first | exact hF | (intro s hs; obtain ⟨a, ha, rfl⟩ := List.mem_map.mp hs; exact hF a ha)
         have hst := oinv_step ax hI nd hF2 (by simpa using hj) (by simpa using hleft) v6.val next1.val hl6
           (by simpa using hv6) (by simpa using hn6)
         simp only [List.getElem_map] at hst
         have hsn' : (frontier.val[j'.val]).val < doc.atoms.val.length := by rw [hal]; exact hsn.1
         rw [atomB_eq _ hsn'] at hst
         h5i_step
         all_goals first | exact ⟨hst, by scalar_tac⟩ | (refine ⟨?_, by scalar_tac⟩; simpa using hst))
  · first | exact h | simpa using h

end KG

/-! ## The Kahn state between rounds -/

namespace KG
variable (g : KG)

/-- Live and already emitted (no longer remaining). -/
def EmF (rem : List Bool) (v : Nat) : Bool := g.liveF v && !remF rem v

/-- The Kahn state after `r` rounds. -/
structure Inv (atoms : List (alloc.vec.Vec U8)) (r : Nat) (k : document.Kahn) : Prop where
  rlen : k.remaining.val.length = g.n
  rem : ∀ v < g.n, remF k.remaining.val v = true ↔ g.L v ∧ ¬ g.Em r v
  ilen : k.indegree.val.length = g.n
  deg : ∀ w < g.n, valAt k.indegree.val w = g.cnt (remF k.remaining.val) w
  left : k.left.val = ((Finset.range g.n).filter (fun u => remF k.remaining.val u = true)).card
  llen : k.layers.val.length = r
  lay : ∀ j (h : j < k.layers.val.length) v,
    v ∈ (k.layers.val[j]).val.map (·.val) ↔ g.L v ∧ ¬ g.B v ∧ g.D v j
  lnd : ∀ L ∈ k.layers.val, (L.val.map (·.val)).Nodup ∧ L.val ≠ []
  ready : ∀ v, v ∈ k.ready.val.map (·.val) ↔ g.L v ∧ ¬ g.B v ∧ g.D v r
  rnd : (k.ready.val.map (·.val)).Nodup
  bytes : k.bytes.val = (k.layers.val.flatMap (fun L => L.val.map (·.val))).flatMap (atomB atoms)
  blen : k.bytes.val.length =
    ∑ v ∈ (Finset.range g.n).filter (fun v => g.EmF k.remaining.val v = true), (atomB atoms v).length
  rle : r ≤ ((Finset.range g.n).filter (fun v => g.EmF k.remaining.val v = true)).card
  blk : k.blocked = true → k.ready.val = [] ∧ 0 < k.left.val

variable {g}

theorem remF_out {l : List Bool} {v : Nat} (h : l.length ≤ v) : remF l v = false := by
  simp [remF, List.getElem?_eq_none_iff.mpr h]

/-- A vertex of the current round is still remaining. -/
theorem ready_rem (ax : g.Ax) {atoms} {r : Nat} {k : document.Kahn} (hI : g.Inv atoms r k) {v : Nat}
    (hv : v ∈ k.ready.val.map (·.val)) : v < g.n ∧ remF k.remaining.val v = true := by
  obtain ⟨hl, hb, hd⟩ := (hI.ready v).mp hv
  refine ⟨hl.1, (hI.rem v hl.1).mpr ⟨hl, ?_⟩⟩
  rintro ⟨-, -, j, hj, hdj⟩
  have := ax.duniq v r j hd hdj
  omega

theorem Rj_full (R0 : Nat → Bool) (F : List Nat) (u : Nat) :
    Rj R0 F F.length u = if u ∈ F then false else R0 u := by
  simp [Rj, List.take_length]

theorem em_succ {r v : Nat} : g.Em (r+1) v ↔ g.Em r v ∨ (g.L v ∧ ¬ g.B v ∧ g.D v r) := by
  constructor
  · rintro ⟨hl, hb, j, hj, hd⟩
    by_cases hjr : j < r
    · exact .inl ⟨hl, hb, j, hjr, hd⟩
    · have : j = r := by omega
      subst this; exact .inr ⟨hl, hb, hd⟩
  · rintro (⟨hl, hb, j, hj, hd⟩ | ⟨hl, hb, hd⟩)
    · exact ⟨hl, hb, j, by omega, hd⟩
    · exact ⟨hl, hb, r, by omega, hd⟩

/-- One round of `emit`, at the level of the invariant. -/
theorem inv_emit (ax : g.Ax) {atoms : List (alloc.vec.Vec U8)} {r : Nat} {k : document.Kahn}
    (hI : g.Inv atoms r k) (hne : k.ready.val ≠ []) (hbk : k.blocked = false)
    (v : alloc.vec.Vec Usize) (v1 : alloc.vec.Vec Bool) (i : Usize) (v2 : alloc.vec.Vec U8)
    (next : alloc.vec.Vec Usize) (v3 : alloc.vec.Vec (alloc.vec.Vec Usize))
    (hO : g.OInv atoms (remF k.remaining.val) (k.ready.val.map (·.val)) k.left.val k.bytes.val
      (k.ready.val.map (·.val)).length v.val v1.val i.val v2.val next.val)
    (hv3 : v3.val = k.layers.val ++ [k.ready]) :
    g.Inv atoms (r+1) { k with indegree := v, remaining := v1, left := i, ready := next, layers := v3, bytes := v2 } ∧ i.val < k.left.val := by
  set F := k.ready.val.map (·.val) with hF
  have hFl : 0 < F.length := by
    rw [hF, List.length_map]; exact List.length_pos_iff.mpr hne
  have hFr : ∀ s ∈ F, s < g.n ∧ remF k.remaining.val s = true := fun s hs => ready_rem ax hI hs
  have hrem : ∀ u, remF v1.val u = if u ∈ F then false else remF k.remaining.val u := by
    intro u; rw [hO.rem, Rj_full]
  have hremv : ∀ u < g.n, remF v1.val u = true ↔ g.L u ∧ ¬ g.Em (r+1) u := by
    intro u hu
    rw [hrem, em_succ]
    by_cases huF : u ∈ F
    · simp only [huF, if_true, Bool.false_eq_true, false_iff, not_and, not_not]
      intro _; exact .inr ((hI.ready u).mp huF)
    · simp only [huF, if_false]
      rw [hI.rem u hu]
      constructor
      · rintro ⟨hl, hn⟩; refine ⟨hl, ?_⟩
        rintro (h | h)
        · exact hn h
        · exact huF ((hI.ready u).mpr h)
      · rintro ⟨hl, hn⟩; exact ⟨hl, fun h => hn (.inl h)⟩
  have hRj : Rj (remF k.remaining.val) F F.length = remF v1.val := by
    funext u; rw [hO.rem]
  -- finset bookkeeping
  have hsub : F.toFinset ⊆ (Finset.range g.n).filter (fun u => remF k.remaining.val u = true) := by
    intro s hs
    have := hFr s (List.mem_toFinset.mp hs)
    exact Finset.mem_filter.mpr ⟨Finset.mem_range.mpr this.1, this.2⟩
  have hcardF : F.toFinset.card = F.length := List.toFinset_card_of_nodup hI.rnd
  have hfilt : (Finset.range g.n).filter (fun u => remF v1.val u = true) =
      (Finset.range g.n).filter (fun u => remF k.remaining.val u = true) \ F.toFinset := by
    ext u
    simp only [Finset.mem_filter, Finset.mem_sdiff, Finset.mem_range, List.mem_toFinset, hrem]
    by_cases huF : u ∈ F <;> simp [huF]
  have hEm : (Finset.range g.n).filter (fun u => g.EmF v1.val u = true) =
      (Finset.range g.n).filter (fun u => g.EmF k.remaining.val u = true) ∪ F.toFinset := by
    ext u
    simp only [Finset.mem_filter, Finset.mem_union, Finset.mem_range, List.mem_toFinset, EmF, hrem,
      Bool.and_eq_true, Bool.not_eq_true']
    by_cases huF : u ∈ F
    · have := (hI.ready u).mp huF
      simp [huF, this.1.1, this.1.2]
    · simp [huF]
  have hdisj : Disjoint ((Finset.range g.n).filter (fun u => g.EmF k.remaining.val u = true)) F.toFinset := by
    rw [Finset.disjoint_left]
    intro u hu huF
    have h1 := (Finset.mem_filter.mp hu).2
    have h2 := (hFr u (List.mem_toFinset.mp huF)).2
    simp [EmF, h2] at h1
  have hleft := hO.left
  have hcard := Finset.card_sdiff_of_subset hsub
  refine ⟨⟨hO.rlen, hremv, hO.ilen, ?_, ?_, ?_, ?_, ?_, ?_, hO.nnd, ?_, ?_, ?_, ?_⟩, by omega⟩
  · intro w hw; rw [hO.deg w hw, hRj]
  · show i.val = _
    rw [hfilt, hcard, hcardF, ← hI.left]; omega
  · show v3.val.length = r + 1
    rw [hv3, List.length_append, hI.llen]; rfl
  · intro j hj u
    show u ∈ (v3.val[j]).val.map (·.val) ↔ _
    have hj' : j < k.layers.val.length + 1 := by
      have : v3.val.length = k.layers.val.length + 1 := by rw [hv3]; simp
      simpa [this] using hj
    simp only [hv3]
    by_cases hjr : j < k.layers.val.length
    · rw [List.getElem_append_left hjr]; exact hI.lay j hjr u
    · have : j = k.layers.val.length := by omega
      subst this
      rw [List.getElem_append_right (le_refl _)]
      simp only [Nat.sub_self, List.getElem_cons_zero]
      rw [hI.ready u, hI.llen]
  · intro L hL
    rw [show v3.val = _ from hv3, List.mem_append, List.mem_singleton] at hL
    rcases hL with hL | hL
    · exact hI.lnd L hL
    · subst hL; exact ⟨hI.rnd, hne⟩
  · intro w
    show w ∈ next.val.map (·.val) ↔ _
    rw [hO.nmem, List.take_length]
    rw [hRj]
    exact next_iff ax hremv F hI.ready w
  · show v2.val = _
    rw [hO.bytes, List.take_length, hv3, List.flatMap_append, List.flatMap_append, hI.bytes]
    simp [F]
  · show v2.val.length = _
    rw [hEm, Finset.sum_union hdisj, hO.bytes, List.take_length, List.length_append, hI.blen,
      List.length_flatMap, List.sum_toFinset _ hI.rnd]
  · show r + 1 ≤ _
    rw [hEm, Finset.card_union_of_disjoint hdisj, hcardF]
    have := hI.rle; omega
  · intro h; simp [hbk] at h

/-- The result of the Kahn loop: the layers are the depth layers, every
unblocked live vertex is layered, the vertices left over are the blocked live
ones, and the flag reports whether any exist. -/
structure Final (atoms : List (alloc.vec.Vec U8)) (rest : List Bool)
    (layers : List (alloc.vec.Vec Usize)) (bytes : List U8) (b : Bool) : Prop where
  lay : ∀ j (h : j < layers.length) v, v ∈ (layers[j]).val.map (·.val) ↔ g.L v ∧ ¬ g.B v ∧ g.D v j
  lnd : ∀ L ∈ layers, (L.val.map (·.val)).Nodup ∧ L.val ≠ []
  all : ∀ v, g.L v → ¬ g.B v → ∃ j < layers.length, g.D v j
  rem : ∀ v < g.n, remF rest v = true ↔ g.L v ∧ g.B v
  blocked : b = true ↔ ∃ v, g.L v ∧ g.B v
  bytes : bytes = (layers.flatMap (fun L => L.val.map (·.val))).flatMap (atomB atoms)
  rlen : rest.length = g.n

theorem final_of (ax : g.Ax) {atoms} {r : Nat} {k : document.Kahn} (hI : g.Inv atoms r k)
    (hready : k.ready.val = []) :
    (∀ v, g.L v → ¬ g.B v → ∃ j < r, g.D v j) ∧
    (∀ v < g.n, remF k.remaining.val v = true ↔ g.L v ∧ g.B v) := by
  have hall : ∀ v, g.L v → ¬ g.B v → ∃ j < r, g.D v j := by
    intro v hl hb
    obtain ⟨d, hd⟩ := ax.dex v hl hb
    by_cases hdr : d < r
    · exact ⟨d, hdr, hd⟩
    · obtain ⟨w, hlw, hbw, hdw⟩ := ax.dbelow v d r hl hb hd (by omega)
      have := (hI.ready w).mpr ⟨hlw, hbw, hdw⟩
      rw [hready] at this; simp at this
  refine ⟨hall, fun v hv => ?_⟩
  rw [hI.rem v hv]
  constructor
  · rintro ⟨hl, hn⟩
    refine ⟨hl, ?_⟩
    by_contra hb
    obtain ⟨j, hj, hd⟩ := hall v hl hb
    exact hn ⟨hl, hb, j, hj, hd⟩
  · rintro ⟨hl, hb⟩
    exact ⟨hl, fun h => h.2.1 hb⟩

theorem final_done (ax : g.Ax) {atoms} {r : Nat} {k : document.Kahn} (hI : g.Inv atoms r k)
    (hdone : k.left.val = 0 ∨ k.blocked = true) :
    g.Final atoms k.remaining.val k.layers.val k.bytes.val (decide (0 < k.left.val) && k.blocked) := by
  have hready : k.ready.val = [] := by
    rcases hdone with h0 | hb
    · rcases hr : k.ready.val with _ | ⟨x, xs⟩
      · rfl
      · have hm : x.val ∈ k.ready.val.map (·.val) := by rw [hr]; simp
        obtain ⟨hn, hrem⟩ := ready_rem ax hI hm
        have : x.val ∈ (Finset.range g.n).filter (fun u => remF k.remaining.val u = true) :=
          Finset.mem_filter.mpr ⟨Finset.mem_range.mpr hn, hrem⟩
        have := Finset.card_pos.mpr ⟨_, this⟩
        rw [← hI.left] at this; omega
    · exact (hI.blk hb).1
  obtain ⟨hall, hrem⟩ := final_of ax hI hready
  refine ⟨hI.lay, hI.lnd, ?_, hrem, ?_, hI.bytes, hI.rlen⟩
  · intro v hl hb; obtain ⟨j, hj, hd⟩ := hall v hl hb; exact ⟨j, by rw [hI.llen]; exact hj, hd⟩
  · constructor
    · intro h
      simp only [Bool.and_eq_true, decide_eq_true_eq] at h
      have : 0 < ((Finset.range g.n).filter (fun u => remF k.remaining.val u = true)).card := by
        rw [← hI.left]; exact h.1
      obtain ⟨v, hv⟩ := Finset.card_pos.mp this
      obtain ⟨hv1, hv2⟩ := Finset.mem_filter.mp hv
      exact ⟨v, (hrem v (Finset.mem_range.mp hv1)).mp hv2⟩
    · rintro ⟨v, hl, hb⟩
      have hr := (hrem v hl.1).mpr ⟨hl, hb⟩
      have hpos : 0 < ((Finset.range g.n).filter (fun u => remF k.remaining.val u = true)).card :=
        Finset.card_pos.mpr ⟨v, Finset.mem_filter.mpr ⟨Finset.mem_range.mpr hl.1, hr⟩⟩
      rw [← hI.left] at hpos
      rcases hdone with h0 | hbk
      · omega
      · simp [hbk, hpos]

theorem emit_spec (ax : g.Ax) (doc : document.Doc) (bv : alloc.vec.Vec (alloc.vec.Vec Usize))
    (hrows : g.Rows bv.val) (hal : doc.atoms.val.length = g.n) (hnm : g.n < Usize.max)
    (roomT : ∑ v ∈ Finset.range g.n, (atomB doc.atoms.val v).length ≤ Usize.max)
    {r : Nat} (k : document.Kahn) (hI : g.Inv doc.atoms.val r k) (hne : k.ready.val ≠ [])
    (hbk : k.blocked = false) :
    document.emit k doc bv ⦃ k' =>
      g.Inv doc.atoms.val (r+1) k' ∧ k'.left.val < k.left.val ∧ k'.blocked = false ⦄ := by
  set F := k.ready.val.map (·.val) with hF
  have hFr : ∀ s ∈ F, s < g.n ∧ remF k.remaining.val s = true := fun s hs => ready_rem ax hI hs
  have hsub : F.toFinset ⊆ (Finset.range g.n).filter (fun u => remF k.remaining.val u = true) := by
    intro s hs
    have := hFr s (List.mem_toFinset.mp hs)
    exact Finset.mem_filter.mpr ⟨Finset.mem_range.mpr this.1, this.2⟩
  have hleft : k.ready.val.length ≤ k.left.val := by
    have := Finset.card_le_card hsub
    rw [List.toFinset_card_of_nodup hI.rnd, ← hI.left] at this
    simpa [F] using this
  have hdisj : Disjoint ((Finset.range g.n).filter (fun u => g.EmF k.remaining.val u = true)) F.toFinset := by
    rw [Finset.disjoint_left]
    intro u hu huF
    have h1 := (Finset.mem_filter.mp hu).2
    have h2 := (hFr u (List.mem_toFinset.mp huF)).2
    simp [EmF, h2] at h1
  have hroom : k.bytes.val.length + (F.map fun u => (atomB doc.atoms.val u).length).sum ≤ Usize.max := by
    rw [hI.blen, ← List.sum_toFinset _ hI.rnd, ← Finset.sum_union hdisj]
    refine le_trans (Finset.sum_le_sum_of_subset ?_) roomT
    intro u hu
    rcases Finset.mem_union.mp hu with h | h
    · exact (Finset.mem_filter.mp h).1
    · exact Finset.mem_range.mpr (hFr u (List.mem_toFinset.mp h)).1
  have h0 : g.OInv doc.atoms.val (remF k.remaining.val) F k.left.val k.bytes.val 0 k.indegree.val
      k.remaining.val k.left.val k.bytes.val (alloc.vec.Vec.new Usize).val := by
    refine ⟨by simp, hI.rlen, ?_, by simp, by simp, hI.ilen, ?_, by simp, ?_⟩
    · intro u; simp [Rj]
    · intro w hw; rw [hI.deg w hw]; congr 1
      try (funext u; simp [Rj])
    · intro w; simp
  have hlay : k.layers.val.length < Usize.max := by
    have h1 := hI.rle
    have h2 := Finset.card_filter_le (Finset.range g.n) (fun v => g.EmF k.remaining.val v = true)
    rw [hI.llen]; simp only [Finset.card_range] at h2; omega
  unfold document.emit
  apply WP.spec_bind (emit_outer_spec ax doc bv k.ready (remF k.remaining.val) k.left.val k.bytes.val
    hrows hal hnm hI.rnd hFr hleft hroom k.indegree k.remaining k.left k.bytes (alloc.vec.Vec.new Usize)
    0#usize h0)
  rintro ⟨v, v1, i, v2, next⟩ hO
  simp only at hO
  h5i_step
  all_goals first
    | exact hlay
    | (obtain ⟨hinv, hlt⟩ := inv_emit ax hI hne hbk v v1 i v2 next _ (by simpa using hO) (by assumption)
       refine ⟨by simpa [hbk] using hinv, ?_⟩
       first | exact ⟨hlt, hbk⟩ | omega | simp_all | (constructor <;> simp_all))

theorem inv_block {atoms} {r : Nat} {k : document.Kahn} (hI : g.Inv atoms r k) (hr : k.ready.val = [])
    (hl : 0 < k.left.val) : g.Inv atoms r { k with blocked := true } :=
  ⟨hI.rlen, hI.rem, hI.ilen, hI.deg, hI.left, hI.llen, hI.lay, hI.lnd, hI.ready, hI.rnd, hI.bytes,
    hI.blen, hI.rle, fun _ => ⟨hr, hl⟩⟩

/-- The Kahn loop of `materialize` computes the depth layers. -/
theorem loop2_spec (ax : g.Ax) (doc : document.Doc) (bv : alloc.vec.Vec (alloc.vec.Vec Usize))
    (hrows : g.Rows bv.val) (hal : doc.atoms.val.length = g.n) (hnm : g.n < Usize.max)
    (roomT : ∑ v ∈ Finset.range g.n, (atomB doc.atoms.val v).length ≤ Usize.max)
    {r : Nat} (k : document.Kahn) (hI : g.Inv doc.atoms.val r k) :
    document.materialize_loop2 doc.deps doc.creator doc.atoms doc.edges doc.insertions doc.deletions
      doc.restored doc.acked doc.orders doc.retractions bv k ⦃ out =>
      g.Final doc.atoms.val out.1.val out.2.1.val out.2.2.1.val out.2.2.2 ⦄ := by
  unfold document.materialize_loop2
  apply loop.spec_decr_nat
    (measure := fun k : document.Kahn => 2 * k.left.val + (if k.blocked then 0 else 1))
    (inv := fun k => ∃ r, g.Inv doc.atoms.val r k)
  · rintro k ⟨r, hI⟩
    unfold document.materialize_loop2.body
    have hdoc : (document.Doc.mk doc.deps doc.creator doc.atoms doc.edges doc.insertions doc.deletions doc.restored doc.acked doc.orders doc.retractions) = doc := rfl
    by_cases h1 : 0#usize < k.left
    · have hl : 0 < k.left.val := by scalar_tac
      rw [if_pos h1]
      by_cases h2 : k.blocked = true
      · rw [if_pos h2]
        simp only [WP.spec_ok]
        have := final_done ax hI (.inr h2)
        simpa [h2, hl] using this
      · rw [if_neg h2]
        have hbk : k.blocked = false := by simpa using h2
        by_cases h3 : k.ready.len = 0#usize
        · rw [if_pos h3]
          simp only [WP.spec_ok]
          have hr : k.ready.val = [] := by
            apply List.eq_nil_of_length_eq_zero
            have := congrArg UScalar.val h3
            simpa using this
          refine ⟨⟨r, inv_block hI hr hl⟩, ?_⟩
          simp [hbk]
        · rw [if_neg h3, hdoc]
          have hne : k.ready.val ≠ [] := by
            intro h; apply h3
            apply UScalar.eq_of_val_eq
            simp [h]
          apply WP.spec_bind (emit_spec ax doc bv hrows hal hnm roomT k hI hne hbk)
          rintro k1 ⟨hI1, hlt, hb1⟩
          simp only [WP.spec_ok]
          refine ⟨⟨r+1, hI1⟩, ?_⟩
          simp [hb1, hbk]; omega
    · rw [if_neg h1]
      simp only [WP.spec_ok]
      have hl0 : k.left.val = 0 := by scalar_tac
      have := final_done ax hI (.inl hl0)
      have hbk : k.blocked = false := by
        cases hb : k.blocked
        · rfl
        · have := (hI.blk hb).2; omega
      simpa [hl0, hbk] using this
  · exact ⟨r, hI⟩

end KG
end o5s_kernel.DocKahn
