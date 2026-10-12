import DocBridge
open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel
/-!
# Decoding the kernel's view

The view `materialize` returns, decoded through the anchor and operation
tables (`Document.decode`), is the specified view (`isView_of`): its layers
are the depth layers, the blocked atoms form the cycle conflict, a layer of two
or more atoms is an order conflict, and the bytes are the atoms' bytes in layer
order. The delete/insert conflicts the kernel finds are exactly the model's
`DI` (`di_iff`), through the dependency rows (`rowReach_anc`: a patch's rows
reach exactly its causal past) and the deletion, insertion and
acknowledgement tables.
-/
namespace o5s_kernel.DocDecode
open DocModel Document DocRep DocKernel DocKahn DocMatModel DocBridge

variable {d : document.Doc} {H : Hist}

/-- The graph the decoded view is read against: liveness by index, the model's depth and blocking. -/
def gH (d : document.Doc) (H : Hist) : KG :=
  KG.mk d.atoms.val.length (liveL d) (fun _ => []) (DH H) (BH H)

theorem gL_iff (hr : Rep d H) (hb : Built H) {a : Nat} :
    (gH d H).L a ↔ a < (anchorList H).length ∧ Live H (anc H a) := by
  have hat := atoms_len hr
  show (a < d.atoms.val.length ∧ (liveL d)[a]?.getD false = true) ↔ _
  constructor
  · rintro ⟨h1, h2⟩; exact ⟨hat ▸ h1, (liveL_iff hr hb h1).mp h2⟩
  · rintro ⟨h1, h2⟩; exact ⟨hat ▸ h1, (liveL_iff hr hb (hat ▸ h1)).mpr h2⟩

/-- The index of a live anchor. -/
theorem live_index (hr : Rep d H) (hb : Built H) {n : NodeId} (h : Live H (.node n)) :
    ∃ a, (gH d H).L a ∧ anc H a = .node n := by
  have hm := DocBridge.live_mem h
  exact ⟨aIdx H (.node n), (gL_iff hr hb).mpr ⟨idx_lt hm, by rw [anc_aIdx hm]; exact h⟩, anc_aIdx hm⟩

theorem layer_mem (hr : Rep d H) (hb : Built H) {rest : List Bool} {layers : List (alloc.vec.Vec Usize)}
    {bytes : List U8} {b : Bool} (hF : (gH d H).Final d.atoms.val rest layers bytes b)
    {k : Nat} (hk : k < layers.length) (n : NodeId) :
    n ∈ nodes H (layers[k]).val ↔ Live H (.node n) ∧ ¬ Blocked H (.node n) ∧ Depth H (.node n) k := by
  rw [mem_nodes]
  constructor
  · rintro ⟨a, ha, hal, he⟩
    obtain ⟨hl, hnb, hd⟩ := (hF.lay k hk a).mp ha
    have hl' := ((gL_iff hr hb).mp hl).2
    rw [he] at hl'
    refine ⟨hl', fun hbk => hnb ⟨hal, by rw [he]; exact hbk⟩, ?_⟩
    obtain ⟨-, hd'⟩ := hd
    rw [he] at hd'; exact hd'
  · rintro ⟨hl, hnb, hd⟩
    obtain ⟨a, hla, he⟩ := live_index hr hb hl
    have hal := ((gL_iff hr hb).mp hla).1
    refine ⟨a, (hF.lay k hk a).mpr ⟨hla, fun ⟨_, hbk⟩ => hnb (he ▸ hbk), hal, he ▸ hd⟩, hal, he⟩

theorem gB_iff {a : Nat} : (gH d H).B a ↔ a < (anchorList H).length ∧ Blocked H (anc H a) := Iff.rfl

theorem blocked_mem (hr : Rep d H) (hb : Built H) {rest : List Bool}
    {layers : List (alloc.vec.Vec Usize)} {bytes : List U8} {b : Bool}
    (hF : (gH d H).Final d.atoms.val rest layers bytes b) (blocked : List Usize)
    (hbl : blocked.map (·.val) = (List.range d.atoms.val.length).filter (fun x => b && remF rest x))
    (n : NodeId) :
    n ∈ nodes H blocked ↔ b = true ∧ Live H (.node n) ∧ Blocked H (.node n) := by
  have hat := atoms_len hr
  rw [mem_nodes, hbl]
  constructor
  · rintro ⟨a, ha, hal, he⟩
    rw [List.mem_filter, List.mem_range, Bool.and_eq_true] at ha
    obtain ⟨ha1, hbt, hr'⟩ := ha
    obtain ⟨hl, hbk⟩ := (hF.rem a (by simpa [gH] using ha1)).mp hr'
    have hl' := ((gL_iff hr hb).mp hl).2
    rw [he] at hl'
    exact ⟨hbt, hl', by have := hbk.2; rw [he] at this; exact this⟩
  · rintro ⟨hbt, hl, hbk⟩
    obtain ⟨a, hla, he⟩ := live_index hr hb hl
    have hal := ((gL_iff hr hb).mp hla).1
    refine ⟨a, ?_, hal, he⟩
    rw [List.mem_filter, List.mem_range, Bool.and_eq_true]
    exact ⟨by rw [hat]; exact hal, hbt, (hF.rem a (by show a < d.atoms.val.length; rw [hat]; exact hal)).mpr ⟨hla, hal, by rw [he]; exact hbk⟩⟩

theorem blocked_ne (hr : Rep d H) (hb : Built H) {rest : List Bool}
    {layers : List (alloc.vec.Vec Usize)} {bytes : List U8} {b : Bool}
    (hF : (gH d H).Final d.atoms.val rest layers bytes b) (blocked : List Usize)
    (hbl : blocked.map (·.val) = (List.range d.atoms.val.length).filter (fun x => b && remF rest x)) :
    blocked ≠ [] ↔ ∃ c, Blocked H c := by
  have hat := atoms_len hr
  constructor
  · intro hne
    obtain ⟨x, hx⟩ := List.exists_mem_of_ne_nil _ hne
    have : x.val ∈ blocked.map (·.val) := List.mem_map.mpr ⟨x, hx, rfl⟩
    rw [hbl, List.mem_filter, List.mem_range, Bool.and_eq_true] at this
    obtain ⟨h1, -, h3⟩ := this
    exact ⟨_, ((hF.rem _ (by simpa [gH] using h1)).mp h3).2.2⟩
  · rintro ⟨c, hc⟩
    have hl := blocked_live hc
    obtain ⟨n, rfl⟩ := live_node hl
    obtain ⟨a, hla, he⟩ := live_index hr hb hl
    have hal := ((gL_iff hr hb).mp hla).1
    have hB : (gH d H).B a := ⟨hal, by rw [he]; exact hc⟩
    have hbt : b = true := hF.blocked.mpr ⟨a, hla, hB⟩
    have hr' := (hF.rem a (by show a < d.atoms.val.length; rw [hat]; exact hal)).mpr ⟨hla, hB⟩
    have : a ∈ blocked.map (·.val) := by
      rw [hbl, List.mem_filter, List.mem_range, Bool.and_eq_true]; exact ⟨by rw [hat]; exact hal, hbt, hr'⟩
    intro h; rw [h] at this; simp at this

theorem filterMap_length_of_isSome {α β} (f : α → Option β) (l : List α) (h : ∀ x ∈ l, (f x).isSome) :
    (l.filterMap f).length = l.length := by
  induction l with
  | nil => rfl
  | cons x t ih =>
    obtain ⟨y, hy⟩ := Option.isSome_iff_exists.mp (h x (by simp))
    rw [List.filterMap_cons, hy]
    simp [ih (fun z hz => h z (by simp [hz]))]

theorem card_nodes (hr : Rep d H) (hb : Built H) {rest : List Bool}
    {layers : List (alloc.vec.Vec Usize)} {bytes : List U8} {b : Bool}
    (hF : (gH d H).Final d.atoms.val rest layers bytes b) {L : alloc.vec.Vec Usize} (hL : L ∈ layers) :
    (nodes H L.val).card = L.val.length := by
  obtain ⟨k, hk, hLk⟩ := List.getElem_of_mem hL
  have hall : ∀ x ∈ L.val, ∃ n, nodeAt H x.val = some n := by
    intro x hx
    rw [← hLk] at hx
    have hm : x.val ∈ (layers[k]).val.map (·.val) := List.mem_map.mpr ⟨x, hx, rfl⟩
    obtain ⟨hl, -, -⟩ := (hF.lay k hk x.val).mp hm
    obtain ⟨hal, hlive⟩ := (gL_iff hr hb).mp hl
    obtain ⟨n, hn⟩ := live_node hlive
    exact ⟨n, nodeAt_iff.mpr ⟨hal, hn⟩⟩
  have hnd : L.val.Nodup := List.Nodup.of_map _ (hF.lnd L hL).1
  unfold nodes
  rw [List.toFinset_card_of_nodup]
  · exact filterMap_length_of_isSome _ _ (fun x hx => by obtain ⟨n, hn⟩ := hall x hx; simp [hn])
  · apply List.Nodup.filterMap ?_ hnd
    intro a a' n hn hn'
    have h1 := nodeAt_iff.mp hn
    have h2 := nodeAt_iff.mp hn'
    apply UScalar.eq_of_val_eq
    have e1 := aIdx_anc hb h1.1
    have e2 := aIdx_anc hb h2.1
    rw [h1.2] at e1; rw [h2.2] at e2; rw [← e1, ← e2]

theorem b_of_blocked {rest : List Bool} {b : Bool} {blocked : List Usize} {N : Nat}
    (hbl : blocked.map (·.val) = (List.range N).filter (fun x => b && remF rest x)) (h : blocked ≠ []) :
    b = true := by
  obtain ⟨x, hx⟩ := List.exists_mem_of_ne_nil _ h
  have : x.val ∈ blocked.map (·.val) := List.mem_map.mpr ⟨x, hx, rfl⟩
  rw [hbl, List.mem_filter, Bool.and_eq_true] at this
  exact this.2.1

theorem nodes_single (hr : Rep d H) (hb : Built H) {rest : List Bool} {layers : List (alloc.vec.Vec Usize)}
    {bytes : List U8} {b : Bool} (hF : (gH d H).Final d.atoms.val rest layers bytes b)
    {k : Nat} (hk : k < layers.length) {x : Usize} (hx : (layers[k]).val = [x]) :
    ∃ m, x.val < (anchorList H).length ∧ anc H x.val = .node m ∧ nodes H (layers[k]).val = {m} := by
  have hm : x.val ∈ (layers[k]).val.map (·.val) := by rw [hx]; simp
  obtain ⟨hl, -, -⟩ := (hF.lay k hk x.val).mp hm
  obtain ⟨hal, hlive⟩ := (gL_iff hr hb).mp hl
  obtain ⟨m, hm'⟩ := live_node hlive
  refine ⟨m, hal, hm', ?_⟩
  ext m'
  rw [mem_nodes, hx, Finset.mem_singleton]
  simp only [List.map_cons, List.map_nil, List.mem_singleton, exists_eq_left]
  constructor
  · rintro ⟨-, he⟩; rw [hm'] at he; cases he; rfl
  · rintro rfl; exact ⟨hal, hm'⟩


theorem bytes_layers (atoms : List (alloc.vec.Vec U8))
    (hbytes : ∀ a < atoms.length, ∀ m, anc H a = .node m →
      (atomB atoms a).map (fun c => UInt8.ofNat c.val) = (atom H m).getD [])
    (hlen : (anchorList H).length = atoms.length) :
    ∀ ls : List (alloc.vec.Vec Usize), (∀ l ∈ ls, ∃ x, l.val = [x] ∧ ∃ m, x.val < (anchorList H).length ∧
      anc H x.val = .node m ∧ nodes H l.val = {m}) →
    ((ls.flatMap (fun L => L.val.map (·.val))).flatMap (atomB atoms)).map (fun c => UInt8.ofNat c.val) =
      ((ls.map (fun l => nodes H l.val)).flatMap Finset.toList).flatMap (fun n => (atom H n).getD []) := by
  intro ls hall
  induction ls with
  | nil => simp
  | cons l t ih =>
    obtain ⟨x, hx, m, hxl, hm, hn⟩ := hall l (by simp)
    have iht := ih (fun l' hl' => hall l' (by simp [hl']))
    have e1 : (l :: t).flatMap (fun L => L.val.map (·.val)) = [x.val] ++ t.flatMap (fun L => L.val.map (·.val)) := by
      simp [hx]
    have e2 : ((l :: t).map (fun l => nodes H l.val)).flatMap Finset.toList =
        [m] ++ (t.map (fun l => nodes H l.val)).flatMap Finset.toList := by
      simp [hn]
    rw [e1, e2, List.flatMap_append, List.flatMap_append, List.map_append, iht]
    congr 1
    simp only [List.flatMap_cons, List.flatMap_nil, List.append_nil]
    exact hbytes x.val (hlen ▸ hxl) m hm

/-- The decoded kernel view is the specified view, given the delete/insert
correspondence and the atom bytes. -/
theorem isView_of (hr : Rep d H) (hb : Built H) (v : document.View) (rest : List Bool) (b : Bool)
    (hF : (gH d H).Final d.atoms.val rest v.layers.val v.bytes.val b)
    (hbl : v.blocked.val.map (·.val) = (List.range d.atoms.val.length).filter (fun x => b && remF rest x))
    (hclean : v.clean = (v.layers.val.all (fun L => L.val.length = 1) && !b && v.delete_inserts.val.isEmpty))
    (hDI : ∀ dd ii A, (∃ x ∈ v.delete_inserts.val, ∃ dd' ii', (deletionList H)[x.deletion.val]? = some dd' ∧
      (insertionList H)[x.insertion.val]? = some ii' ∧
      Conflict.deleteInsert dd ii A = Conflict.deleteInsert dd' ii' (nodes H x.anchors.val)) ↔ DI H dd ii A)
    (hdiv : ∀ x ∈ v.delete_inserts.val, x.deletion.val < (deletionList H).length ∧
      x.insertion.val < (insertionList H).length)
    (hbytes : ∀ a < d.atoms.val.length, ∀ m, anc H a = .node m →
      (atomB d.atoms.val a).map (fun c => UInt8.ofNat c.val) = (atom H m).getD []) :
    IsView H (decode H v) := by
  have hat := atoms_len hr
  have hbne : v.blocked.val ≠ [] ↔ b = true := by
    constructor
    · exact b_of_blocked hbl
    · intro hbt
      obtain ⟨a, hla, hB⟩ := hF.blocked.mp hbt
      have hal := hla.1
      have hr' := (hF.rem a hal).mpr ⟨hla, hB⟩
      have : a ∈ v.blocked.val.map (·.val) := by
        rw [hbl, List.mem_filter, List.mem_range, Bool.and_eq_true]; exact ⟨hal, hbt, hr'⟩
      intro h; rw [h] at this; simp at this
  have hlen1 : ∀ L ∈ v.layers.val, 1 ≤ L.val.length := fun L hL =>
    List.length_pos_iff.mpr (hF.lnd L hL).2
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · -- layers
    intro k hk
    have hk' : k < v.layers.val.length := by simpa [decode] using hk
    ext n
    simp only [decode, List.getElem_map, Finset.mem_coe]
    rw [layer_mem hr hb hF hk' n]
    rfl
  · -- layers_all
    intro n hl hnb
    obtain ⟨a, hla, he⟩ := live_index hr hb hl
    obtain ⟨j, hj, hd⟩ := hF.all a hla (fun ⟨_, hbk⟩ => hnb (he ▸ hbk))
    refine ⟨j, by simpa [decode] using hj, ?_⟩
    rw [getElem!_pos _ j (by simpa [decode] using hj)]
    simp only [decode, List.getElem_map]
    exact (layer_mem hr hb hF hj n).mpr ⟨hl, hnb, by have := hd.2; rw [he] at this; exact this⟩
  · -- layers_nonempty
    intro L hL
    simp only [decode, List.mem_map] at hL
    obtain ⟨l, hl, rfl⟩ := hL
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hl
    obtain ⟨x, hx⟩ := List.exists_mem_of_ne_nil _ (hF.lnd _ hl).2
    have hm : x.val ∈ (v.layers.val[k]).val.map (·.val) := List.mem_map.mpr ⟨x, hx, rfl⟩
    obtain ⟨hlx, -, -⟩ := (hF.lay k hk x.val).mp hm
    obtain ⟨hal, hlive⟩ := (gL_iff hr hb).mp hlx
    obtain ⟨m, hm'⟩ := live_node hlive
    exact ⟨m, mem_nodes.mpr ⟨x.val, hm, hal, hm'⟩⟩
  · -- cycle
    intro B'
    simp only [decode, Set.mem_setOf_eq]
    constructor
    · rintro (⟨l, -, -, h⟩ | ⟨hne, h⟩ | ⟨x, -, dd, ii, -, -, h⟩)
      · cases h
      · cases h
        have hbt := hbne.mp hne
        refine ⟨(blocked_ne hr hb hF _ hbl).mp hne, fun n => ?_⟩
        rw [blocked_mem hr hb hF _ hbl, hbt]; simp
      · cases h
    · rintro ⟨hc, hB⟩
      have hne := (blocked_ne hr hb hF _ hbl).mpr hc
      have hbt := hbne.mp hne
      refine .inr (.inl ⟨hne, ?_⟩)
      congr 1
      ext n
      rw [hB, blocked_mem hr hb hF _ hbl, hbt]; simp
  · -- order
    intro F
    simp only [decode, Set.mem_setOf_eq]
    constructor
    · rintro (⟨l, hl, h2, h⟩ | ⟨-, h⟩ | ⟨x, -, dd, ii, -, -, h⟩)
      · cases h
        exact ⟨List.mem_map.mpr ⟨l, hl, rfl⟩, by rw [card_nodes hr hb hF hl]; exact h2⟩
      · cases h
      · cases h
    · rintro ⟨hF', h2⟩
      obtain ⟨l, hl, rfl⟩ := List.mem_map.mp hF'
      exact .inl ⟨l, hl, by rw [card_nodes hr hb hF hl] at h2; exact h2, rfl⟩
  · -- deleteInsert
    intro dd ii A
    simp only [decode, Set.mem_setOf_eq]
    constructor
    · rintro (⟨l, -, -, h⟩ | ⟨-, h⟩ | h)
      · cases h
      · cases h
      · exact (hDI dd ii A).mp h
    · intro h; exact .inr (.inr ((hDI dd ii A).mpr h))
  · -- bytes_none
    have hnone : (decode H v).bytes = none ↔ v.clean = false := by
      simp only [decode]; cases v.clean <;> simp
    rw [hnone, hclean]
    simp only [decode]
    constructor
    · intro h
      by_cases hall : v.layers.val.all (fun L => L.val.length = 1) = true
      · by_cases hbt : b = true
        · have hne := hbne.mpr hbt
          exact ⟨.cycle (nodes H v.blocked.val), .inr (.inl ⟨hne, rfl⟩)⟩
        · have hie : v.delete_inserts.val.isEmpty = false := by
            simp only [hall, Bool.true_and] at h
            cases hb' : b <;> simp_all
          obtain ⟨x, hx⟩ := List.exists_mem_of_ne_nil _ (by simpa [List.isEmpty_iff] using hie)
          obtain ⟨h1, h2⟩ := hdiv x hx
          exact ⟨.deleteInsert _ _ (nodes H x.anchors.val), .inr (.inr ⟨x, hx, _, _,
            List.getElem?_eq_getElem h1, List.getElem?_eq_getElem h2, rfl⟩)⟩
      · simp only [Bool.not_eq_true, List.all_eq_false] at hall
        obtain ⟨l, hl, hne1⟩ := hall
        have := hlen1 l hl
        exact ⟨.order (nodes H l.val), .inl ⟨l, hl, by simp at hne1; omega, rfl⟩⟩
    · rintro ⟨c, hc⟩
      rcases hc with ⟨l, hl, h2, -⟩ | ⟨hne, -⟩ | ⟨x, hx, -⟩
      · have : v.layers.val.all (fun L => L.val.length = 1) = false :=
          List.all_eq_false.mpr ⟨l, hl, by simp; omega⟩
        simp [this]
      · have := hbne.mp hne; simp [this]
      · have : v.delete_inserts.val.isEmpty = false := by
          cases h : v.delete_inserts.val with
          | nil => rw [h] at hx; simp at hx
          | cons _ _ => rfl
        simp [this]
  · -- bytes_some
    intro bs h
    simp only [decode] at h
    split_ifs at h with hc
    cases h
    rw [hclean] at hc
    simp only [Bool.and_eq_true, Bool.not_eq_true'] at hc
    obtain ⟨⟨hall, -⟩, -⟩ := hc
    rw [List.all_eq_true] at hall
    rw [hF.bytes]
    apply bytes_layers d.atoms.val hbytes hat.symm
    intro l hl
    have h1 : l.val.length = 1 := by simpa using hall l hl
    obtain ⟨x, hx⟩ := List.length_eq_one_iff.mp h1
    obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hl
    obtain ⟨m, hxl, hm, hn⟩ := nodes_single hr hb hF hk hx
    exact ⟨x, hx, m, hxl, hm, hn⟩


/-! ## Delete/insert conflicts and atom bytes -/


theorem deps_len (hr : Rep d H) : d.deps.val.length = H.length := by
  have := congrArg List.length (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps)
  simpa using this

/-- Row `k` of the dependency table lists the indices of patch `k`'s dependencies. -/
theorem deps_row (hr : Rep d H) {k : Nat} (hk : k < d.deps.val.length) (w : Usize) :
    w ∈ (d.deps.val[k]).val ↔ ∃ q ∈ (H[k]'(deps_len hr ▸ hk)).deps, w.val = idx (ids H) q := by
  have e := List.getElem_of_eq (show d.deps.val.map (fun r => r.val.map (·.val)) = _ from hr.deps)
    (by simpa using hk)
  simp only [List.getElem_map] at e
  constructor
  · intro hw
    have : w.val ∈ (d.deps.val[k]).val.map (·.val) := List.mem_map.mpr ⟨w, hw, rfl⟩
    rw [e] at this
    obtain ⟨q, hq, he⟩ := List.mem_map.mp this
    exact ⟨q, hq, he.symm⟩
  · rintro ⟨q, hq, he⟩
    have : w.val ∈ (H[k]'(deps_len hr ▸ hk)).deps.map (idx (ids H)) := List.mem_map.mpr ⟨q, hq, he.symm⟩
    rw [← e] at this
    obtain ⟨w', hw', he'⟩ := List.mem_map.mp this
    rwa [UScalar.eq_of_val_eq he'] at hw'

theorem id_idx (hu : (ids H).Nodup) {k : Nat} (hk : k < H.length) : idx (ids H) (H[k]).id = k := by
  have := idx_getElem hu k (by simpa [ids] using hk)
  simpa [ids] using this

/-- The dependency rows reached from patch `a` are `a` and its strict causal ancestors. -/
theorem rowReach_anc (hr : Rep d H) (cl : Closed H) (hu : (ids H).Nodup) (a v : Usize)
    (ha : a.val < H.length) (hv : v.val < H.length) :
    Indexed.RowReach d.deps.val [a] v ↔ v = a ∨ Anc H (H[v.val]).id (H[a.val]).id := by
  have hdl := deps_len hr
  have hmax : H.length ≤ Usize.max := hdl ▸ d.deps.property
  constructor
  · intro h
    induction h with
    | seed hm => exact .inl (List.mem_singleton.mp hm)
    | @edge u w hreach hu' hw ih =>
      obtain ⟨q, hq, he⟩ := (deps_row hr hu' w).mp hw
      have hqm := cl _ (List.getElem_mem (hdl ▸ hu')) q hq
      have hwl : w.val < H.length := by rw [he]; simpa [ids] using idx_lt hqm
      have hqid : (H[w.val]'hwl).id = q := by
        have := getElem_idx hqm
        simp only [ids, List.getElem_map] at this
        simp only [he]; exact this
      have hstep : Anc H (H[w.val]'hwl).id (H[u.val]'(hdl ▸ hu')).id :=
        Relation.TransGen.single ⟨_, List.getElem_mem _, rfl, hqid ▸ hq⟩
      right
      rcases ih (hdl ▸ hu') with rfl | h
      · exact hstep
      · exact hstep.trans h
  · rintro (rfl | h)
    · exact .seed (List.mem_singleton_self _)
    · -- one step of `Anc` back to rows
      have step : ∀ x z, (∃ P ∈ H, P.id = z ∧ x ∈ P.deps) →
          (∀ iz : Usize, ∀ hz : iz.val < H.length, (H[iz.val]'hz).id = z → Indexed.RowReach d.deps.val [a] iz) →
          ∀ ix : Usize, ∀ hx : ix.val < H.length, (H[ix.val]'hx).id = x → Indexed.RowReach d.deps.val [a] ix := by
        rintro x z ⟨P, hP, hPz, hxP⟩ hz ix hx hix
        obtain ⟨k, hk, rfl⟩ := List.getElem_of_mem hP
        obtain ⟨iz, hiz⟩ := usize_exists k (by omega)
        have hr' := hz iz (by omega) (by simp only [hiz]; exact hPz)
        have hkd : iz.val < d.deps.val.length := by omega
        refine .edge hr' hkd ((deps_row hr hkd ix).mpr ⟨x, by simp only [hiz]; exact hxP, ?_⟩)
        rw [← hix, id_idx hu hx]
      have key : ∀ x y, Anc H x y → y = (H[a.val]).id → ∀ ix : Usize, ∀ hx : ix.val < H.length,
          (H[ix.val]'hx).id = x → Indexed.RowReach d.deps.val [a] ix := by
        intro x y hxy
        induction hxy using Relation.TransGen.head_induction_on with
        | single hR =>
          intro hy
          apply step _ _ hR
          intro iz hz hiz
          have : iz = a := by
            apply UScalar.eq_of_val_eq
            rw [← id_idx hu hz, ← id_idx hu ha, hiz, hy]
          rw [this]; exact .seed (List.mem_singleton_self _)
        | head hR _ ih =>
          intro hy
          exact step _ _ hR (fun iz hz hiz => ih hy iz hz hiz)
      exact key _ _ h rfl v hv rfl

theorem lookup_of_mem {α β} [DecidableEq α] (l : List (α × β)) (h : (l.map Prod.fst).Nodup) {a : α} {b : β}
    (hm : (a, b) ∈ l) : l.lookup a = some b := by
  induction l with
  | nil => simp at hm
  | cons x t ih =>
    obtain ⟨k, w⟩ := x
    rw [List.map_cons, List.nodup_cons] at h
    rcases List.mem_cons.mp hm with e | hm'
    · simp only [Prod.mk.injEq] at e; obtain ⟨rfl, rfl⟩ := e; simp [List.lookup]
    · have hak : a ≠ k := by
        rintro rfl; exact h.1 (List.mem_map.mpr ⟨(a, b), hm', rfl⟩)
      show List.lookup a ((k, w) :: t) = some b
      simp only [List.lookup]
      rw [show (a == k) = false from beq_false_of_ne hak]
      exact ih h.2 hm'

/-- The bytes of the atom at an index are the model's bytes of that atom. -/
theorem atom_bytes (hr : Rep d H) (hb : Built H) {a : Nat} (ha : a < d.atoms.val.length) {m : NodeId}
    (hm : DocMatModel.anc H a = .node m) :
    (atomB d.atoms.val a).map (fun c => UInt8.ofNat c.val) = (atom H m).getD [] := by
  have hat := DocBridge.atoms_len hr
  have hl : a < (anchorList H).length := hat ▸ ha
  rw [DocMatModel.anc_get hl] at hm
  have h2 : 2 ≤ a := by
    by_contra h
    rcases (by omega : a = 0 ∨ a = 1) with rfl | rfl <;> simp [anchorList] at hm
  obtain ⟨j, rfl⟩ : ∃ j, a = j + 2 := ⟨a - 2, by omega⟩
  have hj : j < (DocModel.atoms H).length := by simp [anchorList] at hl; omega
  have hmj : m = ((DocModel.atoms H)[j]).1 := by
    simp [anchorList, List.getElem_append_right] at hm
    exact hm.symm
  have e := List.getElem_of_eq (show d.atoms.val.map (fun a => a.val.map (·.val)) = _ from hr.atoms)
    (by simpa using ha)
  simp only [List.getElem_map, List.getElem_cons_succ] at e
  rw [atomB_eq _ ha]
  have hk := DocMatModel.flatMap_atomsOf_nodup _ (ops_keys_nodup (built_nodup hb))
  have hlk : atom H m = some ((DocModel.atoms H)[j]).2 := by
    unfold atom; rw [hmj]; exact lookup_of_mem _ hk (List.getElem_mem hj)
  rw [hlk, Option.getD_some]
  have : (d.atoms.val[j+2]).val.map (fun c => UInt8.ofNat c.val) =
      ((d.atoms.val[j+2]).val.map (·.val)).map UInt8.ofNat := by simp
  rw [this, e, List.map_map]
  conv_rhs => rw [← List.map_id ((DocModel.atoms H)[j]).2]
  apply List.map_congr_left
  intro x _
  simp

theorem deletion_patch (hr : Rep d H) (hb : Built H) {x : OpId} (hx : x ∈ deletionList H)
    (h : idx (deletionList H) x < d.deletions.val.length) :
    (d.deletions.val[idx (deletionList H) x]'h).patch.val = idx (ids H) x.patch := by
  have e := (show d.deletions.val.map (fun x => (x.patch.val, x.targets.val.map (·.val))) = _
    from hr.deletions)
  rw [deletionsOf_eq_map H hb] at e
  have e2 := congrArg (fun l => l[idx (deletionList H) x]?) e
  simp only [List.getElem?_map, List.getElem?_eq_getElem h,
    List.getElem?_eq_getElem (idx_lt hx), Option.map_some, Option.some.injEq, Prod.mk.injEq] at e2
  rw [e2.1, getElem_idx hx]

theorem insertion_patch (hr : Rep d H) (hb : Built H) {x : OpId} (hx : x ∈ insertionList H)
    (h : idx (insertionList H) x < d.insertions.val.length) :
    (d.insertions.val[idx (insertionList H) x]'h).patch.val = idx (ids H) x.patch := by
  have e := (show d.insertions.val.map (fun x => (x.patch.val, x.left.val, x.right.val)) = _
    from hr.insertions)
  rw [insertionsOf_eq_map H hb] at e
  have e2 := congrArg (fun l => l[idx (insertionList H) x]?) e
  simp only [List.getElem?_map, List.getElem?_eq_getElem h,
    List.getElem?_eq_getElem (idx_lt hx), Option.map_some, Option.some.injEq] at e2
  rw [getElem_idx hx] at e2
  revert e2
  cases insertionOf H x with
  | none => intro e2; simp only [Prod.mk.injEq] at e2; exact e2.1
  | some lr => obtain ⟨l, r⟩ := lr; intro e2; simp only [Prod.mk.injEq] at e2; exact e2.1

theorem mem_deleteInsertsM (t : Nat × Nat × List Nat) :
    t ∈ deleteInsertsM d ↔ ∃ dI iI, dI < d.deletions.val.length ∧ d.restored.val[dI]?.getD false = false ∧
      iI < d.insertions.val.length ∧ conflictAt d dI iI = some t := by
  unfold deleteInsertsM DocKernel.conflictsOf
  simp only [List.mem_flatMap, List.mem_range]
  constructor
  · rintro ⟨dI, hd, hm⟩
    split_ifs at hm with hr
    · simp at hm
    · obtain ⟨iI, hi, hc⟩ := List.mem_filterMap.mp hm
      exact ⟨dI, iI, hd, by simpa using hr, List.mem_range.mp hi, hc⟩
  · rintro ⟨dI, iI, hd, hr, hi, hc⟩
    refine ⟨dI, hd, ?_⟩
    rw [if_neg (by simp [hr])]
    exact List.mem_filterMap.mpr ⟨iI, List.mem_range.mpr hi, hc⟩

theorem idx_id {p : PId} (hp : p ∈ ids H) : (H[idx (ids H) p]'(by simpa [ids] using idx_lt hp)).id = p := by
  have := getElem_idx hp
  simpa [ids] using this

/-- The kernel's concurrency test on patch indices is the model's `Concurrent`. -/
theorem concurrent_iff (hr : Rep d H) (cl : Closed H) (hu : (ids H).Nodup) {p q : PId}
    (hp : p ∈ ids H) (hq : q ∈ ids H) (a b : Usize) (ha : a.val = idx (ids H) p) (hb' : b.val = idx (ids H) q) :
    concurrentM d.deps.val a b ↔ Concurrent H p q := by
  have hal : a.val < H.length := by rw [ha]; simpa [ids] using idx_lt hp
  have hbl : b.val < H.length := by rw [hb']; simpa [ids] using idx_lt hq
  have hpa : (H[a.val]'hal).id = p := by simp only [ha]; exact idx_id hp
  have hqb : (H[b.val]'hbl).id = q := by simp only [hb']; exact idx_id hq
  have hab : a = b ↔ p = q := by
    constructor
    · intro e; rw [← hpa, ← hqb]; simp [e]
    · intro e; apply UScalar.eq_of_val_eq; rw [ha, hb', e]
  unfold concurrentM Concurrent
  rw [rowReach_anc hr cl hu a b hal hbl, rowReach_anc hr cl hu b a hbl hal, hpa, hqb]
  constructor
  · rintro ⟨hne, h1, h2⟩
    refine ⟨fun e => hne (hab.mpr e), fun h => h2 (.inr h), fun h => h1 (.inr h)⟩
  · rintro ⟨hne, h1, h2⟩
    refine ⟨fun e => hne (hab.mp e), ?_, ?_⟩
    · rintro (e | h)
      · exact hne (hab.mp e.symm)
      · exact h2 h
    · rintro (e | h)
      · exact hne (hab.mp e)
      · exact h1 h

theorem insertionList_nodup' (hk : ((DocModel.ops H).map Prod.fst).Nodup) : (insertionList H).Nodup :=
  filterMap_keys_nodup _ (fun x b h => by obtain ⟨o, op⟩ := x; cases op <;> simp_all) _ hk

theorem idx_lt_none {α} [DecidableEq α] {l : List α} {x : α} {k : Nat} (hk : k < l.length)
    (hl : l.length ≤ Usize.max) (e : idx l x = k) : x ∈ l := by
  by_contra h
  unfold idx at e
  rw [if_neg h] at e
  unfold NONE at e
  omega

/-- The insertion's anchors among a deletion's targets, decoded. -/
theorem shared_nodes {ts : List NodeId} {l r : Anchor} (hts : ∀ n ∈ ts, Anchor.node n ∈ anchorList H)
    (hl : l ∈ anchorList H) (hr' : r ∈ anchorList H) (xs : List Usize)
    (hxs : xs.map (·.val) = sharedM (ts.map fun n => aIdx H (.node n)) (aIdx H l) (aIdx H r)) (n : NodeId) :
    n ∈ nodes H xs ↔ n ∈ ts ∧ (l = .node n ∨ r = .node n) := by
  have hin : ∀ y, y ∈ anchorList H → (aIdx H y ∈ ts.map (fun n => aIdx H (.node n)) ↔
      ∃ m ∈ ts, y = .node m) := by
    intro y hy
    constructor
    · intro h
      obtain ⟨m, hm, he⟩ := List.mem_map.mp h
      exact ⟨m, hm, (aIdx_inj (hts m hm) hy he).symm⟩
    · rintro ⟨m, hm, rfl⟩; exact List.mem_map.mpr ⟨m, hm, rfl⟩
  rw [DocMatModel.mem_nodes, hxs]
  unfold sharedM
  simp only [List.mem_append]
  constructor
  · rintro ⟨a, ha, hal, he⟩
    rcases ha with ha | ha
    · split_ifs at ha with hc
      · simp only [List.mem_singleton] at ha; subst ha
        rw [DocBridge.anc_aIdx hl] at he
        obtain ⟨m, hm, rfl⟩ := (hin l hl).mp hc
        cases he; exact ⟨hm, .inl rfl⟩
      · simp at ha
    · split_ifs at ha with hc
      · simp only [List.mem_singleton] at ha; subst ha
        rw [DocBridge.anc_aIdx hr'] at he
        obtain ⟨m, hm, rfl⟩ := (hin r hr').mp hc
        cases he; exact ⟨hm, .inr rfl⟩
      · simp at ha
  · rintro ⟨hn, rfl | rfl⟩
    · refine ⟨aIdx H (.node n), .inl ?_, idx_lt hl, DocBridge.anc_aIdx hl⟩
      rw [if_pos ((hin _ hl).mpr ⟨n, hn, rfl⟩)]; simp
    · refine ⟨aIdx H (.node n), .inr ?_, idx_lt hr', DocBridge.anc_aIdx hr'⟩
      rw [if_pos ((hin _ hr').mpr ⟨n, hn, rfl⟩)]; simp

open Classical in
/-- The kernel's delete/insert conflicts, decoded, are the model's `DI`. -/
theorem di_iff (hr : Rep d H) (hb : Built H) (cl : Closed H)
    (di : List document.DeleteInsert) (hdi : di.map diVal = deleteInsertsM d) (dd ii : OpId)
    (A : Finset NodeId) :
    (∃ x ∈ di, ∃ dd' ii', (deletionList H)[x.deletion.val]? = some dd' ∧
      (insertionList H)[x.insertion.val]? = some ii' ∧
      Conflict.deleteInsert dd ii A = Conflict.deleteInsert dd' ii' (nodes H x.anchors.val)) ↔
    DI H dd ii A := by
  have hu := built_nodup hb
  have hk := ops_keys_nodup hu
  have hdn := deletionList_nodup H hk
  have hin := insertionList_nodup' hk
  have hdl : d.deletions.val.length = (deletionList H).length := by
    have := congrArg List.length (show d.deletions.val.map
      (fun x => (x.patch.val, x.targets.val.map (·.val))) = _ from hr.deletions)
    simpa [deletionsOf_length] using this
  have hil : d.insertions.val.length = (insertionList H).length := by
    have := congrArg List.length (show d.insertions.val.map
      (fun x => (x.patch.val, x.left.val, x.right.val)) = _ from hr.insertions)
    rw [insertionsOf_eq_map H hb] at this; simpa using this
  have hres : d.restored.val = (deletionList H).map fun x => decide (Restored H x) := hr.restored
  have hack : ∀ a b, (a, b) ∈ d.acked.val.map pairVal ↔
      ∃ dd ii, Acked H dd ii ∧ a = idx (deletionList H) dd ∧ b = idx (insertionList H) ii := hr.acked
  -- the conflict at a pair of indices, in model terms
  have key : ∀ x : OpId, x ∈ deletionList H → ∀ y : OpId, y ∈ insertionList H →
      ∀ t, (idx (deletionList H) x < d.deletions.val.length ∧
        d.restored.val[idx (deletionList H) x]?.getD false = false ∧
        idx (insertionList H) y < d.insertions.val.length ∧
        conflictAt d (idx (deletionList H) x) (idx (insertionList H) y) = some t) ↔
      ∃ ts l r, tag H x = some ts ∧ ¬ Restored H x ∧ insertionOf H y = some (l, r) ∧
        sharedM (ts.map fun n => aIdx H (.node n)) (aIdx H l) (aIdx H r) ≠ [] ∧
        Concurrent H x.patch y.patch ∧ ¬ Acked H x y ∧
        t = (idx (deletionList H) x, idx (insertionList H) y,
          sharedM (ts.map fun n => aIdx H (.node n)) (aIdx H l) (aIdx H r)) := by
    intro x hx y hy t
    obtain ⟨ts, hts⟩ := Option.ne_none_iff_exists'.mp ((tag_ne_none_iff H x).mpr hx)
    obtain ⟨⟨l, r⟩, hlr⟩ := Option.ne_none_iff_exists'.mp ((insertionOf_ne_none_iff H y).mpr hy)
    obtain ⟨hdx, hte⟩ := deletion_entry d H hr hb x hx ts hts
    obtain ⟨hiy, hle, hre⟩ := insertion_entry d H hr hb y hy l r hlr
    have hdp := deletion_patch hr hb hx hdx
    have hip := insertion_patch hr hb hy hiy
    have hpx : x.patch ∈ ids H := deletionList_patch_mem H x hx
    have hpy : y.patch ∈ ids H := by
      unfold insertionList at hy
      obtain ⟨⟨o, op⟩, hm, hf⟩ := List.mem_filterMap.mp hy
      cases op <;> simp at hf
      subst hf
      unfold ops at hm
      obtain ⟨P, hP, hm⟩ := List.mem_flatMap.mp hm
      exact List.mem_map.mpr ⟨P, hP, ((mem_opsOf_iff.mp hm).1).symm⟩
    have hconc := concurrent_iff hr cl hu hpx hpy _ _ hdp hip
    have hrest : d.restored.val[idx (deletionList H) x]?.getD false = decide (Restored H x) := by
      rw [hres, List.getElem?_map, List.getElem?_eq_getElem (idx_lt hx)]
      simp [getElem_idx hx]
    have hacked : (idx (deletionList H) x, idx (insertionList H) y) ∈ d.acked.val.map pairVal ↔ Acked H x y := by
      rw [hack]
      constructor
      · rintro ⟨dd2, ii2, hA, h1, h2⟩
        have hm1 := idx_lt_none (by rw [← hdl]; exact hdx) (by rw [← hdl]; exact d.deletions.property) h1.symm
        have hm2 := idx_lt_none (by rw [← hil]; exact hiy) (by rw [← hil]; exact d.insertions.property) h2.symm
        have e1 : dd2 = x := by
          rw [← getElem_idx hm1, ← getElem_idx hx]; simp only [← h1]
        have e2 : ii2 = y := by
          rw [← getElem_idx hm2, ← getElem_idx hy]; simp only [← h2]
        rw [e1, e2] at hA; exact hA
      · intro hA; exact ⟨x, y, hA, rfl, rfl⟩
    rw [conflictAt_some d hdx hiy]
    simp only [hte, hle, hre]
    constructor
    · rintro ⟨-, hrs, -, hc⟩
      split_ifs at hc with hcond
      · obtain ⟨hne, hna, hcc⟩ := hcond
        refine ⟨ts, l, r, hts, ?_, hlr, hne, ?_, ?_, ?_⟩
        · intro hR; rw [hrest] at hrs; simp [hR] at hrs
        · exact hconc.mp hcc
        · intro hA; exact hna ((hacked).mpr hA)
        · exact (Option.some.inj hc).symm
    · rintro ⟨ts', l', r', hts', hnr, hlr', hne, hcc, hna, rfl⟩
      rw [hts] at hts'; cases hts'
      rw [hlr] at hlr'; cases hlr'
      refine ⟨hdx, by rw [hrest]; simp [hnr], hiy, ?_⟩
      rw [if_pos ⟨hne, fun h => hna ((hacked).mp h), hconc.mpr hcc⟩]
  -- a nonempty `sharedM` names a target the insertion is anchored on
  have shared_ne : ∀ (ts : List NodeId) (l r : Anchor), (∀ n ∈ ts, Anchor.node n ∈ anchorList H) →
      l ∈ anchorList H → r ∈ anchorList H →
      (sharedM (ts.map fun n => aIdx H (.node n)) (aIdx H l) (aIdx H r) ≠ [] ↔
        ∃ n, n ∈ ts ∧ (l = .node n ∨ r = .node n)) := by
    intro ts l r hts hl hr'
    have hin : ∀ y, y ∈ anchorList H → (aIdx H y ∈ ts.map (fun n => aIdx H (.node n)) ↔
        ∃ m ∈ ts, y = .node m) := by
      intro y hy
      constructor
      · intro h
        obtain ⟨m, hm, he⟩ := List.mem_map.mp h
        exact ⟨m, hm, (aIdx_inj (hts m hm) hy he).symm⟩
      · rintro ⟨m, hm, rfl⟩; exact List.mem_map.mpr ⟨m, hm, rfl⟩
    unfold sharedM
    constructor
    · intro h
      by_cases h1 : aIdx H l ∈ ts.map fun n => aIdx H (.node n)
      · obtain ⟨m, hm, rfl⟩ := (hin l hl).mp h1; exact ⟨m, hm, .inl rfl⟩
      · by_cases h2 : aIdx H r ∈ ts.map fun n => aIdx H (.node n)
        · obtain ⟨m, hm, rfl⟩ := (hin r hr').mp h2; exact ⟨m, hm, .inr rfl⟩
        · simp [h1, h2] at h
    · rintro ⟨n, hn, rfl | rfl⟩
      · rw [if_pos ((hin _ hl).mpr ⟨n, hn, rfl⟩)]; simp
      · rw [if_pos ((hin _ hr').mpr ⟨n, hn, rfl⟩)]; simp
  constructor
  · rintro ⟨x, hx, dd', ii', h1, h2, he⟩
    cases he
    have hmem : diVal x ∈ deleteInsertsM d := by rw [← hdi]; exact List.mem_map.mpr ⟨x, hx, rfl⟩
    obtain ⟨dI, iI, hd, hrs, hi, hc⟩ := (mem_deleteInsertsM _).mp hmem
    obtain ⟨hdlt, hdd⟩ := List.getElem?_eq_some_iff.mp h1
    obtain ⟨hilt, hii⟩ := List.getElem?_eq_some_iff.mp h2
    have hxd : dd ∈ deletionList H := hdd ▸ List.getElem_mem hdlt
    have hyi : ii ∈ insertionList H := hii ▸ List.getElem_mem hilt
    have hidd : idx (deletionList H) dd = x.deletion.val := by rw [← hdd]; exact idx_getElem hdn _ hdlt
    have hiii : idx (insertionList H) ii = x.insertion.val := by rw [← hii]; exact idx_getElem hin _ hilt
    -- the conflict found at (dI, iI) is x's
    have hcx := hc
    rw [conflictAt_some d hd hi] at hcx
    split_ifs at hcx with hcond
    · have hv := (Option.some.inj hcx).symm
      simp only [diVal, Prod.mk.injEq] at hv
      obtain ⟨e1, e2, e3⟩ := hv
      rw [← e1, ← hidd] at hd hrs hc
      rw [← e2, ← hiii] at hi hc
      obtain ⟨ts, l, r, hts, hnr, hlr, hne, hcc, hna, ht⟩ := (key dd hxd ii hyi (diVal x)).mp ⟨hd, hrs, hi, hc⟩
      simp only [diVal, Prod.mk.injEq] at ht
      have htm := deletion_targets_mem H hb dd ts hts
      obtain ⟨hlm, hrm⟩ := insertion_anchors_mem H hb ii l r hlr
      have hsn := shared_nodes htm hlm hrm x.anchors.val ht.2.2
      obtain ⟨n, hn⟩ := (shared_ne ts l r htm hlm hrm).mp hne
      refine ⟨ts, l, r, hts, hnr, hlr, hsn, ⟨n, (hsn n).mpr hn⟩, hcc, hna⟩
  · rintro ⟨ts, l, r, hts, hnr, hlr, hA, ⟨n, hnA⟩, hcc, hna⟩
    have hxd : dd ∈ deletionList H := (tag_ne_none_iff H dd).mp (by rw [hts]; simp)
    have hyi : ii ∈ insertionList H := (insertionOf_ne_none_iff H ii).mp (by rw [hlr]; simp)
    have htm := deletion_targets_mem H hb dd ts hts
    obtain ⟨hlm, hrm⟩ := insertion_anchors_mem H hb ii l r hlr
    have hne := (shared_ne ts l r htm hlm hrm).mpr ⟨n, (hA n).mp hnA⟩
    obtain ⟨hd, hrs, hi, hc⟩ := (key dd hxd ii hyi _).mpr ⟨ts, l, r, hts, hnr, hlr, hne, hcc, hna, rfl⟩
    have hmem := (mem_deleteInsertsM _).mpr ⟨_, _, hd, hrs, hi, hc⟩
    rw [← hdi] at hmem
    obtain ⟨x, hx, hxv⟩ := List.mem_map.mp hmem
    simp only [diVal, Prod.mk.injEq] at hxv
    obtain ⟨e1, e2, e3⟩ := hxv
    refine ⟨x, hx, dd, ii, ?_, ?_, ?_⟩
    · rw [e1, List.getElem?_eq_getElem (idx_lt hxd), getElem_idx hxd]
    · rw [e2, List.getElem?_eq_getElem (idx_lt hyi), getElem_idx hyi]
    · congr 1
      ext m
      rw [hA m, shared_nodes htm hlm hrm x.anchors.val e3 m]

end o5s_kernel.DocDecode
