#!/usr/bin/env bash
# Experiment 1b driver (protocol: docs/PLAN.md §4.8). Reuses Exp1's git
# baselines from results.jsonl; appends V4/V5 to results-1b.jsonl.
#   scripts/exp1b.sh [repo ...]     (default: ripgrep jq redis git)
set -euo pipefail
source "$(dirname "$0")/env.sh"
MIRRORS=$BENCH_DATA/mirrors
W=$BENCH_DATA/exp1b
OUT=$BENCH_DATA/exp1/results-1b.jsonl
T=$SCRATCH/cargo-target-exp1b
mkdir -p "$W"
(cd "$(dirname "$0")/.." && CARGO_TARGET_DIR=$T cargo build -q --release -p o5s-exp1)
EXP1=$T/release/exp1
REPOS=("$@"); [ ${#REPOS[@]} -eq 0 ] && REPOS=(ripgrep jq redis git)
for r in "${REPOS[@]}"; do
  rm -rf "$W/$r"
  /usr/bin/time -f '%e %M' -o "$W/$r-extract.time" "$EXP1" extract "$MIRRORS/$r.git" "$W/$r" > "$W/$r-extract.log" 2>&1
  for v in v4 v5; do
    line=$("$EXP1" run "$W/$r" $v 4 10000)
    echo "{\"repo\":\"$r\",${line#\{}" | tee -a "$OUT"
  done
  rm -rf "$W/$r"
done
