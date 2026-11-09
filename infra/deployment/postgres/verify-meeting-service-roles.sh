#!/usr/bin/env bash
# Certify meeting-service's least-privilege database roles (SEC-018) against its REAL
# migration histories, on a throwaway database and roles (random names and passwords,
# dropped afterwards). Three set-ups, the same certification:
#
#   (default)            a new database provisioned by provision-service-database.sh: the
#                        runtime role cannot apply migrations; every history applies as the
#                        migrator role.
#   --rehearse-cutover   production branch A: the runtime role owns the database and applied
#                        every history itself (today's release behaviour). The production
#                        cutover (meeting-service-roles-cutover.sql) then runs: its dry run
#                        changes nothing, it refuses a superuser runtime role and the wrong
#                        database, the real run moves everything to the migrator and a re-run
#                        is a no-op. After the certification, the rollback
#                        (meeting-service-roles-rollback.sql) returns everything to the
#                        runtime role, which can migrate again.
#   --rehearse-cutover-from-superuser
#                        production branch B: the service connected as the cluster superuser,
#                        which owns everything. The cutover moves the objects from it
#                        (-v current_owner) to the migrator and a NEW runtime role gets data
#                        access; the rollback returns them to the superuser.
#
# Certification (all set-ups):
#   * every history (re-)applies as the migrator role, in release order;
#   * no object is owned by the runtime role; the migrator owns the database and every
#     object in schema public; the runtime role holds no CREATE / TEMP privilege;
#   * the runtime role reads every migrated table (history tables included), writes
#     representative tables (users, account_sessions, account_security_events), uses
#     sequences, and the triggers it depends on fire under its privileges (the
#     credentials-changed trigger revokes sessions; the security event log stays
#     append-only);
#   * the runtime role is refused DDL with a privilege error: CREATE TABLE / INDEX /
#     FUNCTION, ALTER TABLE, ALTER TYPE, DROP TABLE / TRIGGER, TRUNCATE, DISABLE TRIGGER.
#
#   MEETING_SERVICE_MIGRATION_BINS="<bin> <bin> ..." verify-meeting-service-roles.sh \
#     --admin-url <superuser url> [--rehearse-cutover | --rehearse-cutover-from-superuser]
#
# MEETING_SERVICE_MIGRATION_BINS lists the history binaries in release order (migration,
# account-security-migration, meetings-calendar-migration, profile-notifications-migration,
# account-communications-migration, ...). An entry prefixed with '?' is optional and
# skipped when not built; every other entry must exist.
set -euo pipefail

admin="" mode=fresh
while [ $# -gt 0 ]; do
  case "$1" in
    --admin-url) admin="$2"; shift 2 ;;
    --rehearse-cutover) mode=runtime-owner; shift ;;
    --rehearse-cutover-from-superuser) mode=superuser-owner; shift ;;
    *) echo "verify: unknown argument $1" >&2; exit 2 ;;
  esac
done
[ -n "$admin" ] || { echo "verify: --admin-url is required" >&2; exit 2; }
[ -n "${MEETING_SERVICE_MIGRATION_BINS:-}" ] || { echo "verify: MEETING_SERVICE_MIGRATION_BINS is required" >&2; exit 2; }
here="$(cd "$(dirname "$0")" && pwd)"
work="$(mktemp -d)"
tag="msr$(od -An -N4 -tx1 /dev/urandom | tr -d ' \n')_"
db="${tag}cfmeet_meeting_service"; rt="${tag}meeting_service"; mg="${rt}_migrator"
failures=0

url_for() { # url_for <role> <password> <database>
  python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); host=p.hostname+(":"+str(p.port) if p.port else ""); print(u.urlunsplit(p._replace(netloc=u.quote(sys.argv[2])+":"+u.quote(sys.argv[3])+"@"+host, path="/"+sys.argv[4])))' "$admin" "$1" "$2" "$3"
}
pass() { echo "  ok   $*"; }
fail() { echo "  FAIL $*"; failures=$((failures + 1)); }
expect_ok() {
  local what="$1" out; shift
  if out="$("$@" 2>&1)"; then pass "$what"; else fail "$what: $(echo "$out" | tail -3)"; fi
}
# A refusal counts only when PostgreSQL reports the expected error (not a typo or an outage).
expect_error() {
  local what="$1" pattern="$2" err; shift 2
  if err="$("$@" 2>&1 >/dev/null)"; then fail "$what (was allowed)"; return; fi
  if echo "$err" | grep -qiE "$pattern"; then pass "$what (refused)"; else fail "$what (failed for another reason: $(echo "$err" | tail -3))"; fi
}
expect_refused() { local what="$1"; shift; expect_error "$what" "permission denied|must be owner" "$@"; }
# Prints the single value of a query; used for assertions.
value() { psql "$1" -XAtq -v ON_ERROR_STOP=1 -c "$2"; }
expect_value() { # expect_value <what> <expected> <url> <sql>
  local got; got="$(value "$3" "$4" 2>&1)" || { fail "$1: $got"; return; }
  if [ "$got" = "$2" ]; then pass "$1"; else fail "$1 (expected $2, got $got)"; fi
}
owned_by() { # owned_by <role>: SQL (run as admin) counting what the role owns in the throwaway database
  echo "SELECT count(*) FROM pg_shdepend WHERE refclassid = 'pg_authid'::regclass AND deptype = 'o'
     AND refobjid = (SELECT oid FROM pg_roles WHERE rolname = '$1')
     AND (dbid = (SELECT oid FROM pg_database WHERE datname = '$db')
          OR (classid = 'pg_database'::regclass AND objid = (SELECT oid FROM pg_database WHERE datname = '$db')))"
}
public_owned_by() { # public_owned_by <role>: SQL (run in the database) counting its objects in schema public
  echo "SELECT (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                 WHERE n.nspname = 'public' AND c.relowner = (SELECT oid FROM pg_roles WHERE rolname = '$1'))
             + (SELECT count(*) FROM pg_proc f JOIN pg_namespace n ON n.oid = f.pronamespace
                 WHERE n.nspname = 'public' AND f.proowner = (SELECT oid FROM pg_roles WHERE rolname = '$1'))
             + (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                 WHERE n.nspname = 'public' AND t.typowner = (SELECT oid FROM pg_roles WHERE rolname = '$1'))"
}
cutover() { # cutover <cutover|rollback> [psql -v arguments]
  local file="$here/meeting-service-roles-$1.sql"; shift
  psql "$admin_db_url" -X -v runtime="$rt" -v migrator="$mg" -v database="$db" "$@" -f "$file"
}

cleanup() {
  psql "$admin" -Xq -c "DROP DATABASE IF EXISTS \"$db\" WITH (FORCE)" >/dev/null 2>&1 || true
  psql "$admin" -Xq -c "DROP ROLE IF EXISTS \"$rt\", \"$mg\"" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$work/migrator"
od -An -N24 -tx1 /dev/urandom | tr -d ' \n' > "$work/runtime"
mig_url="$(url_for "$mg" "$(cat "$work/migrator")" "$db")"
rt_url="$(url_for "$rt" "$(cat "$work/runtime")" "$db")"
admin_db_url="$(python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); print(u.urlunsplit(p._replace(path="/"+sys.argv[2])))' "$admin" "$db")"
admin_role="$(value "$admin" "SELECT current_user")"

bins=()
for entry in $MEETING_SERVICE_MIGRATION_BINS; do
  bin="${entry#\?}"
  if [ -x "$bin" ]; then bins+=("$bin")
  elif [ "$bin" != "$entry" ]; then echo "  skip optional $(basename "$bin") (not built)"
  else fail "migration binary $bin is missing"; fi
done
[ "${#bins[@]}" -gt 0 ] || { echo "verify: no meeting-service migration binary found"; exit 1; }

if [ "$mode" = fresh ]; then
  "$here/provision-service-database.sh" --service meeting-service --admin-url "$admin" \
    --migrator-password-file "$work/migrator" --runtime-password-file "$work/runtime" \
    --database "$db" --role-prefix "$tag" >/dev/null
  echo "== migrations (new database provisioned with separate roles)"
  expect_refused "runtime role cannot apply migrations ($(basename "${bins[0]}") up)" \
    env DATABASE_URL="$rt_url" "${bins[0]}" up
  for bin in "${bins[@]}"; do
    expect_ok "$(basename "$bin") up as the migrator role" env DATABASE_URL="$mig_url" "$bin" up
  done
else
  # Passwords travel on psql's standard input, never on a command line.
  psql "$admin" -Xq -v ON_ERROR_STOP=1 >/dev/null <<SQL
CREATE ROLE "$rt" LOGIN NOINHERIT NOSUPERUSER PASSWORD '$(cat "$work/runtime")';
CREATE ROLE "$mg" LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE PASSWORD '$(cat "$work/migrator")';
SQL
  if [ "$mode" = runtime-owner ]; then
    owner="$rt"; owner_url="$rt_url"
    echo "== branch A today: the runtime role owns the database and applies every history"
  else
    owner="$admin_role"; owner_url="$admin_db_url"
    echo "== branch B today: the superuser ($admin_role) owns the database and applies every history"
  fi
  psql "$admin" -Xq -v ON_ERROR_STOP=1 -c "CREATE DATABASE \"$db\" OWNER \"$owner\"" >/dev/null
  for bin in "${bins[@]}"; do
    expect_ok "$(basename "$bin") up as $owner (pre-cutover releases)" env DATABASE_URL="$owner_url" "$bin" up
  done
  before="$(value "$admin_db_url" "$(public_owned_by "$owner")")"
  echo "  $owner owns $before object(s) in schema public before the cutover"
  echo "== production cutover rehearsal"
  expect_ok "cutover dry run" cutover cutover -v current_owner="$owner" -v dry_run=1
  expect_value "dry run changed nothing" "$before" "$admin_db_url" "$(public_owned_by "$owner")"
  expect_error "cutover refuses a superuser runtime role" "must not be a superuser" \
    psql "$admin_db_url" -X -v runtime="$admin_role" -v migrator="$mg" -v database="$db" -f "$here/meeting-service-roles-cutover.sql"
  expect_error "cutover refuses the wrong database" "not the meeting-service database" \
    psql "$admin" -X -v runtime="$rt" -v migrator="$mg" -v database="$db" -v current_owner="$owner" -f "$here/meeting-service-roles-cutover.sql"
  expect_ok "cutover" cutover cutover -v current_owner="$owner"
  expect_ok "cutover re-run is a no-op" cutover cutover -v current_owner="$owner"
  expect_value "the previous owner owns nothing in schema public" 0 "$admin_db_url" "$(public_owned_by "$owner")"
fi
for bin in "${bins[@]}"; do
  expect_ok "$(basename "$bin") up again as the migrator role is a no-op" env DATABASE_URL="$mig_url" "$bin" up
done

echo "== ownership and privileges"
expect_value "no object (database included) is owned by the runtime role" 0 "$admin" "$(owned_by "$rt")"
expect_value "the migrator role owns the database" t "$admin" "SELECT datdba = '$mg'::regrole FROM pg_database WHERE datname = '$db'"
expect_value "every table, sequence, function and type in public is owned by the migrator" 0 "$admin_db_url" "
  SELECT (SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
           WHERE n.nspname = 'public' AND c.relowner <> '$mg'::regrole)
       + (SELECT count(*) FROM pg_proc f JOIN pg_namespace n ON n.oid = f.pronamespace
           WHERE n.nspname = 'public' AND f.proowner <> '$mg'::regrole)
       + (SELECT count(*) FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
           WHERE n.nspname = 'public' AND t.typowner <> '$mg'::regrole)"
expect_value "migrated schema is non-trivial (users, sessions, security events, enums)" 4 "$admin_db_url" "
  SELECT count(*) FROM (VALUES ('users'), ('account_sessions'), ('account_security_events'), ('user_status')) v(n)
   WHERE to_regclass(v.n) IS NOT NULL OR to_regtype(v.n) IS NOT NULL"
expect_value "runtime role: no CREATE on schema public, no CREATE / TEMP on the database, no role attributes" f "$admin_db_url" "
  SELECT has_schema_privilege('$rt', 'public', 'CREATE') OR has_database_privilege('$rt', '$db', 'CREATE')
      OR has_database_privilege('$rt', '$db', 'TEMP')
      OR (SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls FROM pg_roles WHERE rolname = '$rt')"

echo "== runtime data access"
tables="$(value "$mig_url" "SELECT string_agg(format('SELECT 1 FROM %I LIMIT 0', tablename), '; ') FROM pg_tables WHERE schemaname = 'public'")"
expect_ok "runtime role reads every migrated table (history tables included)" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "$tables"
expect_ok "runtime role inserts and updates users" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  INSERT INTO users (username, email, password_hash) VALUES ('sec018_probe', 'sec018_probe@example.invalid', 'probe-hash');
  UPDATE users SET last_login = now() WHERE username = 'sec018_probe'"
expect_ok "runtime role inserts, reads and updates account_sessions" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  INSERT INTO account_sessions (id, user_id, auth_method, authenticated_at, absolute_expires_at, current_refresh_id)
  SELECT gen_random_uuid(), id, 'PASSWORD', now(), now() + interval '1 day', gen_random_uuid() FROM users WHERE username = 'sec018_probe';
  UPDATE account_sessions SET client_label = 'sec018 probe', refresh_generation = refresh_generation + 1
   WHERE user_id = (SELECT id FROM users WHERE username = 'sec018_probe')"
expect_ok "runtime role appends account_security_events (BIGSERIAL sequence)" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  INSERT INTO account_security_events (event_id, user_id, event_type, outcome, actor_kind)
  SELECT gen_random_uuid(), id, 'ROLE_PROBE', 'SUCCESS', 'SYSTEM' FROM users WHERE username = 'sec018_probe'"
expect_ok "runtime role uses sequences (serial nextval / currval / setval; standalone conference_chat_sync_version_seq)" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  SELECT nextval(pg_get_serial_sequence('account_security_events', 'seq'));
  SELECT currval(pg_get_serial_sequence('account_security_events', 'seq'));
  SELECT setval(pg_get_serial_sequence('account_security_events', 'seq'), currval(pg_get_serial_sequence('account_security_events', 'seq')));
  SELECT nextval('conference_chat_sync_version_seq')"
expect_error "account_security_events stays append-only for the runtime role (UPDATE)" "append-only" \
  psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "UPDATE account_security_events SET outcome = 'FAILURE'"
expect_error "account_security_events stays append-only for the runtime role (DELETE)" "append-only" \
  psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "DELETE FROM account_security_events"
expect_ok "runtime role deactivates a user (fires trg_account_security_credentials_changed)" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  UPDATE users SET status = 'inactive' WHERE username = 'sec018_probe'"
expect_value "trigger advanced account_security_state and revoked the open session" t "$rt_url" "
  SELECT EXISTS (SELECT 1 FROM account_security_state s JOIN users u ON u.id = s.user_id
                  WHERE u.username = 'sec018_probe' AND s.credentials_not_before IS NOT NULL)
     AND NOT EXISTS (SELECT 1 FROM account_sessions a JOIN users u ON u.id = a.user_id
                  WHERE u.username = 'sec018_probe' AND a.revoke_reason IS DISTINCT FROM 'ACCOUNT_DISABLED')"
if [ "$(value "$rt_url" "SELECT to_regclass('account_communication_outbox') IS NOT NULL")" = t ]; then
  expect_value "Account Communications trigger wrote its outbox event" t "$rt_url" "
    SELECT EXISTS (SELECT 1 FROM account_communication_outbox o JOIN users u
                    ON o.payload->>'accountReference' = 'cfmeet-user:' || u.id::text
                    WHERE u.username = 'sec018_probe' AND o.payload->>'state' = 'DEACTIVATED')"
  expect_ok "runtime role deletes Account Communications rows" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
    DELETE FROM account_communication_outbox WHERE payload->>'accountReference' IN (SELECT 'cfmeet-user:' || id::text FROM users WHERE username = 'sec018_probe');
    DELETE FROM account_communication_state WHERE user_id IN (SELECT id FROM users WHERE username = 'sec018_probe')"
fi
expect_ok "runtime role deletes account_sessions, account_security_state and users rows" psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "
  DELETE FROM account_sessions WHERE user_id IN (SELECT id FROM users WHERE username = 'sec018_probe');
  DELETE FROM account_security_state WHERE user_id IN (SELECT id FROM users WHERE username = 'sec018_probe');
  DELETE FROM users WHERE username = 'sec018_probe'"

echo "== runtime DDL is refused"
rt_sql() { psql "$rt_url" -Xq -v ON_ERROR_STOP=1 -c "$1"; }
expect_refused "runtime role CREATE TABLE" rt_sql "CREATE TABLE intruder (id INT)"
expect_refused "runtime role CREATE TEMP TABLE" rt_sql "CREATE TEMP TABLE intruder (id INT)"
expect_refused "runtime role ALTER TABLE users ADD COLUMN" rt_sql "ALTER TABLE users ADD COLUMN intruder INT"
expect_refused "runtime role CREATE INDEX" rt_sql "CREATE INDEX intruder ON users (last_login)"
expect_refused "runtime role ALTER TYPE user_status ADD VALUE" rt_sql "ALTER TYPE user_status ADD VALUE 'intruder'"
expect_refused "runtime role CREATE FUNCTION" rt_sql "CREATE FUNCTION intruder() RETURNS INT LANGUAGE sql AS 'SELECT 1'"
expect_refused "runtime role DISABLE TRIGGER" rt_sql "ALTER TABLE account_security_events DISABLE TRIGGER trg_account_security_events_no_update"
expect_refused "runtime role DROP TRIGGER" rt_sql "DROP TRIGGER trg_account_security_credentials_changed ON users"
expect_refused "runtime role DROP TABLE" rt_sql "DROP TABLE account_sessions"
expect_refused "runtime role TRUNCATE" rt_sql "TRUNCATE account_sessions"
expect_refused "runtime role TRUNCATE the security event log" rt_sql "TRUNCATE account_security_events"

if [ "$mode" != fresh ]; then
  echo "== rollback rehearsal"
  expect_ok "rollback dry run" cutover rollback -v owner="$owner" -v database_owner="$owner" -v dry_run=1
  expect_value "rollback dry run changed nothing" 0 "$admin_db_url" "$(public_owned_by "$owner")"
  expect_ok "rollback" cutover rollback -v owner="$owner" -v database_owner="$owner"
  expect_value "migrator role owns nothing after the rollback" 0 "$admin" "$(owned_by "$mg")"
  expect_value "$owner owns every object in schema public again" "$before" "$admin_db_url" "$(public_owned_by "$owner")"
  expect_value "$owner owns the database again" t "$admin" "SELECT datdba = '$owner'::regrole FROM pg_database WHERE datname = '$db'"
  for bin in "${bins[@]}"; do
    expect_ok "$(basename "$bin") up as $owner after the rollback" env DATABASE_URL="$owner_url" "$bin" up
  done
  expect_ok "$owner can run DDL again after the rollback (pre-cutover behaviour)" psql "$owner_url" -Xq -v ON_ERROR_STOP=1 \
    -c "CREATE TABLE rollback_probe (id INT); DROP TABLE rollback_probe"
fi

if [ "$failures" -ne 0 ]; then
  echo "meeting-service database roles: FAILED ($failures check(s))"
  exit 1
fi
echo "meeting-service database roles: OK"
