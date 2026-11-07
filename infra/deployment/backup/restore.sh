#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Restore ONE service database backup produced by backup.sh into an EMPTY database.
#
#   restore.sh --in <file.dump | file.dump.age> <connection of an empty database>
#              [--age-identity-file KEY] [--pg-wrapper CMD]
#
#   <connection>  --database-url URL, --database-url-file F, --database-url-env-file F, the
#                 DATABASE_URL environment variable, or (as restore-drill.sh uses it) an
#                 already prepared libpq environment with PGDATABASE set.
#
# Refuses a corrupted or unverifiable archive and refuses to restore over existing tables
# (a restore never merges into live state). For an encrypted backup the .age checksum is
# verified BEFORE decryption, the archive is decrypted into a private temporary directory
# with the age identity (private key; it lives with the owner / drill host, never on the
# production database host) and the decrypted bytes are verified against <file.dump.sha256>.
# The whole restore is one transaction: it either completes or leaves the target empty.
#
# After a restore, start the service's migrator (it must report nothing pending for the same
# release) and the service's workers: leases in the restored rows have expired by then, so
# pending durable work is reclaimed exactly as after a crash, and every paid / external
# action keeps its idempotency and unknown-outcome rules. Missing key material makes Privacy
# Relay fail closed; it never falls back.
#
# Exit status: 0 restored; 2 usage; 3 a tool is missing; 4 the archive or its .sha256 is
# missing; 5 checksum mismatch or decryption failure; 6 the target database is not empty.
set -euo pipefail
CFB_PROG=restore.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

url="" url_file="" env_file="" url_var=DATABASE_URL in="" identity=""
while [ $# -gt 0 ]; do
  case "$1" in
    --database-url) url="$2"; shift 2 ;;
    --database-url-file) url_file="$2"; shift 2 ;;
    --database-url-env-file) env_file="$2"; shift 2 ;;
    --url-var) url_var="$2"; shift 2 ;;
    --in) in="$2"; shift 2 ;;
    --age-identity-file) identity="$2"; shift 2 ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[ -n "$in" ] || cfb_die 2 "--in is required"
cfb_require_pg_tools pg_restore psql
cfb_require_tools sha256sum
[ -f "$in" ] && [ -f "$in.sha256" ] || cfb_die 4 "$in or its .sha256 is missing"
cfb_check_sha256 "$in" || cfb_die 5 "checksum mismatch for $in"

umask 077
work="$(mktemp -d "${TMPDIR:-/tmp}/cfmeet-restore.XXXXXX")"
trap 'rm -rf "$work"' EXIT

archive="$in"
case "$in" in
  *.age)
    cfb_require_tools age
    [ -n "$identity" ] || cfb_die 2 "$in is encrypted: pass --age-identity-file (the owner's private key, never stored on the production host)"
    [ -r "$identity" ] || cfb_die 2 "age identity file is not readable: $identity"
    plain="${in%.age}"
    [ -f "$plain.sha256" ] || cfb_die 4 "$plain.sha256 (checksum of the decrypted dump) is missing"
    archive="$work/$(basename "$plain")"
    age --decrypt --identity "$identity" --output "$archive" "$in" 2>"$work/age.err" ||
      cfb_die 5 "could not decrypt $in with the given identity"
    cp "$plain.sha256" "$archive.sha256"
    cfb_check_sha256 "$archive" || cfb_die 5 "decrypted archive does not match $plain.sha256"
    ;;
esac

cfb_connect "$url" "$url_file" "$env_file" "$url_var"
existing="$(cfb_public_table_count)"
if [ "$existing" != "0" ]; then
  cfb_die 6 "target database is not empty ($existing tables); refusing to restore"
fi

if [ -n "${CFMEET_PG_WRAPPER:-}" ]; then
  # A client container cannot see host paths: stream the archive on stdin.
  cfb_pg pg_restore --no-owner --no-privileges --exit-on-error --single-transaction --dbname="$PGDATABASE" <"$archive"
else
  pg_restore --no-owner --no-privileges --exit-on-error --single-transaction --dbname="$PGDATABASE" "$archive"
fi
echo "restore.sh: restored $in"
