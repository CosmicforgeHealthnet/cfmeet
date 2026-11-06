#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Restore drill: restore one backup (plain or age-encrypted) into a FRESHLY CREATED, isolated
# database and verify it against its manifest. This is the procedure for both a scheduled
# drill and a real recovery rehearsal; it never touches a live service database.
#
#   restore-drill.sh --in <file.dump | file.dump.age> --target-database <drill_... | restore_...>
#                    <admin connection> [--manifest M] [--age-identity-file KEY]
#                    [--replace-existing-drill] [--drop-after] [--pg-wrapper CMD]
#
#   <admin connection>  --admin-url-file F, --admin-url URL, or CFMEET_DRILL_ADMIN_URL: a role
#                       with CREATEDB on the drill server, pointing at a maintenance database
#                       (e.g. .../postgres). Run drills where the age identity lives (the
#                       owner's machine or a dedicated drill host), not on the production host.
#
# Safety rules:
#   * the target name must start with drill_ or restore_ (so a live database such as
#     cfmeet_calling can never be a target);
#   * a target that already exists is refused unless --replace-existing-drill is given (and
#     even then only a drill_/restore_ database can be dropped);
#   * both checksums are verified before the target database is created.
#
# Exit status: 0 drill passed; 2 usage; 3 tool missing; 4 archive / checksum / manifest file
# missing; 5 checksum mismatch or decryption failure; 8 target name refused; 9 target exists;
# 10 restore or verification failed.
set -euo pipefail
CFB_PROG=restore-drill.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

in="" target="" manifest="" identity="" admin_url="" admin_file="" replace=0 drop_after=0
while [ $# -gt 0 ]; do
  case "$1" in
    --in) in="$2"; shift 2 ;;
    --target-database) target="$2"; shift 2 ;;
    --manifest) manifest="$2"; shift 2 ;;
    --age-identity-file) identity="$2"; shift 2 ;;
    --admin-url) admin_url="$2"; shift 2 ;;
    --admin-url-file) admin_file="$2"; shift 2 ;;
    --replace-existing-drill) replace=1; shift ;;
    --drop-after) drop_after=1; shift ;;
    --pg-wrapper) export CFMEET_PG_WRAPPER="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
[ -n "$in" ] && [ -n "$target" ] || cfb_die 2 "--in and --target-database are required"
[[ "$target" =~ ^(drill|restore)_[a-z0-9_]{1,55}$ ]] ||
  cfb_die 8 "refusing target database '$target': a drill target must be named drill_<...> or restore_<...> (lower-case letters, digits, _)"
cfb_require_pg_tools psql pg_restore
cfb_require_tools sha256sum python3

plain="${in%.age}"
manifest="${manifest:-$plain.manifest.json}"
[ -f "$in" ] && [ -f "$in.sha256" ] || cfb_die 4 "$in or its .sha256 is missing"
[ -f "$manifest" ] || cfb_die 4 "manifest $manifest is missing"
cfb_check_sha256 "$in" || cfb_die 5 "checksum mismatch for $in; refusing to restore"
expected_sha="$(python3 "$CFB_DIR/manifest.py" get "$manifest" dump.sha256)"
if [ "$in" != "$plain" ]; then
  [ -f "$plain.sha256" ] || cfb_die 4 "$plain.sha256 is missing"
  [ "$(awk 'NR == 1 { print $1 }' "$plain.sha256")" = "$expected_sha" ] ||
    cfb_die 5 "$plain.sha256 does not match the manifest"
  [ -n "$identity" ] || cfb_die 2 "$in is encrypted: pass --age-identity-file"
elif [ "$(cfb_sha256 "$in")" != "$expected_sha" ]; then
  cfb_die 5 "$in does not match the manifest checksum"
fi

admin_value="$admin_url"
if [ -z "$admin_value" ] && [ -z "$admin_file" ]; then admin_value="${CFMEET_DRILL_ADMIN_URL:-}"; fi
[ -n "$admin_value" ] || [ -n "$admin_file" ] || cfb_die 2 "an admin connection is required (--admin-url-file, --admin-url or CFMEET_DRILL_ADMIN_URL)"
unset DATABASE_URL
cfb_connect "$admin_value" "$admin_file" ""
maintenance_db="$PGDATABASE"
[ "$maintenance_db" != "$target" ] || cfb_die 8 "the admin connection must point at a maintenance database, not the drill target"

exists="$(cfb_sql "SELECT count(*) FROM pg_database WHERE datname = '$target'")"
if [ "$exists" != 0 ]; then
  [ "$replace" -eq 1 ] || cfb_die 9 "database $target already exists; refusing to restore over it (pass --replace-existing-drill to drop and recreate a previous drill)"
  cfb_log "dropping previous drill database $target"
  cfb_sql "DROP DATABASE \"$target\" WITH (FORCE)" >/dev/null
fi

drop_target() {
  PGDATABASE="$maintenance_db" cfb_sql "DROP DATABASE IF EXISTS \"$target\" WITH (FORCE)" >/dev/null 2>&1 || true
}
if [ "$drop_after" -eq 1 ]; then trap drop_target EXIT; fi
cfb_sql "CREATE DATABASE \"$target\" TEMPLATE template0" >/dev/null

restore_args=(--in "$in")
[ -z "$identity" ] || restore_args+=(--age-identity-file "$identity")
started=$(date +%s)
if ! PGDATABASE="$target" bash "$CFB_DIR/restore.sh" "${restore_args[@]}"; then
  echo "RESTORE_DRILL=FAILED stage=restore target=$target"
  exit 10
fi
restored=$(date +%s)
if ! PGDATABASE="$target" bash "$CFB_DIR/verify-restore.sh" --manifest "$manifest"; then
  echo "RESTORE_DRILL=FAILED stage=verify target=$target"
  exit 10
fi
echo "RESTORE_DRILL=PASS service=$(python3 "$CFB_DIR/manifest.py" get "$manifest" service) target=$target restore_seconds=$((restored - started)) dropped_after=$([ "$drop_after" -eq 1 ] && echo yes || echo no)"
