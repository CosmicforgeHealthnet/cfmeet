#!/usr/bin/env bash
# Restore ONE service database backup produced by backup.sh into an EMPTY database.
#
#   restore.sh --database-url <url of an empty database> --in <file.dump>
#
# Refuses a corrupted or unverifiable archive and refuses to restore over existing tables
# (a restore never merges into live state). The whole restore is one transaction: it either
# completes or leaves the target empty. After a restore, start the service's migrator (it
# must report nothing pending for the same release) and the service's workers: leases in
# the restored rows have expired by then, so pending durable work is reclaimed exactly as
# after a crash, and every paid / external action keeps its idempotency and unknown-outcome
# rules. Missing key material makes Privacy Relay fail closed; it never falls back.
set -euo pipefail

url="" in=""
while [ $# -gt 0 ]; do
  case "$1" in
    --database-url) url="$2"; shift 2 ;;
    --in) in="$2"; shift 2 ;;
    *) echo "restore.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$url" ] && [ -n "$in" ] || { echo "restore.sh: --database-url and --in are required" >&2; exit 2; }
command -v pg_restore >/dev/null || { echo "restore.sh: pg_restore not found" >&2; exit 3; }
[ -f "$in" ] && [ -f "$in.sha256" ] || { echo "restore.sh: $in or its .sha256 is missing" >&2; exit 4; }
( cd "$(dirname "$in")" && sha256sum --check --status "$(basename "$in").sha256" ) \
  || { echo "restore.sh: checksum mismatch for $in" >&2; exit 5; }

existing="$(psql "$url" -XAtq -v ON_ERROR_STOP=1 \
  -c "SELECT count(*) FROM pg_tables WHERE schemaname = 'public'")"
if [ "$existing" != "0" ]; then
  echo "restore.sh: target database is not empty ($existing tables); refusing to restore" >&2
  exit 6
fi

pg_restore --no-owner --no-privileges --exit-on-error --single-transaction --dbname="$url" "$in"
echo "restore.sh: restored $in"
