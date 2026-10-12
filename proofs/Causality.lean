import Indexed
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Causal searches (M5)

`causality::frontier` gives the graph dependencies of a document edit and
the nearest file heads (`documents::nearest`); `causality::sole_head` is the
check `prepare_lines` makes before accepting line ranges against a base. Row
`v` lists the dependencies of patch `v`.
-/
namespace o5s_kernel.Causality
open Indexed

abbrev Rows := alloc.vec.Vec (alloc.vec.Vec Usize)

/-- `u` is a dependency of `v`. -/
def Dep (rows : Rows) (u v : Nat) : Prop :=
  ∃ h : v < rows.val.length, ∃ x ∈ (rows.val[v]).val, x.val = u

/-- `u` is in the causal past of `v` (or is `v`). -/
def Past (rows : Rows) (u v : Nat) : Prop := Relation.ReflTransGen (fun a b => Dep rows a b) u v

/-- The causal past of the roots. -/
def InPast (rows : Rows) (roots : List Usize) (u : Nat) : Prop := ∃ r ∈ roots, Past rows u r.val

/-- `u` is reachable from a root along dependencies whose earlier vertices
(the root included, `u` excluded) do not stop the search. -/
inductive Open (rows : Rows) (stop : List Bool) (roots : List Usize) : Nat → Prop
  | root {r} : r ∈ roots → Open rows stop roots r.val
  | step {u v} : Open rows stop roots v → stop[v]! = false → Dep rows u v → Open rows stop roots u

/-- Indices in range, marks clean: what the native adapter supplies. -/
def Wf (rows : Rows) (stop : List Bool) (roots : List Usize) (marks : alloc.vec.Vec Bool) : Prop :=
  stop.length = rows.val.length ∧ marks.val.length = rows.val.length ∧
  (∀ m ∈ marks.val, m = false) ∧ (∀ r ∈ roots, r.val < rows.val.length) ∧
  (∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length) ∧
  rows.val.length < Usize.max


/-! ## Helpers -/

theorem clean_of_all {l : List Bool} (h : ∀ m ∈ l, m = false) : Clean l := by
  intro v hv; exact h _ (List.getElem_mem _)

theorem all_of_clean {l : List Bool} (h : Clean l) (room : l.length < Usize.max) :
    ∀ m ∈ l, m = false := by
  intro m hm
  obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hm
  have hlt : i < 2 ^ UScalarTy.Usize.numBits := by
    have := Usize.max_def; scalar_tac
  have hv : (UScalar.ofNatCore (ty := .Usize) i hlt).val = i := by simp
  have := h (UScalar.ofNatCore (ty := .Usize) i hlt) (by rw [hv]; exact hi)
  simpa [hv] using this

theorem rows_bounds {rows : Rows}
    (h : ∀ v (h : v < rows.val.length), ∀ x ∈ (rows.val[v]).val, x.val < rows.val.length) :
    ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length := by
  intro row hr v hv
  obtain ⟨i, hi, rfl⟩ := List.getElem_of_mem hr
  exact h i hi v hv

theorem bang {l : List Bool} {i : Nat} (hi : i < l.length) : l[i]! = l[i] := by simp [hi]

theorem take_succ' {α} {l : List α} {i : Nat} (hi : i < l.length) :
    l.take (i + 1) = l.take i ++ [l[i]] := by
  rw [List.take_add_one, List.getElem?_eq_getElem hi]; rfl

/-! ## `frontier` -/

/-- Loop invariant of `frontier`: the queue is the search so far, `found`
the stopping patches among the first `cursor` queued ones. -/
structure FInv (rows : Rows) (stop : List Bool) (roots : List Usize)
    (state : indexed.Traversal) (found : List Usize) (cursor : Nat) : Prop where
  seen : StateInv rows.val.length state
  cursor_bound : cursor ≤ state.queue.val.length
  sound : ∀ v ∈ state.queue.val, Open rows stop roots v.val
  roots_mem : roots ⊆ state.queue.val
  processed : ∀ (j : Nat) (hj : j < state.queue.val.length), j < cursor →
    stop[state.queue.val[j].val]! = false →
    ∀ (hv : state.queue.val[j].val < rows.val.length),
      ∀ v ∈ (rows.val[state.queue.val[j].val]).val, v ∈ state.queue.val
  found_eq : found = (state.queue.val.take cursor).filter (fun v => stop[v.val]!)

theorem finv_stop {rows : Rows} {stop roots state found} {i : Nat}
    (inv : FInv rows stop roots state found i)
    (hi : i < state.queue.val.length) (hs : stop[state.queue.val[i].val]! = true) :
    FInv rows stop roots state (found ++ [state.queue.val[i]]) (i + 1) := by
  refine ⟨inv.seen, hi, inv.sound, inv.roots_mem, ?_, ?_⟩
  · intro j hj hc hst hv v hm
    by_cases he : j = i
    · subst he; rw [hs] at hst; cases hst
    · exact inv.processed j hj (by omega) hst hv v hm
  · rw [inv.found_eq, take_succ' hi, List.filter_append, List.filter_cons, List.filter_nil]
    simp only [hs, if_true]

theorem finv_expand {rows : Rows} {stop roots} {state out : indexed.Traversal} {found}
    {i : Nat} (inv : FInv rows stop roots state found i) (hi : i < state.queue.val.length)
    (hs : stop[state.queue.val[i].val]! = false)
    (hb : state.queue.val[i].val < rows.val.length)
    (qeq : out.queue.val = (rows.val[state.queue.val[i].val]).val.foldl insertStep state.queue.val)
    (seen : StateInv rows.val.length out) : FInv rows stop roots out found (i + 1) := by
  have pref : state.queue.val <+: out.queue.val := by rw [qeq]; exact prefix_insert _ _
  have members (v : Usize) : v ∈ out.queue.val ↔
      v ∈ state.queue.val ∨ v ∈ (rows.val[state.queue.val[i].val]).val := by
    rw [qeq]; exact mem_foldl_insertStep _ _ _
  refine ⟨seen, by have := pref.length_le; omega, ?_, ?_, ?_, ?_⟩
  · intro v hv
    rcases (members v).1 hv with hv | hv
    · exact inv.sound _ hv
    · exact Open.step (inv.sound _ (List.getElem_mem _)) hs ⟨hb, v, hv, rfl⟩
  · intro v hv; exact (members v).2 (.inl (inv.roots_mem hv))
  · intro j hj hc hst hv v hmem
    have hjold : j < state.queue.val.length := by omega
    have eqv : state.queue.val[j] = out.queue.val[j] := pref.getElem hjold
    have hb' : state.queue.val[j].val < rows.val.length := by
      rw [← inv.seen.1]; exact inv.seen.2.bounded _ (List.getElem_mem _)
    have roweq : rows.val[state.queue.val[j].val] = rows.val[out.queue.val[j].val] := by
      congr 1
      exact congrArg (fun x : Usize => x.val) eqv
    have hm : v ∈ (rows.val[state.queue.val[j].val]).val := by
      simpa only [roweq] using hmem
    have hst' : stop[state.queue.val[j].val]! = false := by rw [eqv]; exact hst
    by_cases he : j = i
    · subst j; exact (members v).2 (.inr hm)
    · exact (members v).2 (.inl (inv.processed j hjold (by omega) hst' hb' v hm))
  · obtain ⟨t, ht⟩ := pref
    rw [inv.found_eq, ← ht, List.take_append_of_le_length (by omega), take_succ' hi,
      List.filter_append, List.filter_cons, List.filter_nil]
    simp only [hs, Bool.false_eq_true, if_false, List.append_nil]

theorem finv_complete {rows : Rows} {stop roots state found} {i : Nat}
    (inv : FInv rows stop roots state found i) (done : state.queue.val.length ≤ i) :
    ∀ u, Open rows stop roots u → ∃ x ∈ state.queue.val, x.val = u := by
  intro u h
  induction h with
  | root hr => exact ⟨_, inv.roots_mem hr, rfl⟩
  | step hv hs hd ih =>
    obtain ⟨x, hx, rfl⟩ := ih
    obtain ⟨hb, y, hy, rfl⟩ := hd
    obtain ⟨j, hj, rfl⟩ := List.getElem_of_mem hx
    exact ⟨y, inv.processed j hj (by omega) hs hb y hy, rfl⟩

theorem frontier_loop_spec (rows : Rows) (stop : alloc.vec.Vec Bool) (roots : List Usize)
    (state : indexed.Traversal) (found : alloc.vec.Vec Usize) (i : Usize)
    (inv : FInv rows stop.val roots state found.val i.val)
    (slen : stop.val.length = rows.val.length) (room : rows.val.length < Usize.max)
    (bounds : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < rows.val.length) :
    causality.frontier_loop rows stop state found i ⦃ out =>
      StateInv rows.val.length {marks := out.1, queue := out.2.1} ∧
      (∀ v ∈ out.2.1.val, Open rows stop.val roots v.val) ∧
      (∀ u, Open rows stop.val roots u → ∃ x ∈ out.2.1.val, x.val = u) ∧
      out.2.2.val = out.2.1.val.filter (fun v => stop.val[v.val]!) ⦄ := by
  unfold causality.frontier_loop
  apply loop.spec_decr_nat
    (measure := fun st : indexed.Traversal × alloc.vec.Vec Usize × Usize =>
      rows.val.length + 1 - st.2.2.val)
    (inv := fun st => FInv rows stop.val roots st.1 st.2.1.val st.2.2.val)
  · rintro ⟨st, fd, j⟩ hI
    simp only at hI
    unfold causality.frontier_loop.body
    have qb : ∀ k (hk : k < st.queue.val.length), st.queue.val[k].val < rows.val.length := by
      intro k hk; rw [← hI.seen.1]; exact hI.seen.2.bounded _ (List.getElem_mem _)
    have qs := queue_size hI.seen.2
    rw [hI.seen.1] at qs
    h5i_step
    all_goals try first
      | exact qb _ (by scalar_tac)
      | exact hI.seen
      | exact bounds _ (List.getElem_mem _)
      | (have hj : j.val < st.queue.val.length := by scalar_tac
         have h2 : fd.val.length ≤ j.val := by
           rw [hI.found_eq]; exact (List.length_filter_le _ _).trans (List.length_take_le _ _)
         omega)
      | (have hj : j.val < st.queue.val.length := by scalar_tac
         have hb := qb _ hj
         refine ⟨finv_stop hI hj ?_, by omega⟩
         rw [bang (by rw [slen]; exact hb)]; exact b_post)
    · have hj : j.val < st.queue.val.length := by scalar_tac
      have hb := qb _ hj
      subst v_post
      have hs : stop.val[st.queue.val[j.val].val]! = false := by
        rw [bang (by rw [slen]; exact hb), ← b_post]
        try simpa using ‹¬b = true›
      exact ⟨finv_expand hI hj hs hb x_post x_post1, by omega⟩
    · refine ⟨hI.seen, hI.sound, finv_complete hI (by scalar_tac), ?_⟩
      rw [hI.found_eq, List.take_of_length_le (by scalar_tac)]
      simp
  · exact inv

/-- `frontier` returns exactly the stopping patches the search reaches, each
once, and clean marks. -/
@[step] theorem frontier_spec (rows : Rows) (stop : alloc.vec.Vec Bool)
    (roots : alloc.vec.Vec Usize) (marks : alloc.vec.Vec Bool)
    (wf : Wf rows stop.val roots.val marks) :
    causality.frontier rows stop roots marks ⦃ t =>
      (∀ v, (∃ x ∈ t.queue.val, x.val = v) ↔ Open rows stop.val roots.val v ∧ stop.val[v]! = true) ∧
      (t.queue.val.map (·.val)).Nodup ∧
      t.marks.val.length = rows.val.length ∧ ∀ m ∈ t.marks.val, m = false ⦄ := by
  obtain ⟨slen, mlen, mclean, rb, bnd, room⟩ := wf
  have bounds := rows_bounds bnd
  unfold causality.frontier
  apply WP.spec_bind (enqueue_spec rows.val.length { marks, queue := alloc.vec.Vec.new Usize }
    roots ⟨mlen, empty_seen marks (clean_of_all mclean)⟩ room rb)
  rintro st ⟨hq, hinv⟩
  have finv0 : FInv rows stop.val roots.val st (alloc.vec.Vec.new Usize).val (0#usize).val := by
    refine ⟨hinv, by simp, ?_, ?_, ?_, by simp⟩
    · intro v hv
      rw [hq] at hv
      have hm := (mem_foldl_insertStep roots.val [] v).1 hv
      exact Open.root (by simpa using hm)
    · intro v hv
      rw [hq]
      exact (mem_foldl_insertStep roots.val [] v).2 (.inr hv)
    · intro j hj hc; simp at hc
  apply WP.spec_bind (frontier_loop_spec rows stop roots.val st _ 0#usize finv0 slen room bounds)
  rintro ⟨mk, qu, fd⟩ ⟨hs, hsound, hcomp, hfd⟩
  apply WP.spec_bind (clear_clean_spec { marks := mk, queue := qu } hs.2)
  rintro cl ⟨_, hlen, hclean⟩
  simp only [WP.spec_ok]
  have hlen' : cl.marks.val.length = rows.val.length := by rw [hlen]; exact hs.1
  refine ⟨?_, ?_, hlen', all_of_clean hclean (by rw [hlen']; exact room)⟩
  · intro v
    simp only [hfd, List.mem_filter]
    constructor
    · rintro ⟨x, ⟨hx, hsx⟩, rfl⟩; exact ⟨hsound x hx, hsx⟩
    · rintro ⟨ho, hsv⟩
      obtain ⟨x, hx, rfl⟩ := hcomp v ho
      exact ⟨x, ⟨hx, hsv⟩, rfl⟩
  · rw [hfd]
    have hn : (qu.val.map (·.val)).Nodup :=
      hs.2.nodup.map (fun _ _ he => UScalar.eq_of_val_eq he)
    exact hn.sublist (List.filter_sublist.map _)

theorem covers_aux (rows : Rows) (stop : List Bool) (roots : List Usize) (u : Nat)
    (hs : stop[u]! = true) : ∀ v, Past rows u v → Open rows stop roots v →
      ∃ q, Open rows stop roots q ∧ stop[q]! = true ∧ Past rows u q := by
  intro v h
  induction h with
  | refl => intro ho; exact ⟨u, ho, hs, .refl⟩
  | @tail b c hp hd ih =>
    intro ho
    cases hc : stop[c]! with
    | true => exact ⟨c, ho, hc, hp.tail hd⟩
    | false => exact ih (Open.step ho hc hd)

/-- On an acyclic history (dependencies older than dependents), the nearest
stopping patches include every maximal one: a stopping patch in the past of
the roots lies in the past of some nearest one. -/
theorem frontier_covers (rows : Rows) (stop : List Bool) (roots : List Usize)
    (dag : ∀ u v, Dep rows u v → u < v) (u : Nat) (hu : InPast rows roots u)
    (hs : stop[u]! = true) :
    ∃ q, Open rows stop roots q ∧ stop[q]! = true ∧ Past rows u q := by
  obtain ⟨r, hr, hp⟩ := hu
  exact covers_aux rows stop roots u hs r.val hp (Open.root hr)

/-- A causal head: an editing patch in the past of the roots that is not a
strict ancestor of another one. -/
def Head (rows : Rows) (edits : List Bool) (roots : List Usize) (h : Nat) : Prop :=
  InPast rows roots h ∧ edits[h]! = true ∧
  ∀ w, InPast rows roots w → edits[w]! = true → w ≠ h → ¬ Past rows h w


/-! ## `sole_head` -/

theorem lt_of_get! {l : List Bool} {i : Nat} (h : l[i]! = true) : i < l.length := by
  by_contra hc
  simp [List.getElem?_eq_none (show l.length ≤ i by omega)] at h

theorem past_le {rows : Rows} (dag : ∀ u v, Dep rows u v → u < v) {a b : Nat}
    (h : Past rows a b) : a ≤ b := by
  induction h with
  | refl => exact le_refl _
  | tail _ hd ih => exact le_trans ih (le_of_lt (dag _ _ hd))

theorem trans_lt {rows : Rows} (dag : ∀ u v, Dep rows u v → u < v) {a b : Nat}
    (h : Relation.TransGen (fun a b => Dep rows a b) a b) : a < b := by
  induction h with
  | single hd => exact dag _ _ hd
  | tail _ hd ih => exact lt_trans ih (dag _ _ hd)

theorem past_strict {rows : Rows} {a b : Nat} (h : Past rows a b) (hne : a ≠ b) :
    Relation.TransGen (fun a b => Dep rows a b) a b := by
  rcases Relation.reflTransGen_iff_eq_or_transGen.1 h with h | h
  · exact absurd h.symm hne
  · exact h

theorem open_inpast {rows : Rows} {stop : List Bool} {roots : List Usize} {u : Nat}
    (h : Open rows stop roots u) : InPast rows roots u := by
  induction h with
  | root hr => exact ⟨_, hr, .refl⟩
  | step _ _ hd ih =>
    obtain ⟨r, hr, hp⟩ := ih
    exact ⟨r, hr, Relation.ReflTransGen.head hd hp⟩

/-- On an acyclic history every causal head is among the nearest edits. -/
theorem head_open {rows : Rows} {edits : List Bool} {roots : List Usize}
    (dag : ∀ u v, Dep rows u v → u < v) {h : Nat} (hh : Head rows edits roots h) :
    Open rows edits roots h := by
  obtain ⟨⟨r, hr, hp⟩, he, hmax⟩ := hh
  suffices key : ∀ a, Past rows a r.val → Past rows h a → Open rows edits roots a from
    key h hp .refl
  intro a hpa
  induction hpa using Relation.ReflTransGen.head_induction_on with
  | refl => intro _; exact Open.root hr
  | @head a c hd hc ih =>
    intro hha
    have hhc : Past rows h c := hha.tail hd
    have hlc : h < c := lt_of_le_of_lt (past_le dag hha) (dag _ _ hd)
    have ec : edits[c]! = false := by
      cases hce : edits[c]! with
      | false => rfl
      | true => exact absurd hhc (hmax c ⟨r, hr, hc⟩ hce (by omega))
    exact Open.step (ih hhc) ec hd

/-- Every edit in the causal past lies below some causal head. -/
theorem head_above {rows : Rows} {edits : List Bool} {roots : List Usize}
    (dag : ∀ u v, Dep rows u v → u < v) (q : Nat) (hq : InPast rows roots q)
    (eq : edits[q]! = true) : ∃ h, Head rows edits roots h ∧ Past rows q h := by
  suffices key : ∀ k q, edits.length - q ≤ k → InPast rows roots q → edits[q]! = true →
      ∃ h, Head rows edits roots h ∧ Past rows q h from key _ q le_rfl hq eq
  intro k
  induction k with
  | zero =>
    intro q hk _ eq
    have := lt_of_get! eq
    omega
  | succ k ih =>
    intro q hk hq eq
    by_cases hm : ∀ w, InPast rows roots w → edits[w]! = true → w ≠ q → ¬ Past rows q w
    · exact ⟨q, ⟨hq, eq, hm⟩, .refl⟩
    · push_neg at hm
      obtain ⟨w, hw, ew, hne, hp⟩ := hm
      have hlt : q < w := lt_of_le_of_ne (past_le dag hp) (Ne.symm hne)
      have hwl : w < edits.length := lt_of_get! ew
      obtain ⟨h, hh, hph⟩ := ih w (by omega) hw ew
      exact ⟨h, hh, hp.trans hph⟩

theorem rowreach_trans {rows : Rows} (b : Nat) (hb : b < rows.val.length) (x : Usize) :
    RowReach rows.val (rows.val[b]).val x ↔
      Relation.TransGen (fun a b => Dep rows a b) x.val b := by
  constructor
  · intro h
    induction h with
    | seed hm => exact .single ⟨hb, _, hm, rfl⟩
    | edge _ hu' hm ih => exact .head ⟨hu', _, hm, rfl⟩ ih
  · intro h
    have key : ∀ n, Relation.TransGen (fun a b => Dep rows a b) n b →
        ∃ y : Usize, y.val = n ∧ RowReach rows.val (rows.val[b]).val y := by
      intro n hn
      induction hn using Relation.TransGen.head_induction_on with
      | single hd =>
        obtain ⟨_, z, hz, hzv⟩ := hd
        exact ⟨z, hzv, .seed hz⟩
      | head hd _ ih =>
        obtain ⟨y, hyv, hy⟩ := ih
        subst hyv
        obtain ⟨hcl, z, hz, hzv⟩ := hd
        exact ⟨z, hzv, .edge hy hcl hz⟩
    obtain ⟨y, hyv, hy⟩ := key _ h
    rwa [UScalar.eq_of_val_eq hyv] at hy

/-- What `sole_head` computes from the nearest edits `N` and the strict
ancestors `A` of `base` decides whether `base` is the sole causal head. -/
theorem sole_logic {rows : Rows} {edits : List Bool} {roots : List Usize} (base : Usize)
    (dag : ∀ u v, Dep rows u v → u < v) (N A : List Usize)
    (hN : ∀ v, (∃ x ∈ N, x.val = v) ↔ Open rows edits roots v ∧ edits[v]! = true)
    (hA : ∀ x, x ∈ A ↔ Relation.TransGen (fun a b => Dep rows a b) x.val base.val) :
    (base ∈ N ∧ ∀ q ∈ N, q = base ∨ q ∈ A) ↔ ∀ h, Head rows edits roots h ↔ h = base.val := by
  have memN : ∀ x : Usize, x ∈ N ↔ Open rows edits roots x.val ∧ edits[x.val]! = true := by
    intro x; rw [← hN]; constructor
    · intro hx; exact ⟨x, hx, rfl⟩
    · rintro ⟨y, hy, he⟩; rwa [UScalar.eq_of_val_eq he] at hy
  constructor
  · rintro ⟨hb, hall⟩ h
    obtain ⟨hbo, hbe⟩ := (memN base).1 hb
    constructor
    · intro hh
      obtain ⟨x, hx, rfl⟩ := (hN h).2 ⟨head_open dag hh, hh.2.1⟩
      rcases hall x hx with rfl | hxa
      · rfl
      · have ht := (hA x).1 hxa
        have hlt := trans_lt dag ht
        exact absurd ht.to_reflTransGen (hh.2.2 base.val (open_inpast hbo) hbe (by omega))
    · rintro rfl
      refine ⟨open_inpast hbo, hbe, ?_⟩
      intro w hw ew hne hp
      obtain ⟨q, hqo, hqe, hwq⟩ := frontier_covers rows edits roots dag w hw ew
      obtain ⟨x, hx, rfl⟩ := (hN q).2 ⟨hqo, hqe⟩
      have h1 := past_le dag hp
      have h2 := past_le dag hwq
      rcases hall x hx with rfl | hxa
      · omega
      · have := trans_lt dag ((hA x).1 hxa); omega
  · intro hheads
    have hbh : Head rows edits roots base.val := (hheads base.val).2 rfl
    refine ⟨(memN base).2 ⟨head_open dag hbh, hbh.2.1⟩, ?_⟩
    intro q hq
    by_cases hqb : q = base
    · exact .inl hqb
    · right
      obtain ⟨hqo, hqe⟩ := (memN q).1 hq
      obtain ⟨h, hh, hp⟩ := head_above dag q.val (open_inpast hqo) hqe
      have := (hheads h).1 hh
      subst this
      exact (hA q).2 (past_strict hp (fun e => hqb (UScalar.eq_of_val_eq e)))

def okStep (base : Usize) (A : List Usize) (ok : Bool) (q : Usize) : Bool :=
  if ok then (if q = base then true else decide (q ∈ A)) else false

theorem okStep_fold (base : Usize) (A N : List Usize) (b : Bool) :
    N.foldl (okStep base A) b = true ↔ b = true ∧ ∀ q ∈ N, q = base ∨ q ∈ A := by
  induction N generalizing b with
  | nil => simp
  | cons q N ih =>
    rw [List.foldl_cons, ih]
    cases b <;> by_cases hq : q = base <;> simp [okStep, hq]

@[step] theorem sole_head_loop_spec (base : Usize) (N : alloc.vec.Vec Usize)
    (A : indexed.Traversal) (ok : Bool) :
    causality.sole_head_loop base N A ok 0#usize ⦃ r =>
      r.1 = A ∧ r.2 = N.val.foldl (okStep base A.queue.val) ok ⦄ := by
  unfold causality.sole_head_loop
  apply WP.spec_mono (loop_fold2 N.val (fun _ b => b) (okStep base A.queue.val)
    (fun a _ _ => a = A)
    (fun st => causality.sole_head_loop.body base N st.1 st.2.1 st.2.2) ?_ A ok 0#usize
    (by simp) rfl)
  · rintro ⟨a, b⟩ ⟨h1, h2⟩; exact ⟨h2, by simpa using h1⟩
  · intro a b i hi ha
    subst ha
    unfold causality.sole_head_loop.body
    h5i_step
    all_goals (refine ⟨by scalar_tac, ?_⟩; simp_all [okStep])

/-- `sole_head` decides that `base` is the only causal head of the file. -/
@[step] theorem sole_head_spec (rows : Rows) (edits : alloc.vec.Vec Bool)
    (roots : alloc.vec.Vec Usize) (base : Usize) (marks : alloc.vec.Vec Bool)
    (wf : Wf rows edits.val roots.val marks) (hb : base.val < rows.val.length)
    (dag : ∀ u v, Dep rows u v → u < v) :
    causality.sole_head rows edits roots base marks ⦃ r =>
      (r.1 = true ↔ ∀ h, Head rows edits.val roots.val h ↔ h = base.val) ∧
      r.2.val.length = rows.val.length ∧ ∀ m ∈ r.2.val, m = false ⦄ := by
  have wf' := wf
  obtain ⟨slen, mlen, mclean, rb, bnd, room⟩ := wf'
  have bounds := rows_bounds bnd
  unfold causality.sole_head
  h5i_step
  · exact clean_of_all fun m hm => by
      cases m
      · rfl
      · exact absurd hm near_post3
  · have hN : ∀ v, (∃ x ∈ near.queue.val, x.val = v) ↔
        Open rows edits.val roots.val v ∧ edits.val[v]! = true := fun v => by simpa using near_post v
    have hA : ∀ x, x ∈ ancestors.queue.val ↔
        Relation.TransGen (fun a b => Dep rows a b) x.val base.val := fun x => by
      rw [ancestors_post1 x]; exact rowreach_trans base.val hb x
    refine ⟨?_, ?_⟩
    · rw [okStep_fold]
      simp only [decide_eq_true_eq]
      exact sole_logic base dag near.queue.val ancestors.queue.val hN hA
    · intro ht
      have := all_of_clean ancestors_post3 (by rw [ancestors_post2]; exact room) true ht
      cases this

end o5s_kernel.Causality
