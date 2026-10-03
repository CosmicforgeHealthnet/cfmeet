#!/usr/bin/env bash
# Start local dependencies, then run the meeting service and the web app side by side.
# Ctrl+C stops both application processes (dependencies keep running;
# stop them with `make deps-down`).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MEETING_SERVICE=services/meeting-service
WEB=apps/web

if [[ ! -f "$MEETING_SERVICE/.env" ]]; then
  echo "$MEETING_SERVICE/.env not found. Copy $MEETING_SERVICE/.env.example and fill it in." >&2
  exit 1
fi
if [[ ! -f "$WEB/.env.local" && ! -f "$WEB/.env" ]]; then
  echo "warning: $WEB/.env.local not found; copy $WEB/.env.example" >&2
fi

if command -v docker >/dev/null 2>&1; then
  docker compose up -d postgres redis livekit
else
  echo "warning: docker not found; assuming PostgreSQL, Redis and LiveKit are already running" >&2
fi

(cd "$WEB" && [[ -d node_modules ]] || npm ci)

pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

(cd "$MEETING_SERVICE" && cargo run -p migration -- up && cargo run --bin backend) &
pids+=($!)
(cd "$WEB" && npm run dev) &
pids+=($!)

wait -n "${pids[@]}"
