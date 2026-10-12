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

set_option maxHeartbeats 1000000 in
theorem propose_refines (u : U64) (s : Snapshot) (h : alloc.vec.Vec U8) (deps : alloc.vec.Vec U64)
    (uses : alloc.vec.Vec PathUse) (body : alloc.vec.Vec U8) (r)
    (hr : propose u s h deps uses body = ok r) :
    lift r = Model.propose u (Snapshot.toSt s) h deps.val uses.val body := by
  unfold propose at hr
  h5i_invert hr
  all_goals (try (obtain ⟨added, paths⟩ := x; simp at hr; h5i_invert hr))
  all_goals have hb' := post_of_ok (valid_hash_exact h) hb
  all_goals (try have hb1' := post_of_ok (all_known_spec _ _) hb1)
  all_goals (try have ho' := post_of_ok (find_hash_spec _ _) ho)
  all_goals (try have hrp : liftP _ = Model.registerPaths s.paths.val uses.val [] s.repository.paths := (by simpa using register_paths_loop_ok _ _ _ _ _ _ hr_1 (by simp)))
  case h_1.isFalse.isFalse =>
    have hn1 : s.repository.patches ≠ Model.MAX := hc_2
    have hn2 : s.repository.events ≠ Model.MAX := hc_3
    have e1 : id = Model.inc s.repository.patches := add_one_eq hid
    have e2 : i = Model.inc s.repository.events := add_one_eq hi
    have e3 : v = h := by simpa [H5iAppLib.u8vec_clone] using hv.symm
    have e4 : v1 = body := by simpa [H5iAppLib.u8vec_clone] using hv1.symm
    have e5 : v2 = s.repository.label := by simpa [H5iAppLib.u8vec_clone] using hv2.symm
    unfold event at he
    simp only [ok.injEq] at he
    have w5 := push_ok_val hws5
    have w4 := push_ok_val hws4
    have w3 := propose_loop1_ok _ _ _ _ hws3 (by simp)
    have w2 := propose_loop0_ok _ _ _ _ _ hws2 (by simp)
    have w1 := push_ok_val hws1
    have w0 := push_ok_val hws
    simp only [Model.propose, Snapshot.toSt, lift]
    rw [← hb', ← hb1', ← ho', ← hrp]
    simp only [hc, hc_1, Bool.not_true, Bool.false_eq_true, ↓reduceIte, liftP, Model.MAX, hc_2, hc_3]
    subst_vars
    rw [w5, w4, w3, w2, w1, w0]
    have h0 : (0#usize : Usize).val = 0 := rfl
    simp only [h0, List.drop_zero, H5iAppLib.vec_new_val, List.nil_append, List.append_assoc, List.cons_append,
      List.singleton_append]
  all_goals simp only [Model.propose, Snapshot.toSt, lift]
  · rw [← hb', ← hb1', ← ho', ← hrp]; simp [hc, hc_1, liftP, Model.MAX, hc_2]
  · rw [← hb', ← hb1', ← ho', ← hrp]; simp [hc, hc_1, liftP, Model.MAX, hc_2, hc_3]
  · rw [← hb', ← hb1', ← ho', ← hrp]; simp [hc, hc_1, liftP]
  · rw [← hb', ← hb1', ← ho']; simp [hc, hc_1]
  · rw [← hb', ← hb1']; simp [hc, hc_1]
  · rw [← hb']; simp [hc]

/-! ## Admission -/

theorem insert_ok (v w : alloc.vec.Vec U64) (x : U64)
    (h : h5i_app_std.set.insert core.cmp.PartialEqU64 core.clone.CloneU64 v x = ok w) :
    w.val = if x ∈ v.val then v.val else v.val ++ [x] := by
  unfold h5i_app_std.set.insert at h
  obtain ⟨out, hout, h⟩ := bind_eq_ok.1 h
  obtain ⟨b, hb, h⟩ := bind_eq_ok.1 h
  have ho : out = v := by
    rw [vec_clone_eq core.clone.CloneU64 v (fun _ => rfl)] at hout; simpa using hout.symm
  have hb' := post_of_ok (h5i_app_std.Specs.set.contains_spec core.cmp.PartialEqU64 _ _) hb
  split at h
  · simp only [ok.injEq] at h; subst h ho; simp_all
  · obtain ⟨t, ht, h⟩ := bind_eq_ok.1 h
    simp only [core.clone.CloneU64, ok.injEq] at ht
    rw [push_ok_val h, ← ht, ho]; subst ho; simp_all

def liftK : core.result.Result (alloc.vec.Vec U64) o5s_kernel.patch.MergeError →
    core.result.Result (List U64) o5s_kernel.patch.MergeError
  | .Ok v => .Ok v.val
  | .Err e => .Err e

theorem patch_merge_ok (ch deps : alloc.vec.Vec U64) (a c : Bool) (id : U64) (r)
    (h : o5s_kernel.patch.merge ch deps a c id = ok r) : liftK r = Model.admitK ch.val deps.val a c id := by
  unfold o5s_kernel.patch.merge at h
  h5i_invert h
  all_goals (try have hb' := post_of_ok (h5i_app_std.Specs.set.subset_spec core.cmp.PartialEqU64 _ _) hb)
  · have hsub : (deps.val.all fun d => decide (d ∈ ch.val)) = true := by
      simp only [List.all_eq_true, decide_eq_true_eq]; intro d hd; subst hc_2; simp at hb'; exact hb' hd
    simp [Model.admitK, liftK, hc, hc_1, hsub, insert_ok _ _ _ hv]
  · have hsub : (deps.val.all fun d => decide (d ∈ ch.val)) = false := by
      rw [hb'] at hc_2; simp only [List.all_eq_false, decide_eq_false_iff_not]
      simp only [decide_eq_true_eq, List.subset_def, not_forall] at hc_2
      obtain ⟨d, hd, hn⟩ := hc_2; exact ⟨d, hd, by simpa using hn⟩
    simp [Model.admitK, liftK, hc, hc_1, hsub]
  · simp [Model.admitK, liftK, hc, hc_1]
  · simp [Model.admitK, liftK, hc]

def liftAdm (st : Admission) : Model.Adm := ⟨st.members.val, st.size, st.seq, st.admitted, st.writes.val⟩

theorem admit_one_ok (u : U64) (s : Snapshot) (ch id : U64) (st : Admission) (r)
    (h : admit_one u s ch id st = ok r) :
    (r.1, liftAdm r.2) = Model.admitOne u (Snapshot.toSt s) ch id (liftAdm st) := by
  unfold admit_one at h
  h5i_invert h
  all_goals have ho' := post_of_ok (find_patch_spec _ _) ho
  all_goals simp only [Model.admitOne, Snapshot.toSt, liftAdm]
  all_goals rw [← ho']
  all_goals (try (
    have hp' := post_of_ok (h5i_app_std.Specs.set.contains_spec core.cmp.PartialEqU64 _ _) hpresent
    have hd' := post_of_ok (dependencies_of_spec _ _) hdeps
    have hb' := post_of_ok (approved_spec _ _) hb
    have hb1' := post_of_ok (checks_passed_spec _ _) hb1
    have hk := patch_merge_ok _ _ _ _ _ _ hr_1
    rw [hd', hb', hb1'] at hk
    simp only [Model.depsOf, Model.approvedB, Model.passedB]
    rw [← hk]
    simp only [liftK]))
  all_goals first
    | (unfold merge_error at he1
       cases e <;> simp only [ok.injEq] at he1 <;> subst he1 <;> rfl)
    | (have hp' := post_of_ok (h5i_app_std.Specs.set.contains_spec core.cmp.PartialEqU64 _ _) hpresent
       (try have e1 := add_one_eq hi)
       (try have e2 := add_one_eq hi1)
       (try have e3 := add_one_eq hi2)
       (try have e3 := add_one_eq hi)
       (try (unfold event at he; simp only [ok.injEq] at he))
       (try have w1 := push_ok_val hv1)
       (try have w0 := push_ok_val hv)
       subst_vars
       simp_all [Model.MAX])

theorem merge_loop_ok (user : U64) (r : Repository) (v : alloc.vec.Vec PatchRow) (v1 : alloc.vec.Vec Dependency)
    (v2 : alloc.vec.Vec Approval) (v3 : alloc.vec.Vec CheckReport) (v4 : alloc.vec.Vec Channel)
    (v5 : alloc.vec.Vec Member) (v6 : alloc.vec.Vec PathKind) (v7 : alloc.vec.Vec Event) (v8 : alloc.vec.Vec Body)
    (v9 : alloc.vec.Vec Listing) (ids : alloc.vec.Vec U64) (ch : U64) (st : Admission) (failure : Option Error)
    (k : Usize) (res) (h : merge_loop user r v v1 v2 v3 v4 v5 v6 v7 v8 v9 ids ch st failure k = ok res)
    (hk : k.val ≤ ids.val.length) :
    (liftAdm res.1, res.2) = (ids.val.drop k.val).foldl
      (Model.admitStep user (Snapshot.toSt ⟨r, v, v1, v2, v3, v4, v5, v6, v7, v8, v9⟩) ch) (liftAdm st, failure) := by
  unfold merge_loop at h
  refine H5iAppLib.loop_ok _ (fun x => x.2.2.val ≤ ids.val.length ∧
      (ids.val.drop k.val).foldl (Model.admitStep user (Snapshot.toSt ⟨r, v, v1, v2, v3, v4, v5, v6, v7, v8, v9⟩) ch)
          (liftAdm st, failure) =
        (ids.val.drop x.2.2.val).foldl (Model.admitStep user (Snapshot.toSt ⟨r, v, v1, v2, v3, v4, v5, v6, v7, v8, v9⟩) ch)
          (liftAdm x.1, x.2.1))
    (fun res' => (liftAdm res'.1, res'.2) = (ids.val.drop k.val).foldl
      (Model.admitStep user (Snapshot.toSt ⟨r, v, v1, v2, v3, v4, v5, v6, v7, v8, v9⟩) ch) (liftAdm st, failure))
    (fun x => ids.val.length - x.2.2.val) ?_ (st, failure, k) res ⟨hk, rfl⟩ h
  rintro ⟨a, f, j⟩ c ⟨hj, heq⟩ hc
  simp only at hj heq
  unfold merge_loop.body at hc
  h5i_invert hc
  · obtain ⟨st1, f1⟩ := x
    simp at hc
    obtain ⟨i3, hi3, hc⟩ := bind_eq_ok.1 hc
    simp only [ok.injEq] at hc
    subst hc
    simp only
    have hj' : j.val < ids.val.length := by scalar_tac
    have hi : i3.val = j.val + 1 := by simpa using H5iAppLib.add_ok_val hi3
    refine ⟨⟨by omega, ?_⟩, by omega⟩
    rw [heq, List.drop_eq_getElem_cons hj', List.foldl_cons, hi]
    congr 1
    cases f with
    | none =>
      simp only [core.option.Option.is_none, Option.isNone_none, ↓reduceIte] at hx
      obtain ⟨id, hid, hx⟩ := bind_eq_ok.1 hx
      obtain ⟨p, hp, hx⟩ := bind_eq_ok.1 hx
      obtain ⟨f2, st2⟩ := p
      simp at hx
      obtain ⟨rfl, rfl⟩ := hx
      have hidv : id = ids.val[j.val] := by
        have := H5iAppLib.vec_index_ok_get? hid; simpa [List.getElem?_eq_getElem hj'] using this.symm
      have ha := admit_one_ok _ _ _ _ _ _ hp
      subst hidv
      simp only [Model.admitStep]
      rw [← ha]
    | some e =>
      simp only [core.option.Option.is_none, Option.isNone_some, Bool.false_eq_true, ↓reduceIte, Result.ok.injEq,
        Prod.mk.injEq] at hx
      obtain ⟨rfl, rfl⟩ := hx
      simp [Model.admitStep]
  · simp only
    have : ids.val.length ≤ j.val := by scalar_tac
    rw [heq, List.drop_eq_nil_of_le this]; rfl

theorem merge_refines (u : U64) (s : Snapshot) (n : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64) (r)
    (h : merge u s n ids = ok r) : lift r = Model.merge u (Snapshot.toSt s) n ids.val := by
  unfold merge at h
  h5i_invert h
  all_goals have hb' := post_of_ok (valid_batch_exact ids) hb
  all_goals (try have ho' := post_of_ok (find_channel_spec _ _) ho)
  case h_2 =>
    obtain ⟨st, f⟩ := x_1
    have hl := merge_loop_ok _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ _ hx_1 (by simp)
    have hv' := post_of_ok (members_of_spec _ _) hv
    have hs : Snapshot.toSt ⟨s.repository, s.patches, s.dependencies, s.approvals, s.checks, s.channels, s.members,
        s.paths, s.events, s.bodies, s.listings⟩ = Snapshot.toSt s := rfl
    rw [hs] at hl
    simp only [Model.merge, Snapshot.toSt, Model.findChannel]
    rw [← hb', ← ho']
    simp only [hc, Bool.not_true, Bool.false_eq_true, ↓reduceIte]
    have h0 : (0#usize : Usize).val = 0 := rfl
    simp only [h0, List.drop_zero, liftAdm, hv', H5iAppLib.vec_new_val] at hl
    simp only [Snapshot.toSt] at hl
    rw [← hl]
    simp at h
    cases f with
    | none =>
      simp only at h ⊢
      h5i_invert h
      have e1 : v1 = x.label := by simpa [H5iAppLib.u8vec_clone] using hv1.symm
      have e2 : v2 = s.repository.label := by simpa [H5iAppLib.u8vec_clone] using hv2.symm
      subst e1 e2
      simp [lift, push_ok_val hws1, push_ok_val hws]
    | some e =>
      simp only at h ⊢
      simp only [Result.ok.injEq] at h
      subst h
      rfl
  all_goals simp only [Model.merge, Snapshot.toSt, Model.findChannel, lift]
  all_goals rw [← hb']
  all_goals (try rw [← ho'])
  all_goals simp_all

theorem visible_eq (a : Principal) (r : Repository) (n : alloc.vec.Vec U8) (b) (h : visible a r n = ok b) :
    b = Model.visible a r n := by
  have := post_of_ok (visible_spec a r n) h
  rw [this, Model.visible]

theorem level_loop_spec (roles : alloc.vec.Vec Role) (u : U64) :
    level_loop roles u 0#usize ⦃ l =>
      l = ((roles.val.find? (fun r => decide (r.user = u))).map (·.level)).getD 0#u64 ⦄ := by
  unfold level_loop
  apply WP.spec_mono (loop_search roles.val (fun r => decide (r.user = u)) (fun l : U64 => l)
    (fun _ r => r.level) 0#u64 _ ?_ 0#usize (by simp))
  · intro r hr; rw [hr, searchFrom_findD]; simp
  · intro j hj; unfold level_loop.body; h5i_step

theorem level_eq (roles : alloc.vec.Vec Role) (u : U64) (l) (h : level roles u = ok l) :
    l = Model.level roles.val u := by
  unfold level at h
  by_cases h0 : roles.val = []
  · have : alloc.vec.Vec.len roles = 0#usize := by
      apply UScalar.eq_of_val_eq; simp [alloc.vec.Vec.len, h0]
    simp only [this, ↓reduceIte, ok.injEq] at h
    simp [Model.level, h0, h]
  · have hl : ¬ alloc.vec.Vec.len roles = 0#usize := by
      intro e
      have := congrArg (·.val) e
      simp [alloc.vec.Vec.len] at this
      exact h0 this
    simp only [hl, ↓reduceIte] at h
    have := post_of_ok (level_loop_spec roles u) h
    simp [Model.level, h0, this]

theorem readable_eq (a : Principal) (s : Snapshot) (n : alloc.vec.Vec U8) (b) (h : readable a s n = ok b) :
    b = Model.readable a (Snapshot.toSt s) n := by
  unfold readable at h
  obtain ⟨v, hv, h⟩ := bind_eq_ok.1 h
  have hv' := visible_eq _ _ _ _ hv
  unfold Model.readable
  simp only [Snapshot.toSt] at hv' ⊢
  rw [← hv']
  split at h
  · obtain ⟨l, hl, h⟩ := bind_eq_ok.1 h
    have := level_eq _ _ _ hl
    simp only [ok.injEq] at h
    subst h this
    simp_all
  · simp only [ok.injEq] at h; subst h; simp_all

theorem authorize_eq (a : Principal) (s : Snapshot) (n : alloc.vec.Vec U8) (need : U64) (o)
    (h : authorize a s n need = ok o) : o = Model.authorize a (Snapshot.toSt s) n need := by
  unfold authorize at h
  obtain ⟨b, hb, h⟩ := bind_eq_ok.1 h
  have hb' := readable_eq _ _ _ _ hb
  unfold Model.authorize
  rw [← hb']
  split at h
  · obtain ⟨l, hl, h⟩ := bind_eq_ok.1 h
    have := level_eq _ _ _ hl
    simp only [Snapshot.toSt] at this ⊢
    subst this
    rename_i hbt
    simp only [hbt, Bool.not_true, Bool.false_eq_true, ↓reduceIte]
    split at h <;> simp only [ok.injEq] at h <;> subst h
    · rename_i hlt; simp [hlt]
    · rename_i hlt; simp [hlt]
  · rename_i hbt
    simp only [ok.injEq] at h; subst h
    simp [hbt]

theorem len_zero_iff (v : alloc.vec.Vec Role) : alloc.vec.Vec.len v = 0#usize ↔ v.val = [] := by
  constructor
  · intro e
    have := congrArg (·.val) e
    simpa [alloc.vec.Vec.len] using this
  · intro e
    apply UScalar.eq_of_val_eq
    simp [alloc.vec.Vec.len, e]

theorem grant_spec (a : Principal) (s : Snapshot) (u lvl : U64) :
    grant a s u lvl ⦃ res => lift res = Model.grant a (Snapshot.toSt s) u lvl ⦄ := by
  unfold grant Model.grant
  by_cases h1 : lvl > ADMIN
  · have : ADMIN.val < lvl.val := h1
    simp [h1, this, lift]
  · have h1' : ¬ ADMIN.val < lvl.val := h1
    by_cases hm : s.repository.events = core.num.U64.MAX
    · simp [h1, h1', hm, lift, Model.MAX]
    · have hm' : s.repository.events ≠ Model.MAX := hm
      have hlt := Commands.lt_max hm
      simp only [h1, h1', hm, hm', ↓reduceIte, Snapshot.toSt]
      by_cases hu : u = a.user
      · subst hu
        by_cases h0 : s.roles.val = []
        · have hl := (len_zero_iff s.roles).2 h0
          by_cases hA : lvl = ADMIN
          · subst hA
            simp only [hl, h0, ↓reduceIte, ne_eq, not_true_eq_false, or_self, and_false, bne_self_eq_false,
              Bool.false_eq_true]
            unfold bump_events event
            step*
            all_goals (try scalar_tac)
            have hx := inc_of_val hm' x_post
            have hi := inc_of_val hm' i_post
            subst_vars
            simp [lift, push_ok_val, *]
          · simp [hl, h0, hA, lift]
        · have hl : ¬ alloc.vec.Vec.len s.roles = 0#usize := fun e => h0 ((len_zero_iff s.roles).1 e)
          simp [hl, h0, lift]
      · simp only [hu, ↓reduceIte, false_and, ne_eq, not_false_eq_true, bne_iff_ne]
        unfold bump_events event
        by_cases h0 : s.roles.val = []
        · have hl := (len_zero_iff s.roles).2 h0
          simp only [hl, h0, ↓reduceIte]
          step*
          all_goals (try scalar_tac)
          have hx := inc_of_val hm' x_post
          have hi := inc_of_val hm' i_post
          subst_vars
          simp [lift, push_ok_val, *]
        · have hl : ¬ alloc.vec.Vec.len s.roles = 0#usize := fun e => h0 ((len_zero_iff s.roles).1 e)
          simp only [hl, h0, ↓reduceIte]
          step*
          all_goals (try scalar_tac)
          have hx := inc_of_val hm' x_post
          have hi := inc_of_val hm' i_post
          subst_vars
          simp [lift, push_ok_val, *]

theorem grant_refines (a : Principal) (s : Snapshot) (u lvl : U64) (r)
    (h : grant a s u lvl = ok r) : lift r = Model.grant a (Snapshot.toSt s) u lvl :=
  post_of_ok (grant_spec a s u lvl) h

/-- **Refinement.** Whenever the extracted `transition` returns, its result is
`Model.step` on the same rows. -/
theorem transition_refines (a : Principal) (s : Snapshot) (c : Command) (r)
    (h : transition a s c = ok r) : lift r = Model.step a (Snapshot.toSt s) c := by
  cases c <;> simp only [transition] at h <;> simp only [Model.step]
  case Register n repo => have := post_of_ok (register_refines a s.listings n repo) h; exact this
  case Create n => have := post_of_ok (create_refines a s.repository n) h; exact this
  all_goals first
    | (simp only [Result.ok.injEq] at h; subst h; rfl)
    | (obtain ⟨o, ho, h⟩ := bind_eq_ok.1 h
       have hv := authorize_eq _ _ _ _ _ ho
       simp only [Model.guarded]
       rw [← hv]
       cases o with
       | some e => simp only [Result.ok.injEq] at h; subst h; rfl
       | none =>
         simp only
         first
           | exact fork_refines _ _ _ _ _ h
           | exact propose_refines _ _ _ _ _ _ _ h
           | (have := post_of_ok (approve_refines _ _ _) h; exact this)
           | (have := post_of_ok (check_refines _ _ _ _) h; exact this)
           | exact merge_refines _ _ _ _ _ h
           | exact grant_refines _ _ _ _ _ h)

end o5s_registry.Refine
