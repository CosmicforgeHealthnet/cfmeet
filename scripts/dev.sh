#!/usr/bin/env bash
# Start local dependencies, then run backend and frontend side by side.
# Ctrl+C stops both application processes (dependencies keep running;
# stop them with `make deps-down`).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [[ ! -f backend/.env ]]; then
  echo "backend/.env not found. Copy backend/.env.example and fill it in." >&2
  exit 1
fi
if [[ ! -f frontend/.env.local && ! -f frontend/.env ]]; then
  echo "warning: frontend/.env.local not found; copy frontend/.env.example" >&2
fi

if command -v docker >/dev/null 2>&1; then
  docker compose up -d postgres redis livekit
else
  echo "warning: docker not found; assuming PostgreSQL, Redis and LiveKit are already running" >&2
fi

(cd frontend && [[ -d node_modules ]] || npm ci)

pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

(cd backend && cargo run -p migration -- up && cargo run --bin backend) &
pids+=($!)
(cd frontend && npm run dev) &
pids+=($!)

wait -n "${pids[@]}"
