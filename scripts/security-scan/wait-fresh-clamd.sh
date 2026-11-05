#!/usr/bin/env bash
# Waits for a clamd container to answer, updates its signatures and waits until the
# daily database is at most 72 h old (the gateway refuses older signatures).
#
#   scripts/security-scan/wait-fresh-clamd.sh <container> <host:port>
set -euo pipefail
container="$1"; host="${2%:*}"; port="${2##*:}"
command -v nc >/dev/null || sudo apt-get install -y -qq netcat-openbsd >/dev/null
ask() { printf 'z%s\0' "$1" | nc -w 5 "$host" "$port" | tr -d '\0'; }
for _ in $(seq 1 150); do [ "$(ask PING || true)" = "PONG" ] && break; sleep 2; done
[ "$(ask PING || true)" = "PONG" ] || { docker logs "$container" | tail -50; exit 1; }
echo "before: $(ask VERSION)"
docker exec "$container" freshclam --stdout --foreground || docker exec "$container" freshclam --stdout || true
ask RELOAD || true
for _ in $(seq 1 90); do
  v=$(ask VERSION || true)
  built=$(printf '%s' "$v" | awk -F/ '{print $3}')
  if [ -n "$built" ]; then
    age=$(( $(date -u +%s) - $(date -u -d "$built" +%s) ))
    if [ "$age" -le $((72 * 3600)) ]; then echo "fresh: $v (age ${age}s)"; exit 0; fi
  fi
  sleep 5
done
echo "signatures still stale: $(ask VERSION || true)"; docker logs "$container" | tail -50; exit 1
