#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_INCREMENTAL=0
cargo fmt --all --check
cargo test --workspace
node --test cloudflare/worker.test.mjs crates/server/tests/ui.test.mjs
node --input-type=module -e 'import fs from "node:fs"; import vm from "node:vm"; const html = fs.readFileSync("crates/server/src/index.html", "utf8"); const scripts = [...html.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g)]; if (!scripts.length) throw Error("No UI script found"); for (const script of scripts) new vm.Script(script[1]); console.log("Admin UI JavaScript syntax passed");'
h5i app extract .
h5i app check .
