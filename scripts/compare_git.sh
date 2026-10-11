#!/usr/bin/env bash
# Run o5s and the plain-Git baseline on the same workloads and write one JSON
# report per run. Each run starts a fresh o5s server (local log, or PostgreSQL
# when O5S_DATABASE_URL is set) on its own data directory.
#   scripts/compare_git.sh OUT_DIR [AGENTS] [CONCURRENCY]
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:?OUT_DIR}"; agents="${2:-200}"; concurrency="${3:-32}"
target="${CARGO_TARGET_DIR:-target}"
cargo build -q --release -p o5s-server -p o5s-load -p o5s-cli
mkdir -p "$out"
export H5I_APP_SECRET="compare-$(date +%s%N)" O5S_LOG_REQUESTS=0
port=39$((RANDOM % 90 + 10))
for spec in "disjoint 1" "disjoint 4" "overlap 1" "files 1" "repositories 1"; do
    set -- $spec
    name="$1-s$2-a$agents-c$concurrency"
    data="$out/$name.data"; rm -rf "$data"; mkdir -p "$data"
    O5S_DATA="$data" O5S_BIND="127.0.0.1:$port" O5S_DATABASE_SCHEMA="o5s_cmp_${1}_$2" "$target/release/o5s-server" >"$data/server.log" 2>&1 &
    server=$!
    sleep 1
    "$target/release/o5s-load" compare --url "http://127.0.0.1:$port" --agents "$agents" --concurrency "$concurrency" \
        --workload "$1" --spacing "$2" --dir "$data/git" >"$out/$name.json"
    # The same o5s workload, proposed by `o5s propose` in per-agent Git repositories.
    "$target/release/o5s-load" o5s --url "http://127.0.0.1:$port" --agents "$agents" --concurrency "$concurrency" \
        --workload "$1" --spacing "$2" --cli "$target/release/o5s" >"$out/$name-cli.json"
    kill "$server"; wait "$server" 2>/dev/null || true
    rm -rf "$data"
done
