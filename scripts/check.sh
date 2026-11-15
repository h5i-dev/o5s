#!/usr/bin/env bash
# Pre-commit gate: formatting, clippy (-D warnings), full test suite (all binaries,
# --no-fail-fast). Exits non-zero on any failure.
#   scripts/check.sh && git commit ...
set -euo pipefail
source "$(dirname "$0")/env.sh"
cd "$(dirname "$0")/.."
cargo fmt --all -- --check
cargo clippy -q -j"${P2P_JOBS:-2}" --workspace --all-targets -- -D warnings
log="$TMPDIR/check-test.log"
set +e
cargo test -q -j"${P2P_JOBS:-2}" --workspace --no-fail-fast >"$log" 2>&1
status=$?
set -e
grep -E "^test result|FAILED|panicked" "$log" | grep -vE "^test result: ok" || true
if [ "$status" -ne 0 ]; then
  echo "check: FAILED (cargo test exit $status; full log: $log)" >&2
  exit 1
fi
if grep -qE "FAILED|panicked|^error" "$log"; then
  echo "check: FAILED (failures in test output; full log: $log)" >&2
  exit 1
fi
echo "check: OK"
