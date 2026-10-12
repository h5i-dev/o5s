import DocKernel
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Specifications of the document kernel's commit

`has_pair` (acknowledgement lookup), each committed operation
(`commit_op_spec`), and `commit` as a whole (`commit_spec`): the committed
document's list form is `commitL` of the old one.
-/
namespace o5s_kernel.DocKernel

/-! ## The validation loop -/

theorem validate_loop_spec (doc : document.Doc) (ops : alloc.vec.Vec document.Op) (seen : alloc.vec.Vec Bool)
    (wf : DocWF doc)
    (room : doc.atoms.val.length + (ops.val.map insSize).sum + 2 < Usize.max)
    (room2 : doc.edges.val.length + (ops.val.map (insSize · + 1)).sum + ops.val.length + 2 < Usize.max) :
    document.validate_loop doc.deps doc.creator doc.atoms doc.edges doc.insertions doc.deletions
      doc.restored doc.acked doc.orders doc.retractions ops seen
      ⟨⟨0#u64, 0#usize, 0#usize⟩, alloc.vec.Vec.new _, 0#usize, alloc.vec.Vec.new _⟩ 0#usize ⦃ st =>
      absC st = ops.val.zipIdx.foldl (fun s x => checkOpS s doc seen.val x.2 x.1)
        ⟨(0#u64, 0, 0), [], 0, []⟩ ⦄ := by
  unfold document.validate_loop
  have hl : ops.val.zipIdx.length = ops.val.length := List.length_zipIdx
  apply WP.spec_mono (Indexed.fold_with_inv ops.val.zipIdx absC
    (fun s x => checkOpS s doc seen.val x.2 x.1) (fun s j => VInv doc ops.val s j)
    (fun st => document.validate_loop.body doc.deps doc.creator doc.atoms doc.edges doc.insertions
      doc.deletions doc.restored doc.acked doc.orders doc.retractions ops seen st.1 st.2) ?_ _ 0#usize
    (by simp) ⟨⟨by simp [absC], by simp [absC]⟩, by simp, by simp, by simp⟩)
  · intro r h
    simpa [absC] using h.1
  · intro s i hi hinv
    unfold document.validate_loop.body
    obtain ⟨cw, hc, hp, hr⟩ := hinv
    by_cases hlt : i.val < ops.val.length
    · have hs1 : (List.take (i.val+1) (ops.val.map insSize)).sum ≤ (ops.val.map insSize).sum := sum_take_le' _
      have e1 : (List.take (i.val+1) (ops.val.map insSize)).sum =
          (List.take i.val (ops.val.map insSize)).sum + insSize ops.val[i.val] := by
        rw [List.sum_take_succ _ _ (by simpa using hlt), List.getElem_map]
      have hs2 : (List.take (i.val+1) (ops.val.map (insSize · + 1))).sum ≤ (ops.val.map (insSize · + 1)).sum :=
        sum_take_le' _
      have e2 : (List.take (i.val+1) (ops.val.map (insSize · + 1))).sum =
          (List.take i.val (ops.val.map (insSize · + 1))).sum + (insSize ops.val[i.val] + 1) := by
        rw [List.sum_take_succ _ _ (by simpa using hlt), List.getElem_map]
      have e3 : (ops.val.map (insSize · + 1)).sum = (ops.val.map insSize).sum + ops.val.length := by
        clear e1 e2 hs1 hs2 hc hp hr cw hi hlt room room2
        induction ops.val with
        | nil => simp
        | cons x l ih => simp [ih]; omega
      simp only [← List.map_take] at hc hp hs1 e1 hs2 e2 ⊢
      have hroom : doc.atoms.val.length + s.created.val + insSize ops.val[i.val] + 2 < Usize.max := by omega
      have hroom2 : doc.edges.val.length + s.pending.val.length + s.retracted.val.length +
          insSize ops.val[i.val] + 2 < Usize.max := by
        have : i.val ≤ ops.val.length := by omega
        omega
      h5i_step
      all_goals first
        | (simp only [← o_post, insSize] at hroom hroom2; omega)
        | (simp only [Indexed.FoldInvStep]
           have hdoc : (document.Doc.mk doc.deps doc.creator doc.atoms doc.edges doc.insertions doc.deletions doc.restored doc.acked doc.orders doc.retractions) = doc := rfl
           rw [hdoc] at state1_post
           have hz : i.val < ops.val.zipIdx.length := by simpa using hlt
           refine ⟨hz, ?_, by scalar_tac, ?_⟩
           · rw [state1_post]; try rw [o_post]
             simp only [List.getElem_zipIdx, Nat.zero_add]
           · have hg := checkOpS_growth (absC s) doc seen.val i.val ops.val[i.val]
             try subst o_post
             rw [← state1_post] at hg
             simp only [absC, List.length_map] at hg
             refine ⟨?_, ?_, ?_, ?_⟩
             · rw [state1_post]; exact checkOpS_wf _ doc _ _ _ wf cw
             · rw [i2_post]; simp only [List.map_take]; rw [List.sum_take_succ _ _ (by simpa using hlt), List.getElem_map]; omega
             · rw [i2_post]; simp only [List.map_take]; rw [List.sum_take_succ _ _ (by simpa using hlt), List.getElem_map]; omega
             · rw [i2_post]; omega)
        | (trace_state; sorry)
    · h5i_step
      all_goals first
        | (simp only [Indexed.FoldInvStep]
           have hil : ops.val.zipIdx.length = i.val := by simp; omega
           refine ⟨by omega, trivial, ?_⟩
           rw [hil]
           exact ⟨cw, hc, hp, hr⟩)
        | (simp only [Indexed.FoldInvStep]
           have hil : ops.val.zipIdx.length = i.val := by simp; omega
           refine ⟨by omega, trivial, ?_⟩
           rw [hil]
           unfold VInv
           refine ⟨cw, ?_, ?_, hr⟩
           · simpa [List.map_take] using hc
           · simpa [List.map_take] using hp)
        | (simp only [Indexed.FoldInvStep]
           trace_state
           sorry)


@[step] theorem has_pair_spec (pairs : alloc.vec.Vec (Usize × Usize)) (a b : Usize) :
    document.has_pair pairs a b ⦃ r => r = true ↔ (a.val, b.val) ∈ pairs.val.map pairVal ⦄ := by
  unfold document.has_pair document.has_pair_loop
  apply WP.spec_mono (loop_fold pairs.val (fun f : Bool => f)
    (fun f x => f || (x.1 == a && x.2 == b)) (fun _ _ => True)
    (fun st => document.has_pair_loop.body pairs a b st.1 st.2) ?_ false 0#usize (by simp) trivial)
  · intro r h
    rw [h]
    simp only [List.drop_zero, foldl_or_any (fun x : Usize × Usize => x.1 == a && x.2 == b),
      Bool.false_or, List.any_eq_true, Bool.and_eq_true, beq_iff_eq, List.mem_map, pairVal]
    constructor
    · rintro ⟨x, hx, h1, h2⟩
      exact ⟨x, hx, by rw [h1, h2]⟩
    · rintro ⟨x, hx, he⟩
      simp only [Prod.mk.injEq] at he
      exact ⟨x, hx, UScalar.eq_of_val_eq he.1, UScalar.eq_of_val_eq he.2⟩
  · intro f i hi _
    unfold document.has_pair_loop.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (rw [← found1_post]
         obtain ⟨i2, i3⟩ := found1
         have e1 : ∀ (h : i.val < pairs.val.length), (pairs.val[i.val]'h).1 = i2 := fun h => by
           rw [← found1_post]
         have e2 : ∀ (h : i.val < pairs.val.length), (pairs.val[i.val]'h).2 = i3 := fun h => by
           rw [← found1_post]
         clear found1_post
         h5i_step
         all_goals first | (refine ⟨by scalar_tac, ?_⟩; simp_all) | simp_all | rfl | (by_cases hb : i3 = b <;> simp [hb]) | (exact (beq_eq_decide _ _).symm) | (constructor <;> simp_all <;> by_cases hb : i3 = b <;> simp_all))

/-! ## The commit -/


theorem foldl_triple_snoc (p : Nat) : ∀ (l : List (Nat × Nat)) (acc : List (Nat × Nat × Nat)),
    l.foldl (fun acc e => acc ++ [(e.1, e.2, p)]) acc = acc ++ l.map fun e => (e.1, e.2, p)
  | [], acc => by simp
  | x :: l, acc => by simp [foldl_triple_snoc p l]

theorem commit_insert_loop1_spec (edges : alloc.vec.Vec (Usize × Usize × Usize)) (patch : Usize)
    (links : alloc.vec.Vec (Usize × Usize))
    (room : edges.val.length + links.val.length < Usize.max) :
    document.commit_insert_loop1 edges patch links 0#usize ⦃ v =>
      v.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) =
        edges.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) ++
          links.val.map (fun e => (e.1.val, e.2.val, patch.val)) ⦄ := by
  unfold document.commit_insert_loop1
  apply WP.spec_mono (loop_fold (links.val.map fun e => (e.1.val, e.2.val))
    (fun v : alloc.vec.Vec (Usize × Usize × Usize) => v.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)))
    (fun acc e => acc ++ [(e.1, e.2, patch.val)]) (fun v j => v.val.length = edges.val.length + j)
    (fun st => document.commit_insert_loop1.body patch links st.1 st.2) ?_ edges 0#usize
    (by simp) (by simp))
  · intro v h
    rw [h, show ((0#usize : Usize).val) = 0 by simp, List.drop_zero, foldl_triple_snoc, List.map_map]
    rfl
  · intro v j hj hinv
    unfold document.commit_insert_loop1.body
    h5i_step
    all_goals first
      | (simp_all [FoldStep]; done)
      | (have hjl : j.val < links.val.length := by scalar_tac
         refine ⟨hjl, ?_, ?_, ?_⟩
         · rw [← i1_post]
         · rw [← i1_post]
         · omega)
      | (refine ⟨by simp; scalar_tac, ?_, by scalar_tac, ?_⟩
         · simp_all
         · simp [v1_post]; omega)
      | (trace_state; sorry)

/-- Room for one committed operation. -/
def OpRoom (doc : document.Doc) : document.Op → Prop
  | .Insert _ _ atoms => doc.atoms.val.length + atoms.val.length + 1 < Usize.max ∧
      doc.creator.val.length + atoms.val.length + 1 < Usize.max ∧
      doc.edges.val.length + atoms.val.length + 2 < Usize.max ∧ doc.insertions.val.length + 1 < Usize.max
  | .Delete ts => doc.deletions.val.length + 1 < Usize.max ∧ doc.restored.val.length + 1 < Usize.max ∧
      ts.val.length < Usize.max
  | .Restore d => d.index.val < doc.restored.val.length
  | .Order _ _ => doc.orders.val.length + 1 < Usize.max ∧ doc.edges.val.length + 1 < Usize.max
  | .Keep _ _ => doc.acked.val.length + 1 < Usize.max
  | .Retract o => o.index.val < doc.orders.val.length ∧ doc.retractions.val.length + 1 < Usize.max

theorem commit_op_spec (doc : document.Doc) (patch : Usize) (o : document.Op) (room : OpRoom doc o) :
    document.commit_op doc patch o ⦃ d => absD d = commitOpL (absD doc) patch.val o ⦄ := by
  cases o with
  | Insert l r atoms =>
    simp only [OpRoom] at room
    obtain ⟨r1, r2, r3, r4⟩ := room
    unfold document.commit_op document.commit_insert
    simp only
    apply WP.spec_bind (anchor_index_spec l); intro i hi
    apply WP.spec_bind (anchor_index_spec r); intro i1 hi1
    apply WP.spec_bind (commit_insert_loop0_spec doc.creator doc.atoms patch atoms (by omega) (by omega))
    rintro ⟨v, v1⟩ ⟨hv, hv1⟩
    apply WP.spec_bind (chain_spec (alloc.vec.Vec.new _) i i1 (alloc.vec.Vec.len doc.atoms)
      (alloc.vec.Vec.len atoms) (by simp; omega) (by simp; omega))
    intro links hl
    have hll : links.val.length = atoms.val.length + 1 := by
      have := congrArg List.length hl; simp [chainM_length] at this; omega
    apply WP.spec_bind (commit_insert_loop1_spec doc.edges patch links (by omega))
    intro v2 hv2
    h5i_step
    simp only [absD, commitOpL]
    congr 1
    all_goals first
      | exact hv
      | exact hv1
      | (rw [hv2, List.length_map, ← hl, List.map_map]; rfl)
      | (simp_all; done)
      | (trace_state; sorry)
  | Delete ts =>
    simp only [OpRoom] at room
    obtain ⟨r1, r2, r3⟩ := room
    unfold document.commit_op
    simp only
    apply WP.spec_bind (ref_indices_spec ts); intro v hv
    h5i_step
    simp only [absD, commitOpL]
    congr 1
    all_goals first
      | (simp_all; done)
      | (trace_state; sorry)
  | Restore d =>
    simp only [OpRoom] at room
    unfold document.commit_op
    simp only
    h5i_step
    simp only [absD, commitOpL]
    all_goals first
      | (simp_all; done)
      | (trace_state; sorry)
  | Order b a =>
    simp only [OpRoom] at room
    obtain ⟨r1, r2⟩ := room
    unfold document.commit_op
    simp only
    h5i_step
    simp only [absD, commitOpL]
    all_goals first
      | (simp_all; done)
      | (trace_state; sorry)
  | Keep d i =>
    simp only [OpRoom] at room
    unfold document.commit_op
    simp only
    apply WP.spec_bind (has_pair_spec doc.acked d.index i.index); intro b hb
    by_cases hbt : b = true
    · simp only [hbt, ↓reduceIte]
      have : (d.index.val, i.index.val) ∈ (absD doc).acked := by simpa [absD] using hb.mp hbt
      simp [commitOpL, this]
    · simp only [Bool.not_eq_true] at hbt
      simp only [hbt, Bool.false_eq_true, ↓reduceIte]
      have : (d.index.val, i.index.val) ∉ (absD doc).acked := by
        intro h; exact absurd (hb.mpr (by simpa [absD] using h)) (by simp [hbt])
      h5i_step
      all_goals first
        | (simp only [commitOpL, if_neg this]; simp [absD, v_post, pairVal])
        | (trace_state; sorry)
  | Retract o =>
    simp only [OpRoom] at room
    obtain ⟨r1, r2⟩ := room
    unfold document.commit_op
    simp only
    h5i_step
    all_goals first
      | (simp_all [absD, commitOpL, pairVal, getElem!_pos]; done)
      | (trace_state; sorry)


/-- How much one operation can grow any table. -/
def growth (o : document.Op) : Nat := insSize o + 2

/-- Table sizes of a list-level document. -/
structure Sizes (D0 D : DocL) (g : Nat) : Prop where
  deps : D.deps.length = D0.deps.length
  atoms : D.atoms.length ≤ D0.atoms.length + g
  creator : D.creator.length ≤ D0.creator.length + g
  edges : D.edges.length ≤ D0.edges.length + g
  insertions : D.insertions.length ≤ D0.insertions.length + g
  deletions : D.deletions.length ≤ D0.deletions.length + g
  restored0 : D0.restored.length ≤ D.restored.length
  restored : D.restored.length ≤ D0.restored.length + g
  acked : D.acked.length ≤ D0.acked.length + g
  orders0 : D0.orders.length ≤ D.orders.length
  orders : D.orders.length ≤ D0.orders.length + g
  retractions : D.retractions.length ≤ D0.retractions.length + g

theorem sizes_refl (D : DocL) : Sizes D D 0 := by
  constructor <;> simp

theorem sizes_step (D0 D : DocL) (g p : Nat) (o : document.Op) (h : Sizes D0 D g) :
    Sizes D0 (commitOpL D p o) (g + growth o) := by
  obtain ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12⟩ := h
  cases o with
  | Insert l r atoms =>
    simp only [commitOpL]
    constructor <;> simp [growth, insSize, chainM_length] <;> omega
  | Keep d i =>
    simp only [commitOpL]
    split_ifs <;> constructor <;> (try simp [growth, insSize]) <;> omega
  | Restore d =>
    simp only [commitOpL]
    constructor <;> simp [growth, insSize] <;> omega
  | _ =>
    simp only [commitOpL]
    constructor <;> simp [growth, insSize] <;> omega

theorem sizes_fold (D0 : DocL) (p : Nat) : ∀ (ops : List document.Op) (D : DocL) (g : Nat),
    Sizes D0 D g → Sizes D0 (ops.foldl (fun D o => commitOpL D p o) D) (g + (ops.map growth).sum)
  | [], D, g, h => by simpa using h
  | o :: ops, D, g, h => by
    simp only [List.foldl_cons, List.map_cons, List.sum_cons]
    have := sizes_fold D0 p ops _ _ (sizes_step D0 D g p o h)
    rw [show g + (growth o + (ops.map growth).sum) = g + growth o + (ops.map growth).sum by omega]
    exact this

/-- Room for a whole patch: every table can take its growth, and restored or
retracted entries already exist. -/
structure CommitRoom (d0 : document.Doc) (ops : List document.Op) : Prop where
  room : ∀ n ∈ [d0.atoms.val.length, d0.creator.val.length, d0.edges.val.length,
    d0.insertions.val.length, d0.deletions.val.length, d0.restored.val.length, d0.acked.val.length,
    d0.orders.val.length, d0.retractions.val.length], n + (ops.map growth).sum + 2 < Usize.max
  deletes : ∀ ts, document.Op.Delete ts ∈ ops → ts.val.length < Usize.max
  restores : ∀ d, document.Op.Restore d ∈ ops → d.index.val < d0.restored.val.length
  retracts : ∀ o, document.Op.Retract o ∈ ops → o.index.val < d0.orders.val.length

theorem commitOpL_lengths (D : DocL) (p : Nat) (o : document.Op) :
    (commitOpL D p o).deps = D.deps := by
  cases o <;> simp only [commitOpL] <;> split_ifs <;> rfl

/-- Room for committing `ops` to a document whose list form is `D0`. -/
structure CommitRoomL (D0 : DocL) (ops : List document.Op) : Prop where
  big : D0.atoms.length + D0.creator.length + D0.edges.length + D0.insertions.length +
    D0.deletions.length + D0.restored.length + D0.acked.length + D0.orders.length +
    D0.retractions.length + (ops.map growth).sum + 2 < Usize.max
  deletes : ∀ ts, document.Op.Delete ts ∈ ops → ts.val.length < Usize.max
  restores : ∀ d, document.Op.Restore d ∈ ops → d.index.val < D0.restored.length
  retracts : ∀ o, document.Op.Retract o ∈ ops → o.index.val < D0.orders.length

theorem opRoom_of_sizes (d0 d : document.Doc) (g : Nat) (ops : List document.Op) (o : document.Op)
    (hs : Sizes (absD d0) (absD d) g) (cr : CommitRoomL (absD d0) ops) (ho : o ∈ ops)
    (hg : g + growth o ≤ (ops.map growth).sum) : OpRoom d o := by
  obtain ⟨h1, h2, h3, h4, h5, h6, h7, h8, h9, h10, h11, h12⟩ := hs
  have big := cr.big
  simp only [absD, List.length_map] at h1 h2 h3 h4 h5 h6 h7 h8 h9 h10 h11 h12 big
  cases o with
  | Insert l r atoms =>
    simp only [growth, insSize] at hg
    refine ⟨by omega, by omega, by omega, by omega⟩
  | Delete ts => exact ⟨by simp only [growth, insSize] at hg; omega, by simp only [growth, insSize] at hg; omega,
      cr.deletes ts ho⟩
  | Restore x =>
    have := cr.restores x ho
    simp only [absD] at this
    show x.index.val < d.restored.val.length
    omega
  | Order b a => exact ⟨by simp only [growth, insSize] at hg; omega, by simp only [growth, insSize] at hg; omega⟩
  | Keep x y => show d.acked.val.length + 1 < Usize.max; simp only [growth, insSize] at hg; omega
  | Retract x =>
    have := cr.retracts x ho
    simp only [absD, List.length_map] at this
    exact ⟨by omega, by simp only [growth, insSize] at hg; omega⟩

theorem sum_take_succ_growth (ops : List document.Op) (j : Nat) (hj : j < ops.length) :
    ((ops.take j).map growth).sum + growth ops[j] ≤ (ops.map growth).sum := by
  have h1 := sum_take_le' (l := ops.map growth) (j + 1)
  rw [List.sum_take_succ _ _ (by simpa using hj), List.getElem_map] at h1
  simpa [List.map_take] using h1

theorem commit_loop1_spec (doc0 : document.Doc) (ops : alloc.vec.Vec document.Op) (patch : Usize)
    (cr : CommitRoomL (absD doc0) ops.val) :
    document.commit_loop1 doc0 ops patch 0#usize ⦃ d =>
      absD d = ops.val.foldl (fun D o => commitOpL D patch.val o) (absD doc0) ⦄ := by
  unfold document.commit_loop1
  apply WP.spec_mono (loop_fold ops.val absD (fun D o => commitOpL D patch.val o)
    (fun d j => absD d = (ops.val.take j).foldl (fun D o => commitOpL D patch.val o) (absD doc0))
    (fun st => document.commit_loop1.body ops patch st.1 st.2) ?_ doc0 0#usize (by simp) (by simp))
  · intro d h
    rw [h, show ((0#usize : Usize).val) = 0 by simp, List.drop_zero]
  · intro d j hj hinv
    unfold document.commit_loop1.body
    by_cases hlt : j.val < ops.val.length
    · have hs := sizes_fold (absD doc0) patch.val (ops.val.take j.val) (absD doc0) 0 (sizes_refl _)
      rw [← hinv, Nat.zero_add] at hs
      have hroom := opRoom_of_sizes doc0 d _ ops.val ops.val[j.val] hs cr (List.getElem_mem _)
        (sum_take_succ_growth _ _ hlt)
      h5i_step
      all_goals first
        | (apply WP.spec_bind (commit_op_spec d patch _ hroom)
           intro doc1 hdoc1
           h5i_step
           rw [List.take_succ, List.getElem?_eq_getElem hlt, Option.toList_some, List.foldl_append,
             List.foldl_cons, List.foldl_nil])
    · h5i_step
      all_goals first
        | (simp only [FoldStep]; exact ⟨by omega, rfl⟩)
        | (trace_state; sorry)

theorem foldl_pred_const (p : Nat) : ∀ (ops : List document.Op) (D : DocL), D.deps.length = p + 1 →
    ops.foldl (fun D o => commitOpL D D.deps.length.pred o) D = ops.foldl (fun D o => commitOpL D p o) D
  | [], D, _ => rfl
  | o :: ops, D, h => by
    simp only [List.foldl_cons]
    rw [h, Nat.pred_succ]
    exact foldl_pred_const p ops _ (by rw [commitOpL_lengths]; exact h)

theorem commit_loop0_spec (p : document.Patch) :
    document.commit_loop0 p (alloc.vec.Vec.new Usize) 0#usize ⦃ r => r.1 = p.ops ∧ r.2.val = p.deps.val ⦄ := by
  unfold document.commit_loop0
  apply loop_idx_spec (fun x : alloc.vec.Vec Usize × Usize => document.commit_loop0.body p x.1 x.2)
    (fun x => x.2) p.deps.val.length (fun x => x.1.val = p.deps.val.take x.2.val)
  · rintro ⟨deps, d⟩ hinv hle
    simp only at hinv hle
    unfold document.commit_loop0.body
    h5i_step
    all_goals first
      | (simp_all; done)
      | (refine ⟨?_, by scalar_tac, by scalar_tac⟩
         rename_i h _ i1 _ i1_post deps1 _ deps1_post
         simp only [deps1_post, hinv, i1_post]
         rw [List.take_succ, List.getElem?_eq_getElem (by scalar_tac)]
         simp)
      | (refine ⟨rfl, ?_⟩
         rw [hinv]
         exact List.take_of_length_le (by scalar_tac))
      | (trace_state; sorry)
  · simp
  · simp

theorem commit_spec (doc : document.Doc) (p : document.Patch)
    (room : doc.deps.val.length + 1 < Usize.max) (cr : CommitRoomL (absD doc) p.ops.val) :
    document.commit doc p ⦃ d => absD d = commitL (absD doc) (p.deps.val.map (·.val)) p.ops.val ⦄ := by
  unfold document.commit
  apply WP.spec_bind (commit_loop0_spec p)
  rintro ⟨ops, deps⟩ ⟨hops, hdeps⟩
  simp only at hops hdeps
  subst hops
  h5i_step
  apply WP.spec_mono (commit_loop1_spec { doc with deps := v1 } p.ops doc.deps.len
    ⟨cr.big, cr.deletes, cr.restores, cr.retracts⟩)
  intro d hd
  rw [hd]
  unfold commitL
  rw [foldl_pred_const doc.deps.val.length]
  · congr 1
    simp [absD, v1_post, hdeps]
  · simp [absD]

end o5s_kernel.DocKernel
