#!/usr/bin/env bash
# Start/stop a throwaway local MinIO for the S3 integration tests.
#   scripts/minio-test.sh start   # prints the env vars to export
#   eval "$(scripts/minio-test.sh env)"
#   cargo test -p o5s-backend-s3 --test minio
#   scripts/minio-test.sh stop
# MinIO no longer ships community binaries; build one with `go build` from a
# release tag, or set MINIO_BIN. Data lives on scratch, never on /.
set -euo pipefail
SCRATCH=${SCRATCH:-/mnt/scratch/pico2peta}
MINIO_BIN=${MINIO_BIN:-$SCRATCH/minio/minio}
DIR=${MINIO_TEST_DIR:-$SCRATCH/minio-test}
PORT=${MINIO_TEST_PORT:-19000}
USER_=o5stest
PASS=o5stest-secret-key
envs() {
  echo "export O5S_S3_ENDPOINT=http://127.0.0.1:$PORT"
  echo "export O5S_S3_BUCKET=o5s-test"
  echo "export O5S_S3_REGION=us-east-1"
  echo "export AWS_ACCESS_KEY_ID=$USER_"
  echo "export AWS_SECRET_ACCESS_KEY=$PASS"
}
case "${1:-}" in
  start)
    mkdir -p "$DIR/data"
    if [ -f "$DIR/pid" ] && kill -0 "$(cat "$DIR/pid")" 2>/dev/null; then echo "already running" >&2; envs; exit 0; fi
    MINIO_ROOT_USER=$USER_ MINIO_ROOT_PASSWORD=$PASS MINIO_BROWSER=off \
      nohup "$MINIO_BIN" server "$DIR/data" --address "127.0.0.1:$PORT" --console-address "127.0.0.1:$((PORT+1))" \
      >"$DIR/minio.log" 2>&1 &
    echo $! >"$DIR/pid"
    for _ in $(seq 1 100); do
      curl -sf "http://127.0.0.1:$PORT/minio/health/ready" >/dev/null && { envs; exit 0; }
      sleep 0.1
    done
    echo "minio did not become ready; see $DIR/minio.log" >&2; exit 1 ;;
  stop)
    [ -f "$DIR/pid" ] && kill "$(cat "$DIR/pid")" 2>/dev/null || true
    rm -f "$DIR/pid" ;;
  env) envs ;;
  *) echo "usage: $0 start|stop|env" >&2; exit 2 ;;
esac
