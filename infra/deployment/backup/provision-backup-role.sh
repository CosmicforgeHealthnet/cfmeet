#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Provision a READ-ONLY login role for scheduled backups on one PostgreSQL server, so the
# backup job never needs a service's migrator / owner credentials or the superuser.
#
#   provision-backup-role.sh --admin-url-file F --password-file P --databases "db1 db2 ..."
#                            [--role cfmeet_backup] [--pg-wrapper CMD]
#
# The role gets LOGIN (INHERIT, so the membership applies without SET ROLE), membership of
# the predefined role pg_read_all_data (PostgreSQL >= 14: SELECT on every table, view and
# sequence, which is all pg_dump needs; no CF Meet table uses row-level security) and CONNECT on each listed database (CONNECT is revoked from PUBLIC by
# provision-service-database.sh). It gets no write, DDL, CREATEDB, CREATEROLE, REPLICATION or
# BYPASSRLS. The password is read from a file and never printed. Idempotent.
#
# Exit status: 0 ok; 2 usage; 3 tool missing; other: psql failure.
set -euo pipefail
CFB_PROG=provision-backup-role.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

admin_file="" password_file="" databases="" role=cfmeet_backup
while [ $# -gt 0 ]; do
  case "$1" in
    --admin-url-file) admin_file="$2"; shift 2 ;;
    --password-file) password_file="$2"; shift 2 ;;
    --databases) databases="$2"; shift 2 ;;
    --role) role="$2"; shift 2 ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[ -r "$admin_file" ] && [ -s "$password_file" ] && [ -n "$databases" ] ||
  cfb_die 2 "--admin-url-file, --password-file and --databases are required"
[[ "$role" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || cfb_die 2 "--role must be a plain lower-case identifier"
read -r -a dbs <<<"$databases"
for db in "${dbs[@]}"; do
  [[ "$db" =~ ^[a-z_][a-z0-9_]{0,62}$ ]] || cfb_die 2 "database names must be plain lower-case identifiers"
done
password="$(head -n 1 "$password_file" | tr -d '\r')"
# A restricted alphabet keeps the value free of psql quoting / escape rules.
[[ "$password" =~ ^[A-Za-z0-9._~+/=-]{16,128}$ ]] ||
  cfb_die 2 "the password file must hold 16-128 characters of [A-Za-z0-9._~+/=-] (e.g. openssl rand -hex 32)"
cfb_require_pg_tools psql
cfb_connect "" "$admin_file" ""

# The password reaches psql as a variable on stdin, never on a command line.
{
  printf '\\set role %s\n' "$role"
  printf "\\\\set password '%s'\n" "$password"
  cat <<'SQL'
SELECT format('CREATE ROLE %I LOGIN INHERIT', :'role')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'role') \gexec
SELECT format('ALTER ROLE %I WITH LOGIN INHERIT PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', :'role', :'password') \gexec
SELECT format('GRANT pg_read_all_data TO %I', :'role') \gexec
SQL
  for db in "${dbs[@]}"; do
    printf "SELECT format('GRANT CONNECT ON DATABASE %%I TO %%I', '%s', :'role') \\\\gexec\n" "$db"
  done
} | cfb_pg psql -Xq -v ON_ERROR_STOP=1 >/dev/null
echo "provision-backup-role.sh: role $role can read ${#dbs[@]} database(s)"
