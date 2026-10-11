import Storage
import StdSpecs
/-!
# What the kernel's lookups compute

One spec per helper of `crates/registry/src/lib.rs`, each a total
correctness statement about the extracted function.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema H5iAppLib

namespace o5s_registry.Helpers

attribute [simp] H5iAppLib.u64_val_eq

@[step] theorem find_patch_spec (ps : alloc.vec.Vec PatchRow) (id : U64) :
    find_patch ps id ⦃ o => o = ps.val.find? (fun p => decide (p.id = id)) ⦄ := by
  unfold find_patch find_patch_loop
  apply WP.spec_mono (loop_search ps.val (fun p => decide (p.id = id)) (fun o : Option PatchRow => o)
    (fun _ p => some p) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]
  · intro j hj; unfold find_patch_loop.body; h5i_step

@[step] theorem find_hash_spec (ps : alloc.vec.Vec PatchRow) (h : alloc.vec.Vec U8) :
    find_hash ps h ⦃ o => o = (ps.val.find? (fun p => decide (p.hash.val = h.val))).map (·.id) ⦄ := by
  unfold find_hash find_hash_loop
  apply WP.spec_mono (loop_search ps.val (fun p => decide (p.hash.val = h.val)) (fun o : Option U64 => o)
    (fun _ p => some p.id) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, searchFrom_find]; simp
  · intro j hj; unfold find_hash_loop.body; h5i_step

@[step] theorem find_channel_spec (cs : alloc.vec.Vec Channel) (n : alloc.vec.Vec U8) :
    find_channel cs n ⦃ o => o = cs.val.find? (fun c => decide (c.label.val = n.val)) ⦄ := by
  unfold find_channel find_channel_loop
  apply WP.spec_mono (loop_search cs.val (fun c => decide (c.label.val = n.val)) (fun o : Option Channel => o)
    (fun _ c => some c) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]
  · intro j hj; unfold find_channel_loop.body; h5i_step

@[step] theorem find_path_spec (ks : alloc.vec.Vec PathKind) (p : alloc.vec.Vec U8) :
    find_path ks p ⦃ o => o = ks.val.find? (fun k => decide (k.path.val = p.val)) ⦄ := by
  unfold find_path find_path_loop
  apply WP.spec_mono (loop_search ks.val (fun k => decide (k.path.val = p.val)) (fun o : Option PathKind => o)
    (fun _ k => some k) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]
  · intro j hj; unfold find_path_loop.body; h5i_step

@[step] theorem find_listing_spec (ls : alloc.vec.Vec Listing) (n : alloc.vec.Vec U8) :
    find_listing ls n ⦃ o => o = ls.val.find? (fun l => decide (l.label.val = n.val)) ⦄ := by
  unfold find_listing find_listing_loop
  apply WP.spec_mono (loop_search ls.val (fun l => decide (l.label.val = n.val)) (fun o : Option Listing => o)
    (fun _ l => some l) none _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_find _ _ _ hr]
  · intro j hj; unfold find_listing_loop.body; h5i_step

@[step] theorem listing_key_used_spec (ls : alloc.vec.Vec Listing) (repo : U64) :
    listing_key_used ls repo ⦃ b => b = ls.val.any (fun l => decide (l.repo = repo)) ⦄ := by
  unfold listing_key_used listing_key_used_loop
  apply WP.spec_mono (loop_search ls.val (fun l => decide (l.repo = repo)) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; exact search_any _ _ _ hr
  · intro j hj; unfold listing_key_used_loop.body; h5i_step

@[step] theorem approved_spec (as_ : alloc.vec.Vec Approval) (patch : U64) :
    approved as_ patch ⦃ b => b = as_.val.any (fun a => decide (a.patch = patch)) ⦄ := by
  unfold approved approved_loop
  apply WP.spec_mono (loop_search as_.val (fun a => decide (a.patch = patch)) (fun b : Bool => b)
    (fun _ _ => true) false _ ?_ 0#usize (by simp))
  · intro r hr; exact search_any _ _ _ hr
  · intro j hj; unfold approved_loop.body; h5i_step

@[step] theorem checks_passed_spec (cs : alloc.vec.Vec CheckReport) (patch : U64) :
    checks_passed cs patch ⦃ b => b = ((cs.val.find? (fun c => decide (c.patch = patch))).map (·.passed)).getD false ⦄ := by
  unfold checks_passed checks_passed_loop
  apply WP.spec_mono (loop_search cs.val (fun c => decide (c.patch = patch)) (fun b : Bool => b)
    (fun _ c => c.passed) false _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, searchFrom_findD]; simp
  · intro j hj; unfold checks_passed_loop.body; h5i_step

@[step] theorem members_of_spec (ms : alloc.vec.Vec Member) (ch : U64) :
    members_of ms ch ⦃ v => v.val = (ms.val.filter (fun m => decide (m.channel = ch))).map (·.patch) ⦄ := by
  unfold members_of members_of_loop
  apply WP.spec_mono (loop_fold ms.val (fun o : alloc.vec.Vec U64 => o.val)
    (fun acc m => if decide (m.channel = ch) then acc ++ [m.patch] else acc) (fun o k => o.length ≤ k)
    (fun x => members_of_loop.body ms ch x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter_map]; simp
  · intro o j hj ho
    unfold members_of_loop.body
    h5i_step [FoldStep]

@[step] theorem dependencies_of_spec (ds : alloc.vec.Vec Dependency) (p : U64) :
    dependencies_of ds p ⦃ v => v.val = (ds.val.filter (fun d => decide (d.patch = p))).map (·.dependency) ⦄ := by
  unfold dependencies_of dependencies_of_loop
  apply WP.spec_mono (loop_fold ds.val (fun o : alloc.vec.Vec U64 => o.val)
    (fun acc d => if decide (d.patch = p) then acc ++ [d.dependency] else acc) (fun o k => o.length ≤ k)
    (fun x => dependencies_of_loop.body ds p x.1 x.2) ?_ _ 0#usize (by simp) (by simp))
  · intro r hr; rw [hr, foldl_filter_map]; simp
  · intro o j hj ho
    unfold dependencies_of_loop.body
    h5i_step [FoldStep]

@[step] theorem all_known_spec (ps : alloc.vec.Vec PatchRow) (ids : alloc.vec.Vec U64) :
    all_known ps ids ⦃ b => b = ids.val.all (fun i => ps.val.any (fun p => decide (p.id = i))) ⦄ := by
  unfold all_known all_known_loop
  apply WP.spec_mono (loop_search ids.val (fun i => !(ps.val.any (fun p => decide (p.id = i)))) (fun b : Bool => b)
    (fun _ _ => false) true _ ?_ 0#usize (by simp))
  · intro r hr; rw [search_all _ _ _ hr]; simp
  · intro j hj
    unfold all_known_loop.body
    dsimp only
    split
    · rename_i hlt
      have hj' : j.val < ids.val.length := by scalar_tac
      step as ⟨ x, hx ⟩
      step as ⟨ o, ho ⟩
      cases o with
      | none =>
        have hnone : ∀ p ∈ ps.val, p.id ≠ x := by
          intro p hp e
          have := List.find?_eq_none.1 ho.symm p hp
          simp [e] at this
        simp only [SearchStep, WP.spec_ok]
        left
        refine ⟨hj', ?_, by simp⟩
        subst hx
        simp only [Bool.not_eq_eq_eq_not, Bool.not_true, List.any_eq_false, decide_eq_true_eq]
        exact hnone
      | some q =>
        have hq : q ∈ ps.val := List.mem_of_find?_eq_some ho.symm
        have hqx : q.id = x := by simpa using List.find?_some ho.symm
        step as ⟨ k, hk ⟩
        simp only [SearchStep]
        refine ⟨hj', ?_, hk⟩
        subst hx
        have hany : (ps.val.any fun p => decide (p.id = ids.val[j.val])) = true :=
          List.any_eq_true.2 ⟨q, hq, by simpa using hqx⟩
        simp [hany]
    · simp only [SearchStep, WP.spec_ok]; right; exact ⟨by scalar_tac, by simp⟩

end o5s_registry.Helpers

namespace o5s_registry.Helpers

@[step] theorem name_byte_spec (c : U8) : name_byte c ⦃ _ => True ⦄ := by
  unfold name_byte
  split_ifs <;> simp

/-- A valid name has 1 to 64 bytes (and only name bytes, which the theorems do not need). -/
@[step] theorem valid_name_spec (n : alloc.vec.Vec U8) :
    valid_name n ⦃ b => b = true → 0 < n.val.length ∧ n.val.length ≤ 64 ⦄ := by
  unfold valid_name valid_name_loop
  dsimp only
  split
  · simp
  · split
    · simp
    · rename_i h0 hmax
      simp only [MAX_NAME] at hmax
      apply loop_idx_spec (fun i => valid_name_loop.body n i) (fun i => i) n.val.length (fun _ => True)
      · intro i _ hn
        unfold valid_name_loop.body
        h5i_step
        all_goals (try intro _); all_goals scalar_tac
      · trivial
      · simp

@[step] theorem valid_hash_spec (h : alloc.vec.Vec U8) :
    valid_hash h ⦃ b => b = true → h.val.length = 64 ⦄ := by
  unfold valid_hash valid_hash_loop
  dsimp only
  split
  · simp
  · rename_i h64
    simp only [HASH_LEN, bne_iff_ne, ne_eq, Decidable.not_not] at h64
    apply loop_idx_spec (fun i => valid_hash_loop.body h i) (fun i => i) h.val.length (fun _ => True)
    · intro i _ hn
      unfold valid_hash_loop.body
      h5i_step
      all_goals (try intro _); all_goals scalar_tac
    · trivial
    · simp

@[step] theorem visible_spec (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) :
    visible a r n ⦃ b => b = (decide (REPOSITORY_SCOPE.val ≤ a.scope.val) && r.created &&
      decide (r.org = a.org) && decide (r.label.val = n.val)) ⦄ := by
  unfold visible
  split_ifs <;> step* <;> simp_all [U64.le_equiv] <;> scalar_tac

/-- Adjacent handles are strictly ascending. -/
def ascending (l : List U64) : Prop := ∀ k (h : k + 1 < l.length), l[k].val < l[k + 1].val

@[step] theorem valid_batch_spec (ids : alloc.vec.Vec U64) :
    valid_batch ids ⦃ b => b = true → 0 < ids.val.length ∧ ids.val.length ≤ 10000 ∧ ascending ids.val ⦄ := by
  unfold valid_batch valid_batch_loop
  dsimp only
  split
  · simp
  · split
    · simp
    · rename_i h0 hmax
      simp only [MAX_BATCH] at hmax
      apply loop_idx_spec (fun i => valid_batch_loop.body ids i) (fun i => i) ids.val.length
        (fun i => 1 ≤ i.val ∧ ∀ k, k + 1 < i.val → ∀ hk : k + 1 < ids.val.length,
          ids.val[k].val < ids.val[k + 1].val)
      · intro i ⟨h1, hi⟩ hn
        unfold valid_batch_loop.body
        dsimp only
        split
        · rename_i hlt
          step*
          refine ⟨by scalar_tac, ?_, by scalar_tac, by scalar_tac⟩
          intro k hk hk'
          by_cases hkk : k + 1 < i.val
          · exact hi k hkk hk'
          · have e1 : k = i2.val := by scalar_tac
            subst e1
            have h34 : i3.val < i4.val := by scalar_tac
            have e2 : i2.val + 1 = i.val := by scalar_tac
            simp only [e2, ← i3_post, ← i4_post]
            exact h34
        · simp only [WP.spec_ok]
          intro _
          refine ⟨by scalar_tac, by scalar_tac, fun k hk => hi k (by scalar_tac) hk⟩
      · exact ⟨by simp, fun k hk => by simp at hk⟩
      · have : ids.val.length ≠ 0 := by intro h; apply h0; scalar_tac
        simp; omega

end o5s_registry.Helpers
