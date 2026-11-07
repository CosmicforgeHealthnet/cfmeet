#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Scheduled backup of every configured CF Meet service database (SEC-010).
#
#   backup-all.sh [--config /etc/cfmeet-backup/backup.env] [--services "a b ..."]
#
# For each service in CFMEET_BACKUP_SERVICES (default: every active catalog entry that has a
# connection configured) it runs backup.sh with age encryption into
#   $CFMEET_BACKUP_ROOT/runs/<UTC stamp>/<service>/<service>-<stamp>.dump.age (+ .sha256,
#   .dump.sha256, .dump.manifest.json)
# then writes run-summary.json, copies the run off-host through offsite.sh when a target is
# configured, records the outcome in every manifest, and applies local retention (only after
# a fully successful run, so failing backups never cause good old ones to be pruned).
#
# Configuration (backup.env.example documents every key):
#   CFMEET_BACKUP_ROOT, CFMEET_BACKUP_AGE_RECIPIENTS_FILE (required: no unencrypted
#   scheduled backup is ever written), CFMEET_BACKUP_SERVICES,
#   CFMEET_BACKUP_<SERVICE>_URL_FILE | _URL_ENV_FILE [+ _URL_VAR], CFMEET_BACKUP_<SERVICE>_PG_WRAPPER,
#   CFMEET_BACKUP_KEEP_DAYS / _KEEP_MIN, CFMEET_BACKUP_PRE_MIGRATION_KEEP_DAYS / _KEEP_MIN,
#   CFMEET_BACKUP_OFFSITE_* (see offsite.sh), CFMEET_BACKUP_REQUIRE_OFFSITE=1.
#
# Output ends with KEY=VALUE evidence lines: BACKUP_RESULT, BACKUP_RUN_DIR, OFFSITE_STATUS,
# BACKUP_CLASSIFICATION ("LOCAL_ONLY — not disaster recovery" unless a verified off-host
# copy exists).
#
# Exit status:
#   0  every database dumped, verified and encrypted; off-host copy COPIED, or
#      NOT_CONFIGURED while CFMEET_BACKUP_REQUIRE_OFFSITE is not 1 (classification LOCAL_ONLY)
#   1  at least one database failed (the others were still backed up and copied)
#   2  usage / configuration error (including a missing recipients file) — nothing written
#   3  a required tool is missing
#   4  every database succeeded locally but the configured off-host copy FAILED
#   5  every database succeeded locally but no off-host target is configured while
#      CFMEET_BACKUP_REQUIRE_OFFSITE=1
set -euo pipefail
CFB_PROG=backup-all.sh
# shellcheck source=lib.sh
. "$(cd "$(dirname "$0")" && pwd)/lib.sh"

config="${CFMEET_BACKUP_CONFIG:-/etc/cfmeet-backup/backup.env}"
services_arg=""
while [ $# -gt 0 ]; do
  case "$1" in
    --config) config="$2"; shift 2 ;;
    --services) services_arg="$2"; shift 2 ;;
    *) cfb_die 2 "unknown argument $1" ;;
  esac
done
cfb_load_config "$config"
root="${CFMEET_BACKUP_ROOT:-}"
[ -n "$root" ] || cfb_die 2 "CFMEET_BACKUP_ROOT is required"
recipients="${CFMEET_BACKUP_AGE_RECIPIENTS_FILE:-}"
(cfb_check_recipients "$recipients") || exit 2
cfb_require_tools python3 sha256sum age flock
keep_days="${CFMEET_BACKUP_KEEP_DAYS:-14}"
keep_min="${CFMEET_BACKUP_KEEP_MIN:-7}"
pm_keep_days="${CFMEET_BACKUP_PRE_MIGRATION_KEEP_DAYS:-30}"
pm_keep_min="${CFMEET_BACKUP_PRE_MIGRATION_KEEP_MIN:-5}"
kind="${CFMEET_BACKUP_OFFSITE_KIND:-none}"
case "$kind" in none | rsync | rclone | dir) ;; *) cfb_die 2 "CFMEET_BACKUP_OFFSITE_KIND must be none, rsync, rclone or dir" ;; esac

services=()
read -r -a services <<<"${services_arg:-${CFMEET_BACKUP_SERVICES:-}}"
if [ "${#services[@]}" -eq 0 ]; then
  while IFS= read -r s; do
    if [ -n "$(cfb_service_var "$s" URL_FILE)$(cfb_service_var "$s" URL_ENV_FILE)" ]; then services+=("$s"); fi
  done < <(cfb_catalog_services active)
fi
[ "${#services[@]}" -gt 0 ] || cfb_die 2 "no services configured (CFMEET_BACKUP_SERVICES or CFMEET_BACKUP_<SERVICE>_URL_FILE)"
for s in "${services[@]}"; do
  cfb_require_active_service "$s"
  [ -n "$(cfb_service_var "$s" URL_FILE)$(cfb_service_var "$s" URL_ENV_FILE)" ] ||
    cfb_die 2 "no connection configured for $s (CFMEET_BACKUP_$(printf '%s' "$s" | tr 'a-z-' 'A-Z_')_URL_FILE or _URL_ENV_FILE)"
done

umask 077
mkdir -p "$root/runs"
chmod 0700 "$root"
exec 9>"$root/.lock"
flock -n 9 || cfb_die 2 "another backup run holds $root/.lock"

stamp="$(cfb_utc_stamp)"
rel="runs/$stamp"
run_dir="$root/$rel"
mkdir "$run_dir" || cfb_die 2 "run directory $run_dir already exists"
results="$(mktemp "${TMPDIR:-/tmp}/cfmeet-backup-results.XXXXXX")"
trap 'rm -f "$results"' EXIT
failures=0

for s in "${services[@]}"; do
  mkdir -p "$run_dir/$s"
  out="$run_dir/$s/$s-$stamp.dump"
  args=(--service "$s" --out "$out" --age-recipients-file "$recipients" --label scheduled)
  url_file="$(cfb_service_var "$s" URL_FILE)"
  env_file="$(cfb_service_var "$s" URL_ENV_FILE)"
  url_var="$(cfb_service_var "$s" URL_VAR)"
  if [ -n "$url_file" ]; then args+=(--database-url-file "$url_file"); else args+=(--database-url-env-file "$env_file" --url-var "${url_var:-DATABASE_URL}"); fi
  svc_wrapper="$(cfb_service_var "$s" PG_WRAPPER)"
  svc_wrapper="${svc_wrapper:-${CFMEET_PG_WRAPPER:-}}"
  echo "== $s"
  if (unset DATABASE_URL PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE; CFMEET_PG_WRAPPER="$svc_wrapper" bash "$CFB_DIR/backup.sh" "${args[@]}"); then
    echo "$s|OK|$rel/$s/$s-$stamp.dump.manifest.json|" >>"$results"
  else
    code=$?
    failures=$((failures + 1))
    rm -f "$out" "$out.sha256" "$out.age" "$out.age.sha256" "$out.manifest.json"
    echo "$s|FAILED||backup.sh exit $code" >>"$results"
    cfb_log "$s backup FAILED (exit $code)"
  fi
done

summary="$run_dir/run-summary.json"
manifests=()
while IFS='|' read -r _s status m _d; do
  if [ "$status" = OK ]; then manifests+=("$root/$m"); fi
done <"$results"

offsite_status=NOT_CONFIGURED
offsite_detail=""
if [ "$kind" != none ] && [ "${#manifests[@]}" -gt 0 ]; then
  python3 "$CFB_DIR/manifest.py" summary --out "$summary" --stamp "$stamp" --results "$results" \
    --offsite-status PENDING --target-kind "$kind"
  for m in "${manifests[@]}"; do python3 "$CFB_DIR/manifest.py" set-offsite "$m" --status PENDING --target-kind "$kind"; done
  if bash "$CFB_DIR/offsite.sh" push --root "$root" --path "$rel" >&2; then
    offsite_status=COPIED
  else
    offsite_status=FAILED
    offsite_detail="offsite.sh push failed"
  fi
elif [ "$kind" != none ]; then
  offsite_status=FAILED
  offsite_detail="no database was backed up; nothing to copy"
fi

detail_args=()
[ -z "$offsite_detail" ] || detail_args=(--detail "$offsite_detail")
for m in "${manifests[@]}"; do
  python3 "$CFB_DIR/manifest.py" set-offsite "$m" --status "$offsite_status" --target-kind "$kind" "${detail_args[@]}"
done
python3 "$CFB_DIR/manifest.py" summary --out "$summary" --stamp "$stamp" --results "$results" \
  --offsite-status "$offsite_status" --target-kind "$kind" "${detail_args[@]}"
if [ "$offsite_status" = COPIED ]; then
  # Second pass carries the final manifests / summary (status COPIED) to the target.
  bash "$CFB_DIR/offsite.sh" push --root "$root" --path "$rel" >/dev/null || {
    offsite_status=FAILED
    for m in "${manifests[@]}"; do python3 "$CFB_DIR/manifest.py" set-offsite "$m" --status FAILED --target-kind "$kind" --detail "final manifest copy failed"; done
    python3 "$CFB_DIR/manifest.py" summary --out "$summary" --stamp "$stamp" --results "$results" \
      --offsite-status FAILED --target-kind "$kind" --detail "final manifest copy failed"
  }
fi

if [ "$failures" -eq 0 ]; then
  bash "$CFB_DIR/prune.sh" --dir "$root/runs" --keep-days "$keep_days" --keep-min "$keep_min"
  if [ -d "$root/pre-migration" ]; then
    for d in "$root"/pre-migration/*/; do
      [ -d "$d" ] || continue
      bash "$CFB_DIR/prune.sh" --dir "${d%/}" --keep-days "$pm_keep_days" --keep-min "$pm_keep_min"
    done
  fi
fi

result=OK
[ "$failures" -eq 0 ] || result=PARTIAL_FAILURE
echo "BACKUP_RESULT=$result"
echo "BACKUP_RUN_DIR=$run_dir"
echo "OFFSITE_STATUS=$offsite_status"
if [ "$offsite_status" = COPIED ]; then
  echo "BACKUP_CLASSIFICATION=OFF_HOST_COPY_VERIFIED"
else
  echo "BACKUP_CLASSIFICATION=$CFB_LOCAL_ONLY_NOTE"
fi

if [ "$failures" -ne 0 ]; then exit 1; fi
if [ "$offsite_status" = FAILED ]; then exit 4; fi
if [ "$offsite_status" = NOT_CONFIGURED ] && [ "${CFMEET_BACKUP_REQUIRE_OFFSITE:-0}" = 1 ]; then exit 5; fi
exit 0
