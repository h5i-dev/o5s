# Source this before building: keeps all large build/download state off the nearly-full root disk.
#   source scripts/env.sh
export SCRATCH=/mnt/scratch/pico2peta
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$SCRATCH/cargo-target}"   # an exported target dir (e.g. the net session's) wins
export CARGO_HOME="$SCRATCH/cargo-home"   # crate registry/git checkouts (rustup proxies in ~/.cargo/bin still work)
export TMPDIR="${TMPDIR:-$SCRATCH/tmp}"
export BENCH_DATA="$SCRATCH/bench-data"
export PATH="$HOME/.cargo/bin:$PATH"
# Lean 4 (prebuilt, shared on scratch — do not reinstall)
export ELAN_HOME=/mnt/scratch/ec2-user/elan
export PATH="$ELAN_HOME/bin:$PATH"
# Keep debug builds small: scratch is shared and has filled up before.
export CARGO_INCREMENTAL=0
export CARGO_PROFILE_DEV_DEBUG=line-tables-only
export CARGO_PROFILE_TEST_DEBUG=line-tables-only
