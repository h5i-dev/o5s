import O5sKernel
import StdSpecs

open Aeneas Aeneas.Std Result H5iAppLib o5s_kernel

namespace o5s_kernel.Patch

/-- The specification adds a patch without changing its identity or discarding
any prior patch. List order is deliberately not part of channel identity. -/
def insert (s : List U64) (p : U64) : List U64 :=
  if p ∈ s then s else s ++ [p]

@[simp] theorem mem_insert (s : List U64) (p x : U64) :
    x ∈ insert s p ↔ x ∈ s ∨ x = p := by
  unfold insert
  split <;> simp_all

/-- Refinement of the actual extracted Rust admission function, including
refusals. A finite-capacity precondition rules out Vec allocation overflow. -/
@[step] theorem merge_spec (s d : alloc.vec.Vec U64) (a c : Bool) (p : U64)
    (room : s.val.length < Usize.max) :
    patch.merge s d a c p ⦃ r => match r with
      | .Ok out => a = true ∧ c = true ∧ d.val ⊆ s.val ∧ out.val = insert s.val p
      | .Err .Unapproved => a = false
      | .Err .ChecksFailed => a = true ∧ c = false
      | .Err .MissingDependency => a = true ∧ c = true ∧ ¬ d.val ⊆ s.val ⦄ := by
  unfold patch.merge
  step* <;> simp_all [insert]

/-- A successful admission requires approval, passing checks and every
explicit dependency. This statement talks about extracted executions. -/
theorem merge_authorized (s d out : alloc.vec.Vec U64) (a c : Bool) (p : U64)
    (room : s.val.length < Usize.max)
    (run : patch.merge s d a c p = ok (.Ok out)) :
    a = true ∧ c = true ∧ d.val ⊆ s.val ∧ out.val = insert s.val p := by
  have h := post_of_ok (merge_spec s d a c p room) run
  exact h

/-- Successful integration cannot lose any previously integrated patch. -/
theorem no_lost_patch (s d out : alloc.vec.Vec U64) (a c : Bool) (p x : U64)
    (room : s.val.length < Usize.max)
    (run : patch.merge s d a c p = ok (.Ok out)) (hx : x ∈ s.val) :
    x ∈ out.val := by
  rw [(merge_authorized s d out a c p room run).2.2.2]
  exact (mem_insert s.val p x).2 (.inl hx)

/-- Independent admissions commute as sets, even if their vector order differs. -/
theorem insert_commutes (s : List U64) (p q x : U64) :
    x ∈ insert (insert s p) q ↔ x ∈ insert (insert s q) p := by
  simp only [mem_insert]; tauto

theorem insert_idempotent (s : List U64) (p x : U64) :
    x ∈ insert (insert s p) p ↔ x ∈ insert s p := by
  simp only [mem_insert]; tauto

/-- All valid delivery orders with the same patch membership converge.
The theorem includes arbitrarily long finite agent delivery sequences. -/
def deliver (s patches : List U64) : List U64 := patches.foldl insert s

theorem delivery_membership (s patches : List U64) (x : U64) :
    x ∈ deliver s patches ↔ x ∈ s ∨ x ∈ patches := by
  induction patches generalizing s with
  | nil => simp [deliver]
  | cons p ps ih =>
    simp only [deliver, List.foldl_cons]
    change x ∈ deliver (insert s p) ps ↔ _
    rw [ih, mem_insert]
    simp only [List.mem_cons]; tauto

theorem delivery_converges (s a b : List U64)
    (same : ∀ x, x ∈ a ↔ x ∈ b) :
    ∀ x, x ∈ deliver s a ↔ x ∈ deliver s b := by
  intro x; simp only [delivery_membership, same x]

/-- Dependency closure is preserved by an actual successful Rust admission.
The registry supplies the immutable dependency function. -/
def Closed (deps : U64 → List U64) (s : List U64) : Prop :=
  ∀ p, p ∈ s → deps p ⊆ s

theorem dependency_closed (deps : U64 → List U64)
    (s d out : alloc.vec.Vec U64) (a c : Bool) (p : U64)
    (room : s.val.length < Usize.max) (closed : Closed deps s.val)
    (registry : d.val = deps p)
    (run : patch.merge s d a c p = ok (.Ok out)) : Closed deps out.val := by
  have spec := merge_authorized s d out a c p room run
  rw [spec.2.2.2]
  intro q hq x hx
  rcases (mem_insert s.val p q).1 hq with old | added
  · exact (mem_insert s.val p x).2 (.inl (closed q old hx))
  · subst q
    exact (mem_insert s.val p x).2 (.inl (spec.2.2.1 (registry.symm ▸ hx)))

/-- Connect set commutativity directly to two successful extracted executions
in each order. The admissions retain their guards; invalid orders are refused. -/
theorem successful_orders_converge
    (s d e sp sq spq sqp : alloc.vec.Vec U64) (p q : U64)
    (rs : s.val.length < Usize.max) (rp : sp.val.length < Usize.max)
    (rq : sq.val.length < Usize.max)
    (hp : patch.merge s d true true p = ok (.Ok sp))
    (hq : patch.merge s e true true q = ok (.Ok sq))
    (hpq : patch.merge sp e true true q = ok (.Ok spq))
    (hqp : patch.merge sq d true true p = ok (.Ok sqp)) :
    ∀ x, x ∈ spq.val ↔ x ∈ sqp.val := by
  intro x
  rw [(merge_authorized sp e spq true true q rp hpq).2.2.2,
      (merge_authorized sq d sqp true true p rq hqp).2.2.2,
      (merge_authorized s d sp true true p rs hp).2.2.2,
      (merge_authorized s e sq true true q rs hq).2.2.2]
  exact insert_commutes s.val p q x

end o5s_kernel.Patch
