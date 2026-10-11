import Refine
/-!
# What the registry guarantees

Each theorem is about the extracted `transition`: `Refine.transition_refines`
turns a successful run into a fact about `Model.step`, and the proofs reason
about the model.
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec H5iAppLib

namespace o5s_registry.Guarantees

/-- `transition` succeeds with writes `ws`. -/
def Succeeds (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write) : Prop :=
  ∃ r, transition a s c = ok (.Ok (ws, r))

theorem model_of {a : Principal} {s : Snapshot} {c : Command} {ws : alloc.vec.Vec Write} (h : Succeeds a s c ws) :
    ∃ r, Model.step a (Snapshot.toSt s) c = .Ok (ws.val, r) := by
  obtain ⟨r, hr⟩ := h
  exact ⟨r, (Refine.transition_refines a s c _ hr).symm⟩

/-- A repository command succeeds only on the caller's own created repository. -/
theorem isolation (a : Principal) (s : Snapshot) (c : Command) (ws : alloc.vec.Vec Write)
    (h : Succeeds a s c ws) (hc : ∀ n r, c ≠ .Register n r) (hc' : ∀ n, c ≠ .Create n) :
    s.repository.created = true ∧ s.repository.org = a.org := by
  obtain ⟨r, hm⟩ := model_of h
  cases c <;> simp only [Model.step, Snapshot.toSt] at hm
  all_goals first
    | exact absurd rfl (hc _ _)
    | exact absurd rfl (hc' _)
    | (split at hm
       · rename_i hv
         simp only [Model.visible, Bool.and_eq_true, decide_eq_true_eq] at hv
         exact ⟨hv.1.1.2, hv.1.2⟩
       · simp at hm)
    | simp at hm

/-! ## Admission -/

/-- What an admission batch writes: members of approved, passing patches, and events. -/
def AdmWrites (s : St) (ws : List Write) : Prop :=
  ∀ w ∈ ws, (∃ m, w = .PutMember m ∧ Model.approvedB s m.patch = true ∧ Model.passedB s m.patch = true) ∨
    ∃ e, w = .PutEvent e

theorem admitK_ok {ch deps : List U64} {a c : Bool} {id : U64} {next}
    (h : Model.admitK ch deps a c id = .Ok next) : a = true ∧ c = true := by
  unfold Model.admitK at h
  split_ifs at h <;> simp_all

theorem admitOne_writes (u : U64) (s : St) (ch id : U64) (st : Model.Adm) (h : AdmWrites s st.writes) :
    AdmWrites s (Model.admitOne u s ch id st).2.writes := by
  unfold Model.admitOne
  split
  · exact h
  · split
    · exact h
    · rename_i next hk
      obtain ⟨ha, hc⟩ := admitK_ok hk
      intro w hw
      split_ifs at hw <;> simp only [List.mem_append, List.mem_singleton] at hw <;>
        first
          | exact h w hw
          | (rcases hw with hw | rfl
             · exact h w hw
             · exact .inr ⟨_, rfl⟩)
          | (rcases hw with (hw | rfl) | rfl
             · exact h w hw
             · exact .inl ⟨_, rfl, ha, hc⟩
             · exact .inr ⟨_, rfl⟩)
          | (rcases hw with hw | rfl
             · exact h w hw
             · exact .inl ⟨_, rfl, ha, hc⟩)

theorem fold_writes (u : U64) (s : St) (ch : U64) (ids : List U64) (x : Model.Adm × Option Error)
    (h : AdmWrites s x.1.writes) : AdmWrites s (ids.foldl (Model.admitStep u s ch) x).1.writes := by
  induction ids generalizing x with
  | nil => exact h
  | cons id ids ih =>
    apply ih
    unfold Model.admitStep
    split
    · exact admitOne_writes u s ch id x.1 h
    · exact h

/-- The writes of a successful merge, around the batch's own writes. -/
theorem merge_shape {u : U64} {s : St} {n : alloc.vec.Vec U8} {ids : List U64} {ws r}
    (h : Model.merge u s n ids = .Ok (ws, r)) :
    ∃ (c : Channel) (st : Model.Adm), AdmWrites s st.writes ∧ ws = st.writes ++ [.PutChannel { c with size := st.size }, .SetRepository { s.repo with events := st.seq }] := by
  unfold Model.merge at h
  split_ifs at h
  split at h
  · simp at h
  · rename_i c _
    dsimp only at h
    split at h
    · rename_i st hf
      simp only [core.result.Result.Ok.injEq, Prod.mk.injEq] at h
      refine ⟨c, st, ?_, h.1.symm⟩
      have := fold_writes u s c.id ids
        (⟨(s.members.filter (fun m => decide (m.channel = c.id))).map (·.patch), c.size, s.repo.events, 0#u64, []⟩, none)
        (by intro w hw; simp at hw)
      rw [hf] at this; exact this
    · simp at h

/-- A patch newly admitted by a merge had an approval and a passing check
report in the state the merge read. -/
theorem merge_gate (a : Principal) (s : Snapshot) (r ch : alloc.vec.Vec U8) (ids : alloc.vec.Vec U64)
    (ws : alloc.vec.Vec Write) (h : Succeeds a s (.Merge r ch ids) ws) :
    ∀ m, Write.PutMember m ∈ ws.val →
      (∃ x ∈ s.approvals.val, x.patch = m.patch) ∧ (∃ x ∈ s.checks.val, x.patch = m.patch ∧ x.passed = true) := by
  obtain ⟨rr, hm⟩ := model_of h
  simp only [Model.step] at hm
  split at hm
  · obtain ⟨c, st, hw, he⟩ := merge_shape hm
    intro m hmem
    rw [he] at hmem
    simp only [List.mem_append, List.mem_cons, List.not_mem_nil, or_false, reduceCtorEq] at hmem
    rcases hw _ hmem with ⟨m', hm', ha, hp⟩ | ⟨e, he'⟩
    · simp only [Write.PutMember.injEq] at hm'; subst hm'
      simp only [Model.approvedB, List.any_eq_true, decide_eq_true_eq, Snapshot.toSt] at ha
      simp only [Model.passedB, Snapshot.toSt] at hp
      refine ⟨ha, ?_⟩
      cases hf : List.find? (fun c => decide (c.patch = m.patch)) s.checks.val with
      | none => simp [hf] at hp
      | some x =>
        simp [hf] at hp
        exact ⟨x, List.mem_of_find?_eq_some hf, by simpa using List.find?_some hf, hp⟩
    · cases he'
  · simp at hm

end o5s_registry.Guarantees
