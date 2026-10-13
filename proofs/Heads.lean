import Causality
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Causal heads of a patch set

`causality::heads` gives a workspace proposal its dependencies: the patches
of the snapshot that no snapshot patch depends on. Their causal past is the
snapshot's (`heads_keep_past`), so the proposal depends on exactly what its
author saw while naming only the heads, not every patch of the channel.
-/
namespace o5s_kernel.Heads
open Causality

/-- `v` is a dependency of a patch in `ms`. -/
def Covered (rows : Rows) (ms : List Usize) (v : Nat) : Prop :=
  ∃ m ∈ ms, Dep rows v m.val

def markStep (c : List Bool) (d : Usize) : List Bool := c.set d.val true

/-- The marks after visiting member `m`: every dependency of `m` set. -/
def coverStep (rows : List (alloc.vec.Vec Usize)) (c : List Bool) (m : Usize) : List Bool :=
  if h : m.val < rows.length then (rows[m.val]).val.foldl markStep c else c

theorem take_succ'' {α} {l : List α} {i : Nat} (hi : i < l.length) :
    l.take (i + 1) = l.take i ++ [l[i]] := by
  rw [List.take_add_one, List.getElem?_eq_getElem hi]; rfl

theorem mark_length (ds : List Usize) (c : List Bool) : (ds.foldl markStep c).length = c.length := by
  induction ds generalizing c with
  | nil => rfl
  | cons d tail ih => simpa [List.foldl_cons, markStep] using ih (c.set d.val true)

theorem mark_lookup (ds : List Usize) (c : List Bool) (v : Nat) (hv : v < c.length) :
    (ds.foldl markStep c)[v]'(by rw [mark_length]; exact hv) = true ↔
      c[v] = true ∨ ∃ d ∈ ds, d.val = v := by
  induction ds generalizing c with
  | nil => simp
  | cons d tail ih =>
    simp only [List.foldl_cons]
    rw [ih (markStep c d) (by simpa [markStep] using hv)]
    by_cases he : d.val = v
    · subst he; simp [markStep]
    · simp [markStep, he]

theorem cover_length (rows : List (alloc.vec.Vec Usize)) (ms : List Usize) (c : List Bool) :
    (ms.foldl (coverStep rows) c).length = c.length := by
  induction ms generalizing c with
  | nil => rfl
  | cons m tail ih =>
    simp only [List.foldl_cons]
    rw [ih]
    unfold coverStep
    split
    · exact mark_length _ _
    · rfl

theorem cover_lookup (rows : List (alloc.vec.Vec Usize)) (ms : List Usize) (c : List Bool)
    (v : Nat) (hv : v < c.length) :
    (ms.foldl (coverStep rows) c)[v]'(by rw [cover_length]; exact hv) = true ↔
      c[v] = true ∨ ∃ m ∈ ms, ∃ h : m.val < rows.length, ∃ d ∈ (rows[m.val]).val, d.val = v := by
  induction ms generalizing c with
  | nil => simp
  | cons m tail ih =>
    simp only [List.foldl_cons]
    have hl : v < (coverStep rows c m).length := by
      unfold coverStep; split
      · rw [mark_length]; exact hv
      · exact hv
    rw [ih (coverStep rows c m) hl]
    have step : (coverStep rows c m)[v]'hl = true ↔
        c[v] = true ∨ ∃ h : m.val < rows.length, ∃ d ∈ (rows[m.val]).val, d.val = v := by
      by_cases hm : m.val < rows.length
      · unfold coverStep; simp only [dif_pos hm]; rw [mark_lookup _ _ _ hv]
        exact ⟨fun h => h.elim .inl (fun h => .inr ⟨hm, h⟩), fun h => h.elim .inl (fun ⟨_, h⟩ => .inr h)⟩
      · unfold coverStep; simp only [dif_neg hm]
        exact ⟨.inl, fun h => h.elim id (fun ⟨h, _⟩ => absurd h hm)⟩
    rw [step]
    simp only [List.mem_cons, exists_eq_or_imp]
    rw [or_assoc]

/-! ## The extracted loops -/

theorem inner_spec (deps : alloc.vec.Vec Usize) (cov : alloc.vec.Vec Bool)
    (bnd : ∀ d ∈ deps.val, d.val < cov.val.length) :
    causality.heads_loop0_loop0 cov deps 0#usize ⦃ out =>
      out.val = deps.val.foldl markStep cov.val ⦄ := by
  unfold causality.heads_loop0_loop0
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Bool × Usize => deps.val.length - st.2.val)
    (inv := fun st => st.2.val ≤ deps.val.length ∧ st.1.val.length = cov.val.length ∧
      deps.val.foldl markStep cov.val = (deps.val.drop st.2.val).foldl markStep st.1.val)
  · rintro ⟨c, j⟩ ⟨hj, hl, he⟩
    simp only at hj hl he
    unfold causality.heads_loop0_loop0.body
    h5i_step
    all_goals try and_intros
    all_goals first
      | omega
      | scalar_tac
      | simp [alloc.vec.Vec.set, hl]
      | (rw [List.drop_eq_getElem_cons (by scalar_tac), List.foldl_cons]; rfl)
  · simp

theorem outer_spec (rows : Rows) (members : alloc.vec.Vec Usize) (cov : alloc.vec.Vec Bool)
    (clen : cov.val.length = rows.val.length)
    (mb : ∀ m ∈ members.val, m.val < rows.val.length)
    (bnd : ∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length) :
    causality.heads_loop0 rows members cov 0#usize ⦃ out =>
      out.val = members.val.foldl (coverStep rows.val) cov.val ⦄ := by
  unfold causality.heads_loop0
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Bool × Usize => members.val.length - st.2.val)
    (inv := fun st => st.2.val ≤ members.val.length ∧ st.1.val.length = cov.val.length ∧
      members.val.foldl (coverStep rows.val) cov.val =
        (members.val.drop st.2.val).foldl (coverStep rows.val) st.1.val)
  · rintro ⟨c, i⟩ ⟨hi, hl, he⟩
    simp only at hi hl he
    unfold causality.heads_loop0.body
    h5i_step
    · have h := (by scalar_tac : i.val < members.val.length)
      have hm := mb _ (List.getElem_mem h)
      have hb : ∀ d ∈ ((rows.val)[(members.val[i.val]).val]'hm).val, d.val < c.val.length := by
        intro d hd; rw [hl]; exact bnd _ hm d hd
      subst_vars
      apply WP.spec_bind (inner_spec _ c hb)
      intro out hout
      h5i_step
      all_goals try and_intros
      all_goals first
        | omega
        | scalar_tac
        | (rw [mark_length, hl])
        | (rw [List.drop_eq_getElem_cons h, List.foldl_cons]; unfold coverStep; rw [dif_pos (mb _ (List.getElem_mem h))])
  · simp

theorem filter_spec (members : alloc.vec.Vec Usize) (cov : alloc.vec.Vec Bool)
    (mb : ∀ m ∈ members.val, m.val < cov.val.length) :
    causality.heads_loop1 members cov (alloc.vec.Vec.new Usize) 0#usize ⦃ out =>
      out.val = members.val.filter (fun m => !(cov.val[m.val]?.getD false)) ⦄ := by
  unfold causality.heads_loop1
  apply loop.spec_decr_nat
    (measure := fun st : alloc.vec.Vec Usize × Usize => members.val.length - st.2.val)
    (inv := fun st => st.2.val ≤ members.val.length ∧
      st.1.val = (members.val.take st.2.val).filter (fun m => !(cov.val[m.val]?.getD false)))
  · rintro ⟨o, i⟩ ⟨hi, he⟩
    simp only at hi he
    unfold causality.heads_loop1.body
    h5i_step
    all_goals try (have h2 : o.val.length ≤ i.val := by
                     rw [he]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)
                   scalar_tac)
    all_goals first
      | (rw [List.take_of_length_le (by scalar_tac)])
      | (have h := (by scalar_tac : i.val < members.val.length)
         have hb := mb _ (List.getElem_mem h)
         and_intros
         all_goals first
           | scalar_tac
           | (rw [take_succ'' h, List.filter_append]
              simp_all [List.getElem?_eq_getElem hb]))
  · simp

/-- `heads` returns, in order, the members that no member depends on. -/
@[step] theorem heads_spec (rows : Rows) (members : alloc.vec.Vec Usize)
    (covered : alloc.vec.Vec Bool)
    (clen : covered.val.length = rows.val.length) (clean : ∀ c ∈ covered.val, c = false)
    (mb : ∀ m ∈ members.val, m.val < rows.val.length)
    (bnd : ∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length) :
    causality.heads rows members covered ⦃ hs =>
      hs.val.Sublist members.val ∧
      ∀ v, v ∈ hs.val ↔ v ∈ members.val ∧ ¬ Covered rows members.val v.val ⦄ := by
  unfold causality.heads
  apply WP.spec_bind (outer_spec rows members covered clen mb bnd)
  intro c hc
  have cl : c.val.length = rows.val.length := by rw [hc, cover_length, clen]
  apply WP.spec_mono (filter_spec members c (by rw [cl]; exact mb))
  intro hs hh
  rw [hh]
  refine ⟨List.filter_sublist, fun v => ?_⟩
  rw [List.mem_filter]
  constructor
  · rintro ⟨hv, hn⟩
    refine ⟨hv, fun ⟨m, hm, h, x, hx, hxv⟩ => ?_⟩
    have hb : v.val < c.val.length := by rw [cl]; exact mb _ hv
    have := (cover_lookup rows.val members.val covered.val v.val (by rw [clen]; exact mb _ hv)).2
      (.inr ⟨m, hm, h, x, hx, hxv⟩)
    simp only [← hc] at this
    simp [List.getElem?_eq_getElem hb, this] at hn
  · rintro ⟨hv, hn⟩
    refine ⟨hv, ?_⟩
    have hb : v.val < c.val.length := by rw [cl]; exact mb _ hv
    have hlook := cover_lookup rows.val members.val covered.val v.val (by rw [clen]; exact mb _ hv)
    simp only [← hc] at hlook
    rw [List.getElem?_eq_getElem hb]
    cases hcv : c.val[v.val] with
    | false => rfl
    | true =>
      rcases hlook.1 hcv with h | ⟨m, hm, h, x, hx, hxv⟩
      · have hc0 := clean (covered.val[v.val]'(by rw [clen]; exact mb _ hv)) (List.getElem_mem _)
        simp [hc0] at h
      · exact absurd ⟨m, hm, h, x, hx, hxv⟩ hn

/-! ## The heads keep the causal past -/

theorem head_above (rows : Rows) (dag : ∀ u v, Dep rows u v → u < v) (ms hs : List Usize)
    (h : ∀ v, v ∈ hs ↔ v ∈ ms ∧ ¬ Covered rows ms v.val) :
    ∀ n, ∀ m ∈ ms, rows.val.length - m.val = n → ∃ x ∈ hs, Past rows m.val x.val := by
  intro n
  induction n using Nat.strong_induction_on with
  | _ n ih =>
    intro m hm hn
    by_cases hc : Covered rows ms m.val
    · obtain ⟨m', hm', hd⟩ := hc
      have hlt := dag _ _ hd
      have hb : m'.val < rows.val.length := by obtain ⟨h, _⟩ := hd; exact h
      obtain ⟨x, hx, hp⟩ := ih (rows.val.length - m'.val) (by omega) m' hm' rfl
      exact ⟨x, hx, Relation.ReflTransGen.head hd hp⟩
    · exact ⟨m, (h m).2 ⟨hm, hc⟩, Relation.ReflTransGen.refl⟩

/-- Whatever lies in the causal past of the members lies in the causal past
of the heads, and conversely: depending on the heads is depending on the set. -/
theorem keep_past (rows : Rows) (dag : ∀ u v, Dep rows u v → u < v) (ms hs : List Usize)
    (h : ∀ v, v ∈ hs ↔ v ∈ ms ∧ ¬ Covered rows ms v.val) (u : Nat) :
    (∃ x ∈ hs, Past rows u x.val) ↔ (∃ m ∈ ms, Past rows u m.val) := by
  constructor
  · rintro ⟨x, hx, hp⟩
    exact ⟨x, ((h x).1 hx).1, hp⟩
  · rintro ⟨m, hm, hp⟩
    obtain ⟨x, hx, hq⟩ := head_above rows dag ms hs h _ m hm rfl
    exact ⟨x, hx, hp.trans hq⟩

/-- `heads` on a dependency DAG: the result is a sublist of the members, no
head is a member's dependency, and the heads' causal past is the members'. -/
theorem heads_keep_past (rows : Rows) (members : alloc.vec.Vec Usize)
    (covered : alloc.vec.Vec Bool)
    (clen : covered.val.length = rows.val.length) (clean : ∀ c ∈ covered.val, c = false)
    (mb : ∀ m ∈ members.val, m.val < rows.val.length)
    (bnd : ∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length)
    (dag : ∀ u v, Dep rows u v → u < v) :
    causality.heads rows members covered ⦃ hs =>
      hs.val.Sublist members.val ∧
      (∀ x ∈ hs.val, ¬ Covered rows members.val x.val) ∧
      ∀ u, (∃ x ∈ hs.val, Past rows u x.val) ↔ (∃ m ∈ members.val, Past rows u m.val) ⦄ := by
  apply WP.spec_mono (heads_spec rows members covered clen clean mb bnd)
  rintro hs ⟨hsub, hmem⟩
  exact ⟨hsub, fun x hx => ((hmem x).1 hx).2, keep_past rows dag members.val hs.val hmem⟩

end o5s_kernel.Heads
