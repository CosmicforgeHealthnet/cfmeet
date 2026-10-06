#!/usr/bin/env bash
# Real self-hosted LiveKit certification for meeting-service Parts E–I.
#
# Requirements: a livekit-server binary (LIVEKIT_SERVER_BIN), the rtcprobe WebRTC client
# (RTCPROBE_BIN, `go build ./rtcprobe`), PostgreSQL (TEST_DATABASE_URL) and Redis
# (TEST_REDIS_URL). No Docker needed.
#
# Part G (real composite recording + real object storage) additionally needs:
#   EGRESS_BIN  livekit egress binary (`go build ./cmd/server` in livekit/egress; Chrome +
#               PulseAudio available on the host)
#   MINIO_BIN   MinIO server binary (S3-compatible object storage)
# When both are set, livekit-server is started with Redis (required by Egress), MinIO
# and Egress are started on loopback, and the Part G tests run against them.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
: "${LIVEKIT_SERVER_BIN:?set LIVEKIT_SERVER_BIN to a livekit-server binary}"
: "${RTCPROBE_BIN:?set RTCPROBE_BIN to the rtcprobe binary}"
log="${LIVEKIT_LOG:-/tmp/livekit-certification.log}"
work="$(mktemp -d)"
pids=()
# Teardown is best-effort and never decides the result: the exit status is the test run's.
# (The MinIO container writes root-owned files into $work, which a non-root CI user cannot
# remove; under `set -e` a failing `rm` here would turn a passing run into exit 1.)
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$work" 2>/dev/null || true; }
trap cleanup EXIT

lk_config="$here/livekit.yaml"
if [[ -n "${EGRESS_BIN:-}" && -n "${MINIO_BIN:-}" ]]; then
  redis_hostport="$(echo "${TEST_REDIS_URL:?TEST_REDIS_URL required for egress}" | sed -E 's#^redis://([^/]+).*#\1#')"
  s3_port="${CFMEET_CERT_S3_PORT:-9100}"
  s3_key=cfmeetcert
  s3_secret=cfmeetcert-secret-0123456789
  s3_bucket=cfmeet-artifacts
  { cat "$here/livekit.yaml"; printf 'redis:\n  address: %s\n  db: 3\n' "$redis_hostport"; } >"$work/livekit.yaml"
  lk_config="$work/livekit.yaml"
  MINIO_ROOT_USER="$s3_key" MINIO_ROOT_PASSWORD="$s3_secret" \
    "$MINIO_BIN" server "$work/minio" --address "127.0.0.1:$s3_port" --console-address 127.0.0.1:0 \
    >"$work/minio.log" 2>&1 &
  pids+=($!)
  sed -e "s#@REDIS@#$redis_hostport#" -e "s#@S3_ENDPOINT@#http://127.0.0.1:$s3_port#" \
      -e "s#@S3_KEY@#$s3_key#" -e "s#@S3_SECRET@#$s3_secret#" -e "s#@S3_BUCKET@#$s3_bucket#" \
      "$here/egress.yaml.tmpl" >"$work/egress.yaml"
  export CFMEET_REAL_S3_ENDPOINT="http://127.0.0.1:$s3_port"
  export CFMEET_REAL_S3_BUCKET="$s3_bucket"
  export CFMEET_REAL_S3_ACCESS_KEY="$s3_key"
  export CFMEET_REAL_S3_SECRET_KEY="$s3_secret"
  export CFMEET_REAL_EGRESS=1
fi

# A livekit-server from a previous run may still be releasing its ports (back-to-back
# media-load runs); never start against a half-stopped server.
for _ in $(seq 1 60); do
  curl -sf http://127.0.0.1:7880/ >/dev/null || break
  sleep 1
done
"$LIVEKIT_SERVER_BIN" --config "$lk_config" >"$log" 2>&1 &
pids+=($!)
ready=""
for _ in $(seq 1 150); do
  curl -sf http://127.0.0.1:7880/ >/dev/null && { ready=1; break; }
  sleep 0.2
done
[[ -n "$ready" ]] || { echo "livekit-server did not become ready on 127.0.0.1:7880" >&2; exit 1; }
if [[ -n "${CFMEET_REAL_EGRESS:-}" ]]; then
  for _ in $(seq 1 100); do
    curl -sf "http://127.0.0.1:${CFMEET_CERT_S3_PORT:-9100}/minio/health/live" >/dev/null && break
    sleep 0.2
  done
  EGRESS_CONFIG_FILE="$work/egress.yaml" "$EGRESS_BIN" >"${EGRESS_LOG:-/tmp/livekit-egress.log}" 2>&1 &
  pids+=($!)
  # Egress (a large container on a cold runner) is ready when its health port answers
  # (`health_port` in egress.yaml.tmpl); then give it a moment to register with
  # livekit-server over Redis.
  for _ in $(seq 1 180); do
    curl -s -o /dev/null "http://127.0.0.1:7981/" && break
    sleep 1
  done
  sleep 3
fi
export CFMEET_REAL_LIVEKIT_URL=ws://127.0.0.1:7880
export CFMEET_REAL_LIVEKIT_KEY=cfmeet-cert-key
export CFMEET_REAL_LIVEKIT_SECRET=cfmeet-local-certification-secret-0123456789
export CFMEET_REAL_LIVEKIT_WEBHOOK_PORT=18089
cd "$here/../.."
cargo test --test livekit_certification -- --ignored --nocapture --test-threads=1 ${CERT_FILTER:-}
