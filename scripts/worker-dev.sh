#!/usr/bin/env bash
# Build the Rust Worker and run it in local workerd (wrangler dev, from the
# node:24 Docker image) against a local PostgreSQL through the Hyperdrive
# binding's local connection string. Prints the URL and the dev secret.
#   scripts/worker-dev.sh [PORT]     (O5S_DATABASE_URL defaults to scripts/postgres.sh)
set -euo pipefail
cd "$(dirname "$0")/.."
port="${1:-18791}"
url="${O5S_DATABASE_URL:-$(scripts/postgres.sh)}"
secret="${H5I_APP_SECRET:-worker-dev-secret}"
target="${CARGO_TARGET_DIR:-target}"
(cd crates/worker && worker-build --release)
cargo build -q --release -p o5s-server
O5S_DATABASE_SCHEMA=o5s "$target/release/o5s-server" ddl | psql -q "$url"
stage="$(mktemp -d)"
cp -r crates/worker/build "$stage/"
grep -v '^\[build\]\|^command = ' crates/worker/wrangler.toml >"$stage/wrangler.toml"
echo "H5I_APP_SECRET=$secret" >"$stage/.dev.vars"
chmod -R a+rwX "$stage"
docker rm -f o5s-wrangler >/dev/null 2>&1 || true
docker run -d --name o5s-wrangler --network host -v "$stage":/w -w /w -e HOME=/w/.home -e CI=1 \
    -e CLOUDFLARE_HYPERDRIVE_LOCAL_CONNECTION_STRING_HYPERDRIVE="$url" node:24-slim \
    sh -c "npx -y wrangler@4 dev --ip 127.0.0.1 --port $port --inspector-port $((port + 1))" >/dev/null
until curl -sf "http://127.0.0.1:$port/healthz" >/dev/null; do sleep 2; done
echo "o5s Worker at http://127.0.0.1:$port (H5I_APP_SECRET=$secret); stop with: docker rm -f o5s-wrangler"
