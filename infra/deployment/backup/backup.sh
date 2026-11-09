#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Logical backup of ONE service-owned PostgreSQL database. Every CF Meet service database
# is listed in catalog.conf (meeting-service, communications, developer-platform,
# platform-access, calling, messaging, numbering, privacy-relay, telephony, sms-gateway,
# organizations). Each service is backed up on its own; there is no
# shared platform dump.
#
#   backup.sh --service <name> <connection> --out <file.dump>
#             [--age-recipients-file R [--keep-plaintext]] [--label TEXT] [--pg-wrapper CMD]
#
#   <connection>  one of --database-url-file F (file holding only the URL),
#                 --database-url-env-file F [--url-var NAME] (docker env file, default
#                 DATABASE_URL), --database-url URL (visible in the process list: avoid in
#                 production), or the DATABASE_URL environment variable.
#
# Writes <file.dump> (pg_dump custom format, no owner / privilege statements so it restores
# into a role-separated target), <file.dump.sha256> and <file.dump.manifest.json> (database,
# sizes, checksums, pg_dump / server versions, migration-history row counts).
#
# With --age-recipients-file the dump is encrypted with age to the PUBLIC recipients in R
# (the private key is never needed on this host) into <file.dump.age> + <file.dump.age.sha256>
# and the plaintext dump is deleted (unless --keep-plaintext); <file.dump.sha256> is kept so a
# restore can verify the decrypted bytes. Scheduled and pre-migration backups always encrypt.
#
# The URL is turned into libpq environment variables and never printed or passed to a child
# process's command line. Key material is never part of a database backup: Contact Vault
# keyrings, provider secrets and signing keys live in files outside PostgreSQL and are backed
# up (or re-provisioned) through the secret store.
#
# Exit status: 0 ok; 2 usage / configuration; 3 a required tool is missing or older than the
# server; 4 the database is not the named service's; 5 encryption refused or failed (missing,
# empty or private-key recipients file); 6 the database is unreachable, the dump failed or
# the migration history changed while it ran; 7 the manifest could not be written.
set -euo pipefail
CFB_PROG=backup.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

service="" url="" url_file="" env_file="" url_var=DATABASE_URL out="" recipients="" keep_plain=0 label="manual"
while [ $# -gt 0 ]; do
  case "$1" in
    --service) service="$2"; shift 2 ;;
    --database-url) url="$2"; shift 2 ;;
    --database-url-file) url_file="$2"; shift 2 ;;
    --database-url-env-file) env_file="$2"; shift 2 ;;
    --url-var) url_var="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --age-recipients-file) recipients="$2"; shift 2 ;;
    --keep-plaintext) keep_plain=1; shift ;;
    --label) label="$2"; shift 2 ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
cfb_require_active_service "$service"
[ -n "$out" ] || cfb_die 2 "--out is required"
[[ "$label" =~ ^[A-Za-z0-9][A-Za-z0-9._:-]{0,99}$ ]] || cfb_die 2 "--label may contain only letters, digits and . _ : -"
[ -d "$(dirname "$out")" ] || cfb_die 2 "output directory does not exist: $(dirname "$out")"
cfb_require_pg_tools pg_dump psql
cfb_require_tools sha256sum python3
if [ -n "$recipients" ]; then
  cfb_check_recipients "$recipients"
  cfb_require_tools age
fi
cfb_connect "$url" "$url_file" "$env_file" "$url_var"
database="$PGDATABASE"
cfb_sql "SELECT 1" >/dev/null || cfb_die 6 "cannot connect to the $service database"

# The migration-history (and identity) tables prove the dump belongs to the named service.
for table in "${CFB_REQUIRED[@]}" "${CFB_IDENTITY[@]}"; do
  cfb_table_exists "$table" || cfb_die 4 "$table not found — this is not the $service database"
done

dump_version="$(cfb_pg pg_dump --version)"
server_num="$(cfb_sql "SHOW server_version_num")"
server_version="$(cfb_sql "SHOW server_version")"
if [ "$(cfb_pg_major_of "$dump_version")" -lt $((server_num / 10000)) ]; then
  cfb_die 3 "pg_dump ($dump_version) is older than the server ($server_version); use a client of the same or a newer major version"
fi
db_size="$(cfb_sql "SELECT pg_database_size(current_database())")"
table_count="$(cfb_public_table_count)"

umask 077
work="$(mktemp -d "${TMPDIR:-/tmp}/cfmeet-backup.XXXXXX")"
cleanup() {
  rm -rf "$work"
  rm -f "$out.partial" "$out.age.partial"
}
trap cleanup EXIT

cfb_history_snapshot >"$work/history.before"
# Streamed to stdout so the same command works through a client container (--pg-wrapper).
cfb_pg pg_dump --format=custom --no-owner --no-privileges >"$out.partial" ||
  cfb_die 6 "pg_dump failed for the $service database"
[ -s "$out.partial" ] || cfb_die 6 "pg_dump produced an empty archive"
cfb_history_snapshot >"$work/history.after"
if ! cmp -s "$work/history.before" "$work/history.after"; then
  cfb_die 6 "the $service migration history changed while the dump ran (a migration was applied); run the backup again"
fi
mv "$out.partial" "$out"
cfb_write_sha256 "$out"
dump_sha="$(cfb_sha256 "$out")"
dump_size="$(cfb_size "$out")"

enc_args=()
final="$out"
retained=yes
if [ -n "$recipients" ]; then
  cfb_encrypt "$recipients" "$out" "$out.age" || {
    rm -f "$out" "$out.sha256"
    cfb_die 5 "age encryption failed for the $service dump; the plaintext dump was removed"
  }
  cfb_write_sha256 "$out.age"
  enc_args=(--encrypted-file "$out.age" --encrypted-sha256 "$(cfb_sha256 "$out.age")" --encrypted-size "$(cfb_size "$out.age")")
  while IFS= read -r recipient; do enc_args+=(--recipient "$recipient"); done < <(cfb_recipients_list "$recipients")
  if [ "$keep_plain" -eq 0 ]; then
    rm -f "$out"
    retained=no
  fi
  final="$out.age"
fi

python3 "$CFB_DIR/manifest.py" write --out "$out.manifest.json" --service "$service" --database "$database" \
  --label "$label" --pg-dump-version "$dump_version" --server-version "$server_version" \
  --db-size "$db_size" --table-count "$table_count" --history-file "$work/history.after" \
  --dump-file "$out" --dump-sha256 "$dump_sha" --dump-size "$dump_size" \
  --plaintext-retained "$retained" "${enc_args[@]}" ||
  cfb_die 7 "could not write $out.manifest.json"

echo "backup.sh: $service -> $final"
echo "BACKUP_FILE=$final"
echo "BACKUP_MANIFEST=$out.manifest.json"
