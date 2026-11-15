#!/usr/bin/env bash
# Dogfooding (ROADMAP net item 5): a long-lived pico2peta server on this host that the project
# itself uses: this repo mirrored through the Git front, and pip/npm routed through the
# PyPI and npm proxy-caches.
#
#   scripts/dogfood.sh start | stop | status | mirror | env
#
# Data: /mnt/scratch/pico2peta/dogfood (repo, audit, tokens, log). Port 18100 on 127.0.0.1.
set -euo pipefail
D=/mnt/scratch/pico2peta/dogfood
PORT=${DOGFOOD_PORT:-18100}
BIN=${DOGFOOD_BIN:-/mnt/scratch/pico2peta/dogfood/bin/pico2peta-server}
cd "$(dirname "$0")/.."

init() {
  mkdir -p "$D/bin" "$D/audit"
  if [ ! -f "$D/token" ]; then
    head -c 24 /dev/urandom | base64 | tr -dc 'A-Za-z0-9' > "$D/token"
    chmod 600 "$D/token"
  fi
  local sha
  sha=$(sha256sum < "$D/token" | cut -d' ' -f1)
  cat > "$D/server.toml" <<TOML
[server]
listen = "127.0.0.1:$PORT"
max_upload_bytes = 17179869184

[repo]
path = "$D/repo"
create = true

[auth]
allow_anonymous = false
[[auth.tokens]]
principal = "dogfood"
sha256 = "$sha"

[[policy]]
principal = "dogfood"
namespace = ""
access = "admin"

[audit]
path = "$D/audit/audit.jsonl"
rotate_bytes = 268435456

[gc]
interval_secs = 21600
grace_secs = 3600

[log]
format = "text"
filter = "info"

[pypi.upstream.pypi]
url = "https://pypi.org/simple"

[npm.upstream.npmjs]
url = "https://registry.npmjs.org"

[maven.upstream.central]
url = "https://repo.maven.apache.org/maven2"
TOML
}

case "${1:-status}" in
  start)
    init
    # A copy of the binary, so rebuilding the target dir doesn't change the running server.
    if [ ! -x "$BIN" ] || [ "${DOGFOOD_UPGRADE:-0}" = 1 ]; then
      cp /mnt/scratch/pico2peta/cargo-target-net/release/pico2peta-server "$BIN"
    fi
    nohup "$BIN" serve --config "$D/server.toml" >> "$D/server.log" 2>&1 &
    echo $! > "$D/pid"
    sleep 2
    curl -sf "http://127.0.0.1:$PORT/healthz" && echo " started (pid $(cat "$D/pid"))"
    ;;
  stop)
    [ -f "$D/pid" ] && kill "$(cat "$D/pid")" && rm -f "$D/pid" && echo stopped
    ;;
  status)
    curl -sf "http://127.0.0.1:$PORT/readyz" && echo
    ;;
  mirror)
    # Mirror this repo's branches (net, demo, main) and tags through the Git front.
    git push -q "http://dogfood:$(cat "$D/token")@127.0.0.1:$PORT/git/pico2peta/pico2peta.git" \
      'refs/remotes/origin/*:refs/heads/*' 'refs/tags/*:refs/tags/*' 2>&1 | grep -v "^$" || true
    echo "mirrored to git/pico2peta/pico2peta"
    ;;
  env)
    # Source this to route pip/uv/npm through the proxy-caches.
    t=$(cat "$D/token")
    echo "export PIP_INDEX_URL=http://dogfood:$t@127.0.0.1:$PORT/pypi/pypi/simple/"
    echo "export UV_INDEX_URL=http://dogfood:$t@127.0.0.1:$PORT/pypi/pypi/simple/"
    echo "export npm_config_registry=http://127.0.0.1:$PORT/npm/npmjs/"
    echo "export NPM_CONFIG_USERCONFIG=$D/npmrc"
    printf 'registry=http://127.0.0.1:%s/npm/npmjs/\n//127.0.0.1:%s/npm/npmjs/:_authToken=%s\n' "$PORT" "$PORT" "$t" > "$D/npmrc"
    ;;
esac
