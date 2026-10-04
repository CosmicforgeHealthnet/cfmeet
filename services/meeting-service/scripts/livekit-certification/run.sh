#!/usr/bin/env bash
# Real self-hosted LiveKit certification for meeting-service Part E.
#
# Requirements: a livekit-server binary (LIVEKIT_SERVER_BIN), the rtcprobe WebRTC client
# (RTCPROBE_BIN, `go build ./rtcprobe`), PostgreSQL (TEST_DATABASE_URL) and Redis
# (TEST_REDIS_URL). No Docker needed.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
: "${LIVEKIT_SERVER_BIN:?set LIVEKIT_SERVER_BIN to a livekit-server binary}"
: "${RTCPROBE_BIN:?set RTCPROBE_BIN to the rtcprobe binary}"
log="${LIVEKIT_LOG:-/tmp/livekit-certification.log}"
"$LIVEKIT_SERVER_BIN" --config "$here/livekit.yaml" >"$log" 2>&1 &
lk_pid=$!
trap 'kill $lk_pid 2>/dev/null || true' EXIT
for _ in $(seq 1 50); do
  curl -sf http://127.0.0.1:7880/ >/dev/null && break
  sleep 0.2
done
export CFMEET_REAL_LIVEKIT_URL=ws://127.0.0.1:7880
export CFMEET_REAL_LIVEKIT_KEY=cfmeet-cert-key
export CFMEET_REAL_LIVEKIT_SECRET=cfmeet-local-certification-secret-0123456789
export CFMEET_REAL_LIVEKIT_WEBHOOK_PORT=18089
cd "$here/../.."
cargo test --test livekit_certification -- --ignored --nocapture --test-threads=1
