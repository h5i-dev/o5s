import Lake
open Lake DSL

-- Pinned to the Aeneas commit `h5i app` extracts with (`h5i app doctor`).
require aeneas from git
  "https://github.com/AeneasVerif/aeneas" @ "b86120db3183b0107eb5f2637b11c424cd06ef1c" / "backends/lean"

-- h5i-app's lemmas and tactics, at the revision of the prebuilt package set on
-- the dev host (/mnt/scratch/ec2-user/lake/arith/packages).
require h5i_app_lib from git
  "https://github.com/h5i-dev/h5i" @ "365703f6f4d84841e6829cb5e262780b0600fc47" / "crates/h5i-app-core/proofs"

package o5s_core_proofs

-- The extracted kernel (`h5i app extract`). Do not edit.
lean_lib Generated where
  srcDir := "generated"
  roots := #[`O5sKernel]

-- Hand-written specs and proofs.
@[default_target] lean_lib Proofs where
  roots := #[`Proofs, `PathName]
