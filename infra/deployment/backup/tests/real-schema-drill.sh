#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR/..
# Real-schema backup drill (Blueprint Section 18, J-18): every active catalog database is
# created and migrated with its service's REAL migration binaries (not a sample schema), then
# backed up with the production tooling (read-only backup role, age encryption, off-host
# directory copy), restored from the off-host copy into a fresh isolated drill_ database, and
# the restored schema is compared with the source.
#
#   CFMEET_DRILL_ADMIN_URL=postgres://postgres:...@127.0.0.1:5432/postgres \
#   CFMEET_MIGRATION_BIN_DIR=/path/to/built/migration/binaries \
#     bash infra/deployment/backup/tests/real-schema-drill.sh
#
# The migration binaries are the ones each service ships (`<binary> up` with DATABASE_URL).
# Requires psql / pg_dump / pg_restore (16+), age + age-keygen, python3, flock, sha256sum.
# Prints REAL_SCHEMA_DRILL=PASS and exits 0 only if every check passed.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
tool_dir="$(cd "$here/.." && pwd)"
admin_url="${CFMEET_DRILL_ADMIN_URL:?set CFMEET_DRILL_ADMIN_URL to a throwaway PostgreSQL superuser URL}"
bin_dir="${CFMEET_MIGRATION_BIN_DIR:?set CFMEET_MIGRATION_BIN_DIR to the directory of the built migration binaries}"
work="$(mktemp -d "${TMPDIR:-/tmp}/cfmeet-real-drill.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/host/urls" "$work/owner" "$work/offhost"
chmod 700 "$work/host" "$work/owner"
failures=0

CFB_PROG=real-schema-drill
. "$tool_dir/lib.sh"

pass() { echo "  ok   $*"; }
fail() {
  echo "  FAIL $*"
  failures=$((failures + 1))
}
check() { # check <description> <command...>
  local what="$1"
  shift
  if "$@"; then pass "$what"; else fail "$what"; fi
}
admin_db_url() { python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); print(u.urlunsplit(p._replace(path="/"+sys.argv[2])))' "$admin_url" "$1"; }
url_for() { # url_for <role> <password> <database>
  python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); host=p.hostname+(":"+str(p.port) if p.port else ""); print(u.urlunsplit(p._replace(netloc=u.quote(sys.argv[2],safe="")+":"+u.quote(sys.argv[3],safe="")+"@"+host, path="/"+sys.argv[4], query="")))' "$admin_url" "$1" "$2" "$3"
}
adm() { psql "$admin_url" -XAtq -v ON_ERROR_STOP=1 "$@"; }
dbq() { psql "$(admin_db_url "$1")" -XAtq -v ON_ERROR_STOP=1 -c "$2"; }
real_db() { echo "cfmeet_real_$(printf '%s' "$1" | tr '-' '_')"; }
# Structural fingerprint of the public schema: tables, columns (type, nullability, default),
# constraints (name, kind, columns), indexes, sequences, views, functions and triggers. A
# textual `pg_dump --schema-only` diff is not used: PostgreSQL re-deparses some CHECK
# expressions (IN lists) differently after a restore although they are identical.
schema_of() {
  dbq "$1" "
    SELECT 'column ' || c.table_name || '.' || c.column_name || ' ' || c.data_type || ' ' || c.is_nullable || ' ' || coalesce(c.column_default, '-')
      FROM information_schema.columns c WHERE c.table_schema = 'public'
    UNION ALL
    SELECT 'constraint ' || cl.relname || '.' || co.conname || ' ' || co.contype::text || ' ' ||
           coalesce((SELECT string_agg(a.attname, ',' ORDER BY a.attnum) FROM pg_attribute a WHERE a.attrelid = co.conrelid AND a.attnum = ANY (co.conkey)), '-')
      FROM pg_constraint co JOIN pg_class cl ON cl.oid = co.conrelid JOIN pg_namespace n ON n.oid = cl.relnamespace WHERE n.nspname = 'public'
    UNION ALL
    SELECT 'index ' || t.relname || '.' || i.relname || ' ' || am.amname || ' unique=' || x.indisunique::text ||
           ' partial=' || (x.indpred IS NOT NULL)::text || ' keys=' ||
           coalesce((SELECT string_agg(coalesce(a.attname, 'expr'), ',' ORDER BY k.ord) FROM unnest(x.indkey) WITH ORDINALITY k(attnum, ord)
                     LEFT JOIN pg_attribute a ON a.attrelid = x.indrelid AND a.attnum = k.attnum), '-')
      FROM pg_index x JOIN pg_class i ON i.oid = x.indexrelid JOIN pg_class t ON t.oid = x.indrelid
      JOIN pg_am am ON am.oid = i.relam JOIN pg_namespace n ON n.oid = t.relnamespace WHERE n.nspname = 'public'
    UNION ALL
    SELECT 'sequence ' || sequence_name || ' ' || data_type FROM information_schema.sequences WHERE sequence_schema = 'public'
    UNION ALL
    SELECT 'view ' || table_name FROM information_schema.views WHERE table_schema = 'public'
    UNION ALL
    SELECT 'function ' || p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')'
      FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'public'
    UNION ALL
    SELECT 'trigger ' || c.relname || '.' || t.tgname FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
      JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public' AND NOT t.tgisinternal
    ORDER BY 1"
}

# Every migration binary a service ships, in the order its deployment applies them.
migrations_for() {
  case "$1" in
    meeting-service) echo "migration account-communications-migration account-security-migration meetings-calendar-migration profile-notifications-migration account-hardening-migration organization-integration-migration lifecycle-delivery-migration" ;;
    communications) echo "communications-migration" ;;
    developer-platform) echo "developer-platform-migration" ;;
    platform-access) echo "platform-access-migration" ;;
    calling) echo "calling-migration calling-communications-migration" ;;
    messaging) echo "messaging-migration messaging-communications-migration" ;;
    numbering) echo "numbering-migration" ;;
    privacy-relay) echo "privacy-relay-migration" ;;
    telephony) echo "telephony-migration" ;;
    sms-gateway) echo "sms-gateway-migration" ;;
    organizations) echo "organizations-migration" ;;
    *) return 1 ;;
  esac
}

echo "== tools"
for tool in psql pg_dump pg_restore age age-keygen python3 flock sha256sum; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool"; exit 3; }
done

mapfile -t services < <(awk '!/^#/ && NF && $2 == "active" { print $1 }' "$tool_dir/catalog.conf")
echo "catalog services (${#services[@]}): ${services[*]}"
check "every active catalog service has a known migration set" bash -c '
  for s in "$@"; do case "$s" in meeting-service|communications|developer-platform|platform-access|calling|messaging|numbering|privacy-relay|telephony|sms-gateway|organizations) ;; *) echo "unknown: $s"; exit 1 ;; esac; done' _ "${services[@]}"

echo "== create and migrate every database with its real migrations"
for s in "${services[@]}"; do
  db="$(real_db "$s")"
  adm -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null
  adm -c "CREATE DATABASE $db" >/dev/null
  ok=1
  for m in $(migrations_for "$s"); do
    if [ ! -x "$bin_dir/$m" ]; then
      echo "       missing binary $bin_dir/$m"
      ok=0
      break
    fi
    if ! env -i PATH="$PATH" APP_ENV=test DATABASE_URL="$(admin_db_url "$db")" "$bin_dir/$m" up >"$work/migrate.out" 2>&1; then
      sed 's/^/       | /' "$work/migrate.out" | tail -n 20
      ok=0
      break
    fi
  done
  check "$s: real migrations applied ($(migrations_for "$s" | wc -w | tr -d ' ') binaries)" [ "$ok" = 1 ]
  tables="$(dbq "$db" "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'")"
  check "$s: real schema has $tables tables (more than its migration histories)" [ "$tables" -gt 3 ]
  cfb_load_service "$s"
  check "$s: the catalog's base history ${CFB_REQUIRED[0]} exists" [ "$(dbq "$db" "SELECT (to_regclass('${CFB_REQUIRED[0]}') IS NOT NULL)::int")" = 1 ]
  # Rows that must survive the round trip: one marker row per database.
  dbq "$db" "CREATE TABLE IF NOT EXISTS zz_drill_marker (id int PRIMARY KEY, note text NOT NULL); INSERT INTO zz_drill_marker VALUES (1, 'real-schema drill $s') ON CONFLICT DO NOTHING" >/dev/null
done

echo "== owner key pair and read-only backup role"
age-keygen -o "$work/owner/identity.txt" 2>/dev/null
age-keygen -y "$work/owner/identity.txt" >"$work/host/age-recipients.txt"
printf '%s\n' "$admin_url" >"$work/owner/admin.url"
chmod 600 "$work/owner/admin.url" "$work/owner/identity.txt"
backup_pw="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
printf '%s\n' "$backup_pw" >"$work/host/backup-role.password"
dbs=""
for s in "${services[@]}"; do dbs="$dbs $(real_db "$s")"; done
check "provision the read-only cfmeet_backup role on every real database" \
  bash "$tool_dir/provision-backup-role.sh" --admin-url-file "$work/owner/admin.url" \
  --password-file "$work/host/backup-role.password" --databases "$dbs"
for s in "${services[@]}"; do
  url_for cfmeet_backup "$backup_pw" "$(real_db "$s")" >"$work/host/urls/$s.url"
done
chmod 600 "$work/host/urls/"*.url

{
  echo "CFMEET_BACKUP_ROOT=$work/host/backups"
  echo "CFMEET_BACKUP_AGE_RECIPIENTS_FILE=$work/host/age-recipients.txt"
  echo "CFMEET_BACKUP_SERVICES=${services[*]}"
  for s in "${services[@]}"; do
    echo "CFMEET_BACKUP_$(printf '%s' "$s" | tr 'a-z-' 'A-Z_')_URL_FILE=$work/host/urls/$s.url"
  done
  echo "CFMEET_BACKUP_KEEP_DAYS=14"
  echo "CFMEET_BACKUP_KEEP_MIN=2"
  echo "CFMEET_BACKUP_SOURCE_NAME=real-schema-drill"
  echo "CFMEET_BACKUP_OFFSITE_KIND=dir"
  echo "CFMEET_BACKUP_OFFSITE_DIR=$work/offhost"
} >"$work/host/backup.env"
chmod 600 "$work/host/backup.env"

echo "== encrypted backup of every real database with an off-host copy"
if bash "$tool_dir/backup-all.sh" --config "$work/host/backup.env" >"$work/backup.out" 2>&1; then
  pass "backup-all.sh over the real schemas"
else
  fail "backup-all.sh over the real schemas"
  sed 's/^/       | /' "$work/backup.out" | tail -n 40
fi
run_dir="$(sed -n 's/^BACKUP_RUN_DIR=//p' "$work/backup.out" | tail -n 1)"
check "classified OFF_HOST_COPY_VERIFIED" [ "$(sed -n 's/^BACKUP_CLASSIFICATION=//p' "$work/backup.out" | tail -n 1)" = OFF_HOST_COPY_VERIFIED ]
stamp="$(basename "${run_dir:-missing}")"
check "no plaintext dump anywhere on the host or off-host" [ -z "$(find "$work/host/backups" "$work/offhost" -name '*.dump' -print 2>/dev/null)" ]

echo "== isolated restore of every real database from the off-host copy"
for s in "${services[@]}"; do
  src="$(real_db "$s")"
  cfb_load_service "$s"
  target="drill_real_$(printf '%s' "$s" | tr '-' '_')"
  archive="$work/offhost/real-schema-drill/runs/$stamp/$s/$s-$stamp.dump.age"
  check "$s: off-host archive is age-encrypted" [ "$(head -c 21 "$archive" 2>/dev/null)" = "age-encryption.org/v1" ]
  if bash "$tool_dir/restore-drill.sh" --in "$archive" --age-identity-file "$work/owner/identity.txt" \
    --target-database "$target" --admin-url-file "$work/owner/admin.url" --replace-existing-drill >"$work/restore.out" 2>&1 &&
    grep -q '^RESTORE_DRILL=PASS' "$work/restore.out"; then
    pass "$s: RESTORE_DRILL=PASS"
  else
    fail "$s: restore drill"
    sed 's/^/       | /' "$work/restore.out" | tail -n 30
    continue
  fi
  schema_of "$src" >"$work/src.sql"
  schema_of "$target" >"$work/dst.sql"
  check "$s: restored schema structurally identical to the real migrated schema ($(wc -l <"$work/src.sql" | tr -d ' ') objects)" cmp -s "$work/src.sql" "$work/dst.sql"
  cmp -s "$work/src.sql" "$work/dst.sql" || diff "$work/src.sql" "$work/dst.sql" | head -n 10 | sed 's/^/       | /' 
  check "$s: migration history identical" [ "$(dbq "$src" "SELECT md5(string_agg(version, ',' ORDER BY version)) FROM ${CFB_REQUIRED[0]}")" = \
    "$(dbq "$target" "SELECT md5(string_agg(version, ',' ORDER BY version)) FROM ${CFB_REQUIRED[0]}")" ]
  check "$s: marker row restored" [ "$(dbq "$target" "SELECT note FROM zz_drill_marker WHERE id = 1")" = "real-schema drill $s" ]
  [ -n "${CFMEET_DRILL_KEEP:-}" ] || adm -c "DROP DATABASE IF EXISTS $target WITH (FORCE)" >/dev/null
done

echo "== cleanup"
[ -n "${CFMEET_DRILL_KEEP:-}" ] || for s in "${services[@]}"; do adm -c "DROP DATABASE IF EXISTS $(real_db "$s") WITH (FORCE)" >/dev/null; done

if [ "$failures" = 0 ]; then
  echo "REAL_SCHEMA_DRILL=PASS services=${#services[@]}"
else
  echo "REAL_SCHEMA_DRILL=FAIL failures=$failures"
  exit 1
fi
