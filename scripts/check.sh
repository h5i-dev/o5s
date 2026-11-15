#!/usr/bin/env bash
# Pre-commit gate: formatting, clippy (-D warnings), full test suite. Exits non-zero on any failure.
#   scripts/check.sh && git commit ...
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy -q -j2 --workspace --all-targets -- -D warnings
cargo test -q -j2 --workspace 2>&1 | tee "$TMPDIR/check-test.log" | grep -E "^test result|FAILED|panicked" | grep -vE "test result: ok" || true
! grep -qE "FAILED|panicked|^error" "$TMPDIR/check-test.log"
echo "check: OK"
