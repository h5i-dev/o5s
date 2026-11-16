#!/usr/bin/env bash
# Dogfooding (ROADMAP net item 5): a long-lived o5s server on this host that the project
# itself uses: this repo mirrored through the Git front, and pip/npm routed through the
# PyPI and npm proxy-caches.
#
#   scripts/dogfood.sh start | stop | status | mirror | env | drill | upgrade-drill
#
# Data: /mnt/scratch/pico2peta/dogfood (repo, audit, tokens, log). Port 18100 on 127.0.0.1.
set -euo pipefail
D=/mnt/scratch/pico2peta/dogfood
PORT=${DOGFOOD_PORT:-18100}
BIN=${DOGFOOD_BIN:-/mnt/scratch/pico2peta/dogfood/bin/o5s-server}
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
      cp /mnt/scratch/pico2peta/cargo-target-net/release/o5s-server "$BIN"
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
    git push -q "http://dogfood:$(cat "$D/token")@127.0.0.1:$PORT/git/o5s/o5s.git" \
      'refs/remotes/origin/*:refs/heads/*' 'refs/tags/*:refs/tags/*' 2>&1 | grep -v "^$" || true
    echo "mirrored to git/o5s/o5s"
    ;;
  drill)
    # Backup -> restore -> fsck drill (docs/ARCHITECTURE.md "Backup, restore and integrity"):
    # refs/ first, then objects/, while the server keeps running; then restore elsewhere,
    # fsck it, serve it on another port and clone the mirror from it.
    CLI=${DOGFOOD_CLI:-/mnt/scratch/pico2peta/cargo-target-net/release/o5s}
    B="$D/drill"
    rm -rf "$B"; mkdir -p "$B/backup"
    t0=$(date +%s.%N)
    cp -a "$D/repo/refs" "$B/backup/refs"
    for f in "$D/repo"/*; do
      n=$(basename "$f"); [ "$n" = refs ] && continue
      cp -a "$f" "$B/backup/$n"
    done
    echo "backup: $(du -sh "$B/backup" | cut -f1) in $(echo "$(date +%s.%N) - $t0" | bc) s"
    cp -a "$B/backup" "$B/restored"
    "$CLI" --repo "$B/restored" fsck && echo "fsck of the restore: clean"
    sed -e "s#$D/repo#$B/restored#; s#127.0.0.1:$PORT#127.0.0.1:$((PORT + 1))#; s#$D/audit#$B#; s#interval_secs = 21600#interval_secs = 0#" \
      "$D/server.toml" > "$B/server.toml"
    "$BIN" serve --config "$B/server.toml" >> "$B/server.log" 2>&1 &
    pid=$!
    sleep 2
    t=$(cat "$D/token")
    git clone -q --mirror "http://dogfood:$t@127.0.0.1:$((PORT + 1))/git/o5s/o5s.git" "$B/clone.git"
    git -C "$B/clone.git" fsck --full --no-progress && echo "git fsck of a clone from the restore: clean"
    live=$(git ls-remote "http://dogfood:$t@127.0.0.1:$PORT/git/o5s/o5s.git" | sort | sha256sum)
    rest=$(git ls-remote "http://dogfood:$t@127.0.0.1:$((PORT + 1))/git/o5s/o5s.git" | sort | sha256sum)
    [ "$live" = "$rest" ] && echo "refs: restore == live" || echo "refs DIFFER (writes after the backup?)"
    kill $pid
    ;;
  upgrade-drill)
    # An old binary (DOGFOOD_OLD_BIN, e.g. built from origin/main) creates a repository and
    # writes through its fronts; the new binary then opens it, serves it and fscks it.
    OLD=${DOGFOOD_OLD_BIN:?set DOGFOOD_OLD_BIN}
    CLI=${DOGFOOD_CLI:-/mnt/scratch/pico2peta/cargo-target-net/release/o5s}
    U="$D/upgrade"
    rm -rf "$U"; mkdir -p "$U"
    sed -e "s#$D/repo#$U/repo#; s#127.0.0.1:$PORT#127.0.0.1:$((PORT + 2))#; s#$D/audit/audit.jsonl#$U/audit.jsonl#; s#interval_secs = 21600#interval_secs = 0#" \
      "$D/server.toml" | grep -v "^rotate_bytes" > "$U/server.toml"
    t=$(cat "$D/token")
    url="http://dogfood:$t@127.0.0.1:$((PORT + 2))/git/o5s/o5s.git"
    "$OLD" serve --config "$U/server.toml" >> "$U/old.log" 2>&1 & pid=$!; sleep 2
    git push -q "$url" 'refs/remotes/origin/main:refs/heads/main' 2>&1 | grep -v "^$" || true
    curl -sf -H "Authorization: Bearer $t" -T README.md "http://127.0.0.1:$((PORT + 2))/v1/objects" > "$U/put.json"
    kill $pid; sleep 1
    "$BIN" serve --config "$U/server.toml" >> "$U/new.log" 2>&1 & pid=$!; sleep 2
    h=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["hash"])' "$U/put.json")
    # Objects are readable by hash only from a name that reaches them (object-read authorization).
    curl -sf -o /dev/null -H "Authorization: Bearer $t" -H "Content-Type: application/json" \
      -d "{\"name\":\"dogfood/upgrade/readme\",\"read_set\":[{\"name\":\"dogfood/upgrade/readme\",\"observed\":null}],\"new\":\"$h\"}" \
      "http://127.0.0.1:$((PORT + 2))/v1/replace"
    curl -sf -H "Authorization: Bearer $t" "http://127.0.0.1:$((PORT + 2))/v1/objects/$h" | cmp - README.md && echo "object written by the old binary: readable"
    git clone -q --bare "$url" "$U/clone.git" && git -C "$U/clone.git" fsck --full --no-progress && echo "git repo written by the old binary: clone + fsck clean"
    git push -q "$url" 'refs/remotes/origin/net:refs/heads/net' 2>&1 | grep -v "^$" || true
    git clone -q --bare "$url" "$U/clone2.git" && git -C "$U/clone2.git" fsck --full --no-progress && echo "new pushes on the upgraded repo: clone + fsck clean"
    kill $pid; sleep 1
    "$CLI" --repo "$U/repo" fsck && echo "fsck after the upgrade: clean"
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
