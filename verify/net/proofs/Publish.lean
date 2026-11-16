import O5sNetKernel
import H5iAppLib
/-!
# Publish rules of the fronts: the extracted decisions compute these specs

`crates/net-kernel/src/publish.rs`, used by PyPI/npm/Maven (create-once files), OCI (manifest
pushes), HF (commits) and npm (revision-checked writes).
-/
open Aeneas Aeneas.Std Result H5iAppLib o5s_net_kernel o5s_net_kernel.publish

namespace o5s_net_kernel.Spec.Publish

/-- Create-once: absent → create, identical → unchanged, different → conflict. -/
theorem decide_create_once_spec (e : Existing) :
    decide_create_once e ⦃ r => r = match e with
      | .Absent => .Create
      | .Same => .Unchanged
      | .Different => .Conflict ⦄ := by
  unfold decide_create_once; cases e <;> simp

/-- A release file never changes: a publish over different content is a conflict. -/
theorem release_files_never_change :
    decide_create_once .Different ⦃ r => r = .Conflict ⦄ := by
  unfold decide_create_once; simp

def manifestSpec (byDigest digestOk tagOk : Bool) (missing : Nat) : core.result.Result Unit ManifestNg :=
  if byDigest = true ∧ digestOk = false then .Err .DigestInvalid
  else if byDigest = false ∧ tagOk = false then .Err .TagInvalid
  else if 0 < missing then .Err .BlobUnknown
  else .Ok ()

theorem decide_manifest_spec (d m t : Bool) (n : U64) :
    decide_manifest d m t n ⦃ r => r = manifestSpec d m t n.val ⦄ := by
  unfold decide_manifest
  step* <;> simp_all [manifestSpec] <;> scalar_tac

/-- An accepted manifest push: its digest reference is its body's, or its tag is valid; and
every referenced blob is present. -/
theorem manifest_accepted (d m t : Bool) (n : Nat) (h : manifestSpec d m t n = .Ok ()) :
    (d = true → m = true) ∧ (d = false → t = true) ∧ n = 0 := by
  unfold manifestSpec at h
  cases d <;> cases m <;> cases t <;> split_ifs at h <;> simp_all <;> omega

def commitSpec (parent head : Option (alloc.vec.Vec U8)) (pathsOk lfsOk : Bool) : core.result.Result Unit CommitNg :=
  if parent.isSome ∧ parent ≠ head then .Err .StaleParent
  else if pathsOk = false then .Err .BadPath
  else if lfsOk = false then .Err .MissingLfs
  else .Ok ()

@[step]
theorem same_opt_spec (a b : Option (alloc.vec.Vec U8)) : same_opt a b ⦃ r => r = decide (a = b) ⦄ := by
  unfold same_opt
  split <;> split <;> step* <;> simp_all

theorem decide_commit_spec (parent head : Option (alloc.vec.Vec U8)) (p l : Bool) :
    decide_commit parent head p l ⦃ r => r = commitSpec parent head p l ⦄ := by
  unfold decide_commit
  split <;> step* <;> simp_all [commitSpec]

/-- An accepted HF commit: any given parent is the head, paths are valid, and every LFS file
was uploaded with its size. -/
theorem commit_accepted (parent head : Option (alloc.vec.Vec U8)) (p l : Bool)
    (h : commitSpec parent head p l = .Ok ()) : (parent = none ∨ parent = head) ∧ p = true ∧ l = true := by
  unfold commitSpec at h
  split_ifs at h with h1 h2 h3 <;> simp_all
  cases parent <;> simp_all

theorem decide_rev_spec (g c : alloc.vec.Vec U8) : decide_rev g c ⦃ r => r = decide (g = c) ⦄ := by
  unfold decide_rev; step*

end o5s_net_kernel.Spec.Publish
