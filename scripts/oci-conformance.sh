#!/usr/bin/env bash
# Runs the OCI distribution-spec conformance suite against a throwaway
# pico2peta-server (pull, push, content discovery; deletion is unsupported).
#   scripts/oci-conformance.sh <pico2peta-server binary> [workdir]
# Needs Go (builds the suite from the pinned spec release on first use).
set -euo pipefail
BIN=${1:?usage: $0 <pico2peta-server> [workdir]}
SPEC=${OCI_SPEC_VERSION:-v1.1.1}
WORK=${2:-$(mktemp -d)}
PORT=${OCI_CONF_PORT:-18181}
SUITE="$WORK/conformance-$SPEC.test"
mkdir -p "$WORK"
if [ ! -x "$SUITE" ]; then
  curl -sSfL "https://github.com/opencontainers/distribution-spec/archive/refs/tags/$SPEC.tar.gz" | tar xz -C "$WORK"
  (cd "$WORK/distribution-spec-${SPEC#v}/conformance" && GOPROXY=${GOPROXY:-https://proxy.golang.org,direct} go test -c -o "$SUITE")
fi
SRV="$WORK/server"
rm -rf "$SRV" && mkdir -p "$SRV"
"$BIN" gen-token ci > "$SRV/gen.txt"
TOKEN=$(sed -n 's/^token: //p' "$SRV/gen.txt")
sed -n '/^\[\[auth.tokens\]\]/,$p' "$SRV/gen.txt" | sed 's/auth\.tokens/tokens/' > "$SRV/tokens.toml"
cat > "$SRV/server.toml" <<CFG
[server]
listen = "127.0.0.1:$PORT"
[repo]
path = "$SRV/repo"
create = true
[auth]
token_file = "tokens.toml"
[[policy]]
principal = "ci"
namespace = "oci"
access = "write"
[audit]
path = "$SRV/audit.jsonl"
[log]
format = "text"
filter = "warn"
CFG
"$BIN" serve -c "$SRV/server.toml" > "$SRV/server.log" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do curl -sf "http://127.0.0.1:$PORT/healthz" >/dev/null && break; sleep 0.1; done
cd "$SRV"
OCI_ROOT_URL="http://127.0.0.1:$PORT" OCI_NAMESPACE=conformance/repo OCI_CROSSMOUNT_NAMESPACE=conformance/other \
OCI_USERNAME=ci OCI_PASSWORD="$TOKEN" OCI_TEST_PULL=1 OCI_TEST_PUSH=1 OCI_TEST_CONTENT_DISCOVERY=1 \
OCI_TEST_CONTENT_MANAGEMENT=0 OCI_HIDE_SKIPPED_WORKFLOWS=1 OCI_REPORT_DIR="$SRV" "$SUITE"
"$BIN" verify-audit "$SRV/audit.jsonl"
