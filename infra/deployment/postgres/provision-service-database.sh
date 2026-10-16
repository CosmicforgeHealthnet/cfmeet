#!/usr/bin/env bash
# Provision ONE service-owned PostgreSQL database with two roles (Part J section 98,
# Part K section 120):
#
#   <role>_migrator  owns the database and its schema; used only by the service's migration
#                    binary (e.g. calling-migration) during a release.
#   <role>           the runtime role the service connects with: CONNECT to its own
#                    database only, data access (SELECT/INSERT/UPDATE/DELETE, sequences,
#                    functions) on objects the migrator creates, and no DDL. Services never
#                    run DDL at runtime: they refuse to start while migrations are pending.
#
# CONNECT is revoked from PUBLIC, so no other service's role (and no other login) can open
# this database. Passwords are read from files (secret store mounts), never arguments.
#
#   provision-service-database.sh --service <calling|platform-access|numbering|privacy-relay|telephony|sms-gateway> \
#       --admin-url <superuser/createrole url> --migrator-password-file F --runtime-password-file F \
#       [--database NAME] [--role-prefix PREFIX]
#
# Requires PostgreSQL >= 15 (schema public is owned by the database owner, i.e. the
# migrator role, and PUBLIC has no CREATE on it).
#
# Defaults: database cfmeet_<service with underscores>, roles <service with underscores>
# (matching services/<service>/.env.example) and <...>_migrator. Idempotent.
set -euo pipefail

service="" admin="" mig_file="" app_file="" database="" prefix=""
while [ $# -gt 0 ]; do
  case "$1" in
    --service) service="$2"; shift 2 ;;
    --admin-url) admin="$2"; shift 2 ;;
    --migrator-password-file) mig_file="$2"; shift 2 ;;
    --runtime-password-file) app_file="$2"; shift 2 ;;
    --database) database="$2"; shift 2 ;;
    --role-prefix) prefix="$2"; shift 2 ;;
    *) echo "provision: unknown argument $1" >&2; exit 2 ;;
  esac
done
case "$service" in calling|platform-access|numbering|privacy-relay|telephony|sms-gateway) ;; *) echo "provision: --service must be calling, platform-access, numbering, privacy-relay, telephony or sms-gateway" >&2; exit 2 ;; esac
[ -n "$admin" ] && [ -f "$mig_file" ] && [ -f "$app_file" ] || { echo "provision: --admin-url and both password files are required" >&2; exit 2; }

base="$(echo "$service" | tr '-' '_')"
runtime="${prefix}${base}"
migrator="${runtime}_migrator"
database="${database:-cfmeet_${base}}"

psql "$admin" -Xq -v ON_ERROR_STOP=1 \
  -v migrator="$migrator" -v runtime="$runtime" -v database="$database" \
  -v migrator_password="$(cat "$mig_file")" -v runtime_password="$(cat "$app_file")" <<'SQL'
SELECT format('CREATE ROLE %I LOGIN NOINHERIT', :'migrator')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'migrator') \gexec
SELECT format('CREATE ROLE %I LOGIN NOINHERIT', :'runtime')
 WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = :'runtime') \gexec
SELECT format('ALTER ROLE %I PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', :'migrator', :'migrator_password') \gexec
SELECT format('ALTER ROLE %I PASSWORD %L NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS', :'runtime', :'runtime_password') \gexec
SELECT format('CREATE DATABASE %I OWNER %I', :'database', :'migrator')
 WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = :'database') \gexec
SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', :'database') \gexec
SELECT format('GRANT CONNECT ON DATABASE %I TO %I', :'database', :'runtime') \gexec
SQL

# Schema-level grants run inside the service database, as its owner's administrator.
db_url="$(python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); print(u.urlunsplit(p._replace(path="/"+sys.argv[2])))' "$admin" "$database")"
psql "$db_url" -Xq -v ON_ERROR_STOP=1 -v migrator="$migrator" -v runtime="$runtime" <<'SQL'
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
SELECT format('GRANT USAGE ON SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA public TO %I', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO %I', :'migrator', :'runtime') \gexec
SELECT format('ALTER DEFAULT PRIVILEGES FOR ROLE %I IN SCHEMA public GRANT EXECUTE ON FUNCTIONS TO %I', :'migrator', :'runtime') \gexec
SQL
echo "provisioned $database: migrator role $migrator, runtime role $runtime"
