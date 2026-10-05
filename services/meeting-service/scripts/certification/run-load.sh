#!/usr/bin/env bash
# Part M control-plane load, migration and query-plan certification.
#
# Requires PostgreSQL (TEST_DATABASE_URL) and Redis (TEST_REDIS_URL). Reports are written
# as JSON to CFMEET_CERT_OUT (default ./target/certification). Sizes are environment
# driven (see tests/load_certification.rs); defaults: burst join and WebSocket fanout at
# 25/50/100/250/500, 60 s waiting-room churn, 300-series scheduler race, 300 s soak,
# 3 000-meeting migration dataset, 20 000-space query-plan volume.
#
# LOAD_SELECTION limits the run, e.g. LOAD_SELECTION="burst fanout migration".
# External target: LOAD_BASE_URL + LOAD_DATABASE_URL + LOAD_JWT_SECRET (+ LOAD_SERVER_PID).
set -euo pipefail
cd "$(dirname "$0")/../.."
: "${TEST_DATABASE_URL:?TEST_DATABASE_URL required}"
: "${TEST_REDIS_URL:?TEST_REDIS_URL required}"
export CFMEET_CERT_OUT="${CFMEET_CERT_OUT:-$PWD/target/certification}"
mkdir -p "$CFMEET_CERT_OUT"
selection="${LOAD_SELECTION:-burst fanout churn scheduler soak migration plans}"
run() { # name, test binary, test name
  echo "=== $1"
  cargo test --test "$2" -- --ignored --nocapture --test-threads=1 --exact "$3" 2>&1 | tee "$CFMEET_CERT_OUT/$1.log" | grep -E '^(LOAD|SOAK|MIGRATION|QUERY_PLANS|test result)' | cut -c1-400
}
for s in $selection; do
  case "$s" in
    burst) run burst-join load_certification load_burst_join ;;
    fanout) run ws-fanout load_certification load_ws_fanout ;;
    churn) run waiting-room-churn load_certification load_waiting_room_churn ;;
    scheduler) run scheduler load_certification load_scheduler ;;
    soak) run leak-soak load_certification load_resource_leak_soak ;;
    migration)
      echo "=== migration"
      cargo test --test migration_certification -- --nocapture 2>&1 | tee "$CFMEET_CERT_OUT/migration.log" | grep -E '^(MIGRATION input|test result)' | cut -c1-400 ;;
    plans) run query-plans query_plans query_plans_at_representative_volume ;;
    *) echo "unknown selection: $s" >&2; exit 2 ;;
  esac
done
echo "Reports: $CFMEET_CERT_OUT"
