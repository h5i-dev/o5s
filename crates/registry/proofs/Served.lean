import Narrow
/-!
# PostgreSQL serves narrow loads

`Narrow.narrow_load` assumes the rows a command loads agree with the whole
tenant on every lookup the command makes. This file models what the
PostgreSQL loader (`load` in `crates/pgwire/src/service.rs`) actually returns
and states that it satisfies `Narrow`, which removes that assumption except
for PostgreSQL's own query semantics:

- `SELECT … WHERE tenant AND col = v ORDER BY <primary key>` returns the
  tenant's rows satisfying the condition, sorted by primary key (`query`).
- `rows_where` appends each returned row that is not already loaded
  (`appendNew`); a command with more than `POINT_LOADS` keys loads the whole
  table instead (`query` with a true condition).

`Narrow` compares lists, so order matters: a filtered table must be in
primary-key order in the committed state too. `pg_load_narrow` proves
`Narrow` for every command under `KeyOrdered` (every table in key order).
Reachable states are not `KeyOrdered` (approvals are stored in the order
reviewers approve, so the former `key_ordered` was false), but they keep what
the lookups actually rely on (`Ordered`): members in position order per
channel, unique labels, repositories and check reports, given that each
patch's dependency rows are stored ascending (`DepsAscending`; the native
`prepare` sorts them, but the kernel does not require it). Approvals are only
tested for existence, so `pg_served` holds in every reachable state with
ascending dependency lists.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Narrow o5s_registry.Writes
  H5iAppLib

namespace o5s_registry.Served

-- Row equality, which `rows_where`'s duplicate check needs.
deriving instance DecidableEq for PatchRow, Dependency, Approval, CheckReport, Channel, Member, PathKind,
  Listing

/-- More keys than this load the whole table (`POINT_LOADS`). -/
def pointLoads : Nat := 32

/-- PostgreSQL's answer to a filtered, primary-key-ordered `SELECT`. -/
def query {α κ} [LinearOrder κ] (key : α → κ) (table : List α) (P : α → Bool) : List α :=
  (table.mergeSort fun a b => decide (key a ≤ key b)).filter P

/-- `rows_where`: append the returned rows not already loaded. -/
def appendNew {α} [DecidableEq α] (acc res : List α) : List α :=
  res.foldl (fun out r => if r ∈ out then out else out ++ [r]) acc

/-- Primary keys, as the schema declares them; composite keys compare
lexicographically, as `ORDER BY a, b` does. (Fixed: they were plain `ℕ × ℕ`,
which has no linear order, so `query` did not elaborate.) -/
def patchKey (p : PatchRow) : Nat := p.id.val
def depKey (d : Dependency) : Lex (Nat × Nat) := toLex (d.patch.val, d.dependency.val)
def approvalKey (a : Approval) : Lex (Nat × Nat) := toLex (a.patch.val, a.reviewer.val)
def checkKey (c : CheckReport) : Nat := c.patch.val
def channelKey (c : Channel) : Nat := c.id.val
def memberKey (m : Member) : Lex (Nat × Nat) := toLex (m.channel.val, m.position.val)
def pathKey (k : PathKind) : Nat := k.id.val
def listingKey (l : Listing) : Nat := l.repo.val

/-- One point load per identifier, or the whole table past `POINT_LOADS`.
(Fixed: this was a local polymorphic `let`, which does not parse.) -/
def loadMany {α κ} [DecidableEq α] [LinearOrder κ] (ids : List U64) (key : α → κ) (t : List α)
    (P : α → U64 → Bool) : List α :=
  if ids.length > pointLoads then query key t fun _ => true
  else ids.foldl (fun acc id => appendNew acc (query key t fun r => P r id)) []

/-- The rows `load` returns for command `c` from the committed tenant `full`,
mirroring `crates/pgwire/src/service.rs` branch by branch. Tables a command
does not load are empty. -/
noncomputable def pgLoad (c : Command) (full : St) : St :=
  let none : St := ⟨full.repo, [], [], [], [], [], [], [], [], [], [], []⟩
  -- Every repository command also loads the whole role table.
  let base : St := { none with roles := full.roles }
  let channelsNamed (n : alloc.vec.Vec U8) (acc : List Channel) : List Channel :=
    appendNew acc (query channelKey full.channels fun c => decide (c.label.val = n.val))
  let membersOf (chs : List Channel) (n : alloc.vec.Vec U8) : List Member :=
    match chs.find? (fun c => decide (c.label.val = n.val)) with
    | some c => query memberKey full.members fun m => decide (m.channel = c.id)
    | Option.none => []
  match c with
  | .Register n repo =>
    { none with listings := (appendNew
        (query listingKey full.listings fun l => decide (l.label.val = n.val))
        (query listingKey full.listings fun l => decide (l.repo = repo))) }
  | .Propose _ h deps uses _ =>
    { base with
      patches :=
        if deps.val.length > pointLoads then query patchKey full.patches fun _ => true
        else deps.val.foldl (fun acc d => appendNew acc (query patchKey full.patches fun p => decide (p.id = d)))
          (query patchKey full.patches fun p => decide (p.hash.val = h.val)),
      paths :=
        if uses.val.length > pointLoads then query pathKey full.paths fun _ => true
        else uses.val.foldl (fun acc u => appendNew acc
          (query pathKey full.paths fun k => decide (k.path.val = u.path.val))) [] }
  | .Approve _ p | .Check _ p _ =>
    { base with patches := query patchKey full.patches fun q => decide (q.id = p) }
  | .Fork _ src n =>
    let chs := channelsNamed src (channelsNamed n [])
    { base with channels := chs, members := membersOf chs src }
  | .Merge _ n ids =>
    let chs := channelsNamed n []
    { base with
      channels := chs
      members := membersOf chs n
      patches := loadMany ids.val patchKey full.patches (fun q id => decide (q.id = id))
      deps := loadMany ids.val depKey full.deps (fun d id => decide (d.patch = id))
      approvals := loadMany ids.val approvalKey full.approvals (fun a id => decide (a.patch = id))
      checks := loadMany ids.val checkKey full.checks (fun x id => decide (x.patch = id)) }
  | .Grant .. => base
  | _ => full


/-! ## Lists -/

section Lists
variable {α : Type} [DecidableEq α]

theorem appendNew_def (acc res : List α) : appendNew acc res = res.foldl insertStep acc := rfl

theorem mem_appendNew {acc res : List α} {x : α} : x ∈ appendNew acc res ↔ x ∈ acc ∨ x ∈ res := by
  rw [appendNew_def]; exact mem_foldl_insertStep _ _ _

theorem nodup_appendNew {acc : List α} (res : List α) (h : acc.Nodup) : (appendNew acc res).Nodup := by
  rw [appendNew_def]; exact nodup_foldl_insertStep _ _ h

/-- Appending distinct rows: those not loaded yet, in order. -/
theorem appendNew_eq (acc res : List α) (hn : res.Nodup) :
    appendNew acc res = acc ++ res.filter (fun r => decide (r ∉ acc)) := by
  induction res generalizing acc with
  | nil => simp [appendNew]
  | cons r rs ih =>
    rw [List.nodup_cons] at hn
    show appendNew (if r ∈ acc then acc else acc ++ [r]) rs = _
    by_cases hr : r ∈ acc
    · rw [if_pos hr, ih acc hn.2]; simp [hr]
    · rw [if_neg hr, ih _ hn.2, List.filter_cons, if_pos (by simpa using hr)]
      have : rs.filter (fun x => decide (x ∉ acc ++ [r])) = rs.filter (fun x => decide (x ∉ acc)) := by
        apply List.filter_congr
        intro x hx
        have : x ≠ r := fun e => hn.1 (e ▸ hx)
        simp [this]
      rw [this]; simp

/-- Appending the block of rows of `t` satisfying `Q`, when none or all of
them are loaded already, loads exactly that block and keeps every block
loaded before. -/
theorem block_step (t out : List α) (Q : α → Bool)
    (hsub : ∀ x ∈ out, x ∈ t) (hnd : out.Nodup) (htn : (t.filter Q).Nodup)
    (hcase : out.filter Q = [] ∨ out.filter Q = t.filter Q) :
    (∀ x ∈ appendNew out (t.filter Q), x ∈ t) ∧ (appendNew out (t.filter Q)).Nodup ∧
    (appendNew out (t.filter Q)).filter Q = t.filter Q ∧
    ∀ R : α → Bool, out.filter R = t.filter R → (appendNew out (t.filter Q)).filter R = t.filter R := by
  refine ⟨?_, nodup_appendNew _ hnd, ?_, ?_⟩
  · intro x hx
    rcases mem_appendNew.1 hx with hx | hx
    · exact hsub x hx
    · exact (List.mem_filter.1 hx).1
  · rw [appendNew_eq _ _ htn, List.filter_append]
    rcases hcase with h0 | h0
    · have e1 : (t.filter Q).filter (fun r => decide (r ∉ out)) = t.filter Q := by
        apply List.filter_eq_self.2
        intro x hx
        have hq := (List.mem_filter.1 hx).2
        simp only [decide_eq_true_eq]
        intro hxo
        have : x ∈ out.filter Q := List.mem_filter.2 ⟨hxo, hq⟩
        rw [h0] at this; simp at this
      rw [e1, h0, List.nil_append]
      exact List.filter_eq_self.2 (fun x hx => (List.mem_filter.1 hx).2)
    · have e1 : (t.filter Q).filter (fun r => decide (r ∉ out)) = [] := by
        apply List.filter_eq_nil_iff.2
        intro x hx
        rw [← h0] at hx
        simpa using (List.mem_filter.1 hx).1
      rw [e1, h0]; simp
  · intro R hR
    rw [appendNew_eq _ _ htn, List.filter_append, hR]
    have : ((t.filter Q).filter (fun r => decide (r ∉ out))).filter R = [] := by
      apply List.filter_eq_nil_iff.2
      intro x hx hr
      simp only [List.mem_filter, decide_eq_true_eq] at hx
      have : x ∈ t.filter R := List.mem_filter.2 ⟨hx.1.1, hr⟩
      rw [← hR] at this
      exact hx.2 (List.mem_filter.1 this).1
    rw [this, List.append_nil]

theorem sub_single {l u : List α} (hs : ∀ x ∈ l, x ∈ u) (hn : l.Nodup) (h1 : u.length ≤ 1) :
    l = [] ∨ l = u := by
  match u, h1 with
  | [], _ => left; exact List.eq_nil_iff_forall_not_mem.2 (fun x hx => by simpa using hs x hx)
  | [r], _ =>
    match l, hn with
    | [], _ => left; rfl
    | a :: l', hn =>
      right
      have ha : a = r := by simpa using hs a (by simp)
      have hl' : l' = [] := by
        apply List.eq_nil_iff_forall_not_mem.2
        intro b hb
        have hb' : b = r := by simpa using hs b (by simp [hb])
        rw [List.nodup_cons] at hn
        exact hn.1 (by rw [ha, ← hb']; exact hb)
      rw [ha, hl']

theorem single_case (t out : List α) (Q : α → Bool) (hsub : ∀ x ∈ out, x ∈ t) (hnd : out.Nodup)
    (h1 : (t.filter Q).length ≤ 1) : out.filter Q = [] ∨ out.filter Q = t.filter Q :=
  sub_single (fun x hx => by
    obtain ⟨h1, h2⟩ := List.mem_filter.1 hx; exact List.mem_filter.2 ⟨hsub x h1, h2⟩)
    (hnd.filter _) h1

theorem nodup_le_one {l : List α} (h : l.length ≤ 1) : l.Nodup := by
  match l, h with
  | [], _ => simp
  | [a], _ => simp

/-- Loading one block per lookup, each block at most one row. -/
theorem fold_single {β} (t : List α) (Q : β → α → Bool)
    (h1 : ∀ i, (t.filter (Q i)).length ≤ 1) (ids : List β) :
    ∀ out : List α, (∀ x ∈ out, x ∈ t) → out.Nodup →
      (∀ R : α → Bool, out.filter R = t.filter R →
        (ids.foldl (fun acc j => appendNew acc (t.filter (Q j))) out).filter R = t.filter R) ∧
      ∀ i ∈ ids, (ids.foldl (fun acc j => appendNew acc (t.filter (Q j))) out).filter (Q i) =
        t.filter (Q i) := by
  induction ids with
  | nil => intro out _ _; exact ⟨fun R h => h, by simp⟩
  | cons j js ih =>
    intro out hsub hnd
    obtain ⟨s1, s2, s3, s4⟩ := block_step t out (Q j) hsub hnd (nodup_le_one (h1 j))
      (single_case t out (Q j) hsub hnd (h1 j))
    obtain ⟨ihR, ihI⟩ := ih _ s1 s2
    refine ⟨fun R hR => ihR R (s4 R hR), ?_⟩
    intro i hi
    rcases List.mem_cons.1 hi with rfl | hi
    · exact ihR _ s3
    · exact ihI i hi

/-- Loading one block per identifier, blocks of distinct identifiers disjoint. -/
theorem fold_blocks (t : List α) (f : α → U64)
    (htn : ∀ i, (t.filter (fun x => decide (f x = i))).Nodup) (ids : List U64) :
    ∀ (out : List α) (done : List U64), (∀ x ∈ out, x ∈ t) → out.Nodup → (∀ x ∈ out, f x ∈ done) →
      (∀ i ∈ done, out.filter (fun x => decide (f x = i)) = t.filter (fun x => decide (f x = i))) →
      ∀ i ∈ done ++ ids,
        (ids.foldl (fun acc j => appendNew acc (t.filter (fun x => decide (f x = j)))) out).filter
          (fun x => decide (f x = i)) = t.filter (fun x => decide (f x = i)) := by
  induction ids with
  | nil => intro out done _ _ _ hd i hi; simpa using hd i (by simpa using hi)
  | cons j js ih =>
    intro out done hsub hnd hf hd
    have hcase : out.filter (fun x => decide (f x = j)) = [] ∨
        out.filter (fun x => decide (f x = j)) = t.filter (fun x => decide (f x = j)) := by
      by_cases hj : j ∈ done
      · exact .inr (hd j hj)
      · left
        apply List.filter_eq_nil_iff.2
        intro x hx he
        have : f x = j := by simpa using he
        exact hj (this ▸ hf x hx)
    obtain ⟨s1, s2, s3, s4⟩ := block_step t out _ hsub hnd (htn j) hcase
    have key := ih _ (done ++ [j]) s1 s2 ?_ ?_
    · intro i hi
      exact key i (by simp only [List.mem_append, List.mem_cons, List.mem_singleton] at hi ⊢; tauto)
    · intro x hx
      rcases mem_appendNew.1 hx with hx | hx
      · exact List.mem_append_left _ (hf x hx)
      · have := (List.mem_filter.1 hx).2; simp only [decide_eq_true_eq] at this; simp [this]
    · intro i hi
      rcases List.mem_append.1 hi with hi | hi
      · exact s4 _ (hd i hi)
      · simp only [List.mem_singleton] at hi; subst hi; exact s3

end Lists

section Keys
variable {α κ : Type} [LinearOrder κ]

/-- `ORDER BY` a key keeps, in each filtered block already in key order,
the stored order. -/
theorem filter_mergeSort (key : α → κ) (t : List α) (P : α → Bool)
    (h : (t.filter P).Pairwise fun a b => key a ≤ key b) :
    (t.mergeSort fun a b => decide (key a ≤ key b)).filter P = t.filter P := by
  have hle : (t.filter P).Pairwise fun a b => decide (key a ≤ key b) = true := by simpa using h
  have hsub : List.Sublist (t.filter P) (t.mergeSort (fun a b => decide (key a ≤ key b))) :=
    List.sublist_mergeSort (fun a b c h1 h2 => by simp only [decide_eq_true_eq] at *; exact le_trans h1 h2)
      (fun a b => by simpa using le_total (key a) (key b)) hle List.filter_sublist
  have h2 : List.Sublist (t.filter P) ((t.mergeSort fun a b => decide (key a ≤ key b)).filter P) := by
    have := hsub.filter P
    rwa [List.filter_filter, show (fun a => P a && P a) = P from funext fun a => Bool.and_self _] at this
  have hlen := ((List.mergeSort_perm t (fun a b => decide (key a ≤ key b))).filter P).length_eq
  exact (h2.eq_of_length hlen.symm).symm

theorem query_eq (key : α → κ) (t : List α) (P : α → Bool)
    (h : (t.filter P).Pairwise fun a b => key a ≤ key b) : query key t P = t.filter P :=
  filter_mergeSort key t P h

theorem query_true_filter (key : α → κ) (t : List α) (R : α → Bool)
    (h : (t.filter R).Pairwise fun a b => key a ≤ key b) :
    (query key t fun _ => true).filter R = t.filter R := by
  unfold query; rw [List.filter_true]; exact filter_mergeSort key t R h

theorem agree_query (key : α → κ) (t : List α) (P : α → Bool)
    (h : (t.filter P).Pairwise fun a b => key a ≤ key b) : Agree P (query key t P) t := by
  unfold Agree; rw [query_eq key t P h, List.filter_filter]; simp

theorem pw_filter {key : α → κ} {t : List α} (h : t.Pairwise fun a b => key a < key b)
    (P : α → Bool) : (t.filter P).Pairwise fun a b => key a ≤ key b :=
  (h.sublist List.filter_sublist).imp le_of_lt

theorem pw_one {key : α → κ} {l : List α} (h : l.length ≤ 1) : l.Pairwise fun a b => key a ≤ key b := by
  match l, h with
  | [], _ => simp
  | [a], _ => simp

theorem nodup_of_lt {key : α → κ} {l : List α} (h : l.Pairwise fun a b => key a < key b) : l.Nodup :=
  h.imp (fun hab e => by subst e; exact lt_irrefl _ hab)

end Keys

theorem le_one_of_nodup_map {α β} [DecidableEq β] (g : α → β) {t : List α} (h : (t.map g).Nodup)
    (v : β) : (t.filter fun x => decide (g x = v)).length ≤ 1 := by
  have hn : ((t.filter fun x => decide (g x = v)).map g).Nodup := (List.filter_sublist.map g).nodup h
  have hall : ∀ y ∈ (t.filter fun x => decide (g x = v)).map g, y = v := by
    intro y hy; obtain ⟨x, hx, rfl⟩ := List.mem_map.1 hy; simpa using (List.mem_filter.1 hx).2
  generalize (t.filter fun x => decide (g x = v)) = l at hn hall
  match l, hn, hall with
  | [], _, _ => simp
  | [_], _, _ => simp
  | a :: b :: l, hn, hall =>
    exfalso
    simp only [List.map_cons, List.nodup_cons, List.mem_cons] at hn
    exact hn.1 (.inl ((hall _ (by simp)).trans (hall _ (by simp)).symm))

theorem nodup_map_of_lt {α β} {key : α → Nat} {g : α → β} (hg : ∀ a b, g a = g b → key a = key b)
    {t : List α} (h : t.Pairwise fun a b => key a < key b) : (t.map g).Nodup :=
  List.pairwise_map.2 (h.imp fun hab e => absurd (hg _ _ e) (ne_of_lt hab))


/-- Each table, filtered by any condition `load` uses, is already in
primary-key order in the committed state. -/
def KeyOrdered (s : St) : Prop :=
  (s.patches.Pairwise fun a b => patchKey a < patchKey b) ∧
  (∀ p, (s.deps.filter fun d => decide (d.patch = p)).Pairwise fun a b => depKey a < depKey b) ∧
  (∀ p, (s.approvals.filter fun a => decide (a.patch = p)).Pairwise fun a b => approvalKey a < approvalKey b) ∧
  (s.checks.Pairwise fun a b => checkKey a < checkKey b) ∧
  (s.channels.Pairwise fun a b => channelKey a < channelKey b) ∧
  (∀ c, (s.members.filter fun m => decide (m.channel = c)).Pairwise fun a b => memberKey a < memberKey b) ∧
  (s.paths.Pairwise fun a b => pathKey a < pathKey b) ∧
  (s.listings.Pairwise fun a b => listingKey a < listingKey b)

/-- Every stored dependency list is strictly ascending. -/
def DepsAscending (s : St) : Prop :=
  ∀ p, (s.deps.filter fun d => decide (d.patch = p)).Pairwise fun a b => a.dependency.val < b.dependency.val

/-! ## What each lookup needs -/

/-- The orderings and uniqueness the loads rely on: the members of a channel
and the dependencies of a patch in key order, at most one check report per
patch, directory labels in key order, at most one listing per repository. -/
structure Ordered (s : St) : Prop where
  members : ∀ ch, (s.members.filter fun m => decide (m.channel = ch)).Pairwise
    fun a b => memberKey a < memberKey b
  deps : ∀ p, (s.deps.filter fun d => decide (d.patch = p)).Pairwise fun a b => depKey a < depKey b
  checks : ∀ p, (s.checks.filter fun x => decide (x.patch = p)).length ≤ 1
  labels : ∀ n : List U8, (s.listings.filter fun l => decide (l.label.val = n)).Pairwise
    fun a b => listingKey a < listingKey b
  repos : ∀ r, (s.listings.filter fun l => decide (l.repo = r)).length ≤ 1

/-- Each patch's approvals in key (reviewer) order. -/
def ApprovalsSorted (s : St) : Prop :=
  ∀ p, (s.approvals.filter fun a => decide (a.patch = p)).Pairwise fun a b => approvalKey a < approvalKey b

theorem pw_one' {α} {R : α → α → Prop} {l : List α} (h : l.length ≤ 1) : l.Pairwise R := by
  match l, h with
  | [], _ => simp
  | [a], _ => simp

theorem find_label {full : St} (inv : Invariant.Inv full) {c : Channel} (hc : c ∈ full.channels)
    {n : List U8} (hl : c.label.val = n) :
    full.channels.find? (fun c => decide (c.label.val = n)) = some c := by
  cases hfind : full.channels.find? (fun c : Channel => decide (c.label.val = n)) with
  | none => exact absurd (List.find?_eq_none.1 hfind c hc) (by simp [hl])
  | some c' =>
    have hc' := List.mem_of_find?_eq_some hfind
    have hl' : c'.label.val = n := by simpa using List.find?_some hfind
    have : c' = c := List.inj_on_of_nodup_map inv.channel_labels hc' hc (hl'.trans hl.symm)
    rw [this]

theorem chan_one {full : St} (inv : Invariant.Inv full) (m : List U8) :
    (full.channels.filter fun c => decide (c.label.val = m)).length ≤ 1 :=
  le_one_of_nodup_map (fun c : Channel => c.label.val) inv.channel_labels m

theorem chan_query {full : St} (inv : Invariant.Inv full) (m : List U8) :
    query channelKey full.channels (fun c => decide (c.label.val = m)) =
      full.channels.filter (fun c => decide (c.label.val = m)) :=
  query_eq _ _ _ (pw_one (chan_one inv m))

theorem patch_one {full : St} (inv : Invariant.Inv full) (d : U64) :
    (full.patches.filter fun p => decide (p.id = d)).length ≤ 1 :=
  le_one_of_nodup_map (fun p : PatchRow => p.id) inv.patch_keys d

theorem register_parts (n : alloc.vec.Vec U8) (repo : U64) (full : St) (S : Ordered full) :
    Agree (fun l : Listing => decide (l.label.val = n.val)) (pgLoad (.Register n repo) full).listings
      full.listings ∧
    Agree (fun l : Listing => decide (l.repo = repo)) (pgLoad (.Register n repo) full).listings
      full.listings := by
  unfold pgLoad; dsimp only
  have hql : query listingKey full.listings (fun l => decide (l.label.val = n.val)) =
      full.listings.filter (fun l => decide (l.label.val = n.val)) :=
    query_eq _ _ _ ((S.labels n.val).imp le_of_lt)
  have hqr : query listingKey full.listings (fun l => decide (l.repo = repo)) =
      full.listings.filter (fun l => decide (l.repo = repo)) :=
    query_eq _ _ _ (pw_one (S.repos repo))
  rw [hql, hqr]
  have hsub : ∀ x ∈ full.listings.filter (fun l => decide (l.label.val = n.val)), x ∈ full.listings :=
    fun x hx => (List.mem_filter.1 hx).1
  obtain ⟨-, -, s3, s4⟩ := block_step full.listings _ (fun l => decide (l.repo = repo)) hsub
    (nodup_of_lt (S.labels n.val)) (nodup_le_one (S.repos repo))
    (single_case _ _ _ hsub (nodup_of_lt (S.labels n.val)) (S.repos repo))
  exact ⟨s4 _ (by rw [List.filter_filter]; simp), s3⟩

theorem propose_parts (r h : alloc.vec.Vec U8) (deps : alloc.vec.Vec U64) (uses : alloc.vec.Vec PathUse)
    (b : alloc.vec.Vec U8) (full : St) (inv : Invariant.Inv full) :
    Agree (fun p : PatchRow => decide (p.hash.val = h.val))
      (pgLoad (.Propose r h deps uses b) full).patches full.patches ∧
    (∀ d ∈ deps.val, Agree (fun p : PatchRow => decide (p.id = d))
      (pgLoad (.Propose r h deps uses b) full).patches full.patches) ∧
    ∀ u ∈ uses.val, Agree (fun k : PathKind => decide (k.path.val = u.path.val))
      (pgLoad (.Propose r h deps uses b) full).paths full.paths := by
  unfold pgLoad; dsimp only
  have h1h : (full.patches.filter fun p => decide (p.hash.val = h.val)).length ≤ 1 :=
    le_one_of_nodup_map (fun p : PatchRow => p.hash.val) inv.hashes h.val
  have h1k : ∀ u : PathUse, (full.paths.filter fun k => decide (k.path.val = u.path.val)).length ≤ 1 :=
    fun u => le_one_of_nodup_map (fun k : PathKind => k.path.val) inv.path_labels u.path.val
  have qp : ∀ d, query patchKey full.patches (fun p => decide (p.id = d)) =
      full.patches.filter (fun p => decide (p.id = d)) := fun d => query_eq _ _ _ (pw_one (patch_one inv d))
  have qh : query patchKey full.patches (fun p => decide (p.hash.val = h.val)) =
      full.patches.filter (fun p => decide (p.hash.val = h.val)) := query_eq _ _ _ (pw_one h1h)
  have qk : ∀ u : PathUse, query pathKey full.paths (fun k => decide (k.path.val = u.path.val)) =
      full.paths.filter (fun k => decide (k.path.val = u.path.val)) := fun u => query_eq _ _ _ (pw_one (h1k u))
  simp only [qp, qh, qk]
  have hf := fold_single full.patches (fun (d : U64) (p : PatchRow) => decide (p.id = d)) (patch_one inv)
    deps.val (full.patches.filter fun p => decide (p.hash.val = h.val)) (fun x hx => (List.mem_filter.1 hx).1)
    (nodup_le_one h1h)
  have hk := fold_single full.paths (fun (u : PathUse) (k : PathKind) => decide (k.path.val = u.path.val))
    h1k uses.val [] (by simp) List.nodup_nil
  refine ⟨?_, ?_, ?_⟩
  · split
    · exact query_true_filter _ _ _ (pw_one h1h)
    · exact hf.1 _ (by rw [List.filter_filter]; simp)
  · intro d hd; split
    · exact query_true_filter _ _ _ (pw_one (patch_one inv d))
    · exact hf.2 d hd
  · intro u hu; split
    · exact query_true_filter _ _ _ (pw_one (h1k u))
    · exact hk.2 u hu

theorem fork_parts (r src n : alloc.vec.Vec U8) (full : St) (inv : Invariant.Inv full) (S : Ordered full) :
    Agree (fun c : Channel => decide (c.label.val = n.val)) (pgLoad (.Fork r src n) full).channels
      full.channels ∧
    Agree (fun c : Channel => decide (c.label.val = src.val)) (pgLoad (.Fork r src n) full).channels
      full.channels ∧
    ∀ c ∈ full.channels, c.label.val = src.val →
      Agree (fun m : Member => decide (m.channel = c.id)) (pgLoad (.Fork r src n) full).members
        full.members := by
  unfold pgLoad; dsimp only
  simp only [chan_query inv]
  have hf := fold_single full.channels (fun (m : alloc.vec.Vec U8) (c : Channel) => decide (c.label.val = m.val))
    (fun m => chan_one inv m.val) [n, src] [] (by simp) List.nodup_nil
  have h1 : (appendNew (appendNew [] (full.channels.filter fun c => decide (c.label.val = n.val)))
      (full.channels.filter fun c => decide (c.label.val = src.val))).filter
        (fun c => decide (c.label.val = n.val)) = full.channels.filter (fun c => decide (c.label.val = n.val)) :=
    hf.2 n (by simp)
  have h2 : (appendNew (appendNew [] (full.channels.filter fun c => decide (c.label.val = n.val)))
      (full.channels.filter fun c => decide (c.label.val = src.val))).filter
        (fun c => decide (c.label.val = src.val)) = full.channels.filter (fun c => decide (c.label.val = src.val)) :=
    hf.2 src (by simp)
  refine ⟨h1, h2, ?_⟩
  intro c hc hl
  have hfind : List.find? (fun c : Channel => decide (c.label.val = src.val))
      (appendNew (appendNew [] (full.channels.filter fun c => decide (c.label.val = n.val)))
        (full.channels.filter fun c => decide (c.label.val = src.val))) = some c := by
    rw [← List.head?_filter, h2, List.head?_filter]; exact find_label inv hc hl
  rw [hfind]
  exact agree_query _ _ _ ((S.members c.id).imp le_of_lt)

theorem merge_parts (r n : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64) (full : St)
    (inv : Invariant.Inv full) (S : Ordered full) :
    Agree (fun c : Channel => decide (c.label.val = n.val)) (pgLoad (.Merge r n ids) full).channels
      full.channels ∧
    (∀ c ∈ full.channels, c.label.val = n.val →
      Agree (fun m : Member => decide (m.channel = c.id)) (pgLoad (.Merge r n ids) full).members
        full.members) ∧
    ∀ id ∈ ids.val,
      Agree (fun q : PatchRow => decide (q.id = id)) (pgLoad (.Merge r n ids) full).patches full.patches ∧
      Agree (fun d : Dependency => decide (d.patch = id)) (pgLoad (.Merge r n ids) full).deps full.deps ∧
      Agree (fun x : CheckReport => decide (x.patch = id)) (pgLoad (.Merge r n ids) full).checks
        full.checks := by
  unfold pgLoad loadMany; dsimp only
  simp only [chan_query inv]
  have qp : ∀ d, query patchKey full.patches (fun p => decide (p.id = d)) =
      full.patches.filter (fun p => decide (p.id = d)) := fun d => query_eq _ _ _ (pw_one (patch_one inv d))
  have qd : ∀ d, query depKey full.deps (fun x => decide (x.patch = d)) =
      full.deps.filter (fun x => decide (x.patch = d)) := fun d => query_eq _ _ _ ((S.deps d).imp le_of_lt)
  have qc : ∀ d, query checkKey full.checks (fun x => decide (x.patch = d)) =
      full.checks.filter (fun x => decide (x.patch = d)) := fun d => query_eq _ _ _ (pw_one (S.checks d))
  simp only [qp, qd, qc]
  have h1 : (appendNew [] (full.channels.filter fun c => decide (c.label.val = n.val))).filter
      (fun c => decide (c.label.val = n.val)) = full.channels.filter (fun c => decide (c.label.val = n.val)) :=
    (fold_single full.channels (fun (m : alloc.vec.Vec U8) (c : Channel) => decide (c.label.val = m.val))
      (fun m => chan_one inv m.val) [n] [] (by simp) List.nodup_nil).2 n (by simp)
  refine ⟨h1, ?_, ?_⟩
  · intro c hc hl
    have hfind : List.find? (fun c : Channel => decide (c.label.val = n.val))
        (appendNew [] (full.channels.filter fun c => decide (c.label.val = n.val))) = some c := by
      rw [← List.head?_filter, h1, List.head?_filter]; exact find_label inv hc hl
    rw [hfind]
    exact agree_query _ _ _ ((S.members c.id).imp le_of_lt)
  · intro id hid
    have hid' : id ∈ ([] : List U64) ++ ids.val := by simpa using hid
    refine ⟨?_, ?_, ?_⟩
    · split
      · exact query_true_filter _ _ _ (pw_one (patch_one inv id))
      · exact fold_blocks full.patches (·.id) (fun i => nodup_le_one (patch_one inv i)) ids.val [] []
          (by simp) List.nodup_nil (by simp) (by simp) id hid'
    · split
      · exact query_true_filter _ _ _ ((S.deps id).imp le_of_lt)
      · exact fold_blocks full.deps (·.patch) (fun i => nodup_of_lt (S.deps i)) ids.val [] []
          (by simp) List.nodup_nil (by simp) (by simp) id hid'
    · split
      · exact query_true_filter _ _ _ (pw_one (S.checks id))
      · exact fold_blocks full.checks (·.patch) (fun i => nodup_le_one (S.checks i)) ids.val [] []
          (by simp) List.nodup_nil (by simp) (by simp) id hid'

theorem merge_approvals_agree (r n : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64) (full : St)
    (hA : ApprovalsSorted full) :
    ∀ id ∈ ids.val, Agree (fun a : Approval => decide (a.patch = id))
      (pgLoad (.Merge r n ids) full).approvals full.approvals := by
  unfold pgLoad loadMany; dsimp only
  have qa : ∀ d, query approvalKey full.approvals (fun x => decide (x.patch = d)) =
      full.approvals.filter (fun x => decide (x.patch = d)) := fun d => query_eq _ _ _ ((hA d).imp le_of_lt)
  simp only [qa]
  intro id hid
  have hid' : id ∈ ([] : List U64) ++ ids.val := by simpa using hid
  split
  · exact query_true_filter _ _ _ ((hA id).imp le_of_lt)
  · exact fold_blocks full.approvals (·.patch) (fun i => nodup_of_lt (hA i)) ids.val [] []
      (by simp) List.nodup_nil (by simp) (by simp) id hid'

theorem mem_query {α κ} [LinearOrder κ] {key : α → κ} {t : List α} {P : α → Bool} {x : α} :
    x ∈ query key t P ↔ x ∈ t ∧ P x = true := by
  unfold query; rw [List.mem_filter, List.mem_mergeSort]

theorem mem_fold_appendNew {α β : Type} [DecidableEq α] (B : β → List α) (ids : List β) (acc : List α) (x : α) :
    x ∈ ids.foldl (fun acc j => appendNew acc (B j)) acc ↔ x ∈ acc ∨ ∃ j ∈ ids, x ∈ B j := by
  induction ids generalizing acc with
  | nil => simp
  | cons j js ih =>
    rw [List.foldl_cons, ih, mem_appendNew]
    simp only [List.mem_cons, exists_eq_or_imp]
    tauto

theorem any_eq_of_mem {α} {P : α → Bool} {l1 l2 : List α}
    (h : ∀ x, P x = true → (x ∈ l1 ↔ x ∈ l2)) : l1.any P = l2.any P := by
  apply Bool.eq_iff_iff.2
  simp only [List.any_eq_true]
  constructor
  · rintro ⟨x, hx, hp⟩; exact ⟨x, (h x hp).1 hx, hp⟩
  · rintro ⟨x, hx, hp⟩; exact ⟨x, (h x hp).2 hx, hp⟩

/-- Whatever the order of a patch's approvals, the loaded ones are exactly
the stored ones. -/
theorem merge_approvals_any (r n : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64) (full : St) :
    ∀ id ∈ ids.val, (pgLoad (.Merge r n ids) full).approvals.any (fun a => decide (a.patch = id)) =
      full.approvals.any (fun a => decide (a.patch = id)) := by
  intro id hid
  apply any_eq_of_mem
  intro x hx
  simp only [decide_eq_true_eq] at hx
  unfold pgLoad loadMany; dsimp only
  split
  · rw [mem_query]; simp
  · rw [mem_fold_appendNew]
    simp only [List.not_mem_nil, false_or, mem_query, decide_eq_true_eq]
    constructor
    · rintro ⟨j, -, hx', -⟩; exact hx'
    · intro hx'; exact ⟨id, hid, hx', hx⟩

/-- `Narrow` for the PostgreSQL loads, from the orderings each lookup relies
on (`ApprovalsSorted` is used by `merge` only). -/
theorem narrow_core (c : Command) (full : St) (inv : Invariant.Inv full) (S : Ordered full)
    (hA : (∃ r n ids, c = .Merge r n ids) → ApprovalsSorted full) : Narrow c full (pgLoad c full) := by
  cases c with
  | Register n repo => exact ⟨rfl, fun h => by simp [usesRoles] at h, register_parts n repo full S⟩
  | Propose r h deps uses b => exact ⟨rfl, fun _ => rfl, propose_parts r h deps uses b full inv⟩
  | Approve r p =>
    refine ⟨rfl, fun _ => rfl, ?_⟩
    show Agree _ (query patchKey full.patches fun q => decide (q.id = p)) full.patches
    exact agree_query _ _ _ (pw_one (patch_one inv p))
  | Check r p x =>
    refine ⟨rfl, fun _ => rfl, ?_⟩
    show Agree _ (query patchKey full.patches fun q => decide (q.id = p)) full.patches
    exact agree_query _ _ _ (pw_one (patch_one inv p))
  | Fork r src n => exact ⟨rfl, fun _ => rfl, fork_parts r src n full inv S⟩
  | Merge r n ids =>
    obtain ⟨p1, p2, p3⟩ := merge_parts r n ids full inv S
    exact ⟨rfl, fun _ => rfl, p1, p2, fun id hid => ⟨(p3 id hid).1, (p3 id hid).2.1,
      merge_approvals_agree r n ids full (hA ⟨r, n, ids, rfl⟩) id hid, (p3 id hid).2.2⟩⟩
  | _ => exact ⟨by simp [pgLoad], fun _ => by simp [pgLoad], trivial⟩

theorem ordered_of_key {s : St} (ord : KeyOrdered s) : Ordered s := by
  obtain ⟨oP, oD, oA, oC, oCh, oM, oPa, oL⟩ := ord
  refine ⟨oM, oD, ?_, ?_, ?_⟩
  · intro p
    exact le_one_of_nodup_map (fun x : CheckReport => x.patch)
      (nodup_map_of_lt (key := checkKey) (fun a b e => by simp [checkKey, e]) oC) p
  · intro n; exact oL.sublist List.filter_sublist
  · intro r
    exact le_one_of_nodup_map (fun l : Listing => l.repo)
      (nodup_map_of_lt (key := listingKey) (fun a b e => by simp [listingKey, e]) oL) r

/-- The loader's rows agree with the committed tenant on every lookup. -/
theorem pg_load_narrow (c : Command) (full : St) (inv : Invariant.Inv full)
    (ord : KeyOrdered full) : Narrow c full (pgLoad c full) :=
  narrow_core c full inv (ordered_of_key ord) (fun _ => ord.2.2.1)

/-! ## Reachable states

`KeyOrdered` does not hold in every reachable state (so the former
`key_ordered` was false): approvals are stored in the order reviewers
approve, check reports and listings in the order they are made, and nothing
in `Invariant.Inv` orders a table at all. What does hold, and is all the
loads need, is `Ordered`; approvals are only ever tested for existence. -/

theorem admitOne_agree' {part full : St} {id : U64}
    (h1 : Agree (fun q : PatchRow => decide (q.id = id)) part.patches full.patches)
    (h2 : Agree (fun d : Dependency => decide (d.patch = id)) part.deps full.deps)
    (h3 : part.approvals.any (fun a => decide (a.patch = id)) = full.approvals.any (fun a => decide (a.patch = id)))
    (h4 : Agree (fun x : CheckReport => decide (x.patch = id)) part.checks full.checks) (u ch : U64)
    (st : Model.Adm) : Model.admitOne u part ch id st = Model.admitOne u full ch id st := by
  unfold Model.admitOne Model.depsOf Model.approvedB Model.passedB
  rw [find?_agree h1, filter_agree h2, h3, find?_agree h4]

theorem merge_agree' {full part : St} (hr : part.repo = full.repo) {u : U64} {n : alloc.vec.Vec U8}
    {ids : List U64}
    (h1 : Agree (fun c : Channel => decide (c.label.val = n.val)) part.channels full.channels)
    (h2 : ∀ c ∈ full.channels, c.label.val = n.val →
      Agree (fun m : Member => decide (m.channel = c.id)) part.members full.members)
    (h3 : ∀ id ∈ ids, Agree (fun q : PatchRow => decide (q.id = id)) part.patches full.patches ∧
        Agree (fun d : Dependency => decide (d.patch = id)) part.deps full.deps ∧
        part.approvals.any (fun a => decide (a.patch = id)) = full.approvals.any (fun a => decide (a.patch = id)) ∧
        Agree (fun x : CheckReport => decide (x.patch = id)) part.checks full.checks) :
    Model.merge u part n ids = Model.merge u full n ids := by
  unfold Model.merge Model.findChannel
  rw [find?_agree h1, hr]
  cases hc : List.find? (fun c : Channel => decide (c.label.val = n.val)) full.channels with
  | none => rfl
  | some c' =>
    have hcm := List.mem_of_find?_eq_some hc
    have hl : c'.label.val = n.val := by simpa using List.find?_some hc
    simp only
    rw [filter_agree (h2 c' hcm hl)]
    rw [fold_agree (Model.admitStep u part c'.id) (Model.admitStep u full c'.id) ids _ (by
      intro id hid y
      obtain ⟨g1, g2, g3, g4⟩ := h3 id hid
      unfold Model.admitStep
      rw [admitOne_agree' g1 g2 g3 g4])]

/-- On what the PostgreSQL loader returns the model computes what it computes
on the whole tenant, given `Ordered`. -/
theorem pg_model (a : Principal) (c : Command) (full : St) (inv : Invariant.Inv full)
    (S : Ordered full) : Model.step a (pgLoad c full) c = Model.step a full c := by
  cases c with
  | Merge r n ids =>
    obtain ⟨p1, p2, p3⟩ := merge_parts r n ids full inv S
    have hr : (pgLoad (.Merge r n ids) full).repo = full.repo := rfl
    have hro : (pgLoad (.Merge r n ids) full).roles = full.roles := rfl
    simp only [Model.step]
    rw [merge_agree' hr p1 p2 (fun id hid => ⟨(p3 id hid).1, (p3 id hid).2.1,
      merge_approvals_any r n ids full id hid, (p3 id hid).2.2⟩)]
    simp only [Model.guarded, Model.authorize, Model.readable, hr, hro]
    try rfl
  | _ =>
    exact Narrow.narrow_model a full _ _
      (narrow_core _ full inv S (by rintro ⟨_, _, _, h⟩; cases h))

/-- Each channel's members are stored in position order. -/
def MembersSorted (s : St) : Prop :=
  s.members.Pairwise fun a b => a.channel = b.channel → a.position.val < b.position.val

/-- What reachable states keep: members in position order per channel, unique
directory labels and repositories, one check report per patch. -/
structure ROrd (s : St) : Prop where
  members : MembersSorted s
  labels : (s.listings.map fun l => l.label.val).Nodup
  repos : (s.listings.map (·.repo)).Nodup
  checks : (s.checks.map (·.patch)).Nodup

/-- Member rows a batch admission writes are in increasing position. -/
def JM (st : Model.Adm) : Prop :=
  (st.writes.filterMap memberOf).Pairwise fun a b => a.position.val < b.position.val

theorem admitOne_J {s : St} {c : Channel} {u id : U64} {st : Model.Adm} (hf : Invariant.FI s c st)
    (hj : JM st) (h : (Model.admitOne u s c.id id st).1 = none) : JM (Model.admitOne u s c.id id st).2 := by
  unfold Model.admitOne at h ⊢
  cases hp : s.patches.find? (fun p => decide (p.id = id)) with
  | none => simp [hp] at h
  | some p =>
  simp only [hp] at h ⊢
  cases hk : Model.admitK st.members (Model.depsOf s id) (Model.approvedB s id) (Model.passedB s id) id with
  | Err e => simp [hk] at h
  | Ok next =>
  simp only [hk] at h ⊢
  by_cases hin : id ∈ st.members
  · simp only [hin, ↓reduceIte] at h ⊢
    by_cases hm : st.seq = Model.MAX
    · simp [hm] at h
    simp only [hm, ↓reduceIte]
    unfold JM at hj ⊢; simpa [List.filterMap_append, memberOf] using hj
  · simp only [hin, ↓reduceIte] at h ⊢
    by_cases hm1 : st.size = Model.MAX
    · simp [hm1] at h
    by_cases hm2 : st.admitted = Model.MAX
    · simp [hm1, hm2] at h
    by_cases hm3 : st.seq = Model.MAX
    · simp [hm1, hm2, hm3] at h
    simp only [hm1, hm2, hm3, ↓reduceIte]
    unfold JM
    have hmw : ((st.writes ++ [Write.PutMember ⟨c.id, st.size, id⟩]) ++
        [Write.PutEvent ⟨Model.inc st.seq, u, MERGE, some id, some c.id⟩]).filterMap memberOf =
        st.writes.filterMap memberOf ++ [⟨c.id, st.size, id⟩] := by
      simp [List.filterMap_append, memberOf]
    rw [hmw, List.pairwise_append]
    refine ⟨hj, by simp, ?_⟩
    intro x hx y hy
    simp only [List.mem_singleton] at hy; subst hy
    exact (hf.members x hx).2.2.1

theorem fold_FJ {s : St} {c : Channel} {u : U64} (ids : List U64) (x : Model.Adm × Option Error)
    (hx : x.2 = none → Invariant.FI s c x.1 ∧ JM x.1) :
    (ids.foldl (Model.admitStep u s c.id) x).2 = none →
      Invariant.FI s c (ids.foldl (Model.admitStep u s c.id) x).1 ∧
        JM (ids.foldl (Model.admitStep u s c.id) x).1 := by
  induction ids generalizing x with
  | nil => exact hx
  | cons id ids ih =>
    apply ih
    unfold Model.admitStep
    cases h2 : x.2 with
    | none =>
      simp only
      intro hf
      obtain ⟨f1, j1⟩ := hx h2
      exact ⟨Invariant.admitOne_FI f1 hf, admitOne_J f1 j1 hf⟩
    | some e => simp

theorem merge_FJ {s : St} {u : U64} {n : alloc.vec.Vec U8} {ids : List U64} {ws r}
    (hm : Model.merge u s n ids = .Ok (ws, r)) :
    ∃ c ∈ s.channels, ∃ st, Invariant.FI s c st ∧ JM st ∧
      ws = st.writes ++ [.PutChannel { c with size := st.size }, .SetRepository { s.repo with events := st.seq }] := by
  unfold Model.merge at hm
  split at hm
  · simp at hm
  split at hm
  · simp at hm
  rename_i c hc
  dsimp only at hm
  split at hm
  · rename_i st hfold
    simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at hm
    have := fold_FJ (u := u) ids
      (⟨(s.members.filter (fun m => decide (m.channel = c.id))).map (·.patch), c.size, s.repo.events, 0#u64, []⟩, none)
      (fun _ => ⟨⟨le_refl _, le_refl _, by simp, by simp, by simp, by simp, by intro x; simp⟩, by simp [JM]⟩)
    rw [hfold] at this
    obtain ⟨h1, h2⟩ := this rfl
    exact ⟨c, List.mem_of_find?_eq_some hc, st, h1, h2, hm.1.symm⟩
  · simp at hm

/-- What a successful command writes into the listings and members tables. -/
theorem step_shapes (a : Principal) {s : St} (hs : MembersSorted s) (c : Command) {ws r}
    (h : Model.step a s c = .Ok (ws, r)) :
    (ws.filterMap listingOf = [] ∨ ∃ l : Listing, ws.filterMap listingOf = [l] ∧
      (∀ y ∈ s.listings, y.label.val ≠ l.label.val) ∧ ∀ y ∈ s.listings, y.repo ≠ l.repo) ∧
    (ws.filterMap memberOf).Pairwise (fun a b => a.channel = b.channel → a.position.val < b.position.val) := by
  cases c <;> simp only [Model.step] at h
  case Register n repo =>
    unfold Model.register at h
    repeat' split at h
    all_goals simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    all_goals obtain ⟨rfl, -⟩ := h
    · simp
    · refine ⟨.inr ⟨⟨repo, n⟩, by simp [listingOf], ?_, ?_⟩, by simp [memberOf]⟩
      · intro y hy e
        have := List.find?_eq_none.1 ‹List.find? _ s.listings = none› y hy
        simp [e] at this
      · intro y hy e
        apply ‹¬ (s.listings.any _) = true›
        exact List.any_eq_true.2 ⟨y, hy, by simp [e]⟩
  case Create =>
    unfold Model.create at h
    split_ifs at h <;> simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h; simp [listingOf, memberOf]
  all_goals first
    | (simp at h; done)
    | (replace h := (Model.guarded_ok h).2)
  case Grant =>
    unfold Model.grant at h
    split_ifs at h <;> simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    all_goals obtain ⟨rfl, -⟩ := h
    all_goals refine ⟨.inl ?_, ?_⟩ <;> simp [listingOf, memberOf]
  case Fork =>
    unfold Model.fork at h
    repeat' split at h
    all_goals simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    obtain ⟨rfl, -⟩ := h
    refine ⟨.inl (by simp [List.filterMap_append, listingOf, Function.comp_def]), ?_⟩
    simp only [List.filterMap_append, List.filterMap_cons, List.filterMap_nil, List.filterMap_map, memberOf,
      Function.comp_def, Invariant.filterMap_some_eq_map, Invariant.filterMap_none, List.nil_append,
      List.append_nil]
    rw [List.pairwise_map, List.pairwise_filter]
    exact hs.imp fun {a b} hab ha hb _ => hab (by
      simp only [decide_eq_true_eq] at ha hb; rw [ha, hb])
  case Propose =>
    unfold Model.propose at h
    repeat' split at h
    all_goals simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h
    all_goals obtain ⟨rfl, -⟩ := h
    all_goals simp [List.filterMap_append, listingOf, memberOf, Function.comp_def]
  case Approve | Check =>
    simp only [Model.approve, Model.check] at h
    repeat' split at h
    all_goals (try simp only [reduceCtorEq, core.result.Result.Ok.injEq, Prod.mk.injEq] at h)
    all_goals (obtain ⟨rfl, -⟩ := h; simp [listingOf, memberOf])
  case Merge =>
    obtain ⟨c, hc, st, hf, hj, rfl⟩ := merge_FJ h
    have hl : st.writes.filterMap listingOf = [] := by
      apply List.filterMap_eq_nil_iff.2
      intro w hw
      rcases hf.kinds w hw with ⟨m, rfl⟩ | ⟨e, rfl⟩ <;> rfl
    refine ⟨.inl (by rw [List.filterMap_append, hl]; rfl), ?_⟩
    have e : (st.writes ++ [Write.PutChannel { c with size := st.size },
        Write.SetRepository { s.repo with events := st.seq }]).filterMap memberOf =
        st.writes.filterMap memberOf := by simp [List.filterMap_append, memberOf]
    rw [e]
    unfold JM at hj
    refine hj.imp ?_
    intro a b h _
    exact h

theorem rord_step (a : Principal) {s : St} (hi : Invariant.Inv s) (ho : ROrd s) (c : Command) {ws r}
    (h : Model.step a s c = .Ok (ws, r)) : ROrd (applyAll s ws) := by
  obtain ⟨hl, hx⟩ := step_shapes a ho.members c h
  have hw := (Invariant.step_writes a c h).2
  refine ⟨?_, ?_, ?_, ?_⟩
  · set xs := ws.filterMap memberOf
    have cross : ∀ m ∈ s.members, ∀ x ∈ xs, m.channel = x.channel → m.position.val < x.position.val := by
      intro m hm x hx' e
      obtain ⟨w, hw', hxw⟩ := List.mem_filterMap.1 hx'
      cases w <;> simp [memberOf] at hxw
      subst hxw
      rcases hw _ hw' with ⟨hne, hch⟩ | ⟨c', hc', hch, hpos⟩
      · have := Invariant.channel_of_member hi hm
        have hv := Refine.inc_val hne
        rw [e, hch, hv] at this; omega
      · obtain ⟨c'', hc'', hid, hp⟩ := hi.members_placed m hm
        have : c'' = c' := List.inj_on_of_nodup_map hi.channel_keys hc'' hc' (by rw [hid, e, hch])
        subst this
        omega
    have hkeys : (xs.map fun y => (y.channel, y.position)).Nodup :=
      List.pairwise_map.2 (hx.imp fun {a b} hab e => by
        simp only [Prod.mk.injEq] at e
        have := hab e.1; rw [e.2] at this; exact lt_irrefl _ this)
    have hfresh : ∀ x ∈ xs, ∀ y ∈ s.members,
        (fun y : Member => (y.channel, y.position)) y ≠ (fun y => (y.channel, y.position)) x := by
      intro x hx' y hy e
      simp only [Prod.mk.injEq] at e
      have := cross y hy x hx' e.1; rw [e.2] at this; exact lt_irrefl _ this
    unfold MembersSorted
    rw [applyAll_members, Invariant.puts_fresh _ _ _ hkeys hfresh, List.pairwise_append]
    exact ⟨ho.members, hx, fun m hm x hx' => cross m hm x hx'⟩
  · rw [applyAll_listings]
    rcases hl with h0 | ⟨l, h0, hlab, hrep⟩
    · rw [h0]; exact ho.labels
    · rw [h0, Invariant.puts_single, upsert_fresh _ _ _ (fun y hy e => hrep y hy e), List.map_append,
        List.nodup_append]
      refine ⟨ho.labels, by simp, ?_⟩
      simp only [List.mem_map, List.mem_singleton]
      rintro _ ⟨y, hy, rfl⟩ _ ⟨_, rfl, rfl⟩ e
      exact hlab y hy e
  · rw [applyAll_listings]; exact puts_nodup _ ho.repos
  · rw [applyAll_checks]; exact puts_nodup _ ho.checks

theorem reachable_rord {s : St} (h : Invariant.Reachable s) : ROrd s := by
  induction h with
  | init => exact ⟨by simp [MembersSorted, init], by simp [init], by simp [init], by simp [init]⟩
  | step hr hs ih =>
    obtain ⟨r, hm⟩ := Guarantees.model_of hs
    exact rord_step _ (Invariant.reachable_inv hr) ih _ hm

theorem pair_lt {a b c : Nat} (h : b < c) : toLex (a, b) < toLex (a, c) := Prod.Lex.right _ h

/-- Every dependency list stored in order: the remaining assumption. -/
theorem ordered_of_reachable {s : St} (h : Invariant.Reachable s) (deps : DepsAscending s) : Ordered s := by
  have ho := reachable_rord h
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · intro ch
    apply List.pairwise_filter.2
    refine ho.members.imp fun {a b} hab ha hb => ?_
    simp only [decide_eq_true_eq] at ha hb
    have := hab (ha.trans hb.symm)
    simp only [memberKey, ha, hb]
    exact pair_lt this
  · intro p
    refine (deps p).imp_of_mem fun {a b} ha hb hab => ?_
    have ha' := (List.mem_filter.1 ha).2
    have hb' := (List.mem_filter.1 hb).2
    simp only [decide_eq_true_eq] at ha' hb'
    simp only [depKey, ha', hb']
    exact pair_lt hab
  · intro p; exact le_one_of_nodup_map (fun x : CheckReport => x.patch) ho.checks p
  · intro n; exact pw_one' (le_one_of_nodup_map (fun l : Listing => l.label.val) ho.labels n)
  · intro r; exact le_one_of_nodup_map (fun l : Listing => l.repo) ho.repos r

/-- **PostgreSQL loads.** On the rows the PostgreSQL loader returns, the
extracted `transition` computes what the whole committed tenant specifies,
in every reachable state whose dependency lists are stored in order.

Statement fixed: it assumed `Invariant.Inv` only, which orders no table; a
state satisfying `Inv` whose source channel stores its members out of
position order makes `fork` write the copies in another order than on the
PostgreSQL rows (which come `ORDER BY` position). Reachable states keep that
order (`reachable_rord`). -/
theorem pg_served (a : Principal) (s p : Snapshot) (c : Command)
    (reach : Invariant.Reachable (Snapshot.toSt s)) (deps : DepsAscending (Snapshot.toSt s))
    (loaded : Snapshot.toSt p = pgLoad c (Snapshot.toSt s)) (r) (h : transition a p c = ok r) :
    Refine.lift r = Model.step a (Snapshot.toSt s) c := by
  rw [Refine.transition_refines _ _ _ _ h, loaded]
  exact pg_model a c _ (Invariant.reachable_inv reach) (ordered_of_reachable reach deps)

end o5s_registry.Served
