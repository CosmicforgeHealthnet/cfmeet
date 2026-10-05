#!/bin/sh
# MINIO_BIN shim for run.sh: `minio server <dir> --address A --console-address C`
# runs an S3-compatible MinIO container on the host network.
# MINIO_IMAGE overrides the image (default: the official minio/minio image).
shift            # "server"
dir="$1"; shift
mkdir -p "$dir"
docker rm -f cfmeet-cert-minio >/dev/null 2>&1
trap 'docker rm -f cfmeet-cert-minio >/dev/null 2>&1' TERM INT EXIT
docker run --rm --name cfmeet-cert-minio --network host \
  -e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD -v "$dir":/data \
  "${MINIO_IMAGE:-minio/minio:latest}" server /data "$@" &
wait $!
