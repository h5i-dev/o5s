#!/usr/bin/env bash
# Experiment 3 driver (protocol: docs/PLAN.md §6.6, with its 2026-11-15
# amendment). One JSON line per measurement in $BENCH_DATA/exp3/results.jsonl,
# each tagged with the repo and the commit it ran on (the frozen commit must be
# recorded in docs/EXP1.md before the official run).
#   scripts/exp3.sh repos [repo ...]      (default: ripgrep jq redis git)  variants a0..a4
#   scripts/exp3.sh general               generalization corpora (smollm2 tensors, pypi files): r0 a1 a3 a4
# Every run holds the bench lock. The verdict:
#   EXP1_ELIGIBLE=a3,a4 scripts/exp1-report.py $BENCH_DATA/exp1/results.jsonl,$BENCH_DATA/exp3/results.jsonl
set -euo pipefail
source "$(dirname "$0")/env.sh"
MIRRORS=$BENCH_DATA/mirrors
W=$BENCH_DATA/exp3
OUT=${EXP3_OUT:-$W/results.jsonl}
EXP1=$CARGO_TARGET_DIR/release/exp1
EXP3=$CARGO_TARGET_DIR/release/exp3
mkdir -p "$W"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
(cd "$ROOT" && nice cargo build -q --release -j1 -p pico2peta-exp1 -p pico2peta-exp3)
COMMIT=$(git -C "$ROOT" rev-parse --short HEAD)
DIRTY=$(git -C "$ROOT" status --porcelain -- crates bench | wc -l)
run() { # run <work-dir> <name> <variant>
  local line
  line=$(flock "$SCRATCH/bench.lock" "$EXP3" run "$1" "$3" 4 10000)
  echo "{\"repo\":\"$2\",\"commit\":\"$COMMIT\",\"dirty_files\":$DIRTY,${line#\{}" | tee -a "$OUT"
}
case "${1:-repos}" in
  repos)
    shift || true
    REPOS=("$@"); [ ${#REPOS[@]} -eq 0 ] && REPOS=(ripgrep jq redis git)
    for r in "${REPOS[@]}"; do
      [ -f "$W/$r/meta.tsv" ] || "$EXP1" extract "$MIRRORS/$r.git" "$W/$r"
      for v in a0 a1 a2 a3 a4; do run "$W/$r" "$r" "$v"; done
    done
    ;;
  general)
    [ -f "$W/smollm2/meta.tsv" ] || "$EXP3" spill-files "$W/smollm2" tensors \
      "$BENCH_DATA/weights/SmolLM2-135M.safetensors" "$BENCH_DATA/weights/SmolLM2-135M-Instruct.safetensors"
    [ -f "$W/pypi/meta.tsv" ] || "$EXP3" spill-files "$W/pypi" dirs $(ls -d "$BENCH_DATA"/packages/unpacked/* | sort -V)
    for c in smollm2 pypi; do
      for v in r0 a1 a3 a4; do run "$W/$c" "$c" "$v"; done
    done
    ;;
  *) echo "usage: scripts/exp3.sh repos [repo ...] | general" >&2; exit 2 ;;
esac
