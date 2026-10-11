import Model
import Commands
/-!
# The extracted kernel computes the model

`transition_refines`: whenever the extracted `transition` returns, its result
is `Model.step` on the same rows, with write vectors read as lists.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec o5s_registry.Schema o5s_registry.Helpers
  o5s_registry.Exact H5iAppLib

namespace o5s_registry.Refine

/-- A result with its write vector read as a list. -/
def lift : core.result.Result (alloc.vec.Vec Write × Reply) Error → Model.Out
  | .Ok (ws, r) => .Ok (ws.val, r)
  | .Err e => .Err e

attribute [local simp] H5iAppLib.u64_val_eq

theorem inc_val {x : U64} (h : x ≠ Model.MAX) : (Model.inc x).val = x.val + 1 := by
  have hlt := Commands.lt_max (show ¬x = core.num.U64.MAX from h)
  have hw : UScalarTy.U64.numBits = 64 := rfl
  show (x.bv + 1).toNat = x.val + 1
  rw [BitVec.toNat_add, U64.bv_toNat]
  have h1 : (1 : BitVec UScalarTy.U64.numBits).toNat = 1 := by rw [hw]; rfl
  rw [h1]
  exact Nat.mod_eq_of_lt (by omega)

theorem inc_of_val {x y : U64} (hne : x ≠ Model.MAX) (h : y.val = x.val + 1) : y = Model.inc x :=
  (H5iAppLib.u64_val_eq _ _).1 (by rw [inc_val hne, h])

/-- `x + 1#u64` computes `inc x`. -/
theorem add_one_eq {x y : U64} (h : x + 1#u64 = ok y) : y = Model.inc x := by
  have hy := H5iAppLib.add_ok_val h
  have hb := H5iAppLib.add_ok_bound h
  have hne : x ≠ Model.MAX := by
    intro e; subst e; have := H5iAppLib.u64_max_val; simp [Model.MAX] at hb; scalar_tac
  apply (H5iAppLib.u64_val_eq _ _).1
  rw [inc_val hne]; simpa using hy

theorem register_refines (a : Principal) (ls : alloc.vec.Vec Listing) (n : alloc.vec.Vec U8) (repo : U64) :
    register a ls n repo ⦃ r => lift r = Model.register a ls.val n repo ⦄ := by
  unfold register Model.register
  by_cases h1 : a.scope = a.org
  · by_cases h2 : a.org.val ≥ REPOSITORY_SCOPE.val
    · have : a.org ≥ REPOSITORY_SCOPE := h2
      simp [h1, this, h2, lift]
    · by_cases h3 : repo.val < REPOSITORY_SCOPE.val
      · have : repo < REPOSITORY_SCOPE := h3
        have : ¬ a.org ≥ REPOSITORY_SCOPE := h2
        simp [h1, *, lift]
      · have : ¬ a.org ≥ REPOSITORY_SCOPE := h2
        have : ¬ repo < REPOSITORY_SCOPE := h3
        simp only [h1, bne_self_eq_false, Bool.false_eq_true, ↓reduceIte, this, h2, h3, ‹¬ a.org ≥ REPOSITORY_SCOPE›]
        step with valid_name_exact as ⟨ b, hb ⟩
        subst hb
        simp only [ne_eq, not_true_eq_false, ↓reduceIte]
        cases hv : validName n.val
        · simp [lift]
        · simp only [↓reduceIte, Bool.not_true, Bool.false_eq_true]
          step as ⟨ o, ho ⟩
          rw [← ho]
          cases o with
          | some l => simp only; split <;> simp [lift]
          | none =>
            simp only
            step as ⟨ u, hu ⟩
            rw [← hu]
            cases u <;> simp only [↓reduceIte, Bool.false_eq_true] <;> step* <;> simp_all [lift]
  · simp [h1, lift]

theorem create_refines (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) :
    create a r n ⦃ res => lift res = Model.create a r n ⦄ := by
  unfold create Model.create event
  by_cases h1 : a.scope.val < REPOSITORY_SCOPE.val
  · have : a.scope < REPOSITORY_SCOPE := h1
    simp [this, h1, lift]
  · have : ¬ a.scope < REPOSITORY_SCOPE := h1
    simp only [this, h1, ↓reduceIte]
    step with valid_name_exact as ⟨ b, hb ⟩
    subst hb
    cases hv : validName n.val
    · simp [lift]
    · simp only [↓reduceIte, Bool.not_true, Bool.false_eq_true]
      cases hc : r.created
      · simp only [Bool.false_eq_true, ↓reduceIte]
        step*
        all_goals (try (have := H5iAppLib.usize_max_ge; simp_all; omega))
        have hm : main3 = Model.mainLabel := alloc.vec.Vec.ext _ _ (by
          simp [main3_post, main2_post, main1_post, main_post, Model.mainLabel])
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]
      · simp only [↓reduceIte]
        by_cases ho : r.org = a.org
        · simp only [ho, ↓reduceIte, true_and]
          step as ⟨ e, he ⟩
          split <;> simp_all [lift]
        · simp [ho, lift]

theorem approve_refines (u : U64) (s : Snapshot) (patch : U64) :
    approve u s patch ⦃ res => lift res = Model.approve u (Snapshot.toSt s) patch ⦄ := by
  unfold approve Model.approve
  step as ⟨ o, ho ⟩
  simp only [Snapshot.toSt, ← ho]
  cases o with
  | none => simp [lift]
  | some p =>
    simp only
    by_cases ha : p.author = u
    · simp [ha, lift]
    · by_cases hm : s.repository.events = core.num.U64.MAX
      · simp [ha, hm, lift, Model.MAX]
      · have hm' : s.repository.events ≠ Model.MAX := hm
        simp only [ha, hm, hm', ↓reduceIte]
        unfold bump_events event
        step*
        all_goals (try (have := Commands.lt_max hm; scalar_tac))
        have hx := inc_of_val hm' x_post
        have hi := inc_of_val hm' i_post
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]

theorem check_refines (u : U64) (s : Snapshot) (patch : U64) (passed : Bool) :
    check u s patch passed ⦃ res => lift res = Model.check u (Snapshot.toSt s) patch passed ⦄ := by
  unfold check Model.check
  step as ⟨ o, ho ⟩
  simp only [Snapshot.toSt, ← ho]
  cases o with
  | none => simp [lift]
  | some p =>
    simp only
    by_cases ha : p.author = u
    · simp [ha, lift]
    · by_cases hm : s.repository.events = core.num.U64.MAX
      · simp [ha, hm, lift, Model.MAX]
      · have hm' : s.repository.events ≠ Model.MAX := hm
        simp only [ha, hm, hm', ↓reduceIte]
        unfold bump_events event
        step*
        all_goals (try (have := Commands.lt_max hm; scalar_tac))
        have hx := inc_of_val hm' x_post
        have hi := inc_of_val hm' i_post
        subst_vars
        simp [lift, ws2_post, ws1_post, ws_post]

theorem push_ok_val {α} {v w : alloc.vec.Vec α} {x : α} (h : v.push x = ok w) : w.val = v.val ++ [x] := by
  unfold alloc.vec.Vec.push at h
  dsimp only at h
  split at h
  · simp only [ok.injEq] at h; subst h; simp
  · simp at h

/-- The members a fork copies, in table order. -/
def copied (ms : List Member) (src id : U64) : List Write :=
  (ms.filter (fun m => decide (m.channel = src))).map (fun m => .PutMember { m with channel := id })

theorem fork_loop_ok (v : alloc.vec.Vec Member) (src id : U64) (ws : alloc.vec.Vec Write) (k : Usize)
    (r : alloc.vec.Vec Write) (h : fork_loop v src id ws k = ok r) (hk : k.val ≤ v.val.length) :
    r.val = ws.val ++ copied (v.val.drop k.val) src id := by
  unfold fork_loop at h
  refine H5iAppLib.loop_ok _ (fun x => x.2.val ≤ v.val.length ∧
      ws.val ++ copied (v.val.drop k.val) src id = x.1.val ++ copied (v.val.drop x.2.val) src id)
    (fun r' => r'.val = ws.val ++ copied (v.val.drop k.val) src id) (fun x => v.val.length - x.2.val) ?_ (ws, k) r
    ⟨hk, rfl⟩ h
  rintro ⟨acc, j⟩ c ⟨hj, heq⟩ hc
  unfold fork_loop.body at hc
  dsimp only at hc
  split at hc
  · rename_i hlt
    have hj' : j.val < v.val.length := by scalar_tac
    obtain ⟨m, hm, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨acc', hacc, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨j', hj'', hc⟩ := bind_eq_ok.1 hc
    simp only [ok.injEq] at hc
    subst hc
    have hmv : m = v.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hm; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have hjv := H5iAppLib.add_ok_val hj''
    have hjv' : j'.val = j.val + 1 := by simpa using hjv
    refine ⟨⟨by simp only; omega, ?_⟩, by simp only; omega⟩
    simp only
    rw [heq, List.drop_eq_getElem_cons hj', hjv']
    split at hacc
    · rename_i hch
      rw [push_ok_val hacc, hmv]
      rw [hmv] at hch
      have hf : decide (v.val[j.val].channel = src) = true := by simpa using hch
      simp only [copied, List.filter_cons, hf, ↓reduceIte, List.map_cons, List.append_assoc, List.cons_append,
        List.nil_append]
    · rename_i hch
      simp only [ok.injEq] at hacc; subst hacc
      rw [hmv] at hch
      have hf : decide (v.val[j.val].channel = src) = false := by simpa using hch
      simp only [copied, List.filter_cons, hf, Bool.false_eq_true, ↓reduceIte]
  · simp only [ok.injEq] at hc; subst hc
    simp only
    have : v.val.length ≤ j.val := by scalar_tac
    rw [heq, List.drop_eq_nil_of_le this]; simp [copied]

theorem fork_refines (u : U64) (s : Snapshot) (src n : alloc.vec.Vec U8) (r)
    (h : fork u s src n = ok r) : lift r = Model.fork u (Snapshot.toSt s) src n := by
  unfold fork at h
  h5i_invert h
  all_goals have hb' := post_of_ok (valid_name_exact n) hb
  all_goals (try have ho' := post_of_ok (find_channel_spec _ _) ho)
  all_goals (try have ho1' := post_of_ok (find_channel_spec _ _) ho1)
  case isFalse.isFalse =>
    have e1 : id = Model.inc s.repository.channels := add_one_eq hid
    have e2 : i = Model.inc s.repository.events := add_one_eq hi
    have e3 : v = n := by simpa [H5iAppLib.u8vec_clone] using hv.symm
    have e4 : v1 = s.repository.label := by simpa [H5iAppLib.u8vec_clone] using hv1.symm
    unfold event at he
    simp only [ok.injEq] at he
    have w3 := push_ok_val hws3
    have w2 := push_ok_val hws2
    have w1 := fork_loop_ok _ _ _ _ _ _ hws1 (by simp)
    have w0 := push_ok_val hws
    simp only [Model.fork, Snapshot.toSt, Model.findChannel, lift]
    rw [← hb', ← ho', ← ho1']
    simp only [hc, Bool.not_true, Bool.false_eq_true, ↓reduceIte, Model.MAX, hc_1, hc_2]
    subst_vars
    simp [w3, w2, w1, w0, copied]
  all_goals simp only [Model.fork, Model.findChannel, Snapshot.toSt, lift]
  all_goals rw [← hb']
  all_goals (try rw [← ho'])
  all_goals (try rw [← ho1'])
  all_goals simp_all [Model.MAX]

theorem propose_loop0_ok (deps : alloc.vec.Vec U64) (id : U64) (ws : alloc.vec.Vec Write) (k : Usize)
    (r : alloc.vec.Vec Write) (h : propose_loop0 deps id ws k = ok r) (hk : k.val ≤ deps.val.length) :
    r.val = ws.val ++ (deps.val.drop k.val).map (fun d => .PutDependency ⟨id, d⟩) := by
  unfold propose_loop0 at h
  refine H5iAppLib.loop_ok _ (fun x => x.2.val ≤ deps.val.length ∧
      ws.val ++ (deps.val.drop k.val).map (fun d => .PutDependency ⟨id, d⟩) =
        x.1.val ++ (deps.val.drop x.2.val).map (fun d => .PutDependency ⟨id, d⟩))
    (fun r' => r'.val = ws.val ++ (deps.val.drop k.val).map (fun d => .PutDependency ⟨id, d⟩))
    (fun x => deps.val.length - x.2.val) ?_ (ws, k) r ⟨hk, rfl⟩ h
  rintro ⟨acc, j⟩ c ⟨hj, heq⟩ hc
  unfold propose_loop0.body at hc
  dsimp only at hc
  split at hc
  · have hj' : j.val < deps.val.length := by scalar_tac
    obtain ⟨d, hd, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨acc', hacc, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨j', hj'', hc⟩ := bind_eq_ok.1 hc
    simp only [ok.injEq] at hc
    subst hc
    have hdv : d = deps.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hd; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have hjv' : j'.val = j.val + 1 := by simpa using H5iAppLib.add_ok_val hj''
    refine ⟨⟨by simp only; omega, ?_⟩, by simp only; omega⟩
    simp only
    rw [heq, List.drop_eq_getElem_cons hj', hjv', push_ok_val hacc, hdv]
    simp only [List.map_cons, List.append_assoc, List.cons_append, List.nil_append]
  · simp only [ok.injEq] at hc; subst hc
    simp only
    have : deps.val.length ≤ j.val := by scalar_tac
    rw [heq, List.drop_eq_nil_of_le this]; simp

theorem propose_loop1_ok (added : alloc.vec.Vec PathKind) (ws : alloc.vec.Vec Write) (k : Usize)
    (r : alloc.vec.Vec Write) (h : propose_loop1 added ws k = ok r) (hk : k.val ≤ added.val.length) :
    r.val = ws.val ++ (added.val.drop k.val).map .PutPath := by
  unfold propose_loop1 at h
  refine H5iAppLib.loop_ok _ (fun x => x.2.val ≤ added.val.length ∧
      ws.val ++ (added.val.drop k.val).map .PutPath = x.1.val ++ (added.val.drop x.2.val).map .PutPath)
    (fun r' => r'.val = ws.val ++ (added.val.drop k.val).map .PutPath)
    (fun x => added.val.length - x.2.val) ?_ (ws, k) r ⟨hk, rfl⟩ h
  rintro ⟨acc, j⟩ c ⟨hj, heq⟩ hc
  unfold propose_loop1.body at hc
  dsimp only at hc
  split at hc
  · have hj' : j.val < added.val.length := by scalar_tac
    obtain ⟨d, hd, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨d', hd', hc⟩ := bind_eq_ok.1 hc
    obtain ⟨acc', hacc, hc⟩ := bind_eq_ok.1 hc
    obtain ⟨j', hj'', hc⟩ := bind_eq_ok.1 hc
    simp only [ok.injEq] at hc
    subst hc
    have hdv : d = added.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hd; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have hdd : d' = d := by rw [PathKind.clone_eq] at hd'; simpa using hd'.symm
    have hjv' : j'.val = j.val + 1 := by simpa using H5iAppLib.add_ok_val hj''
    refine ⟨⟨by simp only; omega, ?_⟩, by simp only; omega⟩
    simp only
    rw [heq, List.drop_eq_getElem_cons hj', hjv', push_ok_val hacc, hdd, hdv]
    simp only [List.map_cons, List.append_assoc, List.cons_append, List.nil_append]
  · simp only [ok.injEq] at hc; subst hc
    simp only
    have : added.val.length ≤ j.val := by scalar_tac
    rw [heq, List.drop_eq_nil_of_le this]; simp

def liftP : core.result.Result (alloc.vec.Vec PathKind × U64) Error → core.result.Result (List PathKind × U64) Error
  | .Ok (a, n) => .Ok (a.val, n)
  | .Err e => .Err e

theorem register_paths_loop_ok (known : alloc.vec.Vec PathKind) (uses : alloc.vec.Vec PathUse)
    (added : alloc.vec.Vec PathKind) (next : U64) (k : Usize) (r)
    (h : register_paths_loop known uses added next k = ok r) (hk : k.val ≤ uses.val.length) :
    liftP r = Model.registerPaths known.val (uses.val.drop k.val) added.val next := by
  unfold register_paths_loop at h
  refine H5iAppLib.loop_ok _ (fun x => x.2.2.val ≤ uses.val.length ∧
      Model.registerPaths known.val (uses.val.drop k.val) added.val next =
        Model.registerPaths known.val (uses.val.drop x.2.2.val) x.1.val x.2.1)
    (fun r' => liftP r' = Model.registerPaths known.val (uses.val.drop k.val) added.val next)
    (fun x => uses.val.length - x.2.2.val) ?_ (added, next, k) r ⟨hk, rfl⟩ h
  rintro ⟨acc, nx, j⟩ c ⟨hj, heq⟩ hc
  simp only at hj heq
  unfold register_paths_loop.body at hc
  h5i_invert hc
  all_goals simp only
  case h_1.isTrue =>
    have hj' : j.val < uses.val.length := by scalar_tac
    have huv : u = uses.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hu; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have ho' := post_of_ok (find_path_spec _ _) ho
    rw [heq, List.drop_eq_getElem_cons hj', ← huv]
    simp only [Model.registerPaths]
    rw [← ho']
    cases o with
    | none =>
      dsimp only at hexisting
      have hx := post_of_ok (find_path_spec _ _) hexisting
      simp only [← hx, Model.MAX, hc_2, ↓reduceIte, liftP]
    | some kk => simp at hexisting
  case h_1.isFalse =>
    have hj' : j.val < uses.val.length := by scalar_tac
    have huv : u = uses.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hu; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have ho' := post_of_ok (find_path_spec _ _) ho
    have hn : nx ≠ Model.MAX := hc_2
    have e1 : next1 = Model.inc nx := add_one_eq hnext1
    have e2 : v = u.path := by simpa [H5iAppLib.u8vec_clone] using hv.symm
    have hi : i2.val = j.val + 1 := by simpa using H5iAppLib.add_ok_val hi2
    refine ⟨⟨by omega, ?_⟩, by omega⟩
    rw [heq, List.drop_eq_getElem_cons hj', ← huv, hi, push_ok_val hadded1, e1, e2]
    simp only [Model.registerPaths]
    rw [← ho']
    cases o with
    | none =>
      dsimp only at hexisting
      have hx := post_of_ok (find_path_spec _ _) hexisting
      simp only [← hx, hn, ↓reduceIte]
    | some kk => simp at hexisting
  case h_2.isTrue.isTrue =>
    have hj' : j.val < uses.val.length := by scalar_tac
    have huv : u = uses.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hu; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have ho' := post_of_ok (find_path_spec _ _) ho
    rw [heq, List.drop_eq_getElem_cons hj', ← huv]
    simp only [Model.registerPaths]
    rw [← ho']
    have hne : x.graph ≠ u.graph := by simpa using hc_2
    cases o with
    | none =>
      dsimp only at hexisting
      have hx := post_of_ok (find_path_spec _ _) hexisting
      rw [← hx]; simp_all [liftP]
    | some kk =>
      simp only [ok.injEq, Option.some.injEq] at hexisting
      subst hexisting
      simp_all [liftP]
  case h_2.isTrue.isFalse =>
    have hj' : j.val < uses.val.length := by scalar_tac
    have huv : u = uses.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hu; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have ho' := post_of_ok (find_path_spec _ _) ho
    rw [heq, List.drop_eq_getElem_cons hj', ← huv]
    simp only [Model.registerPaths]
    rw [← ho']
    have hne : x.graph ≠ u.graph := by simpa using hc_2
    cases o with
    | none =>
      dsimp only at hexisting
      have hx := post_of_ok (find_path_spec _ _) hexisting
      rw [← hx]; simp_all [liftP]
    | some kk =>
      simp only [ok.injEq, Option.some.injEq] at hexisting
      subst hexisting
      simp_all [liftP]
  case h_2.isFalse =>
    have hj' : j.val < uses.val.length := by scalar_tac
    have huv : u = uses.val[j.val] := by
      have := H5iAppLib.vec_index_ok_get? hu; simpa [List.getElem?_eq_getElem hj'] using this.symm
    have ho' := post_of_ok (find_path_spec _ _) ho
    have hi : i2.val = j.val + 1 := by simpa using H5iAppLib.add_ok_val hi2
    have hsame : x.graph = u.graph := by simpa using hc_2
    refine ⟨⟨by omega, ?_⟩, by omega⟩
    rw [heq, List.drop_eq_getElem_cons hj', ← huv, hi]
    simp only [Model.registerPaths]
    rw [← ho']
    cases o with
    | none =>
      dsimp only at hexisting
      have hx := post_of_ok (find_path_spec _ _) hexisting
      simp only [← hx, hsame, ne_eq, not_true_eq_false, ↓reduceIte]
    | some kk =>
      simp only [ok.injEq, Option.some.injEq] at hexisting
      subst hexisting
      simp only [hsame, ne_eq, not_true_eq_false, ↓reduceIte]
  case isFalse =>
    have : uses.val.length ≤ j.val := by scalar_tac
    rw [heq, List.drop_eq_nil_of_le this]
    simp [Model.registerPaths, liftP]

end o5s_registry.Refine
