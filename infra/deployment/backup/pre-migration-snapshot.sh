#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Pre-migration snapshot (SEC-011 / DATA-013): an encrypted, checksummed, manifest-described
# dump of ONE service database taken immediately before a release applies migrations.
# A release step must stop if this script exits non-zero.
#
#   pre-migration-snapshot.sh --service <name> <connection> --label <release id>
#       [--age-recipients-file R] [--out-dir DIR] [--pg-wrapper CMD] [--offsite]
#
#   <connection>     --database-url-env-file F [--url-var NAME] (e.g. the private migration
#                    env file a release already writes), --database-url-file F,
#                    --database-url URL, or DATABASE_URL.
#   --label          release identifier, e.g. the GitHub run id or image SHA
#   R                default $CFMEET_BACKUP_AGE_RECIPIENTS_FILE (public recipients only)
#   DIR              default $CFMEET_BACKUP_ROOT/pre-migration (CFMEET_BACKUP_ROOT defaults
#                    to /var/backups/cfmeet)
#   --offsite        also copy the snapshot off-host via offsite.sh (CFMEET_BACKUP_OFFSITE_*);
#                    a configured-but-failed copy is then a failure. A snapshot is primarily
#                    the same-host rollback point for a bad migration.
#
# Writes DIR/<service>/<UTC stamp>-<label>/<service>-<stamp>.dump.age (+ .age.sha256,
# .dump.sha256, .dump.manifest.json), re-verifies the stored checksum and prints evidence:
#   PRE_MIGRATION_SNAPSHOT=<path>  PRE_MIGRATION_SNAPSHOT_SHA256=<hex>  PRE_MIGRATION_MANIFEST=<path>
#
# Exit status: 0 snapshot written and verified; otherwise the failing step's status (2 usage /
# configuration, 3 tool, 4 wrong database, 5 encryption refused, 6 dump failed, 7 manifest,
# 11 post-write verification failed, 12 requested off-host copy failed).
set -euo pipefail
CFB_PROG=pre-migration-snapshot.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

service="" label="" recipients="${CFMEET_BACKUP_AGE_RECIPIENTS_FILE:-}" out_dir="" offsite=0
conn=()
while [ $# -gt 0 ]; do
  case "$1" in
    --service) service="$2"; shift 2 ;;
    --label) label="$2"; shift 2 ;;
    --age-recipients-file) recipients="$2"; shift 2 ;;
    --out-dir) out_dir="$2"; shift 2 ;;
    --offsite) offsite=1; shift ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    --database-url | --database-url-file | --database-url-env-file | --url-var) conn+=("$1" "$2"); shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
cfb_require_active_service "$service"
[[ "$label" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$ ]] || cfb_die 2 "--label (release id: letters, digits, . _ -) is required"
cfb_check_recipients "$recipients"
cfb_require_tools age python3 sha256sum
out_dir="${out_dir:-${CFMEET_BACKUP_ROOT:-/var/backups/cfmeet}/pre-migration}"

umask 077
stamp="$(cfb_utc_stamp)"
dir="$out_dir/$service/$stamp-$label"
mkdir -p "$dir"
out="$dir/$service-$stamp.dump"
echo "pre-migration-snapshot.sh: $service before release $label"
if bash "$CFB_DIR/backup.sh" --service "$service" "${conn[@]}" --out "$out" \
  --age-recipients-file "$recipients" --label "pre-migration:$label"; then
  :
else
  code=$?
  rm -rf "$dir"
  echo "PRE_MIGRATION_SNAPSHOT=FAILED"
  cfb_die "$code" "snapshot of $service FAILED (exit $code): do not run the migrations"
fi

# Re-read what is on disk: the archive, its checksum, the manifest and no plaintext left.
if ! cfb_check_sha256 "$out.age" || [ -e "$out" ] || [ ! -s "$out.manifest.json" ] ||
  [ "$(python3 "$CFB_DIR/manifest.py" get "$out.manifest.json" encryption.sha256)" != "$(cfb_sha256 "$out.age")" ]; then
  echo "PRE_MIGRATION_SNAPSHOT=FAILED"
  cfb_die 11 "post-write verification of the $service snapshot failed: do not run the migrations"
fi

if [ "$offsite" -eq 1 ]; then
  rel_root="$(dirname "$out_dir")"
  rel="$(basename "$out_dir")/$service/$stamp-$label"
  if ! bash "$CFB_DIR/offsite.sh" push --root "$rel_root" --path "$rel" >&2; then
    echo "PRE_MIGRATION_SNAPSHOT_OFFSITE=FAILED"
    cfb_die 12 "the requested off-host copy failed (the local snapshot at $out.age is intact)"
  fi
fi

echo "PRE_MIGRATION_SNAPSHOT=$out.age"
echo "PRE_MIGRATION_SNAPSHOT_SHA256=$(cfb_sha256 "$out.age")"
echo "PRE_MIGRATION_MANIFEST=$out.manifest.json"
