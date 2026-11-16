#!/usr/bin/env bash
# Experiment 1c driver (protocol: docs/PLAN.md §4.9). Reuses Exp1's frozen
# git-aggressive baselines (results.jsonl). Appends V6/V7 and the extra
# Git 2.56 baselines (--path-walk, --name-hash-version=2) to results-1c.jsonl.
# Every timed step runs under the shared bench lock.
#   scripts/exp1c.sh [repo ...]     (default: ripgrep jq redis git)
set -euo pipefail
SCRATCH=/mnt/scratch/pico2peta
BENCH_DATA=$SCRATCH/bench-data
MIRRORS=$BENCH_DATA/mirrors
W=$BENCH_DATA/exp1c
OUT=$BENCH_DATA/exp1/results-1c.jsonl
LOCK=$SCRATCH/bench.lock
GIT256=$SCRATCH/git-2.56
T=${CARGO_TARGET_DIR:-$SCRATCH/cargo-target-net}
mkdir -p "$W"
(cd "$(dirname "$0")/.." && CARGO_TARGET_DIR=$T cargo build -q -j2 --release -p o5s-exp1c)
EXP1C=$T/release/exp1c
REPOS=("$@"); [ ${#REPOS[@]} -eq 0 ] && REPOS=(ripgrep jq redis git)
for r in "${REPOS[@]}"; do
  rm -rf "$W/$r"
  # Extraction is CPU-heavy too: hold the shared bench lock for it as well.
  flock "$LOCK" "$EXP1C" extract "$MIRRORS/$r.git" "$W/$r" > "$W/$r-extract.log" 2>&1
  for v in v6 v7; do
    line=$(flock "$LOCK" "$EXP1C" run "$W/$r" $v 4 10000)
    echo "{\"repo\":\"$r\",${line#\{}" | tee -a "$OUT"
  done
  rm -rf "$W/$r"
  # Extra baselines (reported, not judged): Git 2.56.1, window 250 / depth 50, on fresh copies.
  for mode in path-walk name-hash-version=2; do
    rm -rf "$W/$r.git"
    cp -a "$MIRRORS/$r.git" "$W/$r.git"
    t0=$(date +%s.%N)
    flock "$LOCK" env GIT_EXEC_PATH=$GIT256 "$GIT256/git" -C "$W/$r.git" -c pack.threads=4 \
      repack -adf -q --window=250 --depth=50 "--$mode"
    t1=$(date +%s.%N)
    bytes=$(find "$W/$r.git/objects/pack" \( -name '*.pack' -o -name '*.idx' \) -printf '%s\n' | awk '{s+=$1} END {print s}')
    secs=$(echo "$t1 - $t0" | bc)
    echo "{\"repo\":\"$r\",\"baseline\":\"git-2.56-$mode\",\"bytes\":$bytes,\"secs\":$secs}" | tee -a "$OUT"
    rm -rf "$W/$r.git"
  done
done
