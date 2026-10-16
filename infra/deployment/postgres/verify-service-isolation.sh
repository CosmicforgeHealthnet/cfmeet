#!/usr/bin/env bash
# Certify the per-service database isolation that provision-service-database.sh sets up
# (Part K section 120): on a throwaway set of databases and roles (random names and
# passwords, dropped afterwards) prove that
#
#   * each runtime role reads and writes its own service's data and nothing else;
#   * no runtime role can run DDL (CREATE / ALTER / DROP / TRUNCATE) in its own database;
#   * no role of one service can even connect to another service's database
#     (Calling cannot open Platform Access's, Numbering's, Privacy Relay's, Telephony's or
#     the SMS Gateway's,
#     and so on);
#   * when <SERVICE>_MIGRATION_BIN is set (CALLING_MIGRATION_BIN,
#     PLATFORM_ACCESS_MIGRATION_BIN, NUMBERING_MIGRATION_BIN, PRIVACY_RELAY_MIGRATION_BIN,
#     TELEPHONY_MIGRATION_BIN, SMS_GATEWAY_MIGRATION_BIN), the service's REAL migrations apply as its migrator role
#     and the runtime role can read every table they create.
#
#   verify-service-isolation.sh --admin-url <superuser url>
set -euo pipefail

admin=""
while [ $# -gt 0 ]; do
  case "$1" in
    --admin-url) admin="$2"; shift 2 ;;
    *) echo "verify: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$admin" ] || { echo "verify: --admin-url is required" >&2; exit 2; }
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
tag="iso$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')_"
services=(calling platform-access numbering privacy-relay telephony sms-gateway)
failures=0

url_for() { # url_for <role> <password> <database>
  python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); host=p.hostname+(":"+str(p.port) if p.port else ""); print(u.urlunsplit(p._replace(netloc=u.quote(sys.argv[2])+":"+u.quote(sys.argv[3])+"@"+host, path="/"+sys.argv[4])))' "$admin" "$1" "$2" "$3"
}
base() { echo "$1" | tr '-' '_'; }
db_of() { echo "${tag}cfmeet_$(base "$1")"; }
runtime_of() { echo "${tag}$(base "$1")"; }
pass() { echo "  ok   $*"; }
fail() { echo "  FAIL $*"; failures=$((failures + 1)); }
expect_ok() { local what="$1"; shift; if "$@" >/dev/null 2>&1; then pass "$what"; else fail "$what"; fi; }
# A refusal counts only when PostgreSQL reports a privilege error (not a typo or an outage).
expect_refused() {
  local what="$1" err; shift
  if err="$("$@" 2>&1 >/dev/null)"; then fail "$what (was allowed)"; return; fi
  if echo "$err" | grep -qiE "permission denied|must be owner"; then pass "$what (refused)"; else fail "$what (failed for another reason: $err)"; fi
}

cleanup() {
  for s in "${services[@]}"; do
    psql "$admin" -Xq -c "DROP DATABASE IF EXISTS \"$(db_of "$s")\" WITH (FORCE)" >/dev/null 2>&1 || true
    psql "$admin" -Xq -c "DROP ROLE IF EXISTS \"$(runtime_of "$s")\", \"$(runtime_of "$s")_migrator\"" >/dev/null 2>&1 || true
  done
  rm -rf "$work"
}
trap cleanup EXIT

for s in "${services[@]}"; do
  od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$work/$s.migrator"
  od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$work/$s.runtime"
  "$here/provision-service-database.sh" --service "$s" --admin-url "$admin" \
    --migrator-password-file "$work/$s.migrator" --runtime-password-file "$work/$s.runtime" \
    --database "$(db_of "$s")" --role-prefix "$tag" >/dev/null
done

for s in "${services[@]}"; do
  echo "== $s"
  db="$(db_of "$s")"; rt="$(runtime_of "$s")"; mg="${rt}_migrator"
  mig_url="$(url_for "$mg" "$(cat "$work/$s.migrator")" "$db")"
  rt_url="$(url_for "$rt" "$(cat "$work/$s.runtime")" "$db")"
  env_name="$(echo "$s" | tr 'a-z-' 'A-Z_')_MIGRATION_BIN"
  bin="${!env_name:-}"
  if [ -n "$bin" ]; then
    expect_ok "real migrations apply as the migrator role" env DATABASE_URL="$mig_url" "$bin" up
    tables="$(psql "$mig_url" -XAtq -c "SELECT string_agg(format('SELECT 1 FROM %I LIMIT 0', tablename), '; ') FROM pg_tables WHERE schemaname = 'public'")"
    expect_ok "runtime role reads every migrated table" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "$tables"
  fi
  psql "$mig_url" -Xq -v ON_ERROR_STOP=1 -c "CREATE TABLE isolation_probe (id BIGSERIAL PRIMARY KEY, v TEXT)" >/dev/null
  expect_ok "runtime role inserts / updates / deletes its own data" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 \
    -c "INSERT INTO isolation_probe (v) VALUES ('a'); UPDATE isolation_probe SET v = 'b'; DELETE FROM isolation_probe"
  expect_refused "runtime role CREATE TABLE" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "CREATE TABLE intruder (id INT)"
  expect_refused "runtime role ALTER TABLE" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "ALTER TABLE isolation_probe ADD COLUMN x INT"
  expect_refused "runtime role DROP TABLE" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "DROP TABLE isolation_probe"
  expect_refused "runtime role TRUNCATE" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "TRUNCATE isolation_probe"
  expect_refused "runtime role CREATE TEMP TABLE" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "CREATE TEMP TABLE t (id INT)"
  for other in "${services[@]}"; do
    [ "$other" = "$s" ] && continue
    odb="$(db_of "$other")"
    expect_refused "$s runtime role connects to the $other database" psql "$(url_for "$rt" "$(cat "$work/$s.runtime")" "$odb")" -Xq -c "SELECT 1"
    expect_refused "$s migrator role connects to the $other database" psql "$(url_for "$mg" "$(cat "$work/$s.migrator")" "$odb")" -Xq -c "SELECT 1"
  done
done

if [ "$failures" -ne 0 ]; then
  echo "service database isolation: FAILED ($failures check(s))"
  exit 1
fi
echo "service database isolation: OK"
