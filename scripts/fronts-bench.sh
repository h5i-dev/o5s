#!/usr/bin/env bash
# PLAN §6.5 front benchmarks, one batch: builds the release server, then runs every front's
# benchmark under the shared bench lock and renders the report.
#
#   scripts/fronts-bench.sh [front...]      # default: git oci hf pypi npm maven
#
# Results: $FRONTS_RESULTS (default /mnt/scratch/pico2peta/bench-fronts-results.jsonl),
# report:  $FRONTS_REPORT  (default /mnt/scratch/pico2peta/bench-fronts-report.md).
# Every benchmark deletes what it created; the work dir is /mnt/scratch/pico2peta/bench-fronts.
set -euo pipefail
cd "$(dirname "$0")/.."
source /home/ec2-user/Dev/pico2peta/scripts/env.sh
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR_NET:-/mnt/scratch/pico2peta/cargo-target-net}
export FRONTS_RESULTS=${FRONTS_RESULTS:-/mnt/scratch/pico2peta/bench-fronts-results.jsonl}
REPORT=${FRONTS_REPORT:-/mnt/scratch/pico2peta/bench-fronts-report.md}
FRONTS=("$@")
[ ${#FRONTS[@]} -eq 0 ] && FRONTS=(git oci hf pypi npm maven)

cargo build -j2 -q --release -p o5s-server --bin o5s-server --example xet_dedup
BIN=$CARGO_TARGET_DIR/release/o5s-server
DEDUP=$CARGO_TARGET_DIR/release/examples/xet_dedup
{
  echo "# host: $(uname -n) $(nproc) vCPU; $(date -u +%FT%TZ); commit $(git rev-parse --short HEAD)"
} >&2

for f in "${FRONTS[@]}"; do
  echo "== $f" >&2
  case $f in
    hf) flock /mnt/scratch/pico2peta/bench.lock python3 bench/fronts/hf_bench.py "$BIN" "$DEDUP" ;;
    *) flock /mnt/scratch/pico2peta/bench.lock python3 "bench/fronts/${f}_bench.py" "$BIN" ;;
  esac
done
python3 bench/fronts/report.py "$FRONTS_RESULTS" > "$REPORT"
rm -rf /mnt/scratch/pico2peta/bench-fronts
echo "report: $REPORT" >&2
