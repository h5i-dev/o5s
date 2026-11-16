#!/usr/bin/env bash
# H3, the no-regression rerun (docs/H5I_PLAN.md, PLAN §6.8), core side: every core benchmark in
# docs/BASELINE.md, same scripts and protocol, raw outputs under $OUT (default
# $SCRATCH/h3/<commit>). Run each part in its own announced bench window:
#   scripts/h3-core.sh suite     # baseline-core.sh micro + scale (~25 min)
#   scripts/h3-core.sh generic   # PLAN §6.4, 3 runs (~15 min)
#   scripts/h3-core.sh exp3      # Exp3 repos + generalization (two windows' worth; run per repo if needed)
#   scripts/h3-core.sh retime    # Exp3 build re-time (ripgrep, jq: the ±20 % pairs of the baseline)
#   scripts/h3-core.sh report    # the §6.8 comparison against bench/baseline/*
#   scripts/h3-core.sh recheck   # PLAN §6.8 amendment 2026-11-16: the three controlled re-measurements
#     (needs the baseline-pre-h5i binaries: BASE_BENCH, BASE_EXP3; default $SCRATCH/tmp/{p2p-bench,exp3}-base)
#   scripts/h3-core.sh recheck-report
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
  recheck)
    BB=${BASE_BENCH:-$SCRATCH/tmp/p2p-bench-base}
    BE=${BASE_EXP3:-$SCRATCH/tmp/exp3-base}
    NB=$CARGO_TARGET_DIR/release/p2p-bench
    NE=$CARGO_TARGET_DIR/release/exp3
    (cd "$ROOT" && nice cargo build -q --release -j2 -p pico2peta-micro -p pico2peta-exp3)
    [ -x "$BB" ] && [ -x "$BE" ] || { echo "baseline binaries missing: $BB $BE" >&2; exit 1; }
    ( while true; do echo "## $(date -Is) load $(cut -d' ' -f1-3 /proc/loadavg)"; ps -eo pcpu,user,comm --sort=-pcpu | head -6; sleep 10; done ) > "$OUT/recheck-top.log" 2>&1 &
    SAMPLER=$!
    trap 'kill $SAMPLER 2>/dev/null' EXIT
    WORK=$SCRATCH/bench-micro/baseline
    # (1) ingest put: a quiet start (load < 2.5), 5 rounds, baseline then H3.
    D=$WORK/redis-objs.dump
    git -C "$SCRATCH/bench-data/mirrors/redis.git" cat-file --batch-all-objects --batch | head -c 1153433600 > "$D" || true
    until awk '{exit !($1 < 2.5)}' /proc/loadavg; do sleep 30; done
    echo "# ingest start load $(cut -d' ' -f1 /proc/loadavg) $(date -Is)" > "$OUT/recheck-ingest.txt"
    for r in 1 2 3 4 5; do
      for side in base new; do
        b=$BB; [ $side = new ] && b=$NB
        echo "$side $r $(cut -d' ' -f1 /proc/loadavg) $(flock "$SCRATCH/bench.lock" "$b" ingest "$D" "$WORK" 1024 | grep '"api":"put"')" >> "$OUT/recheck-ingest.txt"
      done
    done
    rm -f -- "$D"
    # (2) durable-write tier: 5 runs of the H3 generic, one put/dd ratio per run.
    : > "$OUT/recheck-generic.txt"
    for r in 1 2 3 4 5; do
      echo "$r $(cut -d' ' -f1 /proc/loadavg) $(flock "$SCRATCH/bench.lock" "$NB" generic "$SCRATCH/bench-micro" 1024)" >> "$OUT/recheck-generic.txt"
    done
    # (3) redis A3 peak RSS: 3 reps each, interleaved.
    : > "$OUT/recheck-rss.txt"
    for r in 1 2 3; do
      for side in base new; do
        e=$BE; [ $side = new ] && e=$NE
        echo "$side $r $(cut -d' ' -f1 /proc/loadavg) $(flock "$SCRATCH/bench.lock" "$e" run "$BENCH_DATA/exp3/redis" a3 4 10000)" >> "$OUT/recheck-rss.txt"
      done
    done
    ;;
  recheck-report)
    python3 - "$OUT" <<'PY'
import json, re, statistics as st, sys
o = sys.argv[1]
def js(line):
    m = re.search(r"\{.*\}", line)
    return json.loads(m.group(0)) if m else None
def verdict(name, base, new, higher_better=True):
    bm, nm = st.median(base), st.median(new)
    allow = max(3.0, (max(base) - min(base)) / bm * 100)
    ch = (nm - bm) / bm * 100 * (1 if higher_better else -1)
    print(f"{name}: baseline {base} median {bm}; H3 {new} median {nm}; change {ch:+.1f}% allowed -{allow:.1f}% -> {'PASS' if ch >= -allow else 'FAIL'}")
ing = [l.split(None, 3) for l in open(f"{o}/recheck-ingest.txt") if l[:4] in ("base", "new ")]
verdict("1. ingest put MB/s", [js(l[3])["mb_s"] for l in ing if l[0] == "base"], [js(l[3])["mb_s"] for l in ing if l[0] == "new"])
rat = [js(l)["write_ratio"] for l in open(f"{o}/recheck-generic.txt") if js(l)]
print(f"2. durable write put/dd ratios {rat} median {st.median(rat)} vs 0.90 -> {'PASS' if st.median(rat) >= 0.90 else 'FAIL'}")
rss = [l.split(None, 3) for l in open(f"{o}/recheck-rss.txt")]
verdict("3. redis A3 peak RSS kB", [js(l[3])["peak_rss_kb"] for l in rss if l[0] == "base"], [js(l[3])["peak_rss_kb"] for l in rss if l[0] == "new"], higher_better=False)
PY
    ;;
  *) echo "usage: $0 suite|generic|exp3 [repo...]|retime|report|recheck|recheck-report" >&2; exit 2 ;;
esac
