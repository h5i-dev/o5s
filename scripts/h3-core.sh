#!/usr/bin/env bash
# H3, the no-regression rerun (docs/H5I_PLAN.md, PLAN §6.8), core side: every core benchmark in
# docs/BASELINE.md, same scripts and protocol, raw outputs under $OUT (default
# $SCRATCH/h3/<commit>). Run each part in its own announced bench window:
#   scripts/h3-core.sh suite     # baseline-core.sh micro + scale (~25 min)
#   scripts/h3-core.sh generic   # PLAN §6.4, 3 runs (~15 min)
#   scripts/h3-core.sh exp3      # Exp3 repos + generalization (two windows' worth; run per repo if needed)
#   scripts/h3-core.sh retime    # Exp3 build re-time (ripgrep, jq: the ±20 % pairs of the baseline)
#   scripts/h3-core.sh report    # the §6.8 comparison against bench/baseline/*
set -euo pipefail
source "$(dirname "$0")/env.sh"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
COMMIT=$(git -C "$ROOT" rev-parse --short HEAD)
OUT=${OUT:-$SCRATCH/h3/$COMMIT}
mkdir -p "$OUT"
case "${1:-}" in
  suite)
    OUT=$OUT/core "$ROOT/scripts/baseline-core.sh" micro
    OUT=$OUT/core "$ROOT/scripts/baseline-core.sh" scale
    ;;
  generic)
    (cd "$ROOT" && nice cargo build -q --release -j2 -p pico2peta-micro)
    for i in 1 2 3; do
      { echo "load $(cut -d' ' -f1 /proc/loadavg) commit $COMMIT"
        flock "$SCRATCH/bench.lock" "$CARGO_TARGET_DIR/release/p2p-bench" generic "$SCRATCH/bench-micro" 1024; } >> "$OUT/generic.txt"
    done
    ;;
  exp3)
    shift
    EXP3_OUT=$OUT/exp3-results.jsonl "$ROOT/scripts/exp3.sh" repos "$@"
    [ $# -eq 0 ] && EXP3_OUT=$OUT/exp3-results.jsonl "$ROOT/scripts/exp3.sh" general
    ;;
  retime)
    for r in ripgrep jq; do EXP3_RETIME_OUT=$OUT/exp3-retime.jsonl "$ROOT/scripts/exp3-retime.sh" "$r" a3; done
    ;;
  report)
    python3 "$ROOT/scripts/baseline-summary.py" "$ROOT/bench/baseline/core-40b6ed6" "$OUT/core" > "$OUT/core-verdict.md" || true
    tail -1 "$OUT/core-verdict.md"
    python3 - "$ROOT/bench/baseline/exp3/results.jsonl" "$OUT/exp3-results.jsonl" <<'PY'
import json, sys
def rows(p):
    out = {}
    for l in open(p):
        d = json.loads(l)
        out[(d["repo"], d["variant"])] = d
    return out
base, new = rows(sys.argv[1]), rows(sys.argv[2])
bad = 0
for k, b in sorted(base.items()):
    n = new.get(k)
    if n is None:
        print("missing", k); bad += 1; continue
    if n["total_bytes"] != b["total_bytes"]:
        print("SIZE DIFFERS", k, b["total_bytes"], n["total_bytes"]); bad += 1
print(f"exp3 sizes: {len(base) - bad}/{len(base)} byte-identical")
PY
    ;;
  *) echo "usage: $0 suite|generic|exp3 [repo...]|retime|report" >&2; exit 2 ;;
esac
