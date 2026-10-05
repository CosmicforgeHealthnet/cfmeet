#!/bin/sh
# MINIO_BIN shim for run.sh: `minio server <dir> --address A --console-address C`
# runs an S3-compatible MinIO container on the host network.
# MINIO_IMAGE overrides the image. Default: the S3-compatible MinIO build certified in
# Phase 2 (pgsty/minio, MinIO RELEASE.2026-08-04T00-00-00Z, pinned by digest) — the
# official minio/minio Docker Hub repository no longer serves public community images.
shift            # "server"
dir="$1"; shift
mkdir -p "$dir"
docker rm -f cfmeet-cert-minio >/dev/null 2>&1
trap 'docker rm -f cfmeet-cert-minio >/dev/null 2>&1' TERM INT EXIT
docker run --rm --name cfmeet-cert-minio --network host \
  -e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD -v "$dir":/data \
  "${MINIO_IMAGE:-pgsty/minio@sha256:b6bfe7239bfc83fb90d31612d9704d86039dd714f7904b3f1ad68f211e602372}" server /data "$@" &
wait $!
