import Lake
open Lake DSL

-- Pinned to the Aeneas commit that generated generated/O5sRegistry.lean.
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

-- h5i-app's Lean library, at the release of the h5i-app 0.1.1 crates.
require h5i_app_lib from git
  "https://github.com/h5i-dev/h5i" @ "v0.5.0" / "crates/h5i-app-core/proofs"

package o5s_registry_proofs

-- Generated Lean: the extracted Rust (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`O5sRegistry, `Schema, `StdSpecs]

-- Hand-written specification and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Spec, `Apply, `Storage, `Helpers, `Commands, `Exact, `Model, `Writes, `Refine, `Guarantees, `Invariant, `Narrow, `Served]
