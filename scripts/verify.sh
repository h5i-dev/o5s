#!/usr/bin/env bash
# Format, tests (PostgreSQL ones when O5S_TEST_DATABASE_URL is set; use
# `scripts/postgres.sh` for a local one), UI checks, and both Lean projects:
# the kernels in proofs/ and the registry in crates/registry/proofs/, plus
# the pending statements in specs/ and crates/registry/specs/.
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_INCREMENTAL=0
cargo fmt --all --check
if [[ -z "${O5S_TEST_DATABASE_URL:-}" ]]; then
    echo "O5S_TEST_DATABASE_URL unset: PostgreSQL tests are skipped"
fi
cargo test --workspace
cargo check -p o5s-worker --target wasm32-unknown-unknown
if ! command -v node >/dev/null; then
    # Node 24 from Docker when it is not installed.
    node() { docker run --rm -i -v "$PWD":/w -w /w node:24-slim node "$@"; }
fi
node --test crates/server/tests/ui.test.mjs
node --input-type=module -e 'import fs from "node:fs"; import vm from "node:vm"; const html = fs.readFileSync("crates/server/src/index.html", "utf8"); const scripts = [...html.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g)]; if (!scripts.length) throw Error("No UI script found"); for (const script of scripts) new vm.Script(script[1]); console.log("Admin UI JavaScript syntax passed");'
h5i app extract .
h5i app check .
h5i app extract crates/registry
h5i app check crates/registry
# Statements still to prove must type-check against the extracted code.
(cd proofs && lake build Specs)
(cd crates/registry/proofs && lake build Specs)
