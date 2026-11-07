#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# Unit tests for infra/deployment/backup that need no PostgreSQL: catalog coverage, URL
# handling (and that it never prints secrets), recipients refusal, drill target naming,
# off-host refusal / copy / verification, retention and the manifest helper.
#
#   bash infra/deployment/backup/tests/unit.sh
#
# Tests that need `age` or `flock` are skipped when the tool is absent (CI installs both,
# so nothing is skipped there: CFMEET_BACKUP_TESTS_STRICT=1 turns a skip into a failure).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tool_dir="$(cd "$here/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cfmeet-backup-unit.XXXXXX")"
trap 'rm -rf "$work"' EXIT
failures=0
skips=0
pass() { echo "  ok   $*"; }
fail() {
  echo "  FAIL $*"
  failures=$((failures + 1))
}
skip() {
  echo "  skip $*"
  skips=$((skips + 1))
}
# expect_exit <code> <description> <command...>: run with output captured in $work/last.out
expect_exit() {
  local want="$1" what="$2" got
  shift 2
  set +e
  "$@" >"$work/last.out" 2>&1
  got=$?
  set -e
  if [ "$got" = "$want" ]; then pass "$what (exit $got)"; else
    fail "$what (exit $got, wanted $want)"
    sed 's/^/       | /' "$work/last.out"
  fi
}
has() { command -v "$1" >/dev/null 2>&1; }
need() {
  if has "$1"; then return 0; fi
  if [ "${CFMEET_BACKUP_TESTS_STRICT:-0}" = 1 ]; then fail "$1 is required in strict mode"; else skip "$2 ($1 not installed)"; fi
  return 1
}

CFB_PROG=unit
# shellcheck source=../lib.sh
. "$tool_dir/lib.sh"

echo "== catalog covers every provisioned service database"
provisioned="$(sed -n 's/^case "\$service" in \([a-z|-]*\)).*/\1/p' "$tool_dir/../postgres/provision-service-database.sh" | tr '|' ' ')"
[ -n "$provisioned" ] && pass "read the provisioning service list" || fail "could not read provision-service-database.sh"
for s in $provisioned meeting-service; do
  if cfb_load_service "$s" && [ "$CFB_STATUS" = active ]; then pass "$s is an active catalog entry"; else fail "$s missing from catalog.conf"; fi
done
cfb_load_service meeting-service
[ "${CFB_REQUIRED[*]}" = "seaql_migrations" ] && pass "meeting-service requires seaql_migrations" || fail "meeting-service required history"
[ "${CFB_OPTIONAL[*]}" = "account_communications_migrations account_communications_facade_migrations account_security_migrations" ] &&
  pass "meeting-service records both additive histories" || fail "meeting-service optional histories"
[ "${CFB_IDENTITY[*]}" = "users meetings" ] && pass "meeting-service identity tables" || fail "meeting-service identity tables"
cfb_load_service organizations && [ "$CFB_STATUS" = active ] && [ "${CFB_REQUIRED[*]}" = "organizations_seaql_migrations" ] &&
  pass "organizations is active and requires its own history" || fail "organizations catalog entry"
expect_exit 2 "backup.sh refuses an unknown service" env CFMEET_PG_WRAPPER=false bash "$tool_dir/backup.sh" --service nope --out "$work/x.dump"

echo "== database URL handling"
secret='S3cr:et/p@ss%word'
encoded='S3cr%3Aet%2Fp%40ss%25word'
out="$(
  unset PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSSLMODE
  cfb_use_url "postgresql://backup_reader:$encoded@db.internal:6543/cfmeet_calling?sslmode=require"
  printf '%s|%s|%s|%s|%s|%s' "$PGHOST" "$PGPORT" "$PGUSER" "$PGPASSWORD" "$PGDATABASE" "$PGSSLMODE"
)"
[ "$out" = "db.internal|6543|backup_reader|$secret|cfmeet_calling|require" ] && pass "URL becomes libpq variables (password decoded)" || fail "URL parsing: got something else"
printed="$(cfb_use_url "postgres://u:$encoded@h/db" 2>&1)"
[ -z "$printed" ] && pass "cfb_use_url prints nothing" || fail "cfb_use_url printed output"
msg="$( (cfb_use_url "postgres://u:$encoded@h/db?evil=$encoded") 2>&1 || true)"
if echo "$msg" | grep -q "unsupported database URL parameter: evil" && ! echo "$msg" | grep -qF "$secret" && ! echo "$msg" | grep -qF "$encoded"; then
  pass "unknown URL parameter refused by name only"
else fail "unknown URL parameter handling"; fi
if (cfb_use_url "mysql://u:p@h/db") 2>/dev/null; then fail "non-postgres URL accepted"; else pass "non-postgres URL refused"; fi
if (cfb_use_url "postgres://u:p@h") 2>/dev/null; then fail "URL without database accepted"; else pass "URL without a database refused"; fi
printf 'OTHER=1\nDATABASE_URL=postgres://u:%s@h:5432/envdb\n' "$encoded" >"$work/svc.env"
got="$(cfb_read_url_source "" "" "$work/svc.env" DATABASE_URL)"
[ "$got" = "postgres://u:$encoded@h:5432/envdb" ] && pass "URL read from a docker env file" || fail "env-file URL"
printf 'postgres://u:%s@h/filedb\r\n' "$encoded" >"$work/url"
got="$(cfb_read_url_source "" "$work/url" "" DATABASE_URL)"
[ "$got" = "postgres://u:$encoded@h/filedb" ] && pass "URL read from a file (CRLF stripped)" || fail "file URL"

echo "== age recipients"
expect_exit 5 "missing recipients file refused" bash -c ". '$tool_dir/lib.sh'; cfb_check_recipients '$work/none.txt'"
expect_exit 5 "empty recipients setting refused" bash -c ". '$tool_dir/lib.sh'; cfb_check_recipients ''"
printf '# owner key\nAGE-SECRET-KEY-1QQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQQ\n' >"$work/private.txt"
expect_exit 5 "recipients file holding a PRIVATE key refused" bash -c ". '$tool_dir/lib.sh'; cfb_check_recipients '$work/private.txt'"
printf '# owner key\nage1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq\n' >"$work/public.txt"
expect_exit 0 "public recipients file accepted" bash -c ". '$tool_dir/lib.sh'; cfb_check_recipients '$work/public.txt'"
expect_exit 5 "backup.sh refuses a missing recipients file before touching the database" \
  env CFMEET_PG_WRAPPER=false bash "$tool_dir/backup.sh" --service calling --out "$work/x.dump" --age-recipients-file "$work/none.txt"
expect_exit 5 "pre-migration-snapshot.sh refuses without recipients" \
  env -u CFMEET_BACKUP_AGE_RECIPIENTS_FILE CFMEET_PG_WRAPPER=false bash "$tool_dir/pre-migration-snapshot.sh" --service meeting-service --label run-1
[ ! -e "$work/x.dump" ] && pass "no dump file was created by the refusals" || fail "a refusal left a dump behind"

echo "== restore drill target naming"
for bad in cfmeet_calling postgres Drill_x drill_ restore- "drill_x;drop"; do
  expect_exit 8 "target '$bad' refused" bash "$tool_dir/restore-drill.sh" --in "$work/x.dump" --target-database "$bad"
done

echo "== off-host copy"
mkdir -p "$work/root/runs/20260101T000000Z/calling" "$work/offhost"
run="$work/root/runs/20260101T000000Z"
{ echo "age-encryption.org/v1"; echo "-> X25519 fake"; } >"$run/calling/calling.dump.age"
(cd "$run/calling" && sha256sum calling.dump.age >calling.dump.age.sha256)
echo '{}' >"$run/calling/calling.dump.manifest.json"
expect_exit 0 "kind none reports NOT_CONFIGURED" env CFMEET_BACKUP_OFFSITE_KIND=none bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
grep -q '^OFFSITE_STATUS=NOT_CONFIGURED$' "$work/last.out" && grep -q 'LOCAL_ONLY' "$work/last.out" && pass "NOT_CONFIGURED is labelled LOCAL_ONLY" || fail "NOT_CONFIGURED output"
expect_exit 0 "dir target copies and verifies" env CFMEET_BACKUP_OFFSITE_KIND=dir CFMEET_BACKUP_OFFSITE_DIR="$work/offhost" CFMEET_BACKUP_SOURCE_NAME=unit \
  bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
cmp -s "$run/calling/calling.dump.age" "$work/offhost/unit/runs/20260101T000000Z/calling/calling.dump.age" && pass "encrypted archive present at the target" || fail "archive not copied"
expect_exit 2 "a path escaping the root is refused" env CFMEET_BACKUP_OFFSITE_KIND=dir CFMEET_BACKUP_OFFSITE_DIR="$work/offhost" \
  bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/../runs
echo plaintext >"$run/calling/calling.dump"
expect_exit 3 "a plaintext dump is never sent off-host" env CFMEET_BACKUP_OFFSITE_KIND=dir CFMEET_BACKUP_OFFSITE_DIR="$work/offhost" \
  bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
rm "$run/calling/calling.dump"
echo "not really encrypted" >"$run/calling/fake.dump.age"
expect_exit 3 "a .age file without an age header is refused" env CFMEET_BACKUP_OFFSITE_KIND=dir CFMEET_BACKUP_OFFSITE_DIR="$work/offhost" \
  bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
rm "$run/calling/fake.dump.age"
expect_exit 2 "an unknown target kind is refused" env CFMEET_BACKUP_OFFSITE_KIND=s3 bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
if need rsync "rsync target (local destination)"; then
  mkdir -p "$work/rsync-target"
  expect_exit 0 "rsync target copies and passes the checksum dry run" env CFMEET_BACKUP_OFFSITE_KIND=rsync CFMEET_BACKUP_OFFSITE_RSYNC_DEST="$work/rsync-target" \
    CFMEET_BACKUP_SOURCE_NAME=unit bash "$tool_dir/offsite.sh" push --root "$work/root" --path runs/20260101T000000Z
fi

echo "== retention"
p="$work/prune"
for d in 20200101T000000Z 20200102T000000Z 20200103T000000Z 20200104T000000Z-release-9; do
  mkdir -p "$p/$d/svc"
  echo '{}' >"$p/$d/svc/svc.dump.manifest.json"
done
fresh="$(date -u +%Y%m%dT%H%M%SZ)"
mkdir -p "$p/$fresh" "$p/20200101T000000Z-foreign" "$p/notes"
echo '{}' >"$p/$fresh/run-summary.json"
expect_exit 0 "prune runs" bash "$tool_dir/prune.sh" --dir "$p" --keep-days 30 --keep-min 3
[ -d "$p/20200104T000000Z-release-9" ] && [ -d "$p/20200103T000000Z" ] && pass "keep-min newest kept even when old" || fail "keep-min not honoured"
[ ! -d "$p/20200101T000000Z" ] && [ ! -d "$p/20200102T000000Z" ] && pass "older backups beyond keep-min pruned" || fail "old backups not pruned"
[ -d "$p/20200101T000000Z-foreign" ] && [ -d "$p/notes" ] && pass "directories without our manifests left alone" || fail "foreign directory pruned"
expect_exit 2 "keep-min 0 refused" bash "$tool_dir/prune.sh" --dir "$p" --keep-days 1 --keep-min 0

echo "== manifest helper"
printf 'seaql_migrations|true|true|3|m3\naccount_communications_migrations|false|false|0|\n' >"$work/history"
echo dump >"$work/m.dump"
python3 "$tool_dir/manifest.py" write --out "$work/m.json" --service meeting-service --database cosmicforge_rtc --label scheduled \
  --pg-dump-version "pg_dump (PostgreSQL) 16.4" --server-version 16.4 --db-size 100 --table-count 5 --history-file "$work/history" \
  --dump-file "$work/m.dump" --dump-sha256 "$(cfb_sha256 "$work/m.dump")" --dump-size 5 --plaintext-retained no \
  --encrypted-file "$work/m.dump.age" --encrypted-sha256 abc --encrypted-size 9 --recipient age1example
[ "$(python3 "$tool_dir/manifest.py" get "$work/m.json" offsite.status)" = NOT_CONFIGURED ] && pass "new manifest: offsite NOT_CONFIGURED" || fail "manifest offsite default"
[ "$(python3 "$tool_dir/manifest.py" get "$work/m.json" offsite.classification)" = "$CFB_LOCAL_ONLY_NOTE" ] && pass "new manifest: classified LOCAL_ONLY" || fail "manifest classification"
[ "$(python3 "$tool_dir/manifest.py" history "$work/m.json")" = "$(cat "$work/history")" ] && pass "history round-trips" || fail "history round-trip"
python3 "$tool_dir/manifest.py" set-offsite "$work/m.json" --status COPIED --target-kind rsync
[ "$(python3 "$tool_dir/manifest.py" get "$work/m.json" offsite.classification)" = OFF_HOST_COPY_VERIFIED ] && pass "COPIED is classified OFF_HOST_COPY_VERIFIED" || fail "COPIED classification"
[ "$(python3 "$tool_dir/manifest.py" get "$work/m.json" encryption.tool)" = age ] && pass "encryption recorded" || fail "encryption block"

echo "== configuration file"
printf 'CFMEET_BACKUP_ROOT=%s\nCFMEET_BACKUP_SERVICES="calling messaging"\n# comment\n' "$work/r" >"$work/ok.env"
chmod 600 "$work/ok.env"
got="$(bash -c ". '$tool_dir/lib.sh'; cfb_load_config '$work/ok.env'; printf '%s|%s' \"\$CFMEET_BACKUP_ROOT\" \"\$CFMEET_BACKUP_SERVICES\"")"
[ "$got" = "$work/r|calling messaging" ] && pass "CFMEET_* lines loaded (quotes stripped)" || fail "config load"
printf 'CFMEET_BACKUP_ROOT=/x\nrm -rf /\n' >"$work/bad.env"
chmod 600 "$work/bad.env"
expect_exit 2 "a config line that is not CFMEET_*=value is refused" bash -c ". '$tool_dir/lib.sh'; cfb_load_config '$work/bad.env'"
printf 'CFMEET_BACKUP_ROOT=%s\n' "$work/r" >"$work/norecip.env"
chmod 600 "$work/norecip.env"
expect_exit 2 "backup-all.sh refuses to run without an age recipients file" bash "$tool_dir/backup-all.sh" --config "$work/norecip.env"
[ ! -d "$work/r" ] && pass "nothing written by the refused run" || fail "refused run wrote files"
chmod 666 "$work/ok.env" 2>/dev/null || true
if [ "$(stat -c %a "$work/ok.env" 2>/dev/null)" = 666 ]; then
  expect_exit 2 "a world-writable config file is refused" bash -c ". '$tool_dir/lib.sh'; cfb_load_config '$work/ok.env'"
else
  skip "world-writable config refusal (filesystem ignores chmod)"
fi

echo
if [ "$failures" -ne 0 ]; then
  echo "backup tooling unit tests: FAILED ($failures failure(s), $skips skipped)"
  exit 1
fi
echo "backup tooling unit tests: OK ($skips skipped)"
