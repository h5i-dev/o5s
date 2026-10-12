import DocModel
open o5s_kernel
/-!
# Kahn depth, one step at a time

The recursive characterisation of `Depth` that Kahn's algorithm follows: an
unblocked live atom has depth `0` exactly when nothing precedes it, and depth
`r + 1` exactly when every predecessor has depth at most `r` and one has depth
`r`. Blocking is inherited along `Sum` and is absent when every predecessor is
unblocked.
-/
namespace o5s_kernel.DocDepth
open DocModel

variable {H : Hist}

theorem pred_unblocked {u v : Anchor} (h : Sum H u v) (hb : ¬ Blocked H v) : ¬ Blocked H u :=
  fun hu => hb (blocked_of_reach hu (Relation.ReflTransGen.single h))

theorem unblocked_of_preds {v : Anchor} (h : ∀ u, Sum H u v → ¬ Blocked H u) : ¬ Blocked H v := by
  rintro ⟨c, hc, hcv⟩
  rcases Relation.ReflTransGen.cases_tail hcv with rfl | ⟨u, hcu, huv⟩
  · obtain ⟨u, hvu, huv⟩ := Relation.TransGen.tail'_iff.mp hc
    exact h u huv ⟨_, hc, hvu⟩
  · exact h u huv ⟨_, hc, hcu⟩

/-- The next-to-last vertex of a chain of length at least two precedes the last. -/
theorem chain_pred {p : List Anchor} (hc : p.IsChain (Sum H)) {v : Anchor} (hl : p.getLast? = some v)
    (h2 : 2 ≤ p.length) : Sum H p[p.length - 2] v := by
  have h := List.isChain_iff_getElem.mp hc (p.length - 2) (by omega)
  have e : p[p.length - 2 + 1] = v := by
    rw [List.getLast?_eq_getElem?] at hl
    have : p.length - 2 + 1 = p.length - 1 := by omega
    simp only [this]
    rw [List.getElem?_eq_getElem (by omega)] at hl
    exact Option.some.inj hl
  rw [e] at h
  exact h

theorem depth_zero_iff {v : Anchor} :
    Depth H v 0 ↔ ∀ u, ¬ Sum H u v := by
  constructor
  · intro hd u hs
    have := hd.2 [u, v] (by simp) (List.isChain_cons_cons.mpr ⟨hs, List.isChain_singleton v⟩)
    simp at this
  · intro h
    refine ⟨⟨[v], rfl, rfl, List.isChain_singleton v⟩, ?_⟩
    intro p hl hc
    by_contra hlen
    push Not at hlen
    exact h _ (chain_pred hc hl (by omega))

theorem depth_succ_iff {v : Anchor} {r : Nat} (hv : Live H v) (hb : ¬ Blocked H v) :
    Depth H v (r+1) ↔ (∀ u, Sum H u v → ∃ k ≤ r, Depth H u k) ∧ ∃ u, Sum H u v ∧ Depth H u r := by
  constructor
  · intro hd
    refine ⟨fun u hs => ?_, ?_⟩
    · obtain ⟨k, hk⟩ := depth_exists hs.1 (pred_unblocked hs hb)
      exact ⟨k, by have := prec_depth_lt hk hd (Relation.TransGen.single hs); omega, hk⟩
    · obtain ⟨⟨p, hpl, hl, hc⟩, -⟩ := id hd
      have hs := chain_pred hc hl (by omega)
      have hpl2 : p.length - 2 = r := by omega
      refine ⟨_, hs, ?_⟩
      have := depth_at hd hv hb hpl hl hc (j := p.length - 2) (by omega)
      have key : ∀ m, Depth H p[p.length - 2] m → m = r → Depth H p[p.length - 2] r := by
        intro m h e; subst e; exact h
      exact key _ this hpl2
  · rintro ⟨hall, u0, hs0, hd0⟩
    obtain ⟨⟨q, hql, hqlast, hqc⟩, -⟩ := id hd0
    refine ⟨⟨q ++ [v], by simp [hql], by simp, snoc_chain hqc hqlast hs0⟩, ?_⟩
    intro p hl hc
    by_cases h2 : 2 ≤ p.length
    · have hs := chain_pred hc hl h2
      obtain ⟨k, hk, hdk⟩ := hall _ hs
      have hpre : (p.take (p.length - 1)).getLast? = some p[p.length - 2] := by
        have := getLast?_take_succ (p := p) (j := p.length - 2) (by omega)
        rwa [show p.length - 2 + 1 = p.length - 1 by omega] at this
      have := hdk.2 _ hpre (hc.take _)
      simp only [List.length_take] at this
      omega
    · omega

end o5s_kernel.DocDepth
