#!/usr/bin/env bash
# Experiment 1 driver (protocol: docs/PLAN.md §4). Results: one JSON line per
# measurement in $BENCH_DATA/exp1/results.jsonl.
#   scripts/exp1.sh [repo ...]     (default: ripgrep jq redis git)
set -euo pipefail
source "$(dirname "$0")/env.sh"
MIRRORS=$BENCH_DATA/mirrors
W=$BENCH_DATA/exp1
OUT=$W/results.jsonl
EXP1=$CARGO_TARGET_DIR/release/exp1
mkdir -p "$W"
(cd "$(dirname "$0")/.." && cargo build -q --release -p pico2peta-exp1)
REPOS=("$@"); [ ${#REPOS[@]} -eq 0 ] && REPOS=(ripgrep jq redis git)

timed() { # timed <logfile> cmd... ; prints "wall_secs peak_rss_kb"
  local log=$1; shift
  /usr/bin/time -f '%e %M' -o "$log.time" "$@" >"$log" 2>&1
  cat "$log.time"
}

for r in "${REPOS[@]}"; do
  src=$MIRRORS/$r.git
  refs=$(git --git-dir "$src" for-each-ref --format='%(objectname) %(refname)' | sha256sum | cut -c1-16)
  for mode in aggressive plain; do
    dst=$W/$r-$mode.git
    rm -rf "$dst"; cp -a "$src" "$dst"
    if [ $mode = aggressive ]; then
      read secs rss < <(timed "$W/$r-$mode.log" git --git-dir "$dst" -c pack.threads=4 gc --aggressive --prune=now)
    else
      read secs rss < <(timed "$W/$r-$mode.log" git --git-dir "$dst" -c pack.threads=4 gc --prune=now)
    fi
    pack=$(find "$dst/objects/pack" -name '*.pack' -printf '%s\n' | awk '{s+=$1} END {print s+0}')
    idx=$(find "$dst/objects/pack" -name '*.idx' -printf '%s\n' | awk '{s+=$1} END {print s+0}')
    extra=$(find "$dst/objects/pack" \( -name '*.rev' -o -name '*.bitmap' \) -printf '%s\n' | awk '{s+=$1} END {print s+0}')
    loose=$(find "$dst/objects" -path "$dst/objects/pack" -prune -o -type f -printf '%s\n' | awk '{s+=$1} END {print s+0}')
    nobj=$(git --git-dir "$dst" count-objects -v | awk '/^in-pack/ {print $2}')
    echo "{\"repo\":\"$r\",\"refs_digest\":\"$refs\",\"variant\":\"git-$mode\",\"pack_bytes\":$pack,\"idx_bytes\":$idx,\"total_bytes\":$((pack+idx)),\"rev_bitmap_bytes\":$extra,\"loose_bytes\":$loose,\"objects\":$nobj,\"build_secs\":$secs,\"peak_rss_kb\":$rss}" | tee -a "$OUT"
    rm -rf "$dst"
  done
  rm -rf "$W/$r"
  read secs rss < <(timed "$W/$r-extract.log" "$EXP1" extract "$src" "$W/$r")
  echo "{\"repo\":\"$r\",\"variant\":\"extract\",\"build_secs\":$secs,\"peak_rss_kb\":$rss,\"note\":\"$(tail -1 "$W/$r-extract.log")\"}" | tee -a "$OUT"
  for v in v0 v1 v2 v3; do
    line=$("$EXP1" run "$W/$r" $v 4 10000)
    echo "{\"repo\":\"$r\",${line#\{}" | tee -a "$OUT"
  done
  rm -rf "$W/$r"
done
