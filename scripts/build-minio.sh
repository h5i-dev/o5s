#!/usr/bin/env bash
# Builds a MinIO server binary from a pinned source release.
# MinIO stopped publishing community binaries and container images in 2025,
# so CI and dev machines build it from source.
#   scripts/build-minio.sh <output-path>
set -euo pipefail
TAG=${MINIO_TAG:-RELEASE.2025-09-07T16-13-09Z}
OUT=${1:?usage: $0 <output-path>}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export GOPROXY=${GOPROXY:-https://proxy.golang.org,direct} GOFLAGS=${GOFLAGS:--p=2} CGO_ENABLED=0
curl -sSfL "https://github.com/minio/minio/archive/refs/tags/$TAG.tar.gz" | tar xz -C "$WORK"
(cd "$WORK/minio-$TAG" && go build -trimpath -o "$OUT" .)
"$OUT" --version | head -1
