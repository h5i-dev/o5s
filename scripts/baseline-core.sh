#!/usr/bin/env bash
# Core benchmark suite for the h5i rebuild's no-regression check (docs/H5I_PLAN.md H0/H3, PLAN.md §6.8).
# The same script, commands and inputs run for the baseline (H0) and after the rebuild (H3).
#
#   scripts/baseline-core.sh micro   # small, commits, stream, reads, ingest: 3 reps each (~25 min)
#   scripts/baseline-core.sh scale   # scale objects 1e6/1e7, names 1e6, prov 1e6, gc 1e7: 1 rep each (~15 min)
#
# Every run holds the bench lock. Each raw output goes to $OUT/<name>-r<rep>.txt, headed by the commit, the
# count of dirty files, the 1-minute load at start and end, and a timestamp. Build first:
#   cargo build --release -p pico2peta-micro
set -euo pipefail
source "$(dirname "$0")/env.sh"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
COMMIT=$(git -C "$ROOT" rev-parse --short HEAD)
B=$CARGO_TARGET_DIR/release/p2p-bench
OUT=${OUT:-$SCRATCH/baseline/$COMMIT}
WORK=$SCRATCH/bench-micro/baseline
LOCK=$SCRATCH/bench.lock
mkdir -p "$OUT" "$WORK"

load() { cut -d' ' -f1 /proc/loadavg; }
# run NAME REPS ARGS...: one raw file per rep.
run() {
  local name=$1 reps=$2
  shift 2
  for r in $(seq 1 "$reps"); do
    {
      echo "# $name rep $r commit $COMMIT dirty $(git -C "$ROOT" status --porcelain | wc -l) load_start $(load) $(date -Is)"
      flock "$LOCK" "$B" "$@"
      echo "# load_end $(load)"
    } > "$OUT/$name-r$r.txt" 2>&1
    echo "$name rep $r done (load $(load))"
  done
}

# Input for `ingest`: the first 1.1 GB of redis's objects in oid order (deterministic), regenerated if missing.
dump() {
  local d=$WORK/redis-objs.dump
  if [ ! -s "$d" ]; then
    git -C "$SCRATCH/bench-data/mirrors/redis.git" cat-file --batch-all-objects --batch | head -c 1153433600 > "$d" || true
  fi
  echo "$d"
}

case "${1:-}" in
  micro)
    run small 3 small "$WORK" 100000
    run commits 3 commits "$WORK" 20000
    run stream 3 stream "$WORK" 512
    run reads 3 reads "$WORK"
    run ingest 3 ingest "$(dump)" "$WORK" 1024
    rm -f -- "$WORK/redis-objs.dump"
    ;;
  scale)
    run scale-objects-1e6 1 scale "$WORK" objects 1000000
    run scale-objects-1e7 1 scale "$WORK" objects 10000000
    run scale-gc-1e7 1 scale "$WORK" gc 10000000
    run scale-names-1e6 1 scale "$WORK" names 1000000
    run scale-prov-1e6 1 scale "$WORK" prov 1000000
    for d in scale-objects-1000000 scale-objects-10000000 scale-names-1000000 scale-prov-1000000; do
      rm -rf -- "${WORK:?}/$d"
    done
    ;;
  *)
    echo "usage: $0 micro|scale" >&2
    exit 2
    ;;
esac
echo "raw outputs: $OUT"
