import DocLayers
import Order
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# The extracted `materialize`, end to end

`materialize_kernel` composes the loop specifications: the delete/insert
conflicts, liveness, the edge and dead rows, the boundary rows and counts of
`materialize_loop1`, the initial Kahn state (`inv_init`), the Kahn rounds
(`loop2_spec`), the blocked atoms and the single-layer test. It is stated for
any depth and blocked predicates under which every boundary graph the code
may build satisfies the Kahn axioms; `DocMatModel` supplies the model's.
-/
namespace o5s_kernel.DocKahn
open DocKernel

/-- The live edges of the kernel's edge table, as index pairs. -/
noncomputable def kE (doc : document.Doc) : List (Usize × Usize) := by
  classical
  exact ((List.range doc.edges.val.length).filter
    (fun i => decide (¬ ∃ x ∈ doc.retractions.val, x.1.val = i))).filterMap
    (fun i => doc.edges.val[i]?.map (fun e => (e.1, e.2.1)))

theorem mem_kE (doc : document.Doc) (a b : Usize) :
    (a, b) ∈ kE doc ↔ ∃ i, ∃ h : i < doc.edges.val.length,
      ¬ (∃ x ∈ doc.retractions.val, x.1.val = i) ∧ (doc.edges.val[i]).1 = a ∧ (doc.edges.val[i]).2.1 = b := by
  classical
  unfold kE
  simp only [List.mem_filterMap, List.mem_filter, List.mem_range, decide_eq_true_eq, Option.map_eq_some_iff]
  constructor
  · rintro ⟨i, ⟨hi, hw⟩, e, he, hab⟩
    rw [List.getElem?_eq_getElem hi] at he
    cases he
    simp only [Prod.mk.injEq] at hab
    exact ⟨i, hi, hw, hab.1, hab.2⟩
  · rintro ⟨i, hi, hw, h1, h2⟩
    exact ⟨i, ⟨hi, hw⟩, _, List.getElem?_eq_getElem hi, by rw [h1, h2]⟩

theorem aliveEdge_iff (doc : document.Doc) (s v : Usize) :
    AliveEdge doc s.val v ↔ (s, v) ∈ kE doc := by
  rw [mem_kE]
  unfold AliveEdge
  constructor
  · rintro ⟨i, hi, hw, h1, h2⟩; exact ⟨i, hi, hw, UScalar.eq_of_val_eq h1, h2⟩
  · rintro ⟨i, hi, hw, h1, h2⟩; exact ⟨i, hi, hw, by rw [h1], h2⟩

/-- The boundary rows `materialize_loop1` produces, as a property of the rows alone. -/
def RowsOK (live : List Bool) (edges : List (Usize × Usize)) (n : Nat)
    (bnd : List (alloc.vec.Vec Usize)) : Prop :=
  bnd.length = n ∧ ∀ u (h : u < bnd.length), (live[u]?.getD false = true →
    ∃ uu : Usize, uu.val = u ∧ BRow live edges uu (bnd[u]).val) ∧
    (live[u]?.getD false = false → (bnd[u]).val = [])

theorem rowsOK_of {live : List Bool} {edges : List (Usize × Usize)} {n : Nat} {m : List Bool}
    {bnd : List (alloc.vec.Vec Usize)} {indeg rem : List Usize} {left : Nat}
    (h : L1Inv live edges n n m bnd indeg rem left) : RowsOK live edges n bnd :=
  ⟨h.blen, h.rows⟩

theorem l1_init (live : List Bool) (edges : List (Usize × Usize)) (n : Usize) :
    L1Inv live edges n.val 0 (List.replicate n.val false) [] (List.replicate n.val 0#usize) [] 0 := by
  refine ⟨by simp, ?_, rfl, ?_, by simp, ?_, by simp, by simp⟩
  · intro v hv; simp at hv ⊢
  · intro u hu; simp at hu
  · intro w
    unfold cntUpTo valAt
    simp only [Finset.range_zero, Finset.filter_empty, Finset.card_empty]
    rw [List.getElem?_replicate]; split_ifs <;> rfl

theorem clone_bool (v : alloc.vec.Vec Bool) : alloc.vec.CloneVec.clone core.clone.CloneBool v = ok v := by
  have h := Slice.clone_spec (clone := core.clone.CloneBool.clone) (s := v.slice) (fun x _ => rfl)
  rw [WP.spec_equiv_exists] at h
  obtain ⟨s', hs, rfl⟩ := h
  simp [alloc.vec.CloneVec.clone, hs]

theorem range_filter_card (n : Nat) (p : Nat → Bool) :
    ((List.range n).filter p).length = ((Finset.range n).filter (fun u => p u = true)).card := by
  rw [← List.toFinset_card_of_nodup (List.nodup_range.filter _)]
  congr 1
  ext u
  simp

theorem zeroCount_iff {indeg : List Usize} {x : Usize} (h : x.val < indeg.length) :
    Order.zeroCount indeg x = true ↔ valAt indeg x.val = 0 := by
  unfold Order.zeroCount valAt
  rw [List.getElem?_eq_getElem h]
  simp only [Option.getD_some, Option.map_some, decide_eq_true_eq]
  constructor
  · intro e; rw [e]; rfl
  · intro e; exact UScalar.eq_of_val_eq (by simpa using e)

namespace KG

/-- The Kahn state `materialize` starts the round loop from. -/
theorem inv_init {g : KG} (ax : g.Ax) (atoms : List (alloc.vec.Vec U8)) {edges : List (Usize × Usize)}
    {m : List Bool} {bnd : List (alloc.vec.Vec Usize)} {rem : List Usize} {left : Nat}
    (indeg1 : alloc.vec.Vec Usize) (live : alloc.vec.Vec Bool) (leftU : Usize) (ready : alloc.vec.Vec Usize)
    (hlive : live.val = g.live) (hlen : g.live.length = g.n) (hbs : g.bs = rowN bnd)
    (hL1 : L1Inv g.live edges g.n g.n m bnd indeg1.val rem left) (hleft : leftU.val = left)
    (hready : ready.val = rem.filter (Order.zeroCount indeg1.val)) :
    g.Inv atoms 0 { indegree := indeg1, remaining := live, left := leftU, ready := ready, layers := alloc.vec.Vec.new _, bytes := alloc.vec.Vec.new U8, blocked := false } := by
  have hR : ∀ u, remF live.val u = g.liveF u := fun u => by rw [hlive]; rfl
  have hcnt : ∀ w, valAt indeg1.val w = g.cnt (remF live.val) w := by
    intro w; rw [hL1.deg w]; unfold cntUpTo KG.cnt; rw [hbs]; congr 1
    apply Finset.filter_congr; intro u _; rw [hR]; rfl
  have hremv : ∀ v, v ∈ rem.map (·.val) ↔ g.L v := by
    intro v; rw [hL1.rem, List.mem_filter, List.mem_range]; rfl
  refine ⟨by simp [hlive, hlen], ?_, by simp [hL1.ilen], fun w _ => hcnt w, ?_, rfl, ?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro v hv
    rw [hR]
    constructor
    · intro h; exact ⟨⟨hv, h⟩, fun ⟨_, _, k, hk, _⟩ => by omega⟩
    · rintro ⟨⟨_, h⟩, _⟩; exact h
  · show leftU.val = _
    rw [hleft, hL1.left, range_filter_card]
    congr 1; ext u; simp [hR, KG.liveF]
  · intro j hj; simp at hj
  · intro L hL; simp at hL
  · intro v
    show v ∈ ready.val.map (·.val) ↔ _
    rw [hready]
    constructor
    · intro h
      obtain ⟨x, hx, rfl⟩ := List.mem_map.mp h
      obtain ⟨hxr, hz⟩ := List.mem_filter.mp hx
      have hl : g.L x.val := (hremv x.val).mp (List.mem_map.mpr ⟨x, hxr, rfl⟩)
      have hxl : x.val < indeg1.val.length := by rw [hL1.ilen]; exact hl.1
      have h0 : g.cnt (remF live.val) x.val = 0 := by rw [← hcnt]; exact (zeroCount_iff hxl).mp hz
      have hno : ∀ u, ¬ g.S u x.val := by
        rintro u ⟨hlu, hm⟩
        exact (cnt_zero.mp h0) u hlu.1 (by rw [hR]; exact hlu.2) hm
      have hbx : ¬ g.B x.val := ax.bunb x.val hl (fun u hs => absurd hs (hno u))
      exact ⟨hl, hbx, (ax.d0 x.val hl hbx).mpr hno⟩
    · rintro ⟨hl, hb, hd⟩
      have hno := (ax.d0 v hl hb).mp hd
      have h0 : g.cnt (remF live.val) v = 0 :=
        cnt_zero.mpr (fun u hu hr hm => hno u ⟨⟨hu, by rw [← hR]; exact hr⟩, hm⟩)
      obtain ⟨x, hxr, rfl⟩ := List.mem_map.mp ((hremv v).mpr hl)
      have hxl : x.val < indeg1.val.length := by rw [hL1.ilen]; exact hl.1
      exact List.mem_map.mpr ⟨x, List.mem_filter.mpr ⟨hxr, (zeroCount_iff hxl).mpr ((hcnt _).trans h0)⟩, rfl⟩
  · show (ready.val.map (·.val)).Nodup
    rw [hready]
    have hnd : (rem.map (·.val)).Nodup := by rw [hL1.rem]; exact List.nodup_range.filter _
    exact (List.Sublist.map _ List.filter_sublist).nodup hnd
  · simp
  · show (alloc.vec.Vec.new U8).val.length = _
    symm
    apply Finset.sum_eq_zero
    intro v hv
    simp [KG.EmF, hR] at hv
  · simp
  · intro h; cases h

end KG

/-- The liveness table `materialize` computes. -/
def liveL (doc : document.Doc) : List Bool :=
  (List.range doc.atoms.val.length).map
    (liveM (coverUpTo doc.deletions.val doc.atoms.val.length doc.deletions.val.length) doc.restored.val)

/-- The extracted `materialize`, against any depth and blocked predicates for which every
graph of boundary rows it may build satisfies the Kahn axioms. -/
theorem materialize_kernel (doc : document.Doc) (wf : DocWF doc) (mwf : MatWF doc)
    (hdel : ∀ x ∈ doc.deletions.val, ∀ t ∈ x.targets.val, t.val < doc.atoms.val.length)
    (droom : (doc.deletions.val.map (·.targets.val.length)).sum < Usize.max)
    (hnm : doc.atoms.val.length < Usize.max) (eroom : doc.edges.val.length < Usize.max)
    (roomT : ∑ v ∈ Finset.range doc.atoms.val.length, (atomB doc.atoms.val v).length ≤ Usize.max)
    (D : Nat → Nat → Prop) (B : Nat → Prop)
    (hax : ∀ bnd, RowsOK (liveL doc) (kE doc) doc.atoms.val.length bnd →
      (KG.mk doc.atoms.val.length (liveL doc) (rowN bnd) D B).Ax) :
    document.materialize doc ⦃ v => ∃ rem b,
      (KG.mk doc.atoms.val.length (liveL doc) (fun _ => []) D B).Final doc.atoms.val rem
        v.layers.val v.bytes.val b ∧
      v.blocked.val.map (·.val) = (List.range doc.atoms.val.length).filter (fun x => b && remF rem x) ∧
      v.delete_inserts.val.map diVal = deleteInsertsM doc ∧
      v.clean = (v.layers.val.all (fun L => L.val.length = 1) && !b && v.delete_inserts.val.isEmpty) ⦄ := by
  unfold document.materialize
  apply WP.spec_bind (delete_inserts_spec doc mwf); intro di hdi
  apply WP.spec_bind (liveness_spec doc hdel droom mwf.restored); intro live hlive
  apply WP.spec_bind (all_rows_spec doc (fun e he => (wf.edges e he).1) wf.retractions eroom)
  rintro rows ⟨hrl, hrm⟩
  apply WP.spec_bind (dead_rows_spec rows live (by rw [hlive, hrl]; simp)); intro dead hdead
  dsimp only
  have hliveLen : live.val.length = doc.atoms.val.length := by rw [hlive]; simp
  have hlive' : live.val = liveL doc := hlive
  have hdeadLen : dead.val.length = doc.atoms.val.length := by
    have := congrArg List.length hdead; simp at this; rw [this, hrl]
  have hdead_u : ∀ u (h : u < dead.val.length), (dead.val[u]).val = deadRow rows.val live.val u := by
    intro u h
    have := List.getElem_of_eq hdead (by simpa using h)
    simpa using this
  have seed_eq : ∀ (s : Usize) (h : s.val < rows.val.length) (v : Usize),
      v ∈ (rows.val[s.val]).val ↔ (s, v) ∈ kE doc := fun s h v => by rw [hrm s.val h v, aliveEdge_iff]
  have row_eq : ∀ u v : Usize, (∃ h : u.val < dead.val.length, v ∈ (dead.val[u.val]).val) ↔
      (u, v) ∈ kE doc ∧ ¬ Boundary.alive live.val u = true := by
    intro u v
    constructor
    · rintro ⟨h, hv⟩
      have hu' : u.val < rows.val.length := by rw [hrl, ← hdeadLen]; exact h
      rw [hdead_u _ h] at hv
      unfold deadRow at hv
      split_ifs at hv with hl
      · simp at hv
      · rw [List.getElem?_eq_getElem hu'] at hv
        simp only [Option.map_some, Option.getD_some] at hv
        exact ⟨(seed_eq u hu' v).mp hv, by unfold Boundary.alive; simpa using hl⟩
    · rintro ⟨he, hal⟩
      obtain ⟨i, hi, hw, h1, h2⟩ := (mem_kE doc u v).mp he
      have hu' : u.val < rows.val.length := by
        rw [hrl, ← h1]; exact (wf.edges _ (List.getElem_mem hi)).1
      refine ⟨by rw [hdeadLen, ← hrl]; exact hu', ?_⟩
      rw [hdead_u]
      unfold deadRow
      unfold Boundary.alive at hal
      rw [if_neg (by simpa using hal), List.getElem?_eq_getElem hu']
      simpa using (seed_eq u hu' v).mpr he
  have seed_b : ∀ row ∈ rows.val, ∀ v ∈ row.val, v.val < doc.atoms.len.val := by
    intro row hrow v hv
    obtain ⟨u, hu, rfl⟩ := List.getElem_of_mem hrow
    obtain ⟨i, hi, -, -, h2⟩ := (hrm u hu v).mp hv
    rw [← h2]; simpa using (wf.edges _ (List.getElem_mem hi)).2
  have dead_b : ∀ row ∈ dead.val, ∀ v ∈ row.val, v.val < doc.atoms.len.val := by
    intro row hrow v hv
    obtain ⟨u, hu, rfl⟩ := List.getElem_of_mem hrow
    rw [hdead_u u hu] at hv
    unfold deadRow at hv
    have hu' : u < rows.val.length := by rw [hrl, ← hdeadLen]; exact hu
    split_ifs at hv
    · simp at hv
    · rw [List.getElem?_eq_getElem hu'] at hv
      simp only [Option.map_some, Option.getD_some] at hv
      exact seed_b _ (List.getElem_mem hu') v hv
  apply WP.spec_bind (falses_spec _); intro marks hmarks
  apply WP.spec_bind (loop0_spec _); intro indeg hindeg
  apply WP.spec_bind (loop1_spec live rows dead doc.atoms.len (kE doc) (by simp [hliveLen])
    (by simp [hrl]) (by simp [hdeadLen]) (by simpa using hnm) seed_b dead_b seed_eq row_eq marks
    (alloc.vec.Vec.new _) indeg (alloc.vec.Vec.new _) 0#usize 0#usize (by simp)
    (by rw [hmarks, hindeg]; exact l1_init _ _ _))
  rintro ⟨bnd, indeg1, rem, left⟩ ⟨m, hL1⟩
  simp only at hL1
  have hn : doc.atoms.len.val = doc.atoms.val.length := by simp
  rw [hn] at hL1
  have hL1' := hL1
  rw [hlive'] at hL1'
  have hrowsOK : RowsOK (liveL doc) (kE doc) doc.atoms.val.length bnd.val := rowsOK_of hL1'
  have ax := hax bnd.val hrowsOK
  have hremb : ∀ v ∈ rem.val, v.val < indeg1.val.length := by
    intro v hv
    have : v.val ∈ rem.val.map (·.val) := List.mem_map.mpr ⟨v, hv, rfl⟩
    rw [hL1.rem, List.mem_filter, List.mem_range] at this
    rw [hL1.ilen]; exact this.1
  apply WP.spec_bind (Order.frontier_counts_spec indeg1 rem hremb); intro ready hready
  dsimp only
  rw [clone_bool]
  apply WP.spec_bind (show (ok live : Result (alloc.vec.Vec Bool)) ⦃ fun x => x = live ⦄ by simp [WP.spec_ok])
  intro x hx
  subst x
  apply WP.spec_bind (KG.loop2_spec (g := KG.mk doc.atoms.val.length (liveL doc) (rowN bnd.val) D B) ax doc bnd
    ⟨by simpa using hL1.blen, fun u hu => by simp [rowN, List.getElem?_eq_getElem hu]⟩ rfl hnm roomT _
    (KG.inv_init ax doc.atoms.val indeg1 live left ready hlive' (by simp [liveL]) rfl hL1' rfl hready))
  rintro ⟨v1, v2, v3, b⟩ hF
  apply WP.spec_bind (loop3_spec doc.atoms.len v1 b (by rw [hF.rlen]; simp)); intro blocked hbl
  apply WP.spec_bind (loop4_spec v2); intro ordered hord
  have hF' : (KG.mk doc.atoms.val.length (liveL doc) (fun _ => []) D B).Final doc.atoms.val v1.val
      v2.val v3.val b := ⟨hF.lay, hF.lnd, hF.all, hF.rem, hF.blocked, hF.bytes, hF.rlen⟩
  have hbl' : blocked.val.map (·.val) = (List.range doc.atoms.val.length).filter (fun x => b && remF v1.val x) := by
    rw [hbl]; simp
  have hdil : (di.len = 0#usize) ↔ di.val = [] := by
    constructor
    · intro h; have := congrArg UScalar.val h; simpa using this
    · intro h; apply UScalar.eq_of_val_eq; simp [h]
  by_cases h1 : ordered = true <;> by_cases h2 : b = true <;>
    simp only [h1, h2, if_true, if_false, Bool.false_eq_true, ↓reduceIte, WP.spec_ok] <;>
    refine ⟨v1.val, b, hF', hbl', hdi, ?_⟩
  all_goals first
    | (simp [h2]; done)
    | (have ha : v2.val.all (fun L => L.val.length = 1) = true := by rw [← hord]; exact h1
       have hb' : b = false := by simpa using h2
       simp only [ha, hb', Bool.not_false, Bool.true_and, decide_eq_decide, hdil, List.isEmpty_iff]
       rw [Bool.eq_iff_iff]; simp [List.isEmpty_iff])
    | (have ha : v2.val.all (fun L => L.val.length = 1) = false := by rw [← hord]; simpa using h1
       simp [ha])

end o5s_kernel.DocKahn
