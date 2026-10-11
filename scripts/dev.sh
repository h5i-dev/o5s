#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
export O5S_DATA="${O5S_DATA:-.o5s}"
mkdir -p "$O5S_DATA"
if [[ ! -f "$O5S_DATA/secret" ]]; then
    (umask 077; python3 -c 'import secrets; print(secrets.token_hex(32))' > "$O5S_DATA/secret")
fi
export H5I_APP_SECRET="${H5I_APP_SECRET:-$(cat "$O5S_DATA/secret")}"
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_INCREMENTAL=0
cargo build -q -p o5s-server
cargo run -q -p o5s-server -- token 1 1
printf '\nAbove: author token. Reviewer token:\n'
cargo run -q -p o5s-server -- token 1 2
printf '\nCheck reporter token:\n'
cargo run -q -p o5s-server -- token 1 3
printf '\nOpen http://127.0.0.1:3000 and paste a token.\n'
exec cargo run -q -p o5s-server
