import Document
import DocKernel
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# How the dense document represents a history (draft)

`Rep d H` says that every field of the kernel document `d` is the dense
encoding of the history `H`, under the interning `Document.lean` fixes:
patches, anchors, insertions, deletions and orders in arrival order.
`Replayed d H` implies `Rep d H` (`rep_replayed`); validation, commit and
materialization are proved against `Rep`.
-/
namespace o5s_kernel.DocRep
open DocModel Document

/-! ## Refusal codes -/

theorem unknown_deletion_ne : document.UNKNOWN_DELETION.val ≠ 0 := by
  unfold document.UNKNOWN_DELETION; simp

theorem unknown_deletion_ne_unobs : document.UNKNOWN_DELETION ≠ document.UNOBSERVED_PATCH := by
  unfold document.UNKNOWN_DELETION document.UNOBSERVED_PATCH; decide

theorem unknown_order_ne_unobs : document.UNKNOWN_ORDER ≠ document.UNOBSERVED_PATCH := by
  unfold document.UNKNOWN_ORDER document.UNOBSERVED_PATCH; decide

theorem usize_exists (k : Nat) (h : k ≤ Usize.max) : ∃ w : Usize, w.val = k := by
  have hm : Usize.max < 2 ^ UScalarTy.Usize.numBits := by simp [Usize.max, Usize.numBits]
  exact ⟨UScalar.ofNatCore k (by omega), by simp⟩

/-! ## Dense indices -/

theorem idx_of_mem {α} [DecidableEq α] {l : List α} {x : α} (h : x ∈ l) : idx l x = l.idxOf x := by
  simp [idx, h]

theorem idx_lt {α} [DecidableEq α] {l : List α} {x : α} (h : x ∈ l) : idx l x < l.length := by
  rw [idx_of_mem h]; exact List.idxOf_lt_length_iff.mpr h

theorem getElem_idx {α} [DecidableEq α] {l : List α} {x : α} (h : x ∈ l) :
    l[idx l x]'(idx_lt h) = x := by
  simp only [idx_of_mem h]; exact List.getElem_idxOf (List.idxOf_lt_length_iff.mpr h)

theorem idx_getElem {α} [DecidableEq α] {l : List α} (hn : l.Nodup) (k : Nat) (hk : k < l.length) :
    idx l l[k] = k := by
  rw [idx_of_mem (List.getElem_mem hk)]; exact List.Nodup.idxOf_getElem hn k hk

/-- Anchor index of an anchor. -/
def aIdx (H : Hist) (a : Anchor) : Nat := idx (anchorList H) a

/-- The encoded order edges: the sentinel, then every fact. -/
def encEdges (H : Hist) : List (Nat × Nat × Nat) :=
  (0, 1, NONE) :: (facts H).map fun f => (aIdx H f.1, aIdx H f.2.1, idx (ids H) f.2.2.patch)

/-- The encoded insertions. -/
def insertionsOf (H : Hist) : List (Nat × Nat × Nat) :=
  (ops H).filterMap fun
    | (o, .insert l r _) => some (idx (ids H) o.patch, aIdx H l, aIdx H r)
    | _ => none

/-- The encoded deletions. -/
def deletionsOf (H : Hist) : List (Nat × List Nat) :=
  (ops H).filterMap fun
    | (o, .delete ts) => some (idx (ids H) o.patch, ts.map fun n => aIdx H (.node n))
    | _ => none

/-- The edge index of order operation `o`: its fact's position plus the sentinel. -/
def orderEdge (H : Hist) (o : OpId) : Nat :=
  match (ops H).find? (fun x => x.1 = o) with
  | some (_, .order b a) => 1 + (facts H).idxOf (.node b, .node a, o)
  | _ => NONE

/-- The encoded retractions. -/
def retractionsOf (H : Hist) : List (Nat × Nat) :=
  (ops H).filterMap fun
    | (o, .retract r) => some (orderEdge H r, idx (ids H) o.patch)
    | _ => none

open Classical in
/-- The list-level document `D` encodes the history `H`. -/
structure RepL (D : DocKernel.DocL) (H : Hist) : Prop where
  deps : D.deps = H.map fun P => P.deps.map (idx (ids H))
  atoms : D.atoms = [] :: [] :: (DocModel.atoms H).map fun a => a.2.map (·.toNat)
  creator : D.creator = NONE :: NONE :: (DocModel.atoms H).map fun a => idx (ids H) a.1.op.patch
  edges : D.edges = encEdges H
  insertions : D.insertions = insertionsOf H
  deletions : D.deletions = deletionsOf H
  restored : D.restored = (deletionList H).map fun x => decide (Restored H x)
  acked : ∀ a b, (a, b) ∈ D.acked ↔
    ∃ dd ii, Acked H dd ii ∧ a = idx (deletionList H) dd ∧ b = idx (insertionList H) ii
  orders : D.orders = (orderList H).map (orderEdge H)
  retractions : D.retractions = retractionsOf H

/-- The kernel document `d` encodes `H`. -/
def Rep (d : document.Doc) (H : Hist) : Prop := RepL (DocKernel.absD d) H

/-- A history the adapter built: unique identities, closed dependencies, and
every patch admitted. -/
def Valid (H : Hist) : Prop := replay [] H = .ok H

/-! ## Validation, through the representation (statements) -/

/-- The kernel's validation state encodes the model's `Own`: pending edges are
own edges in the anchor indices of `H ++ [P]`, retracted edges are the edge
indices of retracted orders. -/
def TrOwn (H : Hist) (P : Patch) (st : DocKernel.CheckS) (own : Own) : Prop :=
  st.pending = own.edges.map (fun e => (aIdx (H ++ [P]) e.1, aIdx (H ++ [P]) e.2)) ∧
  st.retracted = own.retracted.map (orderEdge H)

/-- Dependency rows reach exactly the causal past (in indices). -/
theorem rowReach_iff_observed (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P) (v : Usize)
    (hv' : v.val < H.length) :
    Indexed.RowReach d.deps.val dp.deps.val v ↔ Observed H P.deps (H[v.val]'hv').id := by
  obtain ⟨cl, un, -⟩ := replay_props hv (by simp [Closed]) (by simp [ids])
  have hlen : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hidsl : (ids H).length = H.length := by simp [ids]
  -- ids of H, positionally
  have idsget : ∀ k (hk : k < H.length), (ids H)[k]'(by omega) = H[k].id := by
    intro k hk; simp [ids]
  -- a dependency row lists the indices of the patch's dependencies
  have row : ∀ k (hk : k < H.length) (w : Usize), (∃ h : k < d.deps.val.length, w ∈ d.deps.val[k].val) ↔
      ∃ q ∈ H[k].deps, w.val = idx (ids H) q := by
    intro k hk w
    have e := congrArg (fun l => l[k]?) (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps)
    simp only [List.getElem?_map] at e
    rw [List.getElem?_eq_getElem (by omega), List.getElem?_eq_getElem hk] at e
    simp only [Option.map_some, Option.some.injEq] at e
    constructor
    · rintro ⟨h, hw⟩
      have : w.val ∈ (d.deps.val[k]).val.map (·.val) := List.mem_map_of_mem hw
      rw [e] at this
      obtain ⟨q, hq, e2⟩ := List.mem_map.mp this
      exact ⟨q, hq, e2.symm⟩
    · rintro ⟨q, hq, e2⟩
      refine ⟨by omega, ?_⟩
      have : idx (ids H) q ∈ (d.deps.val[k]'(by omega)).val.map (·.val) := by
        rw [e]; exact List.mem_map_of_mem hq
      obtain ⟨w', hw', e3⟩ := List.mem_map.mp this
      have : w' = w := UScalar.eq_of_val_eq (by rw [e3, e2])
      exact this ▸ hw'
  -- the patch's own dependencies
  have seeds : ∀ w : Usize, w ∈ dp.deps.val ↔ ∃ q ∈ P.deps, w.val = idx (ids H) q := by
    intro w
    constructor
    · intro hw
      have : w.val ∈ dp.deps.val.map (·.val) := List.mem_map_of_mem hw
      rw [ht.1] at this
      obtain ⟨q, hq, e⟩ := List.mem_map.mp this
      exact ⟨q, hq, e.symm⟩
    · rintro ⟨q, hq, e⟩
      have : idx (ids H) q ∈ dp.deps.val.map (·.val) := by rw [ht.1]; exact List.mem_map_of_mem hq
      obtain ⟨w', hw', e3⟩ := List.mem_map.mp this
      have : w' = w := UScalar.eq_of_val_eq (by rw [e3, e])
      exact this ▸ hw'
  -- a member id's index names it back
  have back : ∀ q, q ∈ ids H → ∀ (h : idx (ids H) q < H.length), H[idx (ids H) q].id = q := by
    intro q hq h
    rw [← idsget _ h]; exact getElem_idx hq
  have mem_ids : ∀ k (hk : k < H.length), H[k].id ∈ ids H := fun k hk => List.mem_map_of_mem (List.getElem_mem hk)
  have uniq : ∀ k (hk : k < H.length) (S : Patch), S ∈ H → S.id = H[k].id → H[k] = S := by
    intro k hk S hS e
    unfold ids at un
    exact List.inj_on_of_nodup_map un (List.getElem_mem hk) hS e.symm
  have usz : ∀ k, k < H.length → ∃ w : Usize, w.val = k := fun k hk =>
    usize_exists k (by have := d.deps.property; omega)
  constructor
  · intro h
    induction h with
    | seed hw =>
      rename_i w
      obtain ⟨q, hq, e⟩ := (seeds w).mp hw
      have hqi : q ∈ ids H := adm.2.2.2.1 q hq
      left
      have : H[w.val].id = q := by
        have h2 := back q hqi (by rw [← e]; exact hv')
        simpa only [← e] using h2
      rw [this]; exact hq
    | edge hu hu' hw ih =>
      rename_i u w
      have hub : u.val < H.length := by omega
      obtain ⟨q, hq, e⟩ := (row u.val hub w).mp ⟨hu', hw⟩
      have hqi : q ∈ ids H := cl _ (List.getElem_mem hub) q hq
      have hwq : H[w.val].id = q := by
        have h2 := back q hqi (by rw [← e]; exact hv')
        simpa only [← e] using h2
      have edge1 : Anc H q H[u.val].id := .single ⟨H[u.val], List.getElem_mem hub, rfl, hq⟩
      rw [hwq]
      have ih' : Observed H P.deps (H[u.val]'hub).id := by
        first | exact ih hub idsget | exact ih hub | exact ih idsget | exact ih
      rcases ih' with h1 | ⟨dd, hdd, ha⟩
      · exact Or.inr ⟨_, h1, edge1⟩
      · exact Or.inr ⟨dd, hdd, edge1.trans ha⟩
  · intro h
    have claim : ∀ q, Observed H P.deps q → q ∈ ids H → ∀ w : Usize, w.val = idx (ids H) q →
        Indexed.RowReach d.deps.val dp.deps.val w := by
      intro q hq hqi w hw
      rcases hq with hq | ⟨dd, hdd, ha⟩
      · exact .seed ((seeds w).mpr ⟨q, hq, hw⟩)
      · have hddi : dd ∈ ids H := adm.2.2.2.1 dd hdd
        have hk : idx (ids H) dd < H.length := by rw [← hidsl]; exact idx_lt hddi
        obtain ⟨wd, hwd⟩ := usz _ hk
        have rd : Indexed.RowReach d.deps.val dp.deps.val wd := .seed ((seeds wd).mpr ⟨dd, hdd, hwd⟩)
        induction ha using Relation.TransGen.head_induction_on generalizing w with
        | single hs =>
          rename_i x
          obtain ⟨S, hS, eS, hxS⟩ := hs
          have hSk := uniq _ hk S hS (by rw [back dd hddi hk, eS])
          have hrow := (row _ hk w).mpr ⟨x, hSk ▸ hxS, hw⟩
          exact .edge rd (by rw [hwd]; omega) (by simpa only [hwd] using hrow.2)
        | head hs ht ih =>
          rename_i x c
          obtain ⟨S, hS, eS, hxS⟩ := hs
          have hci : c ∈ ids H := eS ▸ List.mem_map_of_mem hS
          have hck : idx (ids H) c < H.length := by rw [← hidsl]; exact idx_lt hci
          obtain ⟨wc, hwc⟩ := usz _ hck
          have rc := ih hci wc hwc
          have hSk := uniq _ hck S hS (by rw [back c hci hck, eS])
          have hrow := (row _ hck w).mpr ⟨x, hSk ▸ hxS, hw⟩
          exact .edge rc (by rw [hwc]; omega) (by simpa only [hwc] using hrow.2)
    exact claim _ h (mem_ids _ hv') v (by
      rw [← idsget _ hv', idx_getElem (by unfold ids at un; exact un) _ (by omega)])

theorem mem_anchorList_node (H : Hist) (n : NodeId) : Anchor.node n ∈ anchorList H ↔ DocModel.IsAtom H n := by
  unfold anchorList DocModel.IsAtom DocModel.atom
  rw [Ne, lookup_none]
  simp only [List.cons_append, List.nil_append, List.mem_cons, reduceCtorEq, false_or, List.mem_map,
    Anchor.node.injEq]
  push_neg
  constructor
  · rintro ⟨⟨m, b⟩, hm, rfl⟩; exact ⟨b, hm⟩
  · rintro ⟨b, hb⟩; exact ⟨(n, b), hb, rfl⟩

theorem idx_node_ge (H : Hist) (n : NodeId) (h : Anchor.node n ∈ anchorList H) :
    2 ≤ aIdx H (.node n) := by
  unfold aIdx
  rw [idx_of_mem h]
  unfold anchorList
  simp [List.idxOf_cons]

/-- What a valid history's patch observes is in the history. -/
theorem observed_mem (H : Hist) (P : Patch) (hv : Valid H) (adm : Admissible H P) (q : PId)
    (h : Observed H P.deps q) : q ∈ ids H := by
  obtain ⟨cl, -⟩ := replay_props hv (by simp [Closed]) (by simp [ids])
  rcases h with h | ⟨dd, -, ha⟩
  · exact adm.2.2.2.1 q h
  · obtain ⟨c, ⟨S, hS, -, hq⟩, -⟩ := Relation.TransGen.head'_iff.mp ha
    exact cl S hS q hq

/-- The kernel's `seen` marks are the model's observation. -/
theorem seen_iff (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv' : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (k : Nat) (hk : k < H.length) :
    (DocKernel.seenM d.deps.val dp.deps.val)[k]? = some true ↔ Observed H P.deps H[k].id := by
  have hlen : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hk' : k < d.deps.val.length := by omega
  simp only [DocKernel.seenM, List.getElem?_map, List.getElem?_range hk', Option.map_some,
    Option.some.injEq, decide_eq_true_eq]
  constructor
  · rintro ⟨v, hv, h⟩
    have := (rowReach_iff_observed d H P dp hr hv' ht adm v (by omega)).mp h
    simpa only [hv] using this
  · intro h
    obtain ⟨w, hw⟩ := usize_exists k (by have := d.deps.property; omega)
    refine ⟨w, hw, ?_⟩
    apply (rowReach_iff_observed d H P dp hr hv' ht adm _ (by rw [hw]; omega)).mpr
    simpa only [hw] using h


/-- The kernel's anchor check is the model's, through the translation. -/
theorem checkAnchor_tr (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (a : Anchor) (a' : document.Anchor) (hta : TrAnchor H a a') :
    (DocKernel.checkAnchorM a' d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val) = 0#u64 ↔
      checkAnchor H P.deps a = none) := by
  obtain ⟨-, un, -⟩ := replay_props hv (by simp [Closed]) (by simp [ids])
  have hdl : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hal : d.atoms.val.length = (anchorList H).length := by
    have := congrArg List.length (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms); simpa [anchorList] using this
  have hsl : (DocKernel.seenM d.deps.val dp.deps.val).length = H.length := by
    simp [DocKernel.seenM, hdl]
  have hidsl : (ids H).length = H.length := by simp [ids]
  have hdmax : H.length ≤ Usize.max := by have := d.deps.property; omega
  have hamax : (anchorList H).length ≤ Usize.max := by have := d.atoms.property; omega
  cases a with
  | start => cases a' <;> simp_all [TrAnchor, DocKernel.checkAnchorM, checkAnchor]
  | stop => cases a' <;> simp_all [TrAnchor, DocKernel.checkAnchorM, checkAnchor]
  | node n =>
    cases a' with
    | Node r =>
      obtain ⟨hp, hi⟩ := hta
      unfold DocKernel.checkAnchorM checkAnchor
      simp only
      -- the patch index is in range exactly when the patch is in `H`
      have patch_iff : n.op.patch ∈ ids H ↔ r.patch ≠ document.NONE := by
        constructor
        · intro hq e
          have := idx_lt hq
          rw [← hp, e] at this
          simp [document.NONE] at this
          omega
        · intro hne
          by_contra hq
          apply hne
          apply UScalar.eq_of_val_eq
          rw [hp]; simp [idx, hq, NONE, document.NONE]
      have atom_iff : DocModel.IsAtom H n ↔ r.index.val < (anchorList H).length := by
        rw [← mem_anchorList_node]
        constructor
        · intro hm; rw [hi]; exact idx_lt hm
        · intro hlt
          by_contra hm
          rw [hi] at hlt
          simp [idx, hm, NONE] at hlt
          have hb : d.atoms.val.length ≤ Usize.max := by scalar_tac
          omega
      constructor
      · intro h
        split_ifs at h with hz
        · simp [document.UNKNOWN_ANCHOR] at h
        · have hz' := DocKernel.checkRefM_zero (missing := document.UNKNOWN_ANCHOR) DocKernel.unknown_anchor_ne (by rw [h]; rfl)
          obtain ⟨hil, hin, hpn, hpl, hs⟩ := hz'
          have hq := patch_iff.mpr hpn
          have hk : idx (ids H) n.op.patch < H.length := by rw [← hidsl]; exact idx_lt hq
          have obs : Observed H P.deps n.op.patch := by
            have e := (seen_iff d H P dp hr hv ht adm _ hk).mp (by
              rw [← hp]; exact List.getElem?_eq_some_iff.mpr ⟨hpl, hs⟩)
            rwa [show H[idx (ids H) n.op.patch].id = n.op.patch by
              have := getElem_idx hq; simpa [ids] using this] at e
          have hat : DocModel.IsAtom H n := atom_iff.mpr (by rw [← hal]; exact hil)
          simp [obs, hat]
      · intro h
        split_ifs at h with h1 h2
        all_goals simp at h
        have obs : Observed H P.deps n.op.patch := by first | exact (by first | exact not_not.mp h1 | exact h1) | exact h1
        have hat : DocModel.IsAtom H n := by first | exact (by first | exact not_not.mp h2 | exact h2) | exact h2
        have hq := observed_mem H P hv adm _ obs
        have hk : idx (ids H) n.op.patch < H.length := by rw [← hidsl]; exact idx_lt hq
        have hseen := (seen_iff d H P dp hr hv ht adm _ hk).mpr (by
          rwa [show H[idx (ids H) n.op.patch].id = n.op.patch by
            have := getElem_idx hq; simpa [ids] using this])
        have hil := atom_iff.mp hat
        have hge := idx_node_ge H n ((mem_anchorList_node H n).mpr hat)
        have hzero : DocKernel.checkRefM r (DocKernel.seenM d.deps.val dp.deps.val) d.atoms.val.length
            document.UNKNOWN_ANCHOR = 0#u64 := by
          unfold DocKernel.checkRefM
          have hpn := patch_iff.mp hq
          have hpl : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length := by
            rw [hsl, hp]; exact hk
          rw [if_neg hpn, dif_pos hpl]
          have : (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = true := by
            have := hseen
            rw [← hp] at this
            obtain ⟨_, h'⟩ := List.getElem?_eq_some_iff.mp this
            exact h'
          rw [if_pos this, if_neg]
          push_neg
          refine ⟨?_, by rw [hal]; exact hil⟩
          intro e
          have := congrArg (·.val) e
          simp [document.NONE] at this
          omega
        rw [hzero]
        simp only [true_and, ite_eq_right_iff]
        intro hlt
        unfold aIdx at hge
        rw [← hi] at hge
        omega
    | _ => simp [TrAnchor] at hta

/-- Every operation of a validated patch passed its check against some state. -/
theorem foldlM_ok {σ α ε} (f : σ → α → Except ε σ) :
    ∀ (l : List α) (init r : σ), l.foldlM f init = .ok r → ∀ x ∈ l, ∃ s s', f s x = .ok s'
  | [], _, _, _ => by simp
  | a :: l, init, r, h => by
    rw [List.foldlM_cons] at h
    cases hf : f init a with
    | error e => rw [hf] at h; cases h
    | ok s =>
      rw [hf] at h
      intro x hx
      rcases List.mem_cons.mp hx with rfl | hx
      · exact ⟨init, s, hf⟩
      · exact foldlM_ok f l s r h x hx

theorem validate_each_op (H : Hist) (P : Patch) (h : DocModel.validate H P = .ok ()) :
    ∀ x ∈ P.ops.zipIdx, ∃ own own', validOp H P.id P.deps own x.2 x.1 = .ok own' := by
  unfold DocModel.validate at h
  cases hfold : P.ops.zipIdx.foldlM (fun own (o, k) => validOp H P.id P.deps own k o) {} with
  | error e => rw [hfold] at h; cases h
  | ok r =>
    intro x hx
    obtain ⟨s, s', hs⟩ := foldlM_ok _ _ _ _ hfold x hx
    exact ⟨s, s', hs⟩

/-- A history built patch by patch by the adapter. -/
inductive Built : Hist → Prop
  | nil : Built []
  | snoc {H P} : Built H → Admissible H P → DocModel.validate H P = .ok () → Built (H ++ [P])

theorem replayed_built {d : document.Doc} {H : Hist} (h : Replayed d H) : Built H := by
  induction h with
  | nil _ => exact .nil
  | snoc _ _ adm hval _ _ ih => exact .snoc ih adm hval

theorem anchorList_mono (H : Hist) (P : Patch) (a : Anchor) (h : a ∈ anchorList H) :
    a ∈ anchorList (H ++ [P]) := by
  unfold anchorList at h ⊢
  simp only [List.cons_append, List.nil_append, List.mem_cons, List.mem_map] at h ⊢
  rcases h with h | h | ⟨x, hx, e⟩
  · exact Or.inl h
  · exact Or.inr (Or.inl h)
  · refine Or.inr (Or.inr ⟨x, ?_, e⟩)
    unfold DocModel.atoms at hx ⊢
    rw [ops_append, List.flatMap_append]
    exact List.mem_append_left _ hx

/-- An insertion or order that passed validation refers to atoms (or sentinels). -/
theorem validOp_anchors (H : Hist) (self : PId) (deps : List PId) (own own' : Own) (k : Nat) (o : Op)
    (h : validOp H self deps own k o = .ok own') :
    ∀ e ∈ DocModel.edgesOf (⟨self, k⟩, o),
      (e.1 ∈ anchorList H ∨ ∃ l r xs j, o = .insert l r xs ∧ j < xs.length ∧ e.1 = .node ⟨⟨self, k⟩, j⟩) ∧
      (e.2 ∈ anchorList H ∨ ∃ l r xs j, o = .insert l r xs ∧ j < xs.length ∧ e.2 = .node ⟨⟨self, k⟩, j⟩) := by
  have mem_of_check : ∀ a, checkAnchor H deps a = none → a ∈ anchorList H := by
    intro a ha
    cases a with
    | start => simp [anchorList]
    | stop => simp [anchorList]
    | node n =>
      unfold checkAnchor at ha
      simp only at ha
      split_ifs at ha with h1 h2
      exact (mem_anchorList_node H n).mpr (by first | exact (by first | exact not_not.mp h2 | exact h2) | exact h2)
  cases o with
  | insert l r xs =>
    have hl : checkAnchor H deps l = none := by
      cases hc : checkAnchor H deps l with
      | none => rfl
      | some e => simp [validOp, hc] at h
    have hr : checkAnchor H deps r = none := by
      cases hc : checkAnchor H deps r with
      | none => rfl
      | some e => simp [validOp, hl, hc] at h
    intro e he
    simp only [DocModel.edgesOf] at he
    obtain ⟨h1, h2⟩ := List.of_mem_zip he
    constructor
    · rcases List.mem_cons.mp h1 with h1 | h1
      · left; rw [h1]; exact mem_of_check l hl
      · right; simp only [List.mem_map, List.mem_range] at h1; obtain ⟨j, hj', hj⟩ := h1
        exact ⟨l, r, xs, j, rfl, hj', hj.symm⟩
    · rcases List.mem_append.mp h2 with h2 | h2
      · right; simp only [List.mem_map, List.mem_range] at h2; obtain ⟨j, hj', hj⟩ := h2
        exact ⟨l, r, xs, j, rfl, hj', hj.symm⟩
      · left; simp at h2; rw [h2]; exact mem_of_check r hr
  | order b a =>
    have hb : checkAnchor H deps (.node b) = none := by
      cases hc : checkAnchor H deps (.node b) with
      | none => rfl
      | some e => simp [validOp, hc] at h
    have ha : checkAnchor H deps (.node a) = none := by
      cases hc : checkAnchor H deps (.node a) with
      | none => rfl
      | some e => simp [validOp, hb, hc] at h
    intro e he
    simp only [DocModel.edgesOf, List.mem_singleton] at he
    subst he
    exact ⟨Or.inl (mem_of_check _ hb), Or.inl (mem_of_check _ ha)⟩
  | _ => simp [DocModel.edgesOf]

/-- Every edge fact of a built history joins anchors of that history. -/
theorem facts_anchors (H : Hist) (hb : Built H) :
    ∀ f ∈ facts H, f.1 ∈ anchorList H ∧ f.2.1 ∈ anchorList H := by
  induction hb with
  | nil => simp [facts, ops]
  | snoc hb adm hval ih =>
    rename_i H P
    intro f hf
    unfold facts at hf
    rw [ops_append, List.flatMap_append, List.mem_append] at hf
    rcases hf with hf | hf
    · obtain ⟨h1, h2⟩ := ih f (by unfold facts; exact hf)
      exact ⟨anchorList_mono H P _ h1, anchorList_mono H P _ h2⟩
    · obtain ⟨oo, hoo, hm⟩ := List.mem_flatMap.mp hf
      obtain ⟨e, he, rfl⟩ := List.mem_map.mp hm
      obtain ⟨o, op⟩ := oo
      have hP := mem_opsOf_iff.mp hoo
      obtain ⟨own, own', hv⟩ := validate_each_op H P hval (op, o.ordinal)
        (List.mem_zipIdx_iff_getElem?.mpr hP.2)
      have ho : o = ⟨P.id, o.ordinal⟩ := by cases o; simp_all
      rw [ho] at he
      have new_atom : ∀ l r xs j, op = .insert l r xs → j < xs.length →
          Anchor.node ⟨⟨P.id, o.ordinal⟩, j⟩ ∈ anchorList (H ++ [P]) := by
        intro l r xs j hop hj
        apply (mem_anchorList_node _ _).mpr
        unfold DocModel.IsAtom DocModel.atom
        rw [Ne, lookup_none]
        push_neg
        refine ⟨xs[j], ?_⟩
        unfold DocModel.atoms
        apply List.mem_flatMap.mpr
        refine ⟨(⟨P.id, o.ordinal⟩, op), ?_, ?_⟩
        · rw [ops_append]; apply List.mem_append_right; rw [← ho]; exact hoo
        · rw [hop]
          simp only [atomsOf, List.mem_map]
          exact ⟨(xs[j], j), List.mem_zipIdx_iff_getElem?.mpr (by simp [hj]), rfl⟩
      obtain ⟨h1, h2⟩ := validOp_anchors H P.id P.deps own own' o.ordinal op hv e he
      refine ⟨?_, ?_⟩
      · rcases h1 with h1 | ⟨l, r, xs, j, hop, hj, e1⟩
        · exact anchorList_mono H P _ h1
        · simp only; rw [e1]; exact new_atom l r xs j hop hj
      · rcases h2 with h2 | ⟨l, r, xs, j, hop, hj, e2⟩
        · exact anchorList_mono H P _ h2
        · simp only; rw [e2]; exact new_atom l r xs j hop hj

/-- The ids of a built history are distinct. -/
theorem built_nodup {H : Hist} (hb : Built H) : (ids H).Nodup := by
  induction hb with
  | nil => simp [ids]
  | snoc _ adm _ ih => rw [ids_append]; exact nodup_snoc ih adm.1

/-- An order operation's encoded edge is its fact, after the sentinel. -/
theorem orderEdge_spec (H : Hist) (hb : Built H) (r : OpId) (b a : NodeId)
    (hr : (r, Op.order b a) ∈ ops H) :
    ∃ j, ∃ hj : j < (facts H).length, orderEdge H r = 1 + j ∧ (facts H)[j] = (.node b, .node a, r) := by
  have hu := built_nodup hb
  have hfind : (ops H).find? (fun x => decide (x.1 = r)) = some (r, Op.order b a) := by
    rcases hf : (ops H).find? (fun x => decide (x.1 = r)) with _ | ⟨r', op'⟩
    · exact absurd (List.find?_eq_none.mp hf _ hr) (by simp)
    · have hm := List.mem_of_find?_eq_some hf
      have he : r' = r := by simpa using List.find?_some hf
      subst he
      rw [hf, op_unique hu hm hr]
  have hmem : ((Anchor.node b, Anchor.node a, r) : Anchor × Anchor × OpId) ∈ facts H := by
    unfold facts
    exact List.mem_flatMap.mpr ⟨(r, .order b a), hr, by simp [DocModel.edgesOf]⟩
  refine ⟨(facts H).idxOf (.node b, .node a, r), List.idxOf_lt_length_iff.mpr hmem, ?_,
    List.getElem_idxOf _⟩
  unfold orderEdge
  rw [hfind]

/-- Reachability transported along a map that is injective on the carrier. -/
theorem rtg_map_iff {α β} (φ : α → β) (S : α → Prop) (R : α → α → Prop) (R' : β → β → Prop)
    (hinj : ∀ a b, S a → S b → φ a = φ b → a = b)
    (hR : ∀ a b, R a b → S a ∧ S b)
    (hR' : ∀ x y, R' x y → ∃ a b, φ a = x ∧ φ b = y ∧ S a ∧ S b)
    (heq : ∀ a b, S a → S b → (R' (φ a) (φ b) ↔ R a b))
    (a b : α) (ha : S a) (hb : S b) :
    Relation.ReflTransGen R a b ↔ Relation.ReflTransGen R' (φ a) (φ b) := by
  constructor
  · intro h
    clear hb
    induction h with
    | refl => exact .refl
    | tail _ e ih =>
      obtain ⟨hc, hd⟩ := hR _ _ e
      exact ih.tail ((heq _ _ hc hd).mpr e)
  · intro h
    have key : ∀ y, Relation.ReflTransGen R' (φ a) y → ∃ c, φ c = y ∧ S c ∧ Relation.ReflTransGen R a c := by
      intro y h
      induction h with
      | refl => exact ⟨a, rfl, ha, .refl⟩
      | tail _ e ih =>
        obtain ⟨c, hc, hSc, hr⟩ := ih
        obtain ⟨c', d', h1, h2, hS1, hS2⟩ := hR' _ _ e
        have : c' = c := hinj _ _ hS1 hSc (by rw [h1, hc])
        subst this
        refine ⟨d', h2, hS2, hr.tail ((heq _ _ hS1 hS2).mp ?_)⟩
        rw [h1, h2]; exact e
    obtain ⟨c, hc, hSc, hr⟩ := key _ h
    have : c = b := hinj _ _ hSc hb hc
    exact this ▸ hr

theorem aIdx_inj {H : Hist} {a b : Anchor} (ha : a ∈ anchorList H) (hb : b ∈ anchorList H)
    (h : aIdx H a = aIdx H b) : a = b := by
  unfold aIdx at h
  calc a = (anchorList H)[idx (anchorList H) a]'(idx_lt ha) := (getElem_idx ha).symm
    _ = (anchorList H)[idx (anchorList H) b]'(idx_lt hb) := by simp only [h]
    _ = b := getElem_idx hb

/-- Every operation of a built history passed validation against an earlier
prefix, whose anchors are anchors of the history. -/
theorem built_op_valid {H : Hist} (hb : Built H) (x : OpId × Op) (hx : x ∈ ops H) :
    ∃ H0 deps own own', (∀ a ∈ anchorList H0, a ∈ anchorList H) ∧
      validOp H0 x.1.patch deps own x.1.ordinal x.2 = .ok own' := by
  induction hb with
  | nil => simp [ops] at hx
  | snoc hb adm hval ih =>
    rename_i H P
    rw [ops_append, List.mem_append] at hx
    rcases hx with hx | hx
    · obtain ⟨H0, deps, own, own', hsub, hv⟩ := ih hx
      exact ⟨H0, deps, own, own', fun a ha => anchorList_mono H P a (hsub a ha), hv⟩
    · obtain ⟨o, op⟩ := x
      have hP := mem_opsOf_iff.mp hx
      obtain ⟨own, own', hv⟩ := validate_each_op H P hval (op, o.ordinal)
        (List.mem_zipIdx_iff_getElem?.mpr hP.2)
      refine ⟨H, P.deps, own, own', fun a ha => anchorList_mono H P a ha, ?_⟩
      simp only
      rw [hP.1]
      exact hv

theorem checkAnchor_none_mem (H : Hist) (deps : List PId) (a : Anchor)
    (ha : checkAnchor H deps a = none) : a ∈ anchorList H := by
  cases a with
  | start => simp [anchorList]
  | stop => simp [anchorList]
  | node n =>
    unfold checkAnchor at ha
    simp only at ha
    split_ifs at ha with h1 h2
    exact (mem_anchorList_node H n).mpr ((by first | exact not_not.mp h2 | exact h2))

theorem anchorList_append (H : Hist) (P : Patch) :
    anchorList (H ++ [P]) = anchorList H ++ ((opsOf P).flatMap atomsOf).map fun a => .node a.1 := by
  simp [anchorList, DocModel.atoms, ops_append, List.flatMap_append]

theorem aIdx_append (H : Hist) (P : Patch) (a : Anchor) (ha : a ∈ anchorList H) :
    aIdx (H ++ [P]) a = aIdx H a := by
  unfold aIdx
  rw [idx_of_mem (anchorList_mono H P a ha), idx_of_mem ha, anchorList_append,
    List.idxOf_append_of_mem ha]

/-! ## Facts carrying an order operation -/

theorem opsOf_keys (P : Patch) :
    (opsOf P).map Prod.fst = (List.range P.ops.length).map fun k => (⟨P.id, k⟩ : OpId) := by
  apply List.ext_getElem
  · simp [opsOf]
  · intro i h1 h2
    simp [opsOf]

theorem ops_keys_nodup {H : Hist} (hu : (ids H).Nodup) : ((ops H).map Prod.fst).Nodup := by
  unfold ops
  rw [List.map_flatMap, List.nodup_flatMap]
  refine ⟨?_, ?_⟩
  · intro P _
    rw [opsOf_keys]
    refine List.Nodup.map (fun a b h => by cases h; rfl) List.nodup_range
  · unfold ids at hu
    have hp := List.pairwise_map.mp hu
    refine hp.imp ?_
    intro P Q hne x hx hy
    simp only [opsOf_keys, List.mem_map] at hx hy
    obtain ⟨_, _, rfl⟩ := hx
    obtain ⟨_, _, he⟩ := hy
    exact hne (congrArg OpId.patch he).symm

/-- Two positions satisfying a predicate that holds at most once coincide. -/
theorem idx_unique {α} (p : α → Bool) : ∀ (l : List α) (i j : Nat) (hi : i < l.length) (hj : j < l.length),
    (l.filter p).length ≤ 1 → p l[i] → p l[j] → i = j
  | [], _, _, hi, _, _, _, _ => absurd hi (by simp)
  | a :: t, i, j, hi, hj, hl, pi, pj => by
    have pos : ∀ k (hk : k < t.length), p t[k] → 0 < (t.filter p).length := fun k hk h =>
      List.length_pos_of_mem (List.mem_filter.mpr ⟨List.getElem_mem hk, h⟩)
    match i, j with
    | 0, 0 => rfl
    | 0, j + 1 =>
      simp only [List.getElem_cons_zero, List.getElem_cons_succ] at pi pj
      have := pos j (by simpa using hj) pj
      rw [List.filter_cons_of_pos pi, List.length_cons] at hl; omega
    | i + 1, 0 =>
      simp only [List.getElem_cons_zero, List.getElem_cons_succ] at pi pj
      have := pos i (by simpa using hi) pi
      rw [List.filter_cons_of_pos pj, List.length_cons] at hl; omega
    | i + 1, j + 1 =>
      simp only [List.getElem_cons_succ] at pi pj
      have : (t.filter p).length ≤ 1 := by
        refine le_trans ?_ hl
        exact ((List.sublist_cons_self a t).filter p).length_le
      exact congrArg (· + 1) (idx_unique p t i j (by simpa using hi) (by simpa using hj) this pi pj)

theorem flatMap_key {β γ} (r : OpId) (F : OpId × β → List γ) :
    ∀ (L : List (OpId × β)), (L.map Prod.fst).Nodup → (∀ x ∈ L, x.1 ≠ r → F x = []) →
      (∀ x ∈ L, x.1 = r → (F x).length ≤ 1) → (L.flatMap F).length ≤ 1
  | [], _, _, _ => by simp
  | x :: t, hk, hF, hb => by
    rw [List.flatMap_cons, List.length_append]
    rw [List.map_cons, List.nodup_cons] at hk
    by_cases hx : x.1 = r
    · have : t.flatMap F = [] := by
        rw [List.flatMap_eq_nil_iff]
        intro y hy
        refine hF y (List.mem_cons_of_mem _ hy) ?_
        intro hyr
        exact hk.1 (List.mem_map.mpr ⟨y, hy, hyr.trans hx.symm⟩)
      rw [this]
      simpa using hb x (List.mem_cons_self ..) hx
    · rw [hF x (List.mem_cons_self ..) hx]
      simpa using flatMap_key r F t hk.2 (fun y hy => hF y (List.mem_cons_of_mem _ hy))
        (fun y hy => hb y (List.mem_cons_of_mem _ hy))

/-- An order operation contributes the only fact carrying its id. -/
theorem order_filter (H : Hist) (hu : (ids H).Nodup) (r : OpId) (b a : NodeId)
    (hr : (r, Op.order b a) ∈ ops H) :
    ((facts H).filter fun f => f.2.2 == r).length ≤ 1 := by
  unfold facts
  rw [List.filter_flatMap]
  refine flatMap_key r _ (ops H) (ops_keys_nodup hu) ?_ ?_
  · intro x _ hx
    rw [List.filter_eq_nil_iff]
    intro f hf
    obtain ⟨e, _, rfl⟩ := List.mem_map.mp hf
    simpa using hx
  · intro x hx hxr
    obtain ⟨o, op⟩ := x
    simp only at hxr
    subst hxr
    have := op_unique hu hx hr
    subst this
    refine le_trans (List.length_filter_le _ _) ?_
    simp [DocModel.edgesOf]

/-- Facts carrying an order operation's id sit at its encoded edge. -/
theorem order_fact_index (H : Hist) (hu : (ids H).Nodup) (r : OpId) (b a : NodeId)
    (hr : (r, Op.order b a) ∈ ops H) (j : Nat) (hj : j < (facts H).length)
    (h : (facts H)[j].2.2 = r) :
    j = (facts H).idxOf (.node b, .node a, r) := by
  have hmem : ((Anchor.node b, Anchor.node a, r) : Anchor × Anchor × OpId) ∈ facts H := by
    unfold facts
    exact List.mem_flatMap.mpr ⟨(r, .order b a), hr, by simp [DocModel.edgesOf]⟩
  have hi := List.idxOf_lt_length_iff.mpr hmem
  refine idx_unique (fun f => f.2.2 == r) _ _ _ hj hi (order_filter H hu r b a hr) (by simpa using h) ?_
  rw [List.getElem_idxOf hi]; simp


theorem ops_patch_mem {H : Hist} {o : OpId} {op : Op} (h : (o, op) ∈ ops H) : o.patch ∈ ids H := by
  unfold ops at h
  obtain ⟨P, hP, h⟩ := List.mem_flatMap.mp h
  rw [mem_opsOf_iff] at h
  unfold ids
  exact List.mem_map.mpr ⟨P, hP, h.1.symm⟩

/-- A retraction in a built history targets an order operation. -/
theorem built_retract_order {H : Hist} (hb : Built H) {o r : OpId}
    (hx : (o, Op.retract r) ∈ ops H) : IsOrder H r := by
  induction hb with
  | nil => simp [ops] at hx
  | snoc hb adm hval ih =>
    rename_i H P
    have mono : IsOrder H r → IsOrder (H ++ [P]) r := fun ⟨b, a, h⟩ =>
      ⟨b, a, by rw [ops_append]; exact List.mem_append_left _ h⟩
    rw [ops_append, List.mem_append] at hx
    rcases hx with hx | hx
    · exact mono (ih hx)
    · have hP := mem_opsOf_iff.mp hx
      obtain ⟨own, own', hv⟩ := validate_each_op H P hval (Op.retract r, o.ordinal)
        (List.mem_zipIdx_iff_getElem?.mpr hP.2)
      apply mono
      simp only [validOp] at hv
      split_ifs at hv with h1 h2
      exact (by first | exact not_not.mp h2 | exact h2)

/-- Edge index `1 + j` is withdrawn for the patch exactly when fact `j`'s
operation was retracted by an observed patch or by the patch itself. -/
theorem offP_iff (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (ho : TrOwn H P st own)
    (hor : ∀ r ∈ own.retracted, IsOrder H r)
    (j : Nat) (hj : j < (facts H).length) :
    DocKernel.OffP d (DocKernel.seenM d.deps.val dp.deps.val) st (1 + j) ↔
      RetractedBy H (Observed H P.deps) (facts H)[j].2.2 ∨ (facts H)[j].2.2 ∈ own.retracted := by
  have hu := built_nodup hb
  have hlen : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hslen : (DocKernel.seenM d.deps.val dp.deps.val).length = H.length := by
    simp [DocKernel.seenM, hlen]
  have xmem : ∀ n m, (∃ x ∈ d.retractions.val, x.1.val = n ∧ x.2.val = m) ↔ (n, m) ∈ retractionsOf H := by
    intro n m
    rw [← hr.retractions]
    simp only [DocKernel.absD, List.mem_map, DocKernel.pairVal, Prod.mk.injEq]
  have rmem : ∀ n m, (n, m) ∈ retractionsOf H ↔
      ∃ o r, (o, Op.retract r) ∈ ops H ∧ n = orderEdge H r ∧ m = idx (ids H) o.patch := by
    intro n m
    unfold retractionsOf
    rw [List.mem_filterMap]
    constructor
    · rintro ⟨⟨o, op⟩, hm, he⟩
      cases op with
      | retract r =>
        simp only [Option.some.injEq, Prod.mk.injEq] at he
        exact ⟨o, r, hm, he.1.symm, he.2.symm⟩
      | _ => simp at he
    · rintro ⟨o, r, hm, rfl, rfl⟩
      exact ⟨(o, .retract r), hm, rfl⟩
  have seen_at : ∀ o op, (o, op) ∈ ops H →
      ((∃ h : idx (ids H) o.patch < (DocKernel.seenM d.deps.val dp.deps.val).length,
        (DocKernel.seenM d.deps.val dp.deps.val)[idx (ids H) o.patch] = true) ↔ Observed H P.deps o.patch) := by
    intro o op hm
    have hp := ops_patch_mem hm
    have hk : idx (ids H) o.patch < H.length := by
      have := idx_lt hp; simpa [ids] using this
    have hid : H[idx (ids H) o.patch].id = o.patch := by
      have := getElem_idx hp; simpa [ids] using this
    rw [← List.getElem?_eq_some_iff, seen_iff d H P dp hr hv ht adm _ hk, hid]
  have oe : ∀ r, IsOrder H r → (orderEdge H r = 1 + j ↔ (facts H)[j].2.2 = r) := by
    rintro r ⟨b, a, hm⟩
    obtain ⟨j', hj', he, hf⟩ := orderEdge_spec H hb r b a hm
    rw [he]
    constructor
    · intro h
      have : j' = j := by omega
      subst this
      rw [hf]
    · intro h
      have e1 := order_fact_index H hu r b a hm j hj h
      have e2 := order_fact_index H hu r b a hm j' hj' (by rw [hf])
      omega
  unfold DocKernel.OffP
  rw [ho.2]
  constructor
  · rintro (⟨x, hx, h1, h2⟩ | h)
    · left
      obtain ⟨o, r, hm, e1, e2⟩ := (rmem _ _).mp ((xmem _ _).mp ⟨x, hx, rfl, rfl⟩)
      have hfr : (facts H)[j].2.2 = r := (oe r (built_retract_order hb hm)).mp (by rw [← e1, h1])
      refine ⟨o, by rw [hfr]; exact hm, ?_⟩
      rcases h2 with h2 | h2
      · exfalso
        have hH : H = [] := List.eq_nil_of_length_eq_zero (by omega)
        subst hH
        simp [facts, ops] at hj
      · exact (seen_at o _ hm).mp (by rw [← e2]; exact h2)
    · right
      obtain ⟨r, hr', he⟩ := List.mem_map.mp h
      rw [(oe r (hor r hr')).mp he] at *
      exact hr'
  · rintro (⟨o, hm, hobs⟩ | h)
    · left
      have hor' := built_retract_order hb hm
      obtain ⟨x, hx, h1, h2⟩ := (xmem _ _).mpr ((rmem _ _).mpr ⟨o, _, hm, rfl, rfl⟩)
      refine ⟨x, hx, ?_, Or.inr ?_⟩
      · rw [h1]; exact (oe _ hor').mpr rfl
      · rw [h2]; exact (seen_at o _ hm).mpr hobs
    · right
      exact List.mem_map.mpr ⟨_, h, (oe _ (hor _ h)).mpr rfl⟩

theorem none_val : document.NONE.val = NONE := by
  unfold document.NONE NONE
  simp [core.num.Usize.MAX]

/-- Retraction entries of the dense document. -/
theorem retr_mem (d : document.Doc) (H : Hist) (hr : Rep d H) (n m : Nat) :
    (∃ x ∈ d.retractions.val, x.1.val = n ∧ x.2.val = m) ↔
      ∃ o r, (o, Op.retract r) ∈ ops H ∧ n = orderEdge H r ∧ m = idx (ids H) o.patch := by
  have xmem : (∃ x ∈ d.retractions.val, x.1.val = n ∧ x.2.val = m) ↔ (n, m) ∈ retractionsOf H := by
    rw [← hr.retractions]
    simp only [DocKernel.absD, List.mem_map, DocKernel.pairVal, Prod.mk.injEq]
  rw [xmem]
  unfold retractionsOf
  rw [List.mem_filterMap]
  constructor
  · rintro ⟨⟨o, op⟩, hm, he⟩
    cases op with
    | retract r =>
      simp only [Option.some.injEq, Prod.mk.injEq] at he
      exact ⟨o, r, hm, he.1.symm, he.2.symm⟩
    | _ => simp at he
  · rintro ⟨o, r, hm, rfl, rfl⟩
    exact ⟨(o, .retract r), hm, rfl⟩

/-- The `seen` mark at a patch's dense index is its observation. -/
theorem seen_at (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P) (q : PId) (hp : q ∈ ids H) :
    (∃ h : idx (ids H) q < (DocKernel.seenM d.deps.val dp.deps.val).length,
      (DocKernel.seenM d.deps.val dp.deps.val)[idx (ids H) q] = true) ↔ Observed H P.deps q := by
  have hk : idx (ids H) q < H.length := by
    have := idx_lt hp; simpa [ids] using this
  have hid : H[idx (ids H) q].id = q := by
    have := getElem_idx hp; simpa [ids] using this
  rw [← List.getElem?_eq_some_iff, seen_iff d H P dp hr hv ht adm _ hk, hid]

/-- An order operation's edge index is positive and in range. -/
theorem orderEdge_pos (H : Hist) (hb : Built H) (r : OpId) (h : IsOrder H r) :
    ∃ j < (facts H).length, orderEdge H r = 1 + j := by
  obtain ⟨b, a, hm⟩ := h
  obtain ⟨j, hj, he, -⟩ := orderEdge_spec H hb r b a hm
  exact ⟨j, hj, he⟩

/-- The sentinel edge is never withdrawn. -/
theorem offP_zero (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (st : DocKernel.CheckS) (own : Own) (ho : TrOwn H P st own)
    (hor : ∀ r ∈ own.retracted, IsOrder H r) :
    ¬ DocKernel.OffP d (DocKernel.seenM d.deps.val dp.deps.val) st 0 := by
  unfold DocKernel.OffP
  rw [ho.2]
  rintro (⟨x, hx, h1, -⟩ | h)
  · obtain ⟨o, r, hm, e1, -⟩ := (retr_mem d H hr _ _).mp ⟨x, hx, rfl, rfl⟩
    obtain ⟨j, -, he⟩ := orderEdge_pos H hb r (built_retract_order hb hm)
    omega
  · obtain ⟨r, hr', he⟩ := List.mem_map.mp h
    obtain ⟨j, -, he'⟩ := orderEdge_pos H hb r (hor r hr')
    omega

theorem aIdx_start (H : Hist) : aIdx H .start = 0 := by
  simp [aIdx, idx, anchorList]

theorem aIdx_stop (H : Hist) : aIdx H .stop = 1 := by
  simp [aIdx, idx, anchorList]

theorem start_mem (H : Hist) : Anchor.start ∈ anchorList H := by simp [anchorList]
theorem stop_mem (H : Hist) : Anchor.stop ∈ anchorList H := by simp [anchorList]

/-- The kernel's causal relation is the model's `CausalEdge`, through the
anchor map of `H ++ [P]` (the patch's own atoms come after `H`'s). -/
theorem causalRel_tr (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (ho : TrOwn H P st own)
    (hor : ∀ r ∈ own.retracted, IsOrder H r)
    (hoe : ∀ e ∈ own.edges, e.1 ∈ anchorList (H ++ [P]) ∧ e.2 ∈ anchorList (H ++ [P]))
    (u v : Anchor) (hu : u ∈ anchorList (H ++ [P])) (hv' : v ∈ anchorList (H ++ [P])) :
    DocKernel.CausalRel d (DocKernel.seenM d.deps.val dp.deps.val) st
        (aIdx (H ++ [P]) u) (aIdx (H ++ [P]) v) ↔ CausalEdge H P.deps own u v := by
  have hlen : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hHmax : H.length ≤ Usize.max := by have := d.deps.property; omega
  have hE : d.edges.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) = encEdges H := hr.edges
  have hElen : d.edges.val.length = 1 + (facts H).length := by
    have := congrArg List.length hE; simp [encEdges] at this; omega
  have hent : ∀ i (hi : i < d.edges.val.length),
      ((d.edges.val[i]).1.val, (d.edges.val[i]).2.1.val, (d.edges.val[i]).2.2.val) =
        (encEdges H)[i]'(by rw [← hE]; simpa using hi) := by
    intro i hi
    rw [← List.getElem_of_eq hE (by simpa using hi), List.getElem_map]
  have hfa := facts_anchors H hb
  -- fact `j` sits at kernel edge `j + 1`
  have hentj : ∀ j (hj : j < (facts H).length),
      (d.edges.val[j + 1]'(by omega)).1.val = aIdx (H ++ [P]) (facts H)[j].1 ∧
      (d.edges.val[j + 1]'(by omega)).2.1.val = aIdx (H ++ [P]) (facts H)[j].2.1 ∧
      (d.edges.val[j + 1]'(by omega)).2.2.val = idx (ids H) (facts H)[j].2.2.patch := by
    intro j hj
    have e := hent (j + 1) (by omega)
    simp only [encEdges, List.getElem_cons_succ, List.getElem_map, Prod.mk.injEq] at e
    have h1 := hfa _ (List.getElem_mem hj)
    rw [aIdx_append H P _ h1.1, aIdx_append H P _ h1.2]
    exact e
  have hpatch : ∀ j (hj : j < (facts H).length), (facts H)[j].2.2.patch ∈ ids H := by
    intro j hj
    unfold facts at hj ⊢
    have hm := List.getElem_mem hj
    obtain ⟨oo, hoo, hm⟩ := List.mem_flatMap.mp hm
    obtain ⟨e, -, he⟩ := List.mem_map.mp hm
    rw [← he]
    exact ops_patch_mem (op := oo.2) hoo
  have hpend : (aIdx (H ++ [P]) u, aIdx (H ++ [P]) v) ∈ st.pending ↔ (u, v) ∈ own.edges := by
    rw [ho.1, List.mem_map]
    constructor
    · rintro ⟨e, he, hq⟩
      simp only [Prod.mk.injEq] at hq
      have h1 := hoe e he
      have e1 := aIdx_inj h1.1 hu hq.1
      have e2 := aIdx_inj h1.2 hv' hq.2
      rw [← e1, ← e2]; exact he
    · intro h; exact ⟨(u, v), h, rfl⟩
  unfold DocKernel.CausalRel CausalEdge
  rw [hpend]
  constructor
  · rintro (⟨i, hi, hoff, hseen, ha, hb'⟩ | hp)
    · cases i with
      | zero =>
        left
        have e0 := hent 0 hi
        simp only [encEdges, List.getElem_cons_zero, Prod.mk.injEq] at e0
        have eu : u = .start := aIdx_inj hu (start_mem _) (by rw [aIdx_start, ← ha, e0.1])
        have ev : v = .stop := aIdx_inj hv' (stop_mem _) (by rw [aIdx_stop, ← hb', e0.2.1])
        rw [eu, ev]
      | succ j =>
        right; left
        have hj : j < (facts H).length := by omega
        obtain ⟨e1, e2, e3⟩ := hentj j hj
        have h1 := hfa _ (List.getElem_mem hj)
        have hu1 : (facts H)[j].1 = u := aIdx_inj (anchorList_mono H P _ h1.1) hu (by rw [← e1, ha])
        have hv1 : (facts H)[j].2.1 = v := aIdx_inj (anchorList_mono H P _ h1.2) hv' (by rw [← e2, hb'])
        have hnoff := (offP_iff d H P dp hr hb hv ht adm st own ho hor j hj).not.mp
          (by rw [Nat.add_comm]; exact hoff)
        push_neg at hnoff
        refine ⟨(facts H)[j].2.2, ?_, ?_, hnoff.1, hnoff.2⟩
        · rw [← hu1, ← hv1]; exact List.getElem_mem hj
        · rcases hseen with hs | hs
          · exfalso
            have h1 := congrArg UScalar.val hs
            rw [e3, none_val] at h1
            have h2 := idx_lt (hpatch j hj)
            have h3 : (ids H).length = H.length := by simp [ids]
            unfold NONE at h1
            omega
          · apply (seen_at d H P dp hr hv ht adm _ (hpatch j hj)).mp
            obtain ⟨hl, hs⟩ := hs
            simp only [e3] at hl hs
            exact ⟨hl, hs⟩
    · right; right; exact hp
  · rintro (h0 | ⟨o, hm, hobs, hnr, hno⟩ | hp)
    · left
      simp only [Prod.mk.injEq] at h0
      obtain ⟨rfl, rfl⟩ := h0
      have e0 := hent 0 (by omega)
      simp only [encEdges, List.getElem_cons_zero, Prod.mk.injEq] at e0
      refine ⟨0, by omega, offP_zero d H P dp hr hb st own ho hor, Or.inl ?_, ?_, ?_⟩
      · apply UScalar.eq_of_val_eq; rw [e0.2.2, none_val]
      · rw [e0.1, aIdx_start]
      · rw [e0.2.1, aIdx_stop]
    · left
      obtain ⟨j, hj, hf⟩ := List.getElem_of_mem hm
      obtain ⟨e1, e2, e3⟩ := hentj j hj
      have hnoff : ¬ DocKernel.OffP d (DocKernel.seenM d.deps.val dp.deps.val) st (1 + j) := by
        rw [offP_iff d H P dp hr hb hv ht adm st own ho hor j hj, hf]
        push_neg
        exact ⟨hnr, hno⟩
      refine ⟨j + 1, by omega, by rw [Nat.add_comm]; exact hnoff, Or.inr ?_, ?_, ?_⟩
      · have hp := hpatch j hj
        rw [hf] at hp
        obtain ⟨hl, hs⟩ := (seen_at d H P dp hr hv ht adm _ hp).mpr hobs
        refine ⟨by rw [e3, hf]; exact hl, ?_⟩
        simp only [e3, hf]
        exact hs
      · rw [e1, hf]
      · rw [e2, hf]
    · right; exact hp
/-- The kernel's reference check on a translated reference to object `q` of
the interning list `L`. -/
theorem checkRef_tr (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (L : List OpId) (q : OpId) (hL : ∀ x ∈ L, x.patch ∈ ids H) (r : document.Ref)
    (hp : r.patch.val = idx (ids H) q.patch) (hi : r.index.val = idx L q)
    (count : Nat) (hc : count = L.length) (hcl : count ≤ Usize.max) (missing : U64) (hm : missing.val ≠ 0)
    (hm1 : missing ≠ document.UNOBSERVED_PATCH) :
    let k := DocKernel.checkRefM r (DocKernel.seenM d.deps.val dp.deps.val) count missing
    (k = 0#u64 ↔ Observed H P.deps q.patch ∧ q ∈ L) ∧
    (k = document.UNOBSERVED_PATCH ↔ ¬ Observed H P.deps q.patch) ∧
    (k = missing ↔ Observed H P.deps q.patch ∧ q ∉ L) := by
  intro k
  have hdl : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps)
    simpa using this
  have hsl : (DocKernel.seenM d.deps.val dp.deps.val).length = H.length := by
    simp [DocKernel.seenM, hdl]
  have hidsl : (ids H).length = H.length := by simp [ids]
  have hdmax : H.length ≤ Usize.max := by have := d.deps.property; omega
  have nonev : (document.NONE : Usize).val = Usize.max := by simp [document.NONE]
  -- observation, through the seen marks
  have obs_iff : Observed H P.deps q.patch ↔ (r.patch ≠ document.NONE ∧
      ∃ h : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length,
        (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = true) := by
    constructor
    · intro ho
      have hq := observed_mem H P hv adm _ ho
      have hk : idx (ids H) q.patch < H.length := by rw [← hidsl]; exact idx_lt hq
      refine ⟨fun e => by have := congrArg (·.val) e; simp only [nonev, hp] at this; omega,
        by rw [hsl, hp]; exact hk, ?_⟩
      have := (seen_iff d H P dp hr hv ht adm _ hk).mpr (by
        rwa [show H[idx (ids H) q.patch].id = q.patch by
          have := getElem_idx hq; simpa [ids] using this])
      rw [← hp] at this
      exact (List.getElem?_eq_some_iff.mp this).2
    · rintro ⟨hpn, hpl, hs⟩
      have hq : q.patch ∈ ids H := by
        by_contra hq
        apply hpn
        apply UScalar.eq_of_val_eq
        rw [hp, nonev]; simp [idx, hq, NONE]
      have hk : idx (ids H) q.patch < H.length := by rw [← hidsl]; exact idx_lt hq
      have e := (seen_iff d H P dp hr hv ht adm _ hk).mp (by
        rw [← hp]; exact List.getElem?_eq_some_iff.mpr ⟨hpl, hs⟩)
      rwa [show H[idx (ids H) q.patch].id = q.patch by
        have := getElem_idx hq; simpa [ids] using this] at e
  -- membership, through the index
  have mem_iff : q ∈ L ↔ (r.index ≠ document.NONE ∧ r.index.val < count) := by
    constructor
    · intro hq
      have := idx_lt hq
      refine ⟨fun e => by have := congrArg (·.val) e; simp only [nonev, hi] at this; omega, by omega⟩
    · rintro ⟨hn, hlt⟩
      by_contra hq
      rw [hi] at hlt
      simp [idx, hq, NONE] at hlt
      omega
  by_cases ho : Observed H P.deps q.patch
  · obtain ⟨hpn, hpl, hs⟩ := obs_iff.mp ho
    have k_eq : k = (if r.index = document.NONE ∨ count ≤ r.index.val then missing else 0#u64) := by
      show DocKernel.checkRefM _ _ _ _ = _
      unfold DocKernel.checkRefM
      rw [if_neg hpn, dif_pos hpl, if_pos hs]
    by_cases hq : q ∈ L
    · obtain ⟨hn, hlt⟩ := mem_iff.mp hq
      rw [k_eq, if_neg (by push_neg; exact ⟨hn, hlt⟩)]
      simp [ho, hq, document.UNOBSERVED_PATCH]
      intro e; rw [← e] at hm; simp at hm
    · have : r.index = document.NONE ∨ count ≤ r.index.val := by
        by_contra hc; push_neg at hc; exact hq (mem_iff.mpr hc)
      rw [k_eq, if_pos this]
      refine ⟨⟨fun e => by rw [e] at hm; simp at hm, fun h => absurd h.2 hq⟩,
        ⟨fun e => absurd e hm1, fun h => absurd ho h⟩, ⟨fun _ => ⟨ho, hq⟩, fun _ => rfl⟩⟩
  · have k_eq : k = document.UNOBSERVED_PATCH := by
      show DocKernel.checkRefM _ _ _ _ = _
      unfold DocKernel.checkRefM
      by_cases hpn : r.patch = document.NONE
      · rw [if_pos hpn]
      · rw [if_neg hpn]
        by_cases hpl : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length
        · rw [dif_pos hpl]
          have : (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = false := by
            by_contra hc; simp at hc; exact ho (obs_iff.mpr ⟨hpn, hpl, hc⟩)
          rw [if_neg (by simp [this])]
        · rw [dif_neg hpl]
    rw [k_eq]
    refine ⟨⟨fun e => by simp [document.UNOBSERVED_PATCH] at e, fun h => absurd h.1 ho⟩,
      ⟨fun _ => ho, fun _ => rfl⟩, ⟨fun e => absurd e.symm hm1, fun h => absurd h.1 ho⟩⟩

/-! ## One validation step (statements) -/

/-- Atoms created by the first `k` operations of `P`. -/
def createdBy (P : Patch) (k : Nat) : Nat :=
  ((P.ops.take k).map fun | .insert _ _ xs => xs.length | _ => 0).sum

/-- The kernel's validation state after `k` operations matches the model's. -/
def StateRel (H : Hist) (P : Patch) (st : DocKernel.CheckS) (own : Own) (k : Nat) : Prop :=
  st.refusal.1 = 0#u64 ∧ TrOwn H P st own ∧ st.created = createdBy P k ∧
    (∀ r ∈ own.retracted, IsOrder H r) ∧
    (∀ e ∈ own.edges, e.1 ∈ anchorList (H ++ [P]) ∧ e.2 ∈ anchorList (H ++ [P]))

def insCount : Op → Nat
  | .insert _ _ xs => xs.length
  | _ => 0

theorem createdBy_succ (P : Patch) (k : Nat) (o : Op) (ho : P.ops[k]? = some o) :
    createdBy P (k + 1) = createdBy P k + insCount o := by
  unfold createdBy
  rw [List.take_succ, ho]
  cases o <;> simp [insCount]

theorem tag_ne_none_iff (H : Hist) (d : OpId) : tag H d ≠ none ↔ d ∈ deletionList H := by
  unfold tag deletionList
  constructor
  · intro h
    obtain ⟨ts, hts⟩ := Option.ne_none_iff_exists'.mp h
    obtain ⟨⟨o, op⟩, hm, hf⟩ := List.exists_of_findSome?_eq_some hts
    cases op <;> simp at hf
    obtain ⟨rfl, -⟩ := hf
    exact List.mem_filterMap.mpr ⟨_, hm, rfl⟩
  · intro h
    obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp h
    cases op <;> simp at hf
    subst hf
    rw [Ne, List.findSome?_eq_none_iff]
    push_neg
    exact ⟨_, hm, by simp⟩

theorem insertionOf_ne_none_iff (H : Hist) (i : OpId) : insertionOf H i ≠ none ↔ i ∈ insertionList H := by
  unfold insertionOf insertionList
  constructor
  · intro h
    obtain ⟨lr, hlr⟩ := Option.ne_none_iff_exists'.mp h
    obtain ⟨⟨o, op⟩, hm, hf⟩ := List.exists_of_findSome?_eq_some hlr
    cases op <;> simp at hf
    obtain ⟨rfl, -⟩ := hf
    exact List.mem_filterMap.mpr ⟨_, hm, rfl⟩
  · intro h
    obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp h
    cases op <;> simp at hf
    subst hf
    rw [Ne, List.findSome?_eq_none_iff]
    push_neg
    exact ⟨_, hm, by simp⟩

theorem built_valid {H : Hist} (hb : Built H) : Valid H := by
  induction hb with
  | nil => rfl
  | @snoc H P hb0 adm hval ih =>
    unfold Valid at ih ⊢
    rw [replay_append, ih]
    change replay H [P] = .ok (H ++ [P])
    rw [replay, applySpec_inserted adm.1 adm.2.1 adm.2.2.1 adm.2.2.2.1 adm.2.2.2.2 hval]
    rfl

theorem deletionsOf_length (H : Hist) : (deletionsOf H).length = (deletionList H).length := by
  unfold deletionsOf deletionList
  induction ops H with
  | nil => rfl
  | cons x l ih =>
    obtain ⟨o, op⟩ := x
    cases op <;> simp [List.filterMap_cons, ih]

theorem deletionList_patch_mem (H : Hist) (x : OpId) (hx : x ∈ deletionList H) : x.patch ∈ ids H := by
  unfold deletionList at hx
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp hx
  cases op <;> simp at hf
  subst hf
  unfold ops at hm
  obtain ⟨P, hP, hm⟩ := List.mem_flatMap.mp hm
  exact List.mem_map.mpr ⟨P, hP, ((mem_opsOf_iff.mp hm).1).symm⟩

theorem tag_of_mem {H : Hist} (hu : (ids H).Nodup) {o : OpId} {ts : List NodeId}
    (h : (o, Op.delete ts) ∈ ops H) : tag H o = some ts := by
  unfold tag
  generalize hf : List.findSome? _ (ops H) = res
  rcases res with _ | ts'
  · have := List.findSome?_eq_none_iff.mp hf _ h
    simp at this
  · obtain ⟨⟨o2, op2⟩, hm2, hf2⟩ := List.exists_of_findSome?_eq_some hf
    cases op2 <;> simp at hf2
    obtain ⟨rfl, rfl⟩ := hf2
    have := op_unique hu hm2 h
    simp only [Op.delete.injEq] at this
    rw [this]

theorem insertionOf_of_mem {H : Hist} (hu : (ids H).Nodup) {o : OpId} {l r : Anchor} {xs : List (List UInt8)}
    (h : (o, Op.insert l r xs) ∈ ops H) : insertionOf H o = some (l, r) := by
  unfold insertionOf
  generalize hf : List.findSome? _ (ops H) = res
  rcases res with _ | lr
  · have := List.findSome?_eq_none_iff.mp hf _ h
    simp at this
  · obtain ⟨⟨o2, op2⟩, hm2, hf2⟩ := List.exists_of_findSome?_eq_some hf
    cases op2 <;> simp at hf2
    obtain ⟨rfl, rfl⟩ := hf2
    have := op_unique hu hm2 h
    simp only [Op.insert.injEq] at this
    obtain ⟨rfl, rfl, -⟩ := this
    rfl

/-- The encoded deletions, positionally: each deletion's patch and translated tag. -/
theorem deletionsOf_eq_map (H : Hist) (hb : Built H) :
    deletionsOf H = (deletionList H).map fun x =>
      (idx (ids H) x.patch, ((tag H x).getD []).map fun n => aIdx H (.node n)) := by
  have hu := built_nodup hb
  have key : ∀ l : List (OpId × Op), (∀ x ∈ l, x ∈ ops H) →
      l.filterMap (fun x => match x with
        | (o, .delete ts) => some (idx (ids H) o.patch, ts.map (fun n => aIdx H (.node n)))
        | _ => none) =
      (l.filterMap fun | (o, .delete _) => some o | _ => none).map fun x =>
        (idx (ids H) x.patch, ((tag H x).getD []).map fun n => aIdx H (.node n)) := by
    intro l hl
    induction l with
    | nil => rfl
    | cons y l ih =>
      obtain ⟨o, op⟩ := y
      have hrest := ih (fun x hx => hl x (List.mem_cons_of_mem _ hx))
      cases op with
      | delete ts =>
        simp only [List.filterMap_cons, List.map_cons, hrest]
        congr 2
        have hm : (o, Op.delete ts) ∈ ops H := hl _ List.mem_cons_self
        have : tag H o = some ts := tag_of_mem hu hm
        rw [this]; rfl
      | _ => simpa [List.filterMap_cons] using hrest
  exact key (ops H) (fun x hx => hx)

/-- The encoded insertions, positionally: each insertion's patch and anchors. -/
theorem insertionsOf_eq_map (H : Hist) (hb : Built H) :
    insertionsOf H = (insertionList H).map fun x =>
      match insertionOf H x with
      | some (l, r) => (idx (ids H) x.patch, aIdx H l, aIdx H r)
      | none => (idx (ids H) x.patch, NONE, NONE) := by
  have hu := built_nodup hb
  have key : ∀ l : List (OpId × Op), (∀ x ∈ l, x ∈ ops H) →
      l.filterMap (fun x => match x with
        | (o, .insert a b _) => some (idx (ids H) o.patch, aIdx H a, aIdx H b)
        | _ => none) =
      (l.filterMap fun | (o, .insert ..) => some o | _ => none).map fun x =>
        match insertionOf H x with
        | some (l, r) => (idx (ids H) x.patch, aIdx H l, aIdx H r)
        | none => (idx (ids H) x.patch, NONE, NONE) := by
    intro l hl
    induction l with
    | nil => rfl
    | cons y l ih =>
      obtain ⟨o, op⟩ := y
      have hrest := ih (fun x hx => hl x (List.mem_cons_of_mem _ hx))
      cases op with
      | insert a b xs =>
        simp only [List.filterMap_cons, List.map_cons, hrest]
        congr 1
        have hm : (o, Op.insert a b xs) ∈ ops H := hl _ List.mem_cons_self
        have : insertionOf H o = some (a, b) := insertionOf_of_mem hu hm
        rw [this]
      | _ => simpa [List.filterMap_cons] using hrest
  exact key (ops H) (fun x hx => hx)

theorem restore_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (d₀ : OpId) (r : document.Ref) (ho : P.ops[k]? = some (.restore d₀))
    (htr : TrRef r (idx (ids H) d₀.patch) (idx (deletionList H) d₀)) :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Restore r)
    match validOp H P.id P.deps own k (.restore d₀) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  have hdl : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hL : ∀ x ∈ deletionList H, x.patch ∈ ids H := fun x hx => deletionList_patch_mem H x hx
  have key := checkRef_tr d H P dp hr hv ht adm (deletionList H) d₀ hL r htr.1 htr.2
    d.deletions.val.length hdl (by scalar_tac) document.UNKNOWN_DELETION unknown_deletion_ne unknown_deletion_ne_unobs
  obtain ⟨k0, kU, kM⟩ := key
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Restore r) = DocKernel.checkRestoreS st d.deletions.val.length (DocKernel.seenM d.deps.val dp.deps.val) k r := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hcr' : createdBy P (k + 1) = createdBy P k := by
    rw [createdBy_succ P k _ ho]; rfl
  unfold validOp
  by_cases hob : Observed H P.deps d₀.patch
  · by_cases hm : d₀ ∈ deletionList H
    · have htag : tag H d₀ ≠ none := (tag_ne_none_iff H d₀).mpr hm
      simp only [hob, not_true_eq_false, ↓reduceIte, htag]
      have hk := k0.mpr ⟨hob, hm⟩
      rw [hst']
      unfold DocKernel.checkRestoreS
      simp only [hk, ne_eq, not_true_eq_false, ↓reduceIte]
      exact ⟨hz, hown, by rw [hcr', hcr], hor, hoe⟩
    · have htag : tag H d₀ = none := by
        by_contra hc; exact hm ((tag_ne_none_iff H d₀).mp hc)
      simp only [hob, not_true_eq_false, ↓reduceIte, htag]
      have hk := kM.mpr ⟨hob, hm⟩
      rw [hst']
      unfold DocKernel.checkRestoreS DocKernel.refuseS
      simp only [hk]
      refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
      simp [errorOfT, ho, document.UNKNOWN_DELETION]
  · simp only [hob, not_false_eq_true, ↓reduceIte]
    have hk := kU.mpr hob
    rw [hst']
    unfold DocKernel.checkRestoreS DocKernel.refuseS
    simp only [hk]
    refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
    simp [errorOfT, ho, document.UNOBSERVED_PATCH]

theorem isOrder_iff_mem (H : Hist) (o : OpId) : IsOrder H o ↔ o ∈ orderList H := by
  unfold IsOrder orderList
  constructor
  · rintro ⟨b, a, h⟩; exact List.mem_filterMap.mpr ⟨_, h, rfl⟩
  · intro h
    obtain ⟨⟨o', op⟩, hm, hf⟩ := List.mem_filterMap.mp h
    cases op <;> simp at hf
    subst hf
    exact ⟨_, _, hm⟩

theorem orderList_patch_mem (H : Hist) (x : OpId) (hx : x ∈ orderList H) : x.patch ∈ ids H := by
  unfold orderList at hx
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp hx
  cases op <;> simp at hf
  subst hf
  unfold ops at hm
  obtain ⟨P, hP, hm⟩ := List.mem_flatMap.mp hm
  exact List.mem_map.mpr ⟨P, hP, ((mem_opsOf_iff.mp hm).1).symm⟩

theorem retract_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (o₀ : OpId) (r : document.Ref) (ho : P.ops[k]? = some (.retract o₀))
    (htr : TrRef r (idx (ids H) o₀.patch) (idx (orderList H) o₀)) :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Retract r)
    match validOp H P.id P.deps own k (.retract o₀) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  have hol : d.orders.val.length = (orderList H).length := by
    have := congrArg List.length (show d.orders.val.map (·.val) = _ from hr.orders)
    simpa using this
  have key := checkRef_tr d H P dp hr hv ht adm (orderList H) o₀ (orderList_patch_mem H) r htr.1 htr.2
    d.orders.val.length hol (by scalar_tac) document.UNKNOWN_ORDER DocKernel.unknown_order_ne unknown_order_ne_unobs
  obtain ⟨k0, kU, kM⟩ := key
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Retract r) = DocKernel.checkRetractS st d.orders.val (DocKernel.seenM d.deps.val dp.deps.val) k r := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hcr' : createdBy P (k + 1) = createdBy P k := by
    rw [createdBy_succ P k _ ho]; rfl
  unfold validOp
  by_cases hob : Observed H P.deps o₀.patch
  · by_cases hm : o₀ ∈ orderList H
    · have hio : IsOrder H o₀ := (isOrder_iff_mem H o₀).mpr hm
      simp only [hob, not_true_eq_false, ↓reduceIte, hio]
      have hk := k0.mpr ⟨hob, hm⟩
      rw [hst']
      unfold DocKernel.checkRetractS
      simp only [hk, ne_eq, not_true_eq_false, ↓reduceIte]
      refine ⟨hz, ⟨hown.1, ?_⟩, by rw [hcr', hcr], fun x hx => ?_, hoe⟩
      swap
      · simp only [List.mem_append, List.mem_singleton] at hx
        rcases hx with hx | rfl
        · exact hor x hx
        · exact hio
      simp only [hown.2, List.map_append, List.map_singleton]
      congr 1
      have hidx : r.index.val < d.orders.val.length := by rw [htr.2]; have := idx_lt hm; omega
      rw [getElem!_pos d.orders.val r.index.val hidx]
      have e := List.getElem_of_eq (show d.orders.val.map (·.val) = _ from hr.orders) (i := r.index.val)
        (by simpa using hidx)
      simp only [List.getElem_map] at e
      rw [e]
      have key : ∀ i (hi : i < (orderList H).length), i = idx (orderList H) o₀ → (orderList H)[i] = o₀ := by
        rintro i hi rfl; exact getElem_idx hm
      rw [key _ _ htr.2]
    · have hio : ¬ IsOrder H o₀ := fun h => hm ((isOrder_iff_mem H o₀).mp h)
      simp only [hob, not_true_eq_false, ↓reduceIte, hio, not_false_eq_true]
      have hk := kM.mpr ⟨hob, hm⟩
      rw [hst']
      unfold DocKernel.checkRetractS DocKernel.refuseS
      simp only [hk]
      refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
      simp [errorOfT, ho, document.UNKNOWN_ORDER]
  · simp only [hob, not_false_eq_true, ↓reduceIte]
    have hk := kU.mpr hob
    rw [hst']
    unfold DocKernel.checkRetractS DocKernel.refuseS
    simp only [hk]
    refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
    simp [errorOfT, ho, document.UNOBSERVED_PATCH]

/-- Observation of a referenced patch, through the kernel's seen marks. -/
theorem seen_patch_iff (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (q : PId) (r : document.Ref) (hp : r.patch.val = idx (ids H) q) :
    Observed H P.deps q ↔ (r.patch ≠ document.NONE ∧
      ∃ h : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length,
        (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = true) := by
  have hdl : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps)
    simpa using this
  have hsl : (DocKernel.seenM d.deps.val dp.deps.val).length = H.length := by
    simp [DocKernel.seenM, hdl]
  have hidsl : (ids H).length = H.length := by simp [ids]
  have nonev : (document.NONE : Usize).val = Usize.max := by simp [document.NONE]
  have hdmax : H.length ≤ Usize.max := by have := d.deps.property; omega
  have back : ∀ (hq : q ∈ ids H), (H[idx (ids H) q]'(by rw [← hidsl]; exact idx_lt hq)).id = q := by
    intro hq; have := getElem_idx hq; simpa [ids] using this
  constructor
  · intro ho
    have hq := observed_mem H P hv adm _ ho
    have hk : idx (ids H) q < H.length := by rw [← hidsl]; exact idx_lt hq
    refine ⟨fun e => by have := congrArg (·.val) e; simp only [nonev, hp] at this; omega,
      by rw [hsl, hp]; exact hk, ?_⟩
    have := (seen_iff d H P dp hr hv ht adm _ hk).mpr (by rwa [back hq])
    rw [← hp] at this
    exact (List.getElem?_eq_some_iff.mp this).2
  · rintro ⟨hpn, hpl, hs⟩
    have hq : q ∈ ids H := by
      by_contra hq
      apply hpn
      apply UScalar.eq_of_val_eq
      rw [hp, nonev]; simp [idx, hq, NONE]
    have hk : idx (ids H) q < H.length := by rw [← hidsl]; exact idx_lt hq
    have e := (seen_iff d H P dp hr hv ht adm _ hk).mp (by
      rw [← hp]; exact List.getElem?_eq_some_iff.mpr ⟨hpl, hs⟩)
    rwa [back hq] at e

/-- With no count and no missing code, `check_ref` checks observation only. -/
theorem checkRef_obs (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (q : PId) (r : document.Ref) (hp : r.patch.val = idx (ids H) q) :
    let k := DocKernel.checkRefM r (DocKernel.seenM d.deps.val dp.deps.val) document.NONE.val 0#u64
    (k = 0#u64 ↔ Observed H P.deps q) ∧ (k = document.UNOBSERVED_PATCH ↔ ¬ Observed H P.deps q) := by
  intro k
  have hob := seen_patch_iff d H P dp hr hv ht adm q r hp
  by_cases ho : Observed H P.deps q
  · obtain ⟨hpn, hpl, hs⟩ := hob.mp ho
    have : k = 0#u64 := by
      show DocKernel.checkRefM _ _ _ _ = _
      unfold DocKernel.checkRefM
      rw [if_neg hpn, dif_pos hpl, if_pos hs]
      split_ifs <;> rfl
    rw [this]
    simp [ho, document.UNOBSERVED_PATCH]
  · have : k = document.UNOBSERVED_PATCH := by
      show DocKernel.checkRefM _ _ _ _ = _
      unfold DocKernel.checkRefM
      by_cases hpn : r.patch = document.NONE
      · rw [if_pos hpn]
      · rw [if_neg hpn]
        by_cases hpl : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length
        · rw [dif_pos hpl]
          have : (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = false := by
            by_contra hc; simp at hc; exact ho (hob.mpr ⟨hpn, hpl, hc⟩)
          rw [if_neg (by simp [this])]
        · rw [dif_neg hpl]
    rw [this]
    simp [ho, document.UNOBSERVED_PATCH]

/-- A validated deletion's targets are atoms of the history. -/
theorem deletion_targets_mem (H : Hist) (hb : Built H) (x : OpId) (ts : List NodeId)
    (hts : tag H x = some ts) : ∀ n ∈ ts, Anchor.node n ∈ anchorList H := by
  unfold tag at hts
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.exists_of_findSome?_eq_some hts
  cases op <;> simp at hf
  obtain ⟨rfl, rfl⟩ := hf
  obtain ⟨H0, deps, own, own', hsub, hv⟩ := built_op_valid hb _ hm
  intro n hn
  apply hsub
  simp only [validOp] at hv
  split_ifs at hv with h0
  split at hv
  · cases hv
  · rename_i hfind
    rw [List.findSome?_eq_none_iff] at hfind
    obtain ⟨j, hjl, hjn⟩ := List.getElem_of_mem hn
    have := hfind (n, j) (by
      apply List.mem_zipIdx_iff_getElem?.mpr
      simp [hjl, hjn])
    split at this
    · simp at this
    · rename_i hc
      exact checkAnchor_none_mem H0 deps _ hc

/-- A validated insertion's anchors are anchors of the history. -/
theorem insertion_anchors_mem (H : Hist) (hb : Built H) (x : OpId) (l r : Anchor)
    (hlr : insertionOf H x = some (l, r)) : l ∈ anchorList H ∧ r ∈ anchorList H := by
  unfold insertionOf at hlr
  obtain ⟨⟨o, op⟩, hm, hf⟩ := List.exists_of_findSome?_eq_some hlr
  cases op with
  | insert l' r' xs =>
    simp only at hf
    split_ifs at hf
    simp only [Option.some.injEq, Prod.mk.injEq] at hf
    obtain ⟨rfl, rfl⟩ := hf
    obtain ⟨H0, deps, own, own', hsub, hv⟩ := built_op_valid hb _ hm
    have hl : checkAnchor H0 deps l' = none := by
      cases hc : checkAnchor H0 deps l' with
      | none => rfl
      | some e => simp [validOp, hc] at hv
    have hr : checkAnchor H0 deps r' = none := by
      cases hc : checkAnchor H0 deps r' with
      | none => rfl
      | some e => simp [validOp, hl, hc] at hv
    exact ⟨hsub _ (checkAnchor_none_mem H0 deps _ hl), hsub _ (checkAnchor_none_mem H0 deps _ hr)⟩
  | _ => simp at hf

theorem deletion_entry (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H) (x : OpId)
    (hx : x ∈ deletionList H) (ts : List NodeId) (hts : tag H x = some ts) :
    ∃ h : idx (deletionList H) x < d.deletions.val.length,
      (d.deletions.val[idx (deletionList H) x]'h).targets.val.map (·.val) =
        ts.map fun n => aIdx H (.node n) := by
  have e := (show d.deletions.val.map (fun x => (x.patch.val, x.targets.val.map (·.val))) = _
    from hr.deletions)
  rw [deletionsOf_eq_map H hb] at e
  have hl : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length e; simpa using this
  have hi : idx (deletionList H) x < d.deletions.val.length := by rw [hl]; exact idx_lt hx
  refine ⟨hi, ?_⟩
  have e2 := congrArg (fun l => l[idx (deletionList H) x]?) e
  simp only [List.getElem?_map, List.getElem?_eq_getElem hi,
    List.getElem?_eq_getElem (idx_lt hx), Option.map_some, Option.some.injEq, Prod.mk.injEq] at e2
  rw [e2.2, getElem_idx hx, hts]
  rfl

theorem insertion_entry (d : document.Doc) (H : Hist) (hr : Rep d H) (hb : Built H) (x : OpId)
    (hx : x ∈ insertionList H) (l r : Anchor) (hlr : insertionOf H x = some (l, r)) :
    ∃ h : idx (insertionList H) x < d.insertions.val.length,
      (d.insertions.val[idx (insertionList H) x]'h).left.val = aIdx H l ∧
      (d.insertions.val[idx (insertionList H) x]'h).right.val = aIdx H r := by
  have e := (show d.insertions.val.map (fun x => (x.patch.val, x.left.val, x.right.val)) = _
    from hr.insertions)
  rw [insertionsOf_eq_map H hb] at e
  have hl : d.insertions.val.length = (insertionList H).length := by
    have := congrArg List.length e; simpa using this
  have hi : idx (insertionList H) x < d.insertions.val.length := by rw [hl]; exact idx_lt hx
  refine ⟨hi, ?_⟩
  have e2 := congrArg (fun l => l[idx (insertionList H) x]?) e
  simp only [List.getElem?_map, List.getElem?_eq_getElem hi,
    List.getElem?_eq_getElem (idx_lt hx), Option.map_some, Option.some.injEq] at e2
  rw [getElem_idx hx, hlr] at e2
  simp only [Prod.mk.injEq] at e2
  exact ⟨e2.2.1, e2.2.2⟩

theorem keep_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (d₀ i₀ : OpId) (rd ri : document.Ref) (ho : P.ops[k]? = some (.keep d₀ i₀))
    (htr : TrRef rd (idx (ids H) d₀.patch) (idx (deletionList H) d₀) ∧
      TrRef ri (idx (ids H) i₀.patch) (idx (insertionList H) i₀)) :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Keep rd ri)
    match validOp H P.id P.deps own k (.keep d₀ i₀) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  obtain ⟨od0, odU⟩ := checkRef_obs d H P dp hr hv ht adm d₀.patch rd htr.1.1
  obtain ⟨oi0, oiU⟩ := checkRef_obs d H P dp hr hv ht adm i₀.patch ri htr.2.1
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Keep rd ri) = DocKernel.checkKeepS st d (DocKernel.seenM d.deps.val dp.deps.val) k rd ri := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hcr' : createdBy P (k + 1) = createdBy P k := by
    rw [createdBy_succ P k _ ho]; rfl
  have hdl : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hil : d.insertions.val.length = (insertionList H).length := by
    have := congrArg List.length (show d.insertions.val.map
      (fun x => (x.patch.val, x.left.val, x.right.val)) = _ from hr.insertions)
    simpa [insertionsOf_eq_map H hb] using this
  rw [hst']
  unfold validOp DocKernel.checkKeepS
  dsimp only
  by_cases hod : Observed H P.deps d₀.patch
  · by_cases hoi : Observed H P.deps i₀.patch
    · simp only [hod, hoi, not_true_eq_false, ↓reduceIte, od0.mpr hod, oi0.mpr hoi, ne_eq]
      by_cases hdm : d₀ ∈ deletionList H
      · have hdlt : ¬ d.deletions.val.length ≤ rd.index.val := by
          rw [htr.1.2, hdl]; have := idx_lt hdm; omega
        obtain ⟨ts, hts⟩ := Option.ne_none_iff_exists'.mp ((tag_ne_none_iff H d₀).mpr hdm)
        by_cases him : i₀ ∈ insertionList H
        · have hilt : ¬ d.insertions.val.length ≤ ri.index.val := by
            rw [htr.2.2, hil]; have := idx_lt him; omega
          obtain ⟨⟨l, r⟩, hlr⟩ := Option.ne_none_iff_exists'.mp ((insertionOf_ne_none_iff H i₀).mpr him)
          simp only [hts, hlr, dif_neg hdlt, dif_neg hilt]
          obtain ⟨hdi, hdt⟩ := deletion_entry d H hr hb d₀ hdm ts hts
          obtain ⟨hii, hil', hir'⟩ := insertion_entry d H hr hb i₀ him l r hlr
          obtain ⟨hlm, hrm⟩ := insertion_anchors_mem H hb i₀ l r hlr
          have htm := deletion_targets_mem H hb d₀ ts hts
          have hcond : ((d.insertions.val[ri.index.val]'(by omega)).left ∈
                (d.deletions.val[rd.index.val]'(by omega)).targets.val ∨
              (d.insertions.val[ri.index.val]'(by omega)).right ∈
                (d.deletions.val[rd.index.val]'(by omega)).targets.val) ↔
              ∃ n ∈ ts, l = .node n ∨ r = .node n := by
            have e1 : rd.index.val = idx (deletionList H) d₀ := htr.1.2
            have e2 : ri.index.val = idx (insertionList H) i₀ := htr.2.2
            have memv : ∀ (u : Usize) (vs : alloc.vec.Vec Usize), u ∈ vs.val ↔ u.val ∈ vs.val.map (·.val) := by
              intro u vs
              constructor
              · intro h; exact List.mem_map_of_mem h
              · intro h
                obtain ⟨u', hu', e⟩ := List.mem_map.mp h
                exact (UScalar.eq_of_val_eq e) ▸ hu'
            simp only [memv]
            simp only [e1, e2, hdt, hil', hir', List.mem_map]
            constructor
            · rintro (⟨n, hn, e⟩ | ⟨n, hn, e⟩)
              · exact ⟨n, hn, Or.inl (aIdx_inj hlm (htm n hn) e.symm)⟩
              · exact ⟨n, hn, Or.inr (aIdx_inj hrm (htm n hn) e.symm)⟩
            · rintro ⟨n, hn, e | e⟩
              · exact Or.inl ⟨n, hn, by rw [e]⟩
              · exact Or.inr ⟨n, hn, by rw [e]⟩
          by_cases hc : ∃ n ∈ ts, l = .node n ∨ r = .node n
          · rw [if_pos hc, if_pos (hcond.mpr hc)]
            exact ⟨hz, hown, by rw [hcr', hcr], hor, hoe⟩
          · rw [if_neg hc, if_neg (fun x => hc (hcond.mp x))]
            refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
            simp [DocKernel.refuseS, errorOfT, ho, document.INVALID_ACKNOWLEDGEMENT]
        · have hilge : d.insertions.val.length ≤ ri.index.val := by
            rw [htr.2.2, hil]; simp [idx, him, NONE]; scalar_tac
          have hins : insertionOf H i₀ = none := by
            by_contra hc; exact him ((insertionOf_ne_none_iff H i₀).mp hc)
          simp only [hts, hins, dif_neg hdlt, dif_pos hilge, DocKernel.refuseS]
          refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
          simp [errorOfT, ho, document.UNKNOWN_INSERTION]
      · have hdge : d.deletions.val.length ≤ rd.index.val := by
          rw [htr.1.2, hdl]; simp [idx, hdm, NONE]; scalar_tac
        have htag : tag H d₀ = none := by
          by_contra hc; exact hdm ((tag_ne_none_iff H d₀).mp hc)
        simp only [htag, dif_pos hdge, DocKernel.refuseS]
        refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
        simp [errorOfT, ho, document.UNKNOWN_DELETION]
    · simp only [hod, hoi, not_true_eq_false, not_false_eq_true, ↓reduceIte, od0.mpr hod, oiU.mpr hoi,
        DocKernel.refuseS, ne_eq]
      refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
      simp [errorOfT, ho, document.UNOBSERVED_PATCH]
  · simp only [hod, not_false_eq_true, ↓reduceIte, odU.mpr hod, DocKernel.refuseS, ne_eq]
    refine ⟨by simp [DocKernel.refuseS, document.UNKNOWN_DELETION, document.UNOBSERVED_PATCH, document.UNKNOWN_INSERTION, document.INVALID_ACKNOWLEDGEMENT, document.UNKNOWN_ORDER], ?_⟩
    simp [errorOfT, ho, document.UNOBSERVED_PATCH]

/-! ## Order operations -/

/-- The refusal code of an anchor error. -/
def codeOf : Option DocModel.Error → U64
  | some (.unobservedPatch _) => document.UNOBSERVED_PATCH
  | some (.unknownAnchor _) => document.UNKNOWN_ANCHOR
  | _ => 0#u64

theorem checkAnchor_some (H : Hist) (deps : List PId) (n : NodeId) (e : DocModel.Error)
    (h : checkAnchor H deps (.node n) = some e) :
    e = .unobservedPatch n.op.patch ∨ e = .unknownAnchor (.node n) := by
  unfold checkAnchor at h
  simp only at h
  split_ifs at h <;> simp_all

/-- A node check yields one of three codes. -/
theorem checkAnchorM_cases (r : document.Ref) (n : Nat) (seen : List Bool) :
    DocKernel.checkAnchorM (.Node r) n seen = 0#u64 ∨
    DocKernel.checkAnchorM (.Node r) n seen = document.UNOBSERVED_PATCH ∨
    DocKernel.checkAnchorM (.Node r) n seen = document.UNKNOWN_ANCHOR := by
  unfold DocKernel.checkAnchorM DocKernel.checkRefM
  simp only
  split_ifs <;> simp_all

/-- A node check refuses with `UNOBSERVED_PATCH` exactly when the patch is unseen. -/
theorem checkAnchorM_unobs (r : document.Ref) (n : Nat) (seen : List Bool) :
    DocKernel.checkAnchorM (.Node r) n seen = document.UNOBSERVED_PATCH ↔
      ¬ (r.patch ≠ document.NONE ∧ ∃ h : r.patch.val < seen.length, seen[r.patch.val] = true) := by
  have hne : document.UNKNOWN_ANCHOR ≠ document.UNOBSERVED_PATCH := by
    unfold document.UNKNOWN_ANCHOR document.UNOBSERVED_PATCH; decide
  have hne0 : (0#u64 : U64) ≠ document.UNOBSERVED_PATCH := by
    unfold document.UNOBSERVED_PATCH; decide
  unfold DocKernel.checkAnchorM DocKernel.checkRefM
  simp only
  split_ifs <;> simp_all

/-- The kernel's anchor code is the model's anchor error. -/
theorem checkAnchor_code (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (n : NodeId) (r : document.Ref) (hta : TrNode H n r) :
    DocKernel.checkAnchorM (.Node r) d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val) =
      codeOf (checkAnchor H P.deps (.node n)) := by
  have htr := checkAnchor_tr d H P dp hr hv ht adm (.node n) (.Node r) hta
  have hdl : d.deps.val.length = H.length := by
    have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps); simpa using this
  have hsl : (DocKernel.seenM d.deps.val dp.deps.val).length = H.length := by
    simp [DocKernel.seenM, hdl]
  -- the kernel sees the patch exactly when the model observes it
  have obs_iff : (r.patch ≠ document.NONE ∧ ∃ h : r.patch.val < (DocKernel.seenM d.deps.val dp.deps.val).length,
      (DocKernel.seenM d.deps.val dp.deps.val)[r.patch.val] = true) ↔ Observed H P.deps n.op.patch := by
    obtain ⟨hp, -⟩ := hta
    constructor
    · rintro ⟨hpn, hl, hs⟩
      have hq : n.op.patch ∈ ids H := by
        by_contra hq
        apply hpn
        apply UScalar.eq_of_val_eq
        rw [hp, none_val]; simp [idx, hq]
      rw [← seen_at d H P dp hr hv ht adm _ hq]
      rw [← hp]; exact ⟨hl, hs⟩
    · intro ho
      have hq := observed_mem H P hv adm _ ho
      obtain ⟨hl, hs⟩ := (seen_at d H P dp hr hv ht adm _ hq).mpr ho
      refine ⟨?_, by rw [hp]; exact hl, by simp only [hp]; exact hs⟩
      intro e
      have := congrArg UScalar.val e
      rw [hp, none_val] at this
      have := idx_lt hq
      simp [ids] at this
      have : H.length ≤ Usize.max := by have := d.deps.property; omega
      unfold NONE at *
      omega
  have hu := checkAnchorM_unobs r d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val)
  rw [obs_iff] at hu
  by_cases ho : Observed H P.deps n.op.patch
  · by_cases ha : DocModel.IsAtom H n
    · have hn : checkAnchor H P.deps (.node n) = none := by simp [checkAnchor, ho, ha]
      rw [hn, htr.mpr hn]; rfl
    · have hn : checkAnchor H P.deps (.node n) = some (.unknownAnchor (.node n)) := by
        simp [checkAnchor, ho, ha]
      rw [hn]
      rcases checkAnchorM_cases r d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val) with h | h | h
      · exact absurd (htr.mp h) (by rw [hn]; simp)
      · exact absurd (hu.mp h) (not_not.mpr ho)
      · exact h
  · have hn : checkAnchor H P.deps (.node n) = some (.unobservedPatch n.op.patch) := by
      simp [checkAnchor, ho]
    rw [hn]
    exact hu.mpr ho

/-- Kernel causal edges join encoded anchors of `H ++ [P]`. -/
theorem causalRel_img (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H)
    (st : DocKernel.CheckS) (own : Own) (ho : TrOwn H P st own)
    (hoe : ∀ e ∈ own.edges, e.1 ∈ anchorList (H ++ [P]) ∧ e.2 ∈ anchorList (H ++ [P]))
    (x y : Nat) (h : DocKernel.CausalRel d (DocKernel.seenM d.deps.val dp.deps.val) st x y) :
    ∃ u v, aIdx (H ++ [P]) u = x ∧ aIdx (H ++ [P]) v = y ∧
      u ∈ anchorList (H ++ [P]) ∧ v ∈ anchorList (H ++ [P]) := by
  have hE : d.edges.val.map (fun e => (e.1.val, e.2.1.val, e.2.2.val)) = encEdges H := hr.edges
  rcases h with ⟨i, hi, -, -, ha, hb'⟩ | hp
  · have e := List.getElem_of_eq hE (by simpa using hi)
    rw [List.getElem_map] at e
    cases i with
    | zero =>
      simp only [encEdges, List.getElem_cons_zero, Prod.mk.injEq] at e
      exact ⟨.start, .stop, by rw [aIdx_start, ← ha, e.1], by rw [aIdx_stop, ← hb', e.2.1],
        start_mem _, stop_mem _⟩
    | succ j =>
      simp only [encEdges, List.getElem_cons_succ, List.getElem_map, Prod.mk.injEq] at e
      have hj : j < (facts H).length := by
        have := congrArg List.length hE; simp [encEdges] at this; omega
      have h1 := facts_anchors H hb _ (List.getElem_mem hj)
      exact ⟨_, _, by rw [aIdx_append H P _ h1.1, ← ha, e.1], by rw [aIdx_append H P _ h1.2, ← hb', e.2.1],
        anchorList_mono H P _ h1.1, anchorList_mono H P _ h1.2⟩
  · rw [ho.1, List.mem_map] at hp
    obtain ⟨e, he, hq⟩ := hp
    simp only [Prod.mk.injEq] at hq
    exact ⟨e.1, e.2, hq.1, hq.2, (hoe e he).1, (hoe e he).2⟩

/-- The kernel's cycle check is the model's `Reaches`. -/
theorem reaches_tr (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (ho : TrOwn H P st own)
    (hor : ∀ r ∈ own.retracted, IsOrder H r)
    (hoe : ∀ e ∈ own.edges, e.1 ∈ anchorList (H ++ [P]) ∧ e.2 ∈ anchorList (H ++ [P]))
    (u v : Anchor) (hu : u ∈ anchorList (H ++ [P])) (hv' : v ∈ anchorList (H ++ [P])) :
    Relation.ReflTransGen (DocKernel.CausalRel d (DocKernel.seenM d.deps.val dp.deps.val) st)
        (aIdx (H ++ [P]) u) (aIdx (H ++ [P]) v) ↔ Reaches H P.deps own u v := by
  unfold Reaches
  symm
  apply rtg_map_iff (aIdx (H ++ [P])) (· ∈ anchorList (H ++ [P]))
  · intro a b ha hb' h; exact aIdx_inj ha hb' h
  · intro a b h
    rcases h with h | ⟨o, hm, -⟩ | h
    · simp only [Prod.mk.injEq] at h
      rw [h.1, h.2]; exact ⟨start_mem _, stop_mem _⟩
    · have := facts_anchors H hb _ hm
      exact ⟨anchorList_mono H P _ this.1, anchorList_mono H P _ this.2⟩
    · exact hoe _ h
  · intro x y h
    obtain ⟨a, b, h1, h2, h3, h4⟩ := causalRel_img d H P dp hr hb st own ho hoe x y h
    exact ⟨a, b, h1, h2, h3, h4⟩
  · intro a b ha hb'
    exact causalRel_tr d H P dp hr hb hv ht adm st own ho hor hoe a b ha hb'
  · exact hu
  · exact hv'

theorem errorOfT_order (P : Patch) (k : Nat) (b a : NodeId) (ho : P.ops[k]? = some (.order b a))
    (n : NodeId) (item : Nat) (hn : n = if item = 0 then b else a) (e : DocModel.Error)
    (he : e = .unobservedPatch n.op.patch ∨ e = .unknownAnchor (.node n)) :
    errorOfT P (codeOf (some e), k, item) = some e := by
  rcases he with rfl | rfl
  · unfold errorOfT codeOf
    rw [ho]
    cases item <;> simp_all [document.UNOBSERVED_PATCH]
  · unfold errorOfT codeOf
    rw [ho]
    cases item <;> simp_all [document.UNKNOWN_ANCHOR]

theorem order_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (b a : NodeId) (b' a' : document.Ref) (ho : P.ops[k]? = some (.order b a))
    (htr : TrNode H b b' ∧ TrNode H a a') :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Order b' a')
    match validOp H P.id P.deps own k (.order b a) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Order b' a') =
      DocKernel.checkOrderS st d (DocKernel.seenM d.deps.val dp.deps.val) k b' a' := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hcr' : createdBy P (k + 1) = createdBy P k := by
    rw [createdBy_succ P k _ ho]; rfl
  have cb := checkAnchor_code d H P dp hr hv ht adm b b' htr.1
  have ca := checkAnchor_code d H P dp hr hv ht adm a a' htr.2
  have code_ne : ∀ n e, checkAnchor H P.deps (.node n) = some e → codeOf (some e) ≠ 0#u64 := by
    intro n e h
    rcases checkAnchor_some H P.deps n e h with rfl | rfl
    · show document.UNOBSERVED_PATCH ≠ 0#u64; unfold document.UNOBSERVED_PATCH; decide
    · show document.UNKNOWN_ANCHOR ≠ 0#u64; unfold document.UNKNOWN_ANCHOR; decide
  rw [hst']
  unfold validOp DocKernel.checkOrderS
  dsimp only
  rcases hcb : checkAnchor H P.deps (.node b) with _ | eb
  · rcases hca : checkAnchor H P.deps (.node a) with _ | ea
    · have hbm := checkAnchor_none_mem H P.deps _ hcb
      have ham := checkAnchor_none_mem H P.deps _ hca
      have ib : b'.index.val = aIdx (H ++ [P]) (.node b) := by
        rw [aIdx_append H P _ hbm]; exact htr.1.2
      have ia : a'.index.val = aIdx (H ++ [P]) (.node a) := by
        rw [aIdx_append H P _ ham]; exact htr.2.2
      have hreach := reaches_tr d H P dp hr hb hv ht adm st own hown hor hoe (.node a) (.node b)
        (anchorList_mono H P _ ham) (anchorList_mono H P _ hbm)
      rw [cb, hcb, ca, hca]
      simp only [codeOf, ne_eq, not_true_eq_false, ↓reduceIte]
      have hreach' : Relation.ReflTransGen (DocKernel.CausalRel d (DocKernel.seenM d.deps.val dp.deps.val) st)
          a'.index.val b'.index.val ↔ Reaches H P.deps own (.node a) (.node b) := by
        rw [ia, ib]; exact hreach
      by_cases hre : Reaches H P.deps own (.node a) (.node b)
      · simp only [hreach'.mpr hre, ↓reduceIte, hre, DocKernel.refuseS]
        refine ⟨by show document.CYCLE ≠ 0#u64; unfold document.CYCLE; decide, ?_⟩
        simp [errorOfT, ho, document.CYCLE]
      · have hk : ¬ Relation.ReflTransGen (DocKernel.CausalRel d (DocKernel.seenM d.deps.val dp.deps.val) st)
            a'.index.val b'.index.val := fun h => hre (hreach'.mp h)
        simp only [hk, ↓reduceIte, hre]
        refine ⟨hz, ⟨?_, hown.2⟩, by rw [hcr', hcr], hor, ?_⟩
        · simp only [hown.1, List.map_append, List.map_singleton, ia, ib]
        · intro e he
          simp only [List.mem_append, List.mem_singleton] at he
          rcases he with he | rfl
          · exact hoe e he
          · exact ⟨anchorList_mono H P _ hbm, anchorList_mono H P _ ham⟩
    · rw [cb, hcb, ca, hca]
      have hne := code_ne a ea hca
      have z : ¬ ((0#u64 : U64) ≠ 0#u64) := by simp
      simp only [codeOf, z, ↓reduceIte] at hne ⊢
      rw [if_pos hne]
      simp only [DocKernel.refuseS]
      exact ⟨hne, errorOfT_order P k b a ho a 1 (by simp) ea (checkAnchor_some _ _ _ _ hca)⟩
  · rw [cb, hcb]
    try dsimp only
    have hne := code_ne b eb hcb
    rw [if_pos hne]
    simp only [DocKernel.refuseS]
    exact ⟨hne, errorOfT_order P k b a ho b 0 (by simp) eb (checkAnchor_some _ _ _ _ hcb)⟩

/-! ## Insert operations -/

theorem node_patch_mem (H : Hist) (n : NodeId) (h : Anchor.node n ∈ anchorList H) : n.op.patch ∈ ids H := by
  unfold anchorList at h
  simp only [List.cons_append, List.nil_append, List.mem_cons, reduceCtorEq, false_or, List.mem_map,
    Anchor.node.injEq] at h
  obtain ⟨a, ha, rfl⟩ := h
  unfold DocModel.atoms at ha
  obtain ⟨⟨o, op⟩, hm, ha⟩ := List.mem_flatMap.mp ha
  cases op with
  | insert l r xs =>
    simp only [atomsOf, List.mem_map] at ha
    obtain ⟨⟨b, j⟩, -, rfl⟩ := ha
    exact ops_patch_mem hm
  | _ => simp [atomsOf] at ha

theorem atomsOf_length (o : OpId) (op : Op) : (atomsOf (o, op)).length = insCount op := by
  cases op <;> simp [atomsOf, insCount]

theorem node_new_mem (H : Hist) (P : Patch) (k : Nat) (l r : Anchor) (xs : List (List UInt8))
    (ho : P.ops[k]? = some (.insert l r xs)) (j : Nat) (hj : j < xs.length) :
    Anchor.node ⟨⟨P.id, k⟩, j⟩ ∈ anchorList (H ++ [P]) := by
  apply (mem_anchorList_node _ _).mpr
  unfold DocModel.IsAtom DocModel.atom
  rw [Ne, lookup_none]
  push_neg
  refine ⟨xs[j], ?_⟩
  unfold DocModel.atoms
  apply List.mem_flatMap.mpr
  refine ⟨(⟨P.id, k⟩, .insert l r xs), ?_, ?_⟩
  · rw [ops_append]; apply List.mem_append_right; exact mem_opsOf_iff.mpr ⟨rfl, ho⟩
  · simp only [atomsOf, List.mem_map]
    exact ⟨(xs[j], j), List.mem_zipIdx_iff_getElem?.mpr (by simp [hj]), rfl⟩

/-- The `j`-th atom of the patch's `k`-th operation is interned right after
`H`'s anchors and the patch's earlier atoms. -/
theorem aIdx_new (H : Hist) (P : Patch) (hP : P.id ∉ ids H) (k : Nat) (l r : Anchor)
    (xs : List (List UInt8)) (ho : P.ops[k]? = some (.insert l r xs)) (j : Nat) (hj : j < xs.length) :
    aIdx (H ++ [P]) (.node ⟨⟨P.id, k⟩, j⟩) = (anchorList H).length + createdBy P k + j := by
  have hnot : Anchor.node ⟨⟨P.id, k⟩, j⟩ ∉ anchorList H := fun h => hP (node_patch_mem H _ h)
  obtain ⟨hk, hget⟩ := List.getElem?_eq_some_iff.mp ho
  have hsplit : P.ops = P.ops.take k ++ Op.insert l r xs :: P.ops.drop (k + 1) := by
    have e := List.take_append_drop k P.ops
    rw [List.drop_eq_getElem_cons hk, hget] at e
    exact e.symm
  -- the patch's anchors, split around operation `k`
  have hP' : ((opsOf P).flatMap atomsOf).map (fun a => Anchor.node a.1) =
      (((P.ops.take k).zipIdx.map fun (o, i) => ((⟨P.id, i⟩ : OpId), o)).flatMap atomsOf).map
          (fun a => Anchor.node a.1) ++
        ((xs.zipIdx.map fun (b, i) => ((⟨⟨P.id, k⟩, i⟩ : NodeId), b)).map (fun a => Anchor.node a.1) ++
        ((((P.ops.drop (k + 1)).zipIdx (k + 1)).map fun (o, i) => ((⟨P.id, i⟩ : OpId), o)).flatMap atomsOf).map
          (fun a => Anchor.node a.1)) := by
    unfold opsOf
    conv_lhs => rw [hsplit]
    rw [List.zipIdx_append, List.zipIdx_cons, List.map_append, List.map_cons, List.flatMap_append,
      List.flatMap_cons, List.map_append, List.map_append]
    simp [List.length_take, Nat.min_eq_left hk.le, atomsOf]
  have hpre : (((P.ops.take k).zipIdx.map fun (o, i) => ((⟨P.id, i⟩ : OpId), o)).flatMap atomsOf).length =
      createdBy P k := by
    rw [List.length_flatMap]
    unfold createdBy
    congr 1
    apply List.ext_getElem
    · simp
    · intro i h1 h2
      simp only [List.getElem_map, List.getElem_zipIdx, atomsOf_length]
      generalize (List.take k P.ops)[i]'(by simpa using h2) = o
      cases o <;> rfl
  have hnpre : Anchor.node ⟨⟨P.id, k⟩, j⟩ ∉
      (((P.ops.take k).zipIdx.map fun (o, i) => ((⟨P.id, i⟩ : OpId), o)).flatMap atomsOf).map
        (fun a => Anchor.node a.1) := by
    intro hm
    obtain ⟨a, ha, he⟩ := List.mem_map.mp hm
    obtain ⟨oo, hoo, ha⟩ := List.mem_flatMap.mp ha
    obtain ⟨⟨o, i⟩, hi, rfl⟩ := List.mem_map.mp hoo
    have hil : i < k := by
      have := (List.mem_zipIdx hi).2.1
      simp [List.length_take, Nat.min_eq_left hk.le] at this
      omega
    cases o with
    | insert l' r' xs' =>
      simp only [atomsOf, List.mem_map] at ha
      obtain ⟨⟨b, j'⟩, -, rfl⟩ := ha
      simp only [Anchor.node.injEq, NodeId.mk.injEq, OpId.mk.injEq] at he
      omega
    | _ => simp [atomsOf] at ha
  have hmid : List.idxOf (Anchor.node ⟨⟨P.id, k⟩, j⟩)
      ((xs.zipIdx.map fun (b, i) => ((⟨⟨P.id, k⟩, i⟩ : NodeId), b)).map (fun a => Anchor.node a.1)) = j := by
    have hl : ((xs.zipIdx.map fun (b, i) => ((⟨⟨P.id, k⟩, i⟩ : NodeId), b)).map
        (fun a => Anchor.node a.1)) = (List.range xs.length).map fun i => Anchor.node ⟨⟨P.id, k⟩, i⟩ := by
      apply List.ext_getElem
      · simp
      · intro i h1 h2; simp
    rw [hl]
    have hn : ((List.range xs.length).map fun i => Anchor.node ⟨⟨P.id, k⟩, i⟩).Nodup :=
      List.Nodup.map (fun a b h => by simpa using h) List.nodup_range
    have := List.Nodup.idxOf_getElem hn j (by simpa using hj)
    simpa using this
  have hmemmid : Anchor.node ⟨⟨P.id, k⟩, j⟩ ∈
      ((xs.zipIdx.map fun (b, i) => ((⟨⟨P.id, k⟩, i⟩ : NodeId), b)).map (fun a => Anchor.node a.1)) := by
    simp only [List.mem_map]
    refine ⟨(⟨⟨P.id, k⟩, j⟩, xs[j]), ⟨(xs[j], j), List.mem_zipIdx_iff_getElem?.mpr (by simp [hj]), rfl⟩, rfl⟩
  have hmem : Anchor.node ⟨⟨P.id, k⟩, j⟩ ∈ anchorList (H ++ [P]) := by
    rw [anchorList_append, hP']
    exact List.mem_append_right _ (List.mem_append_right _ (List.mem_append_left _ hmemmid))
  unfold aIdx
  rw [idx_of_mem hmem, anchorList_append, List.idxOf_append_of_notMem hnot, hP',
    List.idxOf_append_of_notMem hnpre, List.length_map, hpre, List.idxOf_append_of_mem hmemmid, hmid]
  omega

theorem zip_cons_snoc {α} : ∀ (X : List α) (a b : α),
    (a :: X).zip (X ++ [b]) = (a :: X).zip X ++ [((a :: X).getLast (by simp), b)]
  | [], a, b => by simp
  | x :: X, a, b => by
    have := zip_cons_snoc X x b
    simp only [List.cons_append, List.zip_cons_cons] at this ⊢
    rw [this]
    simp

theorem zip_cons_snoc_left {α} : ∀ (X : List α) (a c b : α),
    (a :: (X ++ [c])).zip (X ++ [b]) = (a :: X).zip (X ++ [b])
  | [], a, c, b => by simp
  | x :: X, a, c, b => by
    have := zip_cons_snoc_left X x c b
    simp only [List.cons_append, List.zip_cons_cons] at this ⊢
    rw [this]

theorem foldl_chainStep_closed (first left : Nat) : ∀ n : Nat,
    (List.range n).foldl (DocKernel.chainStep first) ([], left) =
      ((left :: (List.range n).map (first + ·)).zip ((List.range n).map (first + ·)),
       ((left :: (List.range n).map (first + ·)).getLast (by simp)))
  | 0 => by simp
  | n + 1 => by
    rw [List.range_succ, List.foldl_append, foldl_chainStep_closed first left n]
    simp only [List.foldl_cons, List.foldl_nil, DocKernel.chainStep, List.map_append, List.map_cons, List.map_nil]
    congr 1
    · rw [← zip_cons_snoc]
      exact (zip_cons_snoc_left ((List.range n).map (first + ·)) left (first + n) (first + n)).symm
    · simp [List.getLast_cons]

theorem chainM_closed (left right first n : Nat) :
    DocKernel.chainM left right first n =
      (left :: (List.range n).map (first + ·)).zip ((List.range n).map (first + ·) ++ [right]) := by
  unfold DocKernel.chainM
  rw [foldl_chainStep_closed, zip_cons_snoc]


theorem map_zip_pair {α β} (φ : α → β) : ∀ (A B : List α),
    (A.zip B).map (fun e => (φ e.1, φ e.2)) = (A.map φ).zip (B.map φ)
  | [], _ => by simp
  | _ :: _, [] => by simp
  | a :: A, b :: B => by simp [map_zip_pair φ A B]

theorem anchorIdx_tr (H : Hist) (a : Anchor) (a' : document.Anchor) (hta : TrAnchor H a a') :
    DocKernel.anchorIdx a' = aIdx H a := by
  cases a <;> cases a' <;> simp only [TrAnchor] at hta <;>
    simp only [DocKernel.anchorIdx, aIdx_start, aIdx_stop]
  exact hta.2

theorem checkAnchor_code' (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (a : Anchor) (a' : document.Anchor) (hta : TrAnchor H a a') :
    DocKernel.checkAnchorM a' d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val) =
      codeOf (checkAnchor H P.deps a) := by
  cases a <;> cases a' <;> simp only [TrAnchor] at hta
  all_goals first
    | exact checkAnchor_code d H P dp hr hv ht adm _ _ hta
    | simp [DocKernel.checkAnchorM, checkAnchor, codeOf]

theorem checkAnchor_some' (H : Hist) (deps : List PId) (a : Anchor) (e : DocModel.Error)
    (h : checkAnchor H deps a = some e) :
    ∃ n, a = .node n ∧ (e = .unobservedPatch n.op.patch ∨ e = .unknownAnchor (.node n)) := by
  cases a with
  | node n => exact ⟨n, rfl, checkAnchor_some H deps n e h⟩
  | _ => simp [checkAnchor] at h

theorem errorOfT_insert (P : Patch) (k : Nat) (l r : Anchor) (xs : List (List UInt8))
    (ho : P.ops[k]? = some (.insert l r xs))
    (n : NodeId) (item : Nat) (hn : (if item = 0 then l else r) = .node n) (e : DocModel.Error)
    (he : e = .unobservedPatch n.op.patch ∨ e = .unknownAnchor (.node n)) :
    errorOfT P (codeOf (some e), k, item) = some e := by
  rcases he with rfl | rfl
  · unfold errorOfT codeOf
    rw [ho]
    cases item <;> simp_all [document.UNOBSERVED_PATCH]
  · unfold errorOfT codeOf
    rw [ho]
    cases item <;> simp_all [document.UNKNOWN_ANCHOR]

theorem code_ne' (H : Hist) (deps : List PId) (a : Anchor) (e : DocModel.Error)
    (h : checkAnchor H deps a = some e) : codeOf (some e) ≠ 0#u64 := by
  obtain ⟨n, -, he⟩ := checkAnchor_some' H deps a e h
  rcases he with rfl | rfl
  · show document.UNOBSERVED_PATCH ≠ 0#u64; unfold document.UNOBSERVED_PATCH; decide
  · show document.UNKNOWN_ANCHOR ≠ 0#u64; unfold document.UNKNOWN_ANCHOR; decide

theorem insert_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (l r : Anchor) (xs : List (List UInt8)) (l' r' : document.Anchor) (xs' : alloc.vec.Vec (alloc.vec.Vec U8))
    (ho : P.ops[k]? = some (.insert l r xs))
    (htr : TrAnchor H l l' ∧ TrAnchor H r r' ∧
      xs'.val.map (fun x => x.val.map (·.val)) = xs.map (fun x => x.map (·.toNat))) :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Insert l' r' xs')
    match validOp H P.id P.deps own k (.insert l r xs) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Insert l' r' xs') =
      DocKernel.checkInsertS st d (DocKernel.seenM d.deps.val dp.deps.val) k l' r' xs'.val := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hlen : xs'.val.length = xs.length := by
    have := congrArg List.length htr.2.2; simpa using this
  have hal : d.atoms.val.length = (anchorList H).length := by
    have := congrArg List.length (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms)
    simpa [anchorList] using this
  have cl := checkAnchor_code' d H P dp hr hv ht adm l l' htr.1
  have cr := checkAnchor_code' d H P dp hr hv ht adm r r' htr.2.1
  rw [hst']
  unfold validOp DocKernel.checkInsertS
  dsimp only
  rcases hcl : checkAnchor H P.deps l with _ | el
  · rcases hcr' : checkAnchor H P.deps r with _ | er
    · have hlm := checkAnchor_none_mem H P.deps _ hcl
      have hrm := checkAnchor_none_mem H P.deps _ hcr'
      have hlm' := anchorList_mono H P _ hlm
      have hrm' := anchorList_mono H P _ hrm
      have il : DocKernel.anchorIdx l' = aIdx (H ++ [P]) l := by
        rw [anchorIdx_tr H l l' htr.1, aIdx_append H P _ hlm]
      have ir : DocKernel.anchorIdx r' = aIdx (H ++ [P]) r := by
        rw [anchorIdx_tr H r r' htr.2.1, aIdx_append H P _ hrm]
      rw [cl, hcl, cr, hcr']
      have z : ¬ ((0#u64 : U64) ≠ 0#u64) := by simp
      simp only [codeOf, z, ↓reduceIte]
      have hx2 : (∀ x ∈ xs'.val, x.val ≠ []) ↔ ∀ x ∈ xs, x ≠ [] := by
        have e := htr.2.2
        constructor
        · intro h x hx hnil
          have : ([] : List Nat) ∈ xs'.val.map (fun x => x.val.map (·.val)) := by
            rw [e]; exact List.mem_map.mpr ⟨x, hx, by rw [hnil]; rfl⟩
          obtain ⟨y, hy, hy'⟩ := List.mem_map.mp this
          exact h y hy (List.map_eq_nil_iff.mp hy')
        · intro h y hy hnil
          have : ([] : List Nat) ∈ xs.map (fun x => x.map (·.toNat)) := by
            rw [← e]; exact List.mem_map.mpr ⟨y, hy, by rw [hnil]; rfl⟩
          obtain ⟨x, hx, hx'⟩ := List.mem_map.mp this
          exact h x hx (List.map_eq_nil_iff.mp hx')
      have hne : xs'.val ≠ [] ↔ xs ≠ [] := by
        rw [Ne, Ne, ← List.length_eq_zero_iff, ← List.length_eq_zero_iff, hlen]
      have h1 : aIdx (H ++ [P]) l = 1 ↔ l = .stop :=
        ⟨fun h => aIdx_inj hlm' (stop_mem _) (by rw [h, aIdx_stop]), fun h => by rw [h, aIdx_stop]⟩
      have h0 : aIdx (H ++ [P]) r = 0 ↔ r = .start :=
        ⟨fun h => aIdx_inj hrm' (start_mem _) (by rw [h, aIdx_start]), fun h => by rw [h, aIdx_start]⟩
      have h2 : aIdx (H ++ [P]) l = aIdx (H ++ [P]) r ↔ l = r :=
        ⟨fun h => aIdx_inj hlm' hrm' h, fun h => by rw [h]⟩
      have hinv : (¬ (xs'.val ≠ [] ∧ ∀ x ∈ xs'.val, x.val ≠ []) ∨ DocKernel.anchorIdx l' = 1 ∨
          DocKernel.anchorIdx r' = 0 ∨ DocKernel.anchorIdx l' = DocKernel.anchorIdx r') ↔
          (xs = [] ∨ (∃ x ∈ xs, x = []) ∨ l = .stop ∨ r = .start ∨ l = r) := by
        rw [il, ir, h1, h0, h2, hne, hx2]
        constructor
        · rintro (h | h | h | h)
          · by_cases hx : xs = []
            · exact Or.inl hx
            · right; left
              by_contra hc
              push_neg at hc
              exact h ⟨hx, fun x hx' e => hc x hx' e⟩
          · exact Or.inr (Or.inr (Or.inl h))
          · exact Or.inr (Or.inr (Or.inr (Or.inl h)))
          · exact Or.inr (Or.inr (Or.inr (Or.inr h)))
        · rintro (h | ⟨x, hx, h⟩ | h | h | h)
          · exact Or.inl (fun hc => hc.1 h)
          · exact Or.inl (fun hc => hc.2 x hx h)
          · exact Or.inr (Or.inl h)
          · exact Or.inr (Or.inr (Or.inl h))
          · exact Or.inr (Or.inr (Or.inr h))
      by_cases hinvM : xs = [] ∨ (∃ x ∈ xs, x = []) ∨ l = .stop ∨ r = .start ∨ l = r
      · rw [if_pos (hinv.mpr hinvM), if_pos hinvM]
        simp only [DocKernel.refuseS]
        exact ⟨by unfold document.INVALID_INSERTION; decide,
          by simp [errorOfT, ho, document.INVALID_INSERTION]⟩
      · rw [if_neg (fun h => hinvM (hinv.mp h)), if_neg hinvM]
        have hreach := reaches_tr d H P dp hr hb hv ht adm st own hown hor hoe l r hlm' hrm'
        rw [il, ir]
        by_cases hre : Reaches H P.deps own l r
        · rw [if_neg (not_not.mpr (hreach.mpr hre)), if_neg (not_not.mpr hre)]
          refine ⟨hz, ⟨?_, hown.2⟩, ?_, hor, ?_⟩
          · simp only [hown.1, List.map_append]
            congr 1
            unfold DocModel.edgesOf
            simp only []
            rw [map_zip_pair, chainM_closed, List.map_cons, List.map_append, List.map_singleton,
              List.map_map, hlen]
            have hn : ∀ j ∈ List.range xs.length,
                (aIdx (H ++ [P]) ∘ fun j => Anchor.node ⟨⟨P.id, k⟩, j⟩) j =
                  d.atoms.val.length + st.created + j := by
              intro j hj
              simp only [Function.comp_apply]
              rw [aIdx_new H P adm.1 k l r xs ho j (List.mem_range.mp hj), hal, hcr]
            rw [List.map_congr_left hn]
          · rw [createdBy_succ P k _ ho, hcr, hlen]; rfl
          · intro e he
            simp only [List.mem_append] at he
            rcases he with he | he
            · exact hoe e he
            · unfold DocModel.edgesOf at he
              simp only [] at he
              have hnode : ∀ j < xs.length, Anchor.node ⟨⟨P.id, k⟩, j⟩ ∈ anchorList (H ++ [P]) := by
                intro j hj
                exact node_new_mem H P k l r xs ho j hj
              have hz := List.of_mem_zip he
              constructor
              · rcases List.mem_cons.mp hz.1 with h | h
                · rw [h]; exact hlm'
                · obtain ⟨j, hj, hje⟩ := List.mem_map.mp h
                  rw [← hje]; exact hnode j (List.mem_range.mp hj)
              · rcases List.mem_append.mp hz.2 with h | h
                · obtain ⟨j, hj, hje⟩ := List.mem_map.mp h
                  rw [← hje]; exact hnode j (List.mem_range.mp hj)
                · rw [List.mem_singleton.mp h]; exact hrm'
        · rw [if_pos (fun h => hre (hreach.mp h)), if_pos hre]
          simp only [DocKernel.refuseS]
          exact ⟨by unfold document.UNORDERED_ANCHORS; decide,
            by simp [errorOfT, ho, document.UNORDERED_ANCHORS]⟩
    · rw [cl, hcl, cr, hcr']
      have hne := code_ne' H P.deps r er hcr'
      have z : ¬ ((0#u64 : U64) ≠ 0#u64) := by simp
      simp only [codeOf, z, ↓reduceIte] at hne ⊢
      rw [if_pos hne]
      simp only [DocKernel.refuseS]
      obtain ⟨n, hn, he⟩ := checkAnchor_some' H P.deps r er hcr'
      exact ⟨hne, errorOfT_insert P k l r xs ho n 1 (by simpa using hn) er he⟩
  · rw [cl, hcl]
    try dsimp only
    have hne := code_ne' H P.deps l el hcl
    rw [if_pos hne]
    simp only [DocKernel.refuseS]
    obtain ⟨n, hn, he⟩ := checkAnchor_some' H P.deps l el hcl
    exact ⟨hne, errorOfT_insert P k l r xs ho n 0 (by simpa using hn) el he⟩

/-! ## Delete operations -/

/-- The model's per-target check of a deletion. -/
noncomputable def delF (H : Hist) (deps : List PId) (ts : List NodeId) : NodeId × Nat → Option DocModel.Error :=
  fun (t, j) =>
    match checkAnchor H deps (.node t) with
    | some e => some e
    | none => if t ∈ ts.take j then some (.duplicateTarget t) else none

/-- The refusal code of a deletion error. -/
def codeD : DocModel.Error → U64
  | .duplicateTarget _ => document.DUPLICATE_TARGET
  | e => codeOf (some e)

/-- A deletion error names target `j`. -/
def DelErr (ts : List NodeId) (j : Nat) (hj : j < ts.length) (e : DocModel.Error) : Prop :=
  e = .unobservedPatch ts[j].op.patch ∨ e = .unknownAnchor (.node ts[j]) ∨ e = .duplicateTarget ts[j]

theorem codeD_ne (ts : List NodeId) (j : Nat) (hj : j < ts.length) (e : DocModel.Error) (h : DelErr ts j hj e) :
    codeD e ≠ 0#u64 := by
  rcases h with rfl | rfl | rfl
  · show document.UNOBSERVED_PATCH ≠ 0#u64; unfold document.UNOBSERVED_PATCH; decide
  · show document.UNKNOWN_ANCHOR ≠ 0#u64; unfold document.UNKNOWN_ANCHOR; decide
  · show document.DUPLICATE_TARGET ≠ 0#u64; unfold document.DUPLICATE_TARGET; decide

theorem errorOfT_delete (P : Patch) (k : Nat) (ts : List NodeId) (ho : P.ops[k]? = some (.delete ts))
    (j : Nat) (hj : j < ts.length) (e : DocModel.Error) (h : DelErr ts j hj e) :
    errorOfT P (codeD e, k, j) = some e := by
  unfold errorOfT
  rw [ho]
  rcases h with rfl | rfl | rfl
  · simp [codeD, codeOf, document.UNOBSERVED_PATCH, List.getElem?_eq_getElem hj]
  · simp [codeD, codeOf, document.UNKNOWN_ANCHOR, List.getElem?_eq_getElem hj]
  · simp [codeD, document.DUPLICATE_TARGET, List.getElem?_eq_getElem hj]

/-- The kernel's target loop is the model's search, prefix by prefix. -/
theorem delete_loop (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hv : Valid H) (ht : Tr H P dp) (adm : Admissible H P)
    (refusal : U64 × Nat × Nat) (hz : refusal.1 = 0#u64) (k : Nat)
    (ts : List NodeId) (rs : List document.Ref) (hF : List.Forall₂ (TrNode H) ts rs) :
    ∀ i, i ≤ ts.length →
      match (ts.take i).zipIdx.findSome? (delF H P.deps ts) with
      | none => (List.range i).foldl (DocKernel.deleteStep d (DocKernel.seenM d.deps.val dp.deps.val) k rs)
          (refusal, []) = (refusal, (rs.take i).map (·.index))
      | some e => ∃ j, ∃ hj : j < ts.length,
          ((List.range i).foldl (DocKernel.deleteStep d (DocKernel.seenM d.deps.val dp.deps.val) k rs)
            (refusal, [])).1 = (codeD e, k, j) ∧ DelErr ts j hj e := by
  have hlen := hF.length_eq
  have hT : ∀ i (h1 : i < ts.length) (h2 : i < rs.length), TrNode H ts[i] rs[i] := by
    intro i h1 h2
    have := hF.get h1 h2
    simpa using this
  intro i
  induction i with
  | zero => intro _; simp
  | succ i ih =>
    intro hi
    have ih := ih (by omega)
    have hi1 : i < ts.length := by omega
    have hi2 : i < rs.length := by omega
    have htake : (ts.take (i + 1)).zipIdx = (ts.take i).zipIdx ++ [(ts[i], i)] := by
      rw [List.take_succ, List.getElem?_eq_getElem hi1, Option.toList_some, List.zipIdx_append]
      simp [Nat.min_eq_left hi1.le]
    rw [htake, List.findSome?_append, List.range_succ, List.foldl_append, List.foldl_cons, List.foldl_nil]
    revert ih
    rcases hG : (ts.take i).zipIdx.findSome? (delF H P.deps ts) with _ | e
    · intro ih
      simp only [Option.none_or, ih]
      rw [DocKernel.deleteStep]
      rw [dif_pos ⟨hz, hi2⟩]
      have hcode := checkAnchor_code d H P dp hr hv ht adm ts[i] rs[i] (hT i hi1 hi2)
      -- earlier targets passed their anchor checks
      have prior : ∀ i' (h' : i' < i), checkAnchor H P.deps (.node (ts[i]'(by omega))) = none →
          checkAnchor H P.deps (.node (ts[i']'(by omega))) = none := by
        intro i' h' _
        have hm : (ts[i']'(by omega), i') ∈ (ts.take i).zipIdx :=
          List.mem_zipIdx_iff_getElem?.mpr (by simp [h', List.getElem?_take])
        have := List.findSome?_eq_none_iff.mp hG _ hm
        unfold delF at this
        cases hc : checkAnchor H P.deps (.node (ts[i']'(by omega))) with
        | none => rfl
        | some e' => simp [hc] at this
      simp only [List.findSome?_cons, List.findSome?_nil]
      rcases hc : checkAnchor H P.deps (.node ts[i]) with _ | e
      · have hk0 : DocKernel.checkAnchorM (.Node rs[i]) d.atoms.val.length (DocKernel.seenM d.deps.val dp.deps.val) = 0#u64 := by
          rw [hcode, hc]; rfl
        have hmi := checkAnchor_none_mem H P.deps _ hc
        have hdup : rs[i].index ∈ (rs.take i).map (·.index) ↔ ts[i] ∈ ts.take i := by
          rw [List.mem_map, List.mem_iff_getElem]
          constructor
          · rintro ⟨x, hx, hxe⟩
            obtain ⟨i', hi', rfl⟩ := List.mem_iff_getElem.mp hx
            simp only [List.length_take] at hi'
            have hi'' : i' < i := by omega
            refine ⟨i', by simp; omega, ?_⟩
            have hm' := checkAnchor_none_mem H P.deps _ (prior i' hi'' hc)
            have e1 := (hT i' (by omega) (by omega)).2
            have e2 := (hT i hi1 hi2).2
            have hv' : (rs[i']'(by omega)).index.val = rs[i].index.val := by
              rw [← hxe]; simp
            rw [e1, e2] at hv'
            have := aIdx_inj hm' hmi hv'
            simp only [Anchor.node.injEq] at this
            simp [this]
          · rintro ⟨i', hi', hte⟩
            simp only [List.length_take] at hi'
            refine ⟨(rs.take i)[i']'(by simp; omega), List.getElem_mem _, ?_⟩
            apply UScalar.eq_of_val_eq
            simp only [List.getElem_take]
            rw [(hT i' (by omega) (by omega)).2, (hT i hi1 hi2).2]
            simp only [List.getElem_take] at hte
            rw [hte]
        unfold delF
        simp only [hc]
        by_cases hd : ts[i] ∈ ts.take i
        · rw [if_pos hd]
          simp only [Option.some_or]
          refine ⟨i, hi1, ?_, Or.inr (Or.inr rfl)⟩
          rw [if_neg (by rw [hk0]; simp), if_pos (hdup.mpr hd)]
          rfl
        · rw [if_neg hd]
          simp only [Option.none_or]
          rw [if_neg (by rw [hk0]; simp), if_neg (fun h => hd (hdup.mp h))]
          rw [List.take_succ, List.getElem?_eq_getElem hi2, Option.toList_some, List.map_append]
          rfl
      · have hne := code_ne' H P.deps _ e hc
        unfold delF
        simp only [hc, Option.some_or]
        obtain ⟨n, hn, he⟩ := checkAnchor_some' H P.deps _ e hc
        simp only [Anchor.node.injEq] at hn
        refine ⟨i, hi1, ?_, ?_⟩
        · rw [if_pos (by rw [hcode, hc]; exact hne), hcode, hc]
          rcases he with rfl | rfl <;> rfl
        · unfold DelErr; rw [hn]; rcases he with rfl | rfl
          · exact Or.inl rfl
          · exact Or.inr (Or.inl rfl)
    · intro ih
      simp only [Option.some_or]
      obtain ⟨j, hj, h1, h2⟩ := ih
      refine ⟨j, hj, ?_, h2⟩
      rw [DocKernel.deleteStep]
      rw [dif_neg (by rw [h1]; exact fun h => codeD_ne ts j hj e h2 h.1)]
      exact h1

theorem delete_step (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (ts : List NodeId) (rs : alloc.vec.Vec document.Ref) (ho : P.ops[k]? = some (.delete ts))
    (htr : List.Forall₂ (TrNode H) ts rs.val) :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k (.Delete rs)
    match validOp H P.id P.deps own k (.delete ts) with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧ errorOfT P st'.refusal = some e := by
  dsimp only
  obtain ⟨hz, hown, hcr, hor, hoe⟩ := hs
  have hv := built_valid hb
  have hst' : DocKernel.checkOpS st d (DocKernel.seenM d.deps.val dp.deps.val) k (.Delete rs) =
      DocKernel.checkDeleteS st d (DocKernel.seenM d.deps.val dp.deps.val) k rs.val := by
    unfold DocKernel.checkOpS
    rw [if_neg (by rw [hz]; simp)]
  have hlen : rs.val.length = ts.length := htr.length_eq.symm
  have hcr' : createdBy P (k + 1) = createdBy P k := by
    rw [createdBy_succ P k _ ho]; rfl
  rw [hst', show validOp H P.id P.deps own k (.delete ts) =
    if ts = [] then .error .emptyDeletion else
      match ts.zipIdx.findSome? (delF H P.deps ts) with
      | some e => .error e
      | none => .ok own from rfl]
  unfold DocKernel.checkDeleteS
  try dsimp only
  by_cases h0 : ts = []
  · have h0' : rs.val = [] := List.length_eq_zero_iff.mp (by rw [hlen, h0]; rfl)
    rw [if_pos h0, if_pos h0']
    simp only [DocKernel.refuseS]
    exact ⟨by unfold document.EMPTY_DELETION; decide, by simp [errorOfT, ho, document.EMPTY_DELETION]⟩
  · have h0' : rs.val ≠ [] := fun h => h0 (List.length_eq_zero_iff.mp (by rw [← hlen, h]; rfl))
    rw [if_neg h0, if_neg h0']
    have L := delete_loop d H P dp hr hv ht adm st.refusal hz k ts rs.val htr ts.length le_rfl
    rw [List.take_length] at L
    rw [hlen]
    revert L
    rcases ts.zipIdx.findSome? (delF H P.deps ts) with _ | e
    · intro L
      simp only []
      rw [L]
      exact ⟨hz, hown, by rw [hcr', hcr], hor, hoe⟩
    · intro L
      obtain ⟨j, hj, h1, h2⟩ := L
      simp only []
      rw [h1]
      exact ⟨codeD_ne ts j hj e h2, errorOfT_delete P k ts ho j hj e h2⟩

/-! ## Commit keeps the representation (statements) -/

/-- One operation: the kernel refuses exactly when the model does, with the
same error, and otherwise both continue in step. -/
theorem step_sim (d : document.Doc) (H : Hist) (P : Patch) (dp : document.Patch)
    (hr : Rep d H) (hb : Built H) (ht : Tr H P dp) (adm : Admissible H P)
    (st : DocKernel.CheckS) (own : Own) (k : Nat) (hs : StateRel H P st own k)
    (o : Op) (o' : document.Op) (ho : P.ops[k]? = some o) (htr : TrOp H o o') :
    let seen := DocKernel.seenM d.deps.val dp.deps.val
    let st' := DocKernel.checkOpS st d seen k o'
    match validOp H P.id P.deps own k o with
    | .ok own' => StateRel H P st' own' (k + 1)
    | .error e => st'.refusal.1 ≠ 0#u64 ∧
        errorOfT P st'.refusal = some e := by
  cases o <;> cases o' <;> simp only [TrOp] at htr
  case insert.Insert l r xs l' r' xs' => exact insert_step d H P dp hr hb ht adm st own k hs l r xs l' r' xs' ho htr
  case delete.Delete ts rs => exact delete_step d H P dp hr hb ht adm st own k hs ts rs ho htr
  case restore.Restore d₀ r => exact restore_step d H P dp hr hb ht adm st own k hs d₀ r ho htr
  case order.Order b a b' a' => exact order_step d H P dp hr hb ht adm st own k hs b a b' a' ho htr
  case keep.Keep d₀ i₀ rd ri => exact keep_step d H P dp hr hb ht adm st own k hs d₀ i₀ rd ri ho htr
  case retract.Retract o₀ r => exact retract_step d H P dp hr hb ht adm st own k hs o₀ r ho htr

/-- A prefix of a patch's operations. -/
def prefixPatch (P : Patch) (j : Nat) : Patch := { P with ops := P.ops.take j }

/-- A replayed history is one the model admits patch by patch. -/
theorem replayed_valid {d : document.Doc} {H : Hist} (h : Replayed d H) : Valid H := by
  induction h with
  | nil _ => rfl
  | @snoc d0 d1 H P dp0 hr0 ht0 adm hval hroom hc ih =>
    unfold Valid at ih ⊢
    rw [replay_append, ih]
    change replay H [P] = .ok (H ++ [P])
    rw [replay, applySpec_inserted adm.1 adm.2.1 adm.2.2.1 adm.2.2.2.1 adm.2.2.2.2 hval]
    rfl

end o5s_kernel.DocRep
