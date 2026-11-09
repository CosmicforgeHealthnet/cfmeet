#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Verify a restored service database against the manifest written by backup.sh.
#
#   verify-restore.sh --manifest <file.dump.manifest.json> <connection of the restored db>
#                     [--pg-wrapper CMD]
#
#   <connection>  --database-url-file F, --database-url URL, DATABASE_URL, or an already
#                 prepared libpq environment with PGDATABASE set (restore-drill.sh).
#
# Checks (each printed as ok / FAIL; nothing about row contents or credentials is printed):
#   * the restored database has public tables (count > 0) and exactly as many as the source;
#   * every REQUIRED migration-history table of the service (catalog.conf) is present and
#     non-empty, and every identity table is present;
#   * every history table the manifest recorded as present is present with the same row
#     count and the same latest version.
#
# Exit status: 0 all checks passed; 1 at least one check failed; 2 usage; 3 tool missing.
set -euo pipefail
CFB_PROG=verify-restore.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

manifest="" url="" url_file=""
while [ $# -gt 0 ]; do
  case "$1" in
    --manifest) manifest="$2"; shift 2 ;;
    --database-url) url="$2"; shift 2 ;;
    --database-url-file) url_file="$2"; shift 2 ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[ -f "$manifest" ] || cfb_die 2 "--manifest is required and must exist"
cfb_require_pg_tools psql
cfb_require_tools python3
service="$(python3 "$CFB_DIR/manifest.py" get "$manifest" service)"
cfb_require_active_service "$service"
cfb_connect "$url" "$url_file" ""

failures=0
pass() { echo "  ok   $*"; }
fail() {
  echo "  FAIL $*"
  failures=$((failures + 1))
}

echo "== verify restored $service database $PGDATABASE against $(basename "$manifest")"
tables="$(cfb_public_table_count)"
expected_tables="$(python3 "$CFB_DIR/manifest.py" get "$manifest" public_table_count)"
if [ "$tables" -gt 0 ]; then pass "public tables present ($tables)"; else fail "no public tables restored"; fi
if [ "$tables" = "$expected_tables" ]; then pass "table count matches the manifest"; else fail "table count $tables, manifest recorded $expected_tables"; fi

for table in "${CFB_REQUIRED[@]}"; do
  if cfb_table_exists "$table"; then
    rows="$(cfb_sql "SELECT count(*) FROM public.$table")"
    if [ "$rows" -gt 0 ]; then pass "required history $table present and non-empty ($rows rows)"; else fail "required history $table is empty"; fi
  else
    fail "required history $table is missing"
  fi
done
for table in "${CFB_IDENTITY[@]}"; do
  if cfb_table_exists "$table"; then pass "identity table $table present"; else fail "identity table $table is missing"; fi
done

while IFS='|' read -r name _required present rows max_version; do
  [ -n "$name" ] || continue
  [[ "$name" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || { fail "manifest names an invalid table"; continue; }
  if [ "$present" != true ]; then
    if cfb_table_exists "$name"; then fail "$name exists but was absent at backup time"; else pass "$name absent, as at backup time"; fi
    continue
  fi
  if ! cfb_table_exists "$name"; then
    fail "$name recorded in the manifest but missing after restore"
    continue
  fi
  got_rows="$(cfb_sql "SELECT count(*) FROM public.$name")"
  got_max="$(cfb_sql "SELECT coalesce(max(version)::text, '') FROM public.$name")"
  if [ "$got_rows" = "$rows" ]; then pass "$name rows match the manifest ($rows)"; else fail "$name has $got_rows rows, manifest recorded $rows"; fi
  if [ "$got_max" = "$max_version" ]; then pass "$name latest version matches"; else fail "$name latest version differs from the manifest"; fi
done < <(python3 "$CFB_DIR/manifest.py" history "$manifest")

if [ "$failures" -ne 0 ]; then
  echo "RESTORE_VERIFICATION=FAILED ($failures check(s))"
  exit 1
fi
echo "RESTORE_VERIFICATION=PASS"
