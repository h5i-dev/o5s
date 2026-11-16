#!/usr/bin/env bash
# PLAN §6.6 build-time rule: when a build ratio is within ±20 % of 1.0, re-time
# both sides back to back under the bench lock; the re-timed pair decides.
#   scripts/exp3-retime.sh <repo> <variant>...
# For each variant: `git gc --aggressive` (Git 2.50.1, as the frozen baseline,
# pack.threads=4, fresh mirror copy), then the variant, inside ONE lock hold.
# Appends {"repo","variant":"retime-<v>","git_secs","ours_secs","ratio",...} to
# $BENCH_DATA/exp3/retime.jsonl.
set -euo pipefail
source "$(dirname "$0")/env.sh"
r=$1; shift
W=$BENCH_DATA/exp3
EXP3=$CARGO_TARGET_DIR/release/exp3
OUT=${EXP3_RETIME_OUT:-$W/retime.jsonl}
[ "$(git --version)" = "git version 2.50.1" ] || { echo "baseline needs git 2.50.1" >&2; exit 1; }
for v in "$@"; do
  flock "$SCRATCH/bench.lock" bash -c '
    set -euo pipefail
    r=$1; v=$2; W=$3; EXP3=$4; OUT=$5
    dst=$W/retime-$r.git
    case "$dst" in /mnt/scratch/pico2peta/*/retime-*.git) ;; *) echo "refusing to touch $dst" >&2; exit 1 ;; esac
    rm -rf -- "$dst"; cp -a "$BENCH_DATA/mirrors/$r.git" "$dst"
    load0=$(cut -d" " -f1 /proc/loadavg)
    t0=$(date +%s.%N)
    git --git-dir "$dst" -c pack.threads=4 gc --aggressive --prune=now >/dev/null 2>&1
    t1=$(date +%s.%N)
    rm -rf -- "$dst"
    line=$("$EXP3" run "$W/$r" "$v" 4 10000)
    ours=$(printf "%s" "$line" | python3 -c "import json,sys; print(json.load(sys.stdin)[\"build_secs\"])")
    git_s=$(python3 -c "print(round($t1-$t0,2))")
    load1=$(cut -d" " -f1 /proc/loadavg)
    echo "{\"repo\":\"$r\",\"variant\":\"retime-$v\",\"git_secs\":$git_s,\"ours_secs\":$ours,\"ratio\":$(python3 -c "print(round($ours/$git_s,3))"),\"load_start\":$load0,\"load_end\":$load1}" | tee -a "$OUT"
  ' _ "$r" "$v" "$W" "$EXP3" "$OUT"
done
