import O5sNetKernel
import H5iAppLib
/-!
# The access policy: what the extracted `permits` and `visible` compute

`crates/net-kernel/src/access.rs`. The framework theorem (`Authz.lean`) says a committed
command's writes are `permits`-ted and its rows `visible`; this file says what those are:

* `allows_spec`: a principal holds `need` on a name iff some grant has at least `need` and a
  prefix the name is segment-aligned under (`underB`, `""` covers everything);
* `permits_spec`: a ref write is permitted iff the principal may write the name;
* `visible_object_spec`: an object row is visible iff the snapshot lists cells reaching it and
  the principal may read one of them. With core's cell model (a grant covers a ref iff it
  covers the ref's cell), that is: iff the object is reachable from a name the principal may
  read.
-/
open Aeneas Aeneas.Std Result H5iAppLib o5s_net_kernel

namespace o5s_net_kernel.Spec.Policy

abbrev B (l : List U8) : List Nat := l.map (·.val)

/-- `s` is `p`, or lies under `p/`; the empty prefix covers everything. -/
def underB (s p : List Nat) : Bool :=
  p.isEmpty || (p.isPrefixOf s && (s.length == p.length || s[p.length]? == some 47))

/-- Some grant has at least `need` on `name`. -/
def allowsS (gs : List access.Grant) (nm : List Nat) (need : Nat) : Bool :=
  gs.any (fun g => decide (need ≤ g.access.val) && underB nm (B g.prefix.val))

theorem take_eq_of_prefix (p s : List Nat) (h : p.length ≤ s.length) :
    p.isPrefixOf s = true ↔ s.take p.length = p := by
  rw [List.isPrefixOf_iff_prefix, List.prefix_iff_eq_take]
  exact ⟨fun h => h.symm, fun h => h.symm⟩

@[step]
theorem is_under_loop_spec (s p : Slice U8) (i : Usize)
    (hl : p.length ≤ s.length) (hi : i.val ≤ p.length)
    (ht : (B s.val).take i.val = (B p.val).take i.val) :
    access.is_under_loop s p i ⦃ b => b = ((B p.val).isPrefixOf (B s.val) &&
      ((B s.val).length == (B p.val).length || (B s.val)[(B p.val).length]? == some 47)) ⦄ := by
  unfold access.is_under_loop
  apply loop.spec_decr_nat (measure := fun (j : Usize) => p.length - j.val)
    (inv := fun j => j.val ≤ p.length ∧ (B s.val).take j.val = (B p.val).take j.val)
  · intro j ⟨h1, h2⟩
    have hlp : (B p.val).length = p.length := by simp
    have hls : (B s.val).length = s.length := by simp
    have get : ∀ (l : Slice U8) (k : Nat) (hk : k < l.length), (B l.val)[k]? = some (l.val[k]'(by simpa using hk)).val := by
      intro l k hk; simp [B, List.getElem?_map, List.getElem?_eq_getElem (by simpa using hk)]
    -- At the end of the loop (j = |p|), p is a prefix of s.
    have full : j.val = p.length → (B p.val).isPrefixOf (B s.val) = true := by
      intro hj
      rw [take_eq_of_prefix _ _ (by rw [hlp, hls]; exact hl), hlp, ← hj, h2, hj, ← hlp, List.take_length]
    unfold access.is_under_loop.body
    step*
    · -- mismatch at j: not a prefix
      have hpre : (B p.val).isPrefixOf (B s.val) = false := by
        cases hh : (B p.val).isPrefixOf (B s.val)
        · rfl
        · exfalso
          rw [take_eq_of_prefix _ _ (by rw [hlp, hls]; exact hl)] at hh
          have e : (B s.val)[j.val]? = (B p.val)[j.val]? := by
            rw [← hh, List.getElem?_take]; simp; scalar_tac
          rw [get s j.val (by scalar_tac), get p j.val (by scalar_tac)] at e
          simp only [Option.some.injEq] at e
          have : i2 = i3 := by rw [i2_post, i3_post]; exact UScalar.eq_of_val_eq e
          simp_all
      simp [hpre]
    · refine ⟨by scalar_tac, ?_, by scalar_tac⟩
      have heq : i2 = i3 := UScalar.eq_of_val_eq (by simpa using ‹¬(i2 != i3) = true›)
      rw [i4_post, List.take_add_one, List.take_add_one, h2, get s j.val (by scalar_tac), get p j.val (by scalar_tac)]
      rw [← i2_post, ← i3_post, heq]
    all_goals first
      | (have hj : j.val = p.length := by scalar_tac
         have hlen : (B s.val).length = (B p.val).length := by rw [hlp, hls]; scalar_tac
         simp [full hj, hlen]; done)
      | (have hj : j.val = p.length := by scalar_tac
         have hne : ¬ (B s.val).length = (B p.val).length := by rw [hlp, hls]; scalar_tac
         rw [hlp, get s p.length (by scalar_tac)]
         simp only [full hj, Bool.true_and, hne, beq_iff_eq, false_or, Option.some.injEq]
         rw [i5_post]
         constructor
         · intro h; rw [h]; rfl
         · intro h; exact UScalar.eq_of_val_eq (by simpa using h))
  · exact ⟨hi, ht⟩

@[step]
theorem is_under_spec (s p : Slice U8) :
    access.is_under s p ⦃ b => b = underB (B s.val) (B p.val) ⦄ := by
  unfold access.is_under
  have hlp : (B p.val).length = p.length := by simp
  have hls : (B s.val).length = s.length := by simp
  step*
  · have : B p.val = [] := List.eq_nil_of_length_eq_zero (by rw [hlp]; scalar_tac)
    simp [underB, this]
  · have hne : (B p.val).isEmpty = false := by
      cases h : B p.val with
      | nil => have := congrArg List.length h; simp at this; scalar_tac
      | cons _ _ => rfl
    have hnp : (B p.val).isPrefixOf (B s.val) = false := by
      cases h : (B p.val).isPrefixOf (B s.val)
      · rfl
      · have := List.IsPrefix.length_le ((List.isPrefixOf_iff_prefix).1 h); rw [hlp, hls] at this; scalar_tac
    simp [underB, hne, hnp]
  all_goals first
    | (simp; done)
    | (have hne : (B p.val).isEmpty = false := by
         cases h : B p.val with
         | nil => have := congrArg List.length h; simp at this; scalar_tac
         | cons _ _ => rfl
       simp only [underB, hne, Bool.false_or]
       simp_all)

@[step]
theorem allows_spec (p : access.Principal) (nm : Slice U8) (need : U8) :
    access.allows p nm need ⦃ b => b = allowsS p.grants.val (B nm.val) need.val ⦄ := by
  unfold access.allows access.allows_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_any (alloc.vec.Vec.deref p.grants)
    (fun g => decide (need.val ≤ g.access.val) && underB (B nm.val) (B g.prefix.val)) true false _ ?_)
  · intro y hy; rw [hy]; simp only [allowsS, vec_deref_val]
    rw [Bool.eq_iff_iff]; simp [List.any_eq_true]
  · intro i hi
    unfold access.allows_loop.body
    h5i_iter [IterSearchStep, vec_deref_val]
    all_goals (exact (vec_deref_val p.grants) ▸ ‹i < (alloc.vec.Vec.deref p.grants).val.length›)

/-- Some cell is readable. -/
def readsSomeS (p : access.Principal) (cells : List (alloc.vec.Vec U8)) : Bool :=
  cells.any (fun c => allowsS p.grants.val (B c.val) 1)

@[step]
theorem reads_some_spec (p : access.Principal) (cells : alloc.vec.Vec (alloc.vec.Vec U8)) :
    access.reads_some p cells ⦃ b => b = readsSomeS p cells.val ⦄ := by
  unfold access.reads_some access.reads_some_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_any (alloc.vec.Vec.deref cells)
    (fun c => allowsS p.grants.val (B c.val) 1) true false _ ?_)
  · intro y hy; rw [hy]; simp only [readsSomeS, vec_deref_val]
    rw [Bool.eq_iff_iff]; simp [List.any_eq_true]
  · intro i hi
    unfold access.reads_some_loop.body
    h5i_iter [IterSearchStep, access.READ, vec_deref_val]
    all_goals (exact (vec_deref_val cells) ▸ ‹i < (alloc.vec.Vec.deref cells).val.length›)

/-- The cells reaching `h` in the snapshot (the first entry for `h`). -/
def cellsS (s : access.Snapshot) (h : alloc.vec.Vec U8) : Option (alloc.vec.Vec (alloc.vec.Vec U8)) :=
  (s.objects.val.find? (fun e => decide (e.1 = h))).map (·.2)

@[step]
theorem cells_of_spec (s : access.Snapshot) (h : alloc.vec.Vec U8) :
    access.cells_of s h ⦃ r => r = cellsS s h ⦄ := by
  unfold access.cells_of access.cells_of_loop
  step*
  try subst_vars
  apply WP.spec_mono (iter_search (alloc.vec.Vec.deref s.objects) (fun e => decide (e.1 = h)) id
    (fun _ x => some x.2) none _ ?_ 0 (by simp))
  · intro y hy; rw [id_eq] at hy; rw [hy, searchFrom_find]; simp [cellsS, vec_deref_val]
  · intro i hi
    unfold access.cells_of_loop.body
    rw [H5iAppLib.slice_iter_next]
    split
    · rename_i hlt
      simp only [bind_ok]
      rcases hx : (alloc.vec.Vec.deref s.objects).val[i] with ⟨a, v⟩
      show (match (some (a, v) : Option _) with
        | none => ok (ControlFlow.done none)
        | some e =>
          (match e with
          | (v0, v1) => do
            let b ← alloc.vec.partial_eq.PartialEqVec.eq core.cmp.PartialEqU8 v0 h
            if b = true then do
              let v2 ← alloc.vec.CloneVec.clone (core.clone.CloneallocvecVec core.clone.CloneU8) v1
              ok (ControlFlow.done (some v2))
            else ok (ControlFlow.cont ({ slice := alloc.vec.Vec.deref s.objects, i := i + 1 } : SIter _)))) ⦃ _ ⦄
      dsimp only
      step*
      · unfold IterSearchStep; left
        refine ⟨hlt, ?_, ?_⟩
        · erw [hx]; simp_all
        · erw [hx]; simp_all
      · unfold IterSearchStep
        refine ⟨hlt, ?_, rfl⟩
        erw [hx]; simp_all
    · step*
      unfold IterSearchStep; right; simp_all

/-- **The object-read policy.** An object row is visible to `actor` iff the snapshot lists
cells reaching it and `actor` may read one of them. -/
theorem visible_object_spec (actor : access.Principal) (s : access.Snapshot) (h : alloc.vec.Vec U8) :
    access.Access.Insts.H5i_app_stdAuthzPolicyPrincipalSnapshotWriteRow.visible actor s (access.Row.Object h)
      ⦃ b => b = match cellsS s h with
        | some cs => readsSomeS actor cs.val
        | none => false ⦄ := by
  unfold access.Access.Insts.H5i_app_stdAuthzPolicyPrincipalSnapshotWriteRow.visible
  step*

/-- **The ref-write policy.** A ref write is permitted iff `actor` holds write on the name. -/
theorem permits_spec (actor : access.Principal) (s : access.Snapshot) (n : alloc.vec.Vec U8) :
    access.Access.Insts.H5i_app_stdAuthzPolicyPrincipalSnapshotWriteRow.permits actor s (access.Write.Ref n)
      ⦃ b => b = allowsS actor.grants.val (B n.val) 2 ⦄ := by
  unfold access.Access.Insts.H5i_app_stdAuthzPolicyPrincipalSnapshotWriteRow.permits
  step*
  all_goals simp_all [access.WRITE, vec_deref_val]

end o5s_net_kernel.Spec.Policy
