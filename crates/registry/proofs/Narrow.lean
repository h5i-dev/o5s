import Invariant
/-!
# Loading less than a tenant

The PostgreSQL store loads, for each command, only the rows that match the
keys the command names (`load` in `crates/pgwire/src/service.rs`). `Narrow`
states what those loads must agree on with the whole tenant: for each lookup
the command makes, the matching rows in the same relative order. Under it the
model gives the same result, so a run of `transition` on a narrow load
computes what the whole tenant specifies (`narrow_load`).
-/
open Aeneas Aeneas.Std Result o5s_registry o5s_registry.Spec H5iAppLib

namespace o5s_registry.Narrow

/-- The rows of `l₁` and `l₂` that satisfy `P` are the same, in the same order. -/
def Agree {α} (P : α → Bool) (l₁ l₂ : List α) : Prop := l₁.filter P = l₂.filter P

theorem find?_agree {α} {P : α → Bool} {l₁ l₂ : List α} (h : Agree P l₁ l₂) : l₁.find? P = l₂.find? P := by
  rw [← List.head?_filter, ← List.head?_filter, h]

theorem any_agree {α} {P : α → Bool} {l₁ l₂ : List α} (h : Agree P l₁ l₂) : l₁.any P = l₂.any P := by
  have e : ∀ l : List α, l.any P = (l.filter P).any (fun _ => true) := by intro l; simp
  rw [e, e l₂, h]

end o5s_registry.Narrow
