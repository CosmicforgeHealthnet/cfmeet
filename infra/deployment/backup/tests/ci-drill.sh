#!/usr/bin/env bash
# shellcheck source-path=SCRIPTDIR
# End-to-end backup / restore drill for infra/deployment/backup against a THROWAWAY
# PostgreSQL server (the CI service container). Never point it at a real database.
#
#   CFMEET_DRILL_ADMIN_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
#     bash infra/deployment/backup/tests/ci-drill.sh
#
# Requires psql / pg_dump / pg_restore (16+), age + age-keygen, rsync, rclone, python3, flock;
# docker for the client-container (wrapper) case. Optional: CFMEET_DRILL_EVIDENCE_DIR receives
# the manifests and run summaries (never dumps or keys).
#
# Simulates the production split: the "host" directory holds only the PUBLIC age recipient
# and a read-only backup role's URL; the "owner" directory holds the private age identity and
# the drill server's admin URL; "offhost" stands in for an independent backup location.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tool_dir="$(cd "$here/.." && pwd)"
admin_url="${CFMEET_DRILL_ADMIN_URL:?set CFMEET_DRILL_ADMIN_URL to a throwaway PostgreSQL superuser URL}"
work="$(mktemp -d "${TMPDIR:-/tmp}/cfmeet-drill.XXXXXX")"
evidence="${CFMEET_DRILL_EVIDENCE_DIR:-$work/evidence}"
log="$work/all-output.log"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/host" "$work/owner" "$work/offhost" "$evidence"
chmod 700 "$work/host" "$work/owner"
: >"$log"
failures=0

CFB_PROG=ci-drill
# shellcheck source=../lib.sh
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
# expect_exit <code> <description> <command...>: output in $work/last.out (and the full log)
expect_exit() {
  local want="$1" what="$2" got
  shift 2
  set +e
  "$@" >"$work/last.out" 2>&1
  got=$?
  set -e
  { echo "### $what"; cat "$work/last.out"; } >>"$log"
  if [ "$got" = "$want" ]; then
    pass "$what (exit $got)"
    grep -E '^[A-Z_]+=' "$work/last.out" | sed 's/^/       /' || true
  else
    fail "$what (exit $got, wanted $want)"
    sed 's/^/       | /' "$work/last.out"
  fi
}
last_value() { sed -n "s/^$1=//p" "$work/last.out" | tail -n 1; }

url_for() { # url_for <role> <password> <database>
  python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); host=p.hostname+(":"+str(p.port) if p.port else ""); print(u.urlunsplit(p._replace(netloc=u.quote(sys.argv[2],safe="")+":"+u.quote(sys.argv[3],safe="")+"@"+host, path="/"+sys.argv[4], query="")))' "$admin_url" "$1" "$2" "$3"
}
admin_db_url() { python3 -c 'import sys,urllib.parse as u; p=u.urlsplit(sys.argv[1]); print(u.urlunsplit(p._replace(path="/"+sys.argv[2])))' "$admin_url" "$1"; }
adm() { psql "$admin_url" -XAtq -v ON_ERROR_STOP=1 "$@"; }
dbq() { psql "$(admin_db_url "$1")" -XAtq -v ON_ERROR_STOP=1 -c "$2"; }
db_exists() { [ "$(adm -c "SELECT count(*) FROM pg_database WHERE datname = '$1'")" = 1 ]; }
sample_db() { echo "cfmeet_$(printf '%s' "$1" | tr '-' '_')_sample"; }
mj() { python3 "$tool_dir/manifest.py" get "$@"; }
# fails_with <url> <sql> <pattern>: the statement fails AND the error matches <pattern>
# (so a typo or an outage never counts as a successful refusal).
fails_with() {
  local err
  if err="$(psql "$1" -XAtq -v ON_ERROR_STOP=1 -c "$2" 2>&1 >/dev/null)"; then return 1; fi
  echo "$err" | grep -qiE "$3"
}

echo "== tools"
for tool in psql pg_dump pg_restore age age-keygen rsync rclone python3 flock sha256sum; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool"; exit 3; }
done
pg_dump --version
age --version
adm -c "SELECT version()"

echo "== owner key pair (private identity stays in owner/, host gets only the public recipient)"
age-keygen -o "$work/owner/identity.txt" 2>/dev/null
age-keygen -y "$work/owner/identity.txt" >"$work/host/age-recipients.txt"
check "host recipients file holds no private key" bash -c "! grep -q AGE-SECRET-KEY '$work/host/age-recipients.txt'"
printf '%s\n' "$admin_url" >"$work/owner/admin.url"
chmod 600 "$work/owner/admin.url" "$work/owner/identity.txt"

echo "== sample service databases (realistic schemas, migration histories, data)"
mapfile -t services < <(cfb_catalog_services active)
history_sql() { # history_sql <table> <rows>
  local t="$1" n="$2" i
  echo "CREATE TABLE $t (version varchar PRIMARY KEY, applied_at bigint NOT NULL);"
  for i in $(seq 1 "$n"); do
    printf "INSERT INTO %s VALUES ('m2026%04d_%06d_step_%d', %d);\n" "$t" "$((1000 + i))" "$i" "$i" "$((1760000000 + i))"
  done
}
for s in "${services[@]}"; do
  db="$(sample_db "$s")"
  adm -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null
  adm -c "CREATE DATABASE $db" >/dev/null
  adm -c "REVOKE ALL ON DATABASE $db FROM PUBLIC" >/dev/null
  cfb_load_service "$s"
  {
    n=3
    for t in "${CFB_REQUIRED[@]}"; do history_sql "$t" "$n"; n=$((n + 2)); done
    case "$s" in
      meeting-service)
        history_sql account_communications_migrations 2
        history_sql account_communications_facade_migrations 1
        cat <<'SQL'
CREATE TABLE users (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), email text NOT NULL UNIQUE,
  username text NOT NULL, password_hash text, role text NOT NULL CHECK (role IN ('user','admin','guest')),
  status text NOT NULL DEFAULT 'active', created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE meetings (id uuid PRIMARY KEY DEFAULT gen_random_uuid(), host_id uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  title text NOT NULL, room_name text NOT NULL UNIQUE, starts_at timestamptz, settings jsonb NOT NULL DEFAULT '{}');
CREATE TABLE participants (meeting_id uuid NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
  user_id uuid NOT NULL REFERENCES users(id), role text NOT NULL, joined_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (meeting_id, user_id));
CREATE TABLE session_logs (id bigserial PRIMARY KEY, meeting_id uuid REFERENCES meetings(id), event text NOT NULL, at timestamptz NOT NULL DEFAULT now());
CREATE INDEX meetings_host_idx ON meetings (host_id);
CREATE VIEW active_meetings AS SELECT id, title FROM meetings WHERE starts_at IS NOT NULL;
CREATE FUNCTION touch_updated_at() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.updated_at = now(); RETURN NEW; END $$;
CREATE TRIGGER users_touch BEFORE UPDATE ON users FOR EACH ROW EXECUTE FUNCTION touch_updated_at();
INSERT INTO users (email, username, password_hash, role)
  SELECT 'user' || g || '@example.test', 'user' || g, md5('pw' || g), CASE WHEN g = 1 THEN 'admin' ELSE 'user' END FROM generate_series(1, 25) g;
INSERT INTO meetings (host_id, title, room_name, starts_at, settings)
  SELECT u.id, 'Meeting ' || g, 'room-' || g, now() + g * interval '1 hour', jsonb_build_object('waiting_room', g % 2 = 0)
  FROM generate_series(1, 40) g JOIN LATERAL (SELECT id FROM users ORDER BY email OFFSET (g % 25) LIMIT 1) u ON true;
INSERT INTO participants (meeting_id, user_id, role)
  SELECT m.id, u.id, 'attendee' FROM (SELECT id FROM meetings ORDER BY room_name LIMIT 10) m CROSS JOIN (SELECT id FROM users ORDER BY email LIMIT 10) u;
INSERT INTO session_logs (meeting_id, event) SELECT id, 'started' FROM meetings;
UPDATE users SET status = 'active' WHERE email = 'user1@example.test';
SQL
        ;;
      communications)
        history_sql communications_unified_seaql_migrations 2
        cat <<'SQL'
CREATE TABLE communication_profiles (principal_id uuid PRIMARY KEY DEFAULT gen_random_uuid(), cf_meet_id text NOT NULL UNIQUE,
  display_name text NOT NULL, credentials_not_before timestamptz, created_at timestamptz NOT NULL DEFAULT now());
INSERT INTO communication_profiles (cf_meet_id, display_name) SELECT 'CF-' || lpad(g::text, 8, '0'), 'Person ' || g FROM generate_series(1, 20) g;
SQL
        ;;
      calling)
        # calling_communications_migrations deliberately absent: an optional history that has
        # not been applied yet must be recorded as absent and stay absent after restore.
        cat <<'SQL'
CREATE TABLE calls (id bigserial PRIMARY KEY, tenant_id uuid NOT NULL, current_state text NOT NULL
  CHECK (current_state IN ('RINGING','CONNECTED','ENDED')), created_at timestamptz NOT NULL DEFAULT now());
INSERT INTO calls (tenant_id, current_state) SELECT gen_random_uuid(), (ARRAY['RINGING','CONNECTED','ENDED'])[1 + g % 3] FROM generate_series(1, 30) g;
SQL
        ;;
    esac
    for t in "${CFB_IDENTITY[@]}"; do
      case "$s:$t" in meeting-service:* | communications:communication_profiles) ;; *) echo "CREATE TABLE $t (id bigserial PRIMARY KEY);" ;; esac
    done
    prefix="$(printf '%s' "$s" | tr '-' '_')"
    echo "CREATE TABLE ${prefix}_records (id bigserial PRIMARY KEY, payload jsonb NOT NULL, at timestamptz NOT NULL DEFAULT now());"
    echo "INSERT INTO ${prefix}_records (payload) SELECT jsonb_build_object('n', g) FROM generate_series(1, 50) g;"
  } | psql "$(admin_db_url "$db")" -Xq -v ON_ERROR_STOP=1 >/dev/null
  pass "created $db"
done
adm -c "DROP DATABASE IF EXISTS cfmeet_impostor_sample WITH (FORCE)" >/dev/null
adm -c "CREATE DATABASE cfmeet_impostor_sample" >/dev/null
history_sql seaql_migrations 2 | psql "$(admin_db_url cfmeet_impostor_sample)" -Xq -v ON_ERROR_STOP=1 >/dev/null
adm -c "DROP DATABASE IF EXISTS cfmeet_ungranted_sample" >/dev/null
adm -c "CREATE DATABASE cfmeet_ungranted_sample" >/dev/null
adm -c "REVOKE ALL ON DATABASE cfmeet_ungranted_sample FROM PUBLIC" >/dev/null

echo "== read-only backup role (provision-backup-role.sh)"
backup_pw="$(od -An -N24 -tx1 /dev/urandom | tr -d ' \n')"
printf '%s\n' "$backup_pw" >"$work/host/backup-role.password"
dbs=""
for s in "${services[@]}"; do dbs="$dbs $(sample_db "$s")"; done
dbs="$dbs cfmeet_impostor_sample"
expect_exit 0 "provision the cfmeet_backup role" bash "$tool_dir/provision-backup-role.sh" --admin-url-file "$work/owner/admin.url" \
  --password-file "$work/host/backup-role.password" --databases "$dbs"
expect_exit 0 "provisioning is idempotent" bash "$tool_dir/provision-backup-role.sh" --admin-url-file "$work/owner/admin.url" \
  --password-file "$work/host/backup-role.password" --databases "$dbs"
reader_url="$(url_for cfmeet_backup "$backup_pw" "$(sample_db calling)")"
check "backup role can read" [ "$(psql "$reader_url" -XAtq -c 'SELECT count(*) FROM calls')" = 30 ]
check "backup role cannot write" fails_with "$reader_url" "INSERT INTO calls (tenant_id, current_state) VALUES (gen_random_uuid(), 'ENDED')" "permission denied"
check "backup role cannot run DDL" fails_with "$reader_url" "CREATE TABLE intruder (id int)" "permission denied"
check "backup role cannot connect to an ungranted database" fails_with "$(url_for cfmeet_backup "$backup_pw" cfmeet_ungranted_sample)" "SELECT 1" "permission denied"
mkdir -p "$work/host/urls"
for s in "${services[@]}" impostor; do
  db="$(sample_db "$s")"
  url_for cfmeet_backup "$backup_pw" "$db" >"$work/host/urls/$s.url"
done
chmod 600 "$work/host/urls/"*.url

write_config() { # write_config <file> <offsite kind> [extra lines...]
  local file="$1" kind="$2"
  shift 2
  {
    echo "CFMEET_BACKUP_ROOT=$work/host/backups"
    echo "CFMEET_BACKUP_AGE_RECIPIENTS_FILE=$work/host/age-recipients.txt"
    echo "CFMEET_BACKUP_SERVICES=${services[*]}"
    for s in "${services[@]}"; do
      echo "CFMEET_BACKUP_$(printf '%s' "$s" | tr 'a-z-' 'A-Z_')_URL_FILE=$work/host/urls/$s.url"
    done
    echo "CFMEET_BACKUP_KEEP_DAYS=14"
    echo "CFMEET_BACKUP_KEEP_MIN=2"
    echo "CFMEET_BACKUP_SOURCE_NAME=ci-host"
    echo "CFMEET_BACKUP_OFFSITE_KIND=$kind"
    printf '%s\n' "$@"
  } >"$file"
  chmod 600 "$file"
}

echo "== scheduled run: every database -> encrypted -> off-host copy (dir target)"
mkdir -p "$work/host/backups/runs"
for stale in 20200101T000000Z 20200102T000000Z 20200103T000000Z; do
  mkdir -p "$work/host/backups/runs/$stale"
  echo '{}' >"$work/host/backups/runs/$stale/run-summary.json"
done
write_config "$work/host/backup-dir.env" dir "CFMEET_BACKUP_OFFSITE_DIR=$work/offhost"
expect_exit 0 "backup-all.sh with an off-host target" bash "$tool_dir/backup-all.sh" --config "$work/host/backup-dir.env"
run_dir="$(last_value BACKUP_RUN_DIR)"
stamp="$(basename "$run_dir")"
check "OFFSITE_STATUS=COPIED" [ "$(last_value OFFSITE_STATUS)" = COPIED ]
check "classified OFF_HOST_COPY_VERIFIED" [ "$(last_value BACKUP_CLASSIFICATION)" = OFF_HOST_COPY_VERIFIED ]
check "run summary result OK" [ "$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["result"])' "$run_dir/run-summary.json")" = OK ]
offhost_run="$work/offhost/ci-host/runs/$stamp"
for s in "${services[@]}"; do
  base="$run_dir/$s/$s-$stamp.dump"
  check "$s: encrypted archive, checksums and manifest written" test -s "$base.age" -a -s "$base.age.sha256" -a -s "$base.sha256" -a -s "$base.manifest.json"
  check "$s: no plaintext dump left on the host" test ! -e "$base"
  check "$s: archive is age-encrypted" [ "$(head -c 21 "$base.age")" = "age-encryption.org/v1" ]
  check "$s: manifest encryption=age, offsite=COPIED" [ "$(mj "$base.manifest.json" encryption.tool)/$(mj "$base.manifest.json" offsite.status)" = "age/COPIED" ]
  check "$s: off-host copy identical" cmp -s "$base.age" "$offhost_run/$s/$s-$stamp.dump.age"
  check "$s: off-host manifest carries the final COPIED status" [ "$(mj "$offhost_run/$s/$s-$stamp.dump.manifest.json" offsite.status)" = COPIED ]
  cfb_load_service "$s"
  first="${CFB_REQUIRED[0]}"
  src_rows="$(dbq "$(sample_db "$s")" "SELECT count(*) FROM $first")"
  rec_rows="$(python3 "$tool_dir/manifest.py" history "$base.manifest.json" | awk -F'|' -v t="$first" '$1 == t { print $4 }')"
  check "$s: manifest records $first rows ($src_rows)" [ "$src_rows" = "$rec_rows" ]
done
m="$run_dir/meeting-service/meeting-service-$stamp.dump.manifest.json"
check "meeting-service: base + both communications histories present, account_security not yet applied" \
  [ "$(python3 "$tool_dir/manifest.py" history "$m" | awk -F'|' '$3 == "true"' | wc -l | tr -d ' ')" = 3 ]
m="$run_dir/calling/calling-$stamp.dump.manifest.json"
check "calling: unapplied optional history recorded absent" \
  [ "$(python3 "$tool_dir/manifest.py" history "$m" | awk -F'|' '$1 == "calling_communications_migrations" { print $3 }')" = false ]
check "no plaintext .dump anywhere on the host or off-host" [ -z "$(find "$work/host/backups" "$work/offhost" -name '*.dump' -print)" ]
check "retention pruned stale runs beyond keep-min" test ! -e "$work/host/backups/runs/20200101T000000Z" -a ! -e "$work/host/backups/runs/20200102T000000Z"
check "retention kept keep-min newest runs" test -e "$work/host/backups/runs/20200103T000000Z" -a -e "$run_dir"

echo "== restore drill from the OFF-HOST copies (owner identity, fresh drill_ databases)"
for s in "${services[@]}"; do
  target="drill_$(printf '%s' "$s" | tr '-' '_')"
  keep=(--drop-after)
  [ "$s" != meeting-service ] || keep=()
  expect_exit 0 "$s restore drill" bash "$tool_dir/restore-drill.sh" --in "$offhost_run/$s/$s-$stamp.dump.age" \
    --age-identity-file "$work/owner/identity.txt" --target-database "$target" --admin-url-file "$work/owner/admin.url" "${keep[@]}"
  check "$s: RESTORE_DRILL=PASS reported" grep -q '^RESTORE_DRILL=PASS' "$work/last.out"
done
for t in users meetings participants session_logs; do
  check "meeting-service: $t contents identical after restore" \
    [ "$(dbq "$(sample_db meeting-service)" "SELECT md5(coalesce(string_agg(x::text, ',' ORDER BY x::text), '')) FROM $t x")" = \
      "$(dbq drill_meeting_service "SELECT md5(coalesce(string_agg(x::text, ',' ORDER BY x::text), '')) FROM $t x")" ]
done
check "meeting-service: sequence position restored" \
  [ "$(dbq "$(sample_db meeting-service)" "SELECT last_value FROM session_logs_id_seq")" = "$(dbq drill_meeting_service "SELECT last_value FROM session_logs_id_seq")" ]
check "meeting-service: view, function and trigger restored" \
  [ "$(dbq drill_meeting_service "SELECT (to_regclass('active_meetings') IS NOT NULL)::int + (to_regproc('touch_updated_at') IS NOT NULL)::int + (SELECT count(*) FROM pg_trigger WHERE tgname = 'users_touch')")" = 3 ]
check "meeting-service: foreign keys enforced after restore" fails_with "$(admin_db_url drill_meeting_service)" \
  "INSERT INTO meetings (host_id, title, room_name) VALUES (gen_random_uuid(), 'x', 'orphan')" "violates foreign key"
adm -c "DROP DATABASE drill_meeting_service WITH (FORCE)" >/dev/null
for s in "${services[@]}"; do
  target="drill_$(printf '%s' "$s" | tr '-' '_')"
  if [ "$s" = meeting-service ]; then continue; fi
  if db_exists "$target"; then fail "$s: --drop-after left $target behind"; else pass "$s: --drop-after removed the drill database"; fi
done

echo "== no off-host target: still runs, reports LOCAL_ONLY honestly"
sleep 1
write_config "$work/host/backup-none.env" none
expect_exit 0 "backup-all.sh without an off-host target" bash "$tool_dir/backup-all.sh" --config "$work/host/backup-none.env"
check "OFFSITE_STATUS=NOT_CONFIGURED" [ "$(last_value OFFSITE_STATUS)" = NOT_CONFIGURED ]
check "classified LOCAL_ONLY — not disaster recovery" [ "$(last_value BACKUP_CLASSIFICATION)" = "$CFB_LOCAL_ONLY_NOTE" ]
none_run="$(last_value BACKUP_RUN_DIR)"
none_stamp="$(basename "$none_run")"
check "manifest says NOT_CONFIGURED / LOCAL_ONLY" \
  [ "$(mj "$none_run/calling/calling-$none_stamp.dump.manifest.json" offsite.status)|$(mj "$none_run/calling/calling-$none_stamp.dump.manifest.json" offsite.classification)" = "NOT_CONFIGURED|$CFB_LOCAL_ONLY_NOTE" ]
sleep 1
write_config "$work/host/backup-require.env" none "CFMEET_BACKUP_REQUIRE_OFFSITE=1"
expect_exit 5 "REQUIRE_OFFSITE=1 without a target exits 5 (backups still written)" bash "$tool_dir/backup-all.sh" --config "$work/host/backup-require.env" --services calling

echo "== rsync and rclone targets"
sleep 1
mkdir -p "$work/rsync-target"
write_config "$work/host/backup-rsync.env" rsync "CFMEET_BACKUP_OFFSITE_RSYNC_DEST=$work/rsync-target"
expect_exit 0 "backup-all.sh with the rsync target" bash "$tool_dir/backup-all.sh" --config "$work/host/backup-rsync.env" --services "calling meeting-service"
check "rsync: OFFSITE_STATUS=COPIED" [ "$(last_value OFFSITE_STATUS)" = COPIED ]
sleep 1
mkdir -p "$work/rclone-target"
printf '[ci-local]\ntype = local\n' >"$work/host/rclone.conf"
chmod 600 "$work/host/rclone.conf"
write_config "$work/host/backup-rclone.env" rclone "CFMEET_BACKUP_OFFSITE_RCLONE_REMOTE=ci-local:$work/rclone-target" "CFMEET_BACKUP_OFFSITE_RCLONE_CONFIG=$work/host/rclone.conf"
expect_exit 0 "backup-all.sh with the rclone target" bash "$tool_dir/backup-all.sh" --config "$work/host/backup-rclone.env" --services calling
check "rclone: OFFSITE_STATUS=COPIED" [ "$(last_value OFFSITE_STATUS)" = COPIED ]
rc_stamp="$(basename "$(last_value BACKUP_RUN_DIR)")"
expect_exit 0 "restore drill from the rclone copy" bash "$tool_dir/restore-drill.sh" \
  --in "$work/rclone-target/ci-host/runs/$rc_stamp/calling/calling-$rc_stamp.dump.age" --age-identity-file "$work/owner/identity.txt" \
  --target-database drill_from_rclone --admin-url-file "$work/owner/admin.url" --drop-after

echo "== failure cases"
calling_set="$run_dir/calling/calling-$stamp.dump"
mkdir -p "$work/tamper"
cp "$calling_set.age" "$calling_set.age.sha256" "$calling_set.sha256" "$calling_set.manifest.json" "$work/tamper/"
t="$work/tamper/$(basename "$calling_set")"
python3 -c 'import sys; p=sys.argv[1]; b=bytearray(open(p,"rb").read()); b[-10]^=0xFF; open(p,"wb").write(bytes(b))' "$t.age"
expect_exit 5 "a tampered encrypted archive is refused before anything is created" bash "$tool_dir/restore-drill.sh" --in "$t.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_tamper --admin-url-file "$work/owner/admin.url"
if db_exists drill_tamper; then fail "a drill database was created for the tampered archive"; else pass "no drill database was created for the tampered archive"; fi
# Re-encrypted different plaintext with a recomputed outer checksum: the inner check catches it.
age --decrypt --identity "$work/owner/identity.txt" --output "$work/tamper/plain.dump" "$calling_set.age"
python3 -c 'import sys; p=sys.argv[1]; b=bytearray(open(p,"rb").read()); b[len(b)//2]^=0xFF; open(p,"wb").write(bytes(b))' "$work/tamper/plain.dump"
age --encrypt --recipients-file "$work/host/age-recipients.txt" --output "$t.age" "$work/tamper/plain.dump"
rm "$work/tamper/plain.dump"
(cd "$work/tamper" && sha256sum "$(basename "$t.age")" >"$(basename "$t.age").sha256")
expect_exit 10 "decrypted bytes that do not match the dump checksum are refused" bash "$tool_dir/restore-drill.sh" --in "$t.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_inner_tamper --admin-url-file "$work/owner/admin.url" --drop-after
check "the inner checksum refusal is reported" grep -q "decrypted archive does not match" "$work/last.out"
cp "$calling_set.age.sha256" "$t.age.sha256"
expect_exit 5 "a checksum file that does not match is refused" bash "$tool_dir/restore-drill.sh" --in "$t.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_tamper --admin-url-file "$work/owner/admin.url"

before="$(dbq "$(sample_db calling)" "SELECT count(*) FROM calls")"
expect_exit 8 "restore over an existing NON-drill database is refused" bash "$tool_dir/restore-drill.sh" --in "$calling_set.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database "$(sample_db calling)" --admin-url-file "$work/owner/admin.url"
check "the live-named database was not touched" [ "$(dbq "$(sample_db calling)" "SELECT count(*) FROM calls")" = "$before" ]
adm -c "DROP DATABASE IF EXISTS drill_existing WITH (FORCE)" >/dev/null
adm -c "CREATE DATABASE drill_existing" >/dev/null
expect_exit 9 "restore over an existing drill database without the flag is refused" bash "$tool_dir/restore-drill.sh" --in "$calling_set.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_existing --admin-url-file "$work/owner/admin.url"
expect_exit 0 "--replace-existing-drill recreates a previous drill database" bash "$tool_dir/restore-drill.sh" --in "$calling_set.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_existing --admin-url-file "$work/owner/admin.url" --replace-existing-drill --drop-after
expect_exit 2 "an encrypted archive without an identity is refused" bash "$tool_dir/restore-drill.sh" --in "$calling_set.age" \
  --target-database drill_noid --admin-url-file "$work/owner/admin.url"

mkdir -p "$work/neg"
expect_exit 5 "backup with a missing recipients file is refused" bash "$tool_dir/backup.sh" --service calling \
  --database-url-file "$work/host/urls/calling.url" --out "$work/neg/a.dump" --age-recipients-file "$work/host/missing.txt"
cp "$work/owner/identity.txt" "$work/neg/private-as-recipients.txt"
expect_exit 5 "backup with a PRIVATE key as recipients is refused" bash "$tool_dir/backup.sh" --service calling \
  --database-url-file "$work/host/urls/calling.url" --out "$work/neg/a.dump" --age-recipients-file "$work/neg/private-as-recipients.txt"
check "the refusals left no dump" [ -z "$(find "$work/neg" -type f ! -name private-as-recipients.txt -print)" ]
printf 'CFMEET_BACKUP_ROOT=%s\nCFMEET_BACKUP_SERVICES=calling\nCFMEET_BACKUP_CALLING_URL_FILE=%s\n' "$work/neg/root" "$work/host/urls/calling.url" >"$work/neg/norecip.env"
chmod 600 "$work/neg/norecip.env"
expect_exit 2 "backup-all.sh without a recipients file is refused" bash "$tool_dir/backup-all.sh" --config "$work/neg/norecip.env"
expect_exit 4 "the wrong service's database is refused (calling URL as meeting-service)" bash "$tool_dir/backup.sh" --service meeting-service \
  --database-url-file "$work/host/urls/calling.url" --out "$work/neg/b.dump"
expect_exit 4 "a database with seaql_migrations but no users/meetings is not meeting-service" bash "$tool_dir/backup.sh" --service meeting-service \
  --database-url-file "$work/host/urls/impostor.url" --out "$work/neg/c.dump"

echo "== legacy interface used by the services' disaster-recovery tests"
mkdir -p "$work/legacy"
expect_exit 0 "backup.sh --service calling --database-url URL --out F (unencrypted)" bash "$tool_dir/backup.sh" --service calling \
  --database-url "$(admin_db_url "$(sample_db calling)")" --out "$work/legacy/calling.dump"
check "legacy outputs F and F.sha256" test -s "$work/legacy/calling.dump" -a -s "$work/legacy/calling.dump.sha256"
adm -c "DROP DATABASE IF EXISTS legacy_restore_target WITH (FORCE)" >/dev/null
adm -c "CREATE DATABASE legacy_restore_target" >/dev/null
expect_exit 0 "restore.sh --database-url EMPTY --in F" bash "$tool_dir/restore.sh" --database-url "$(admin_db_url legacy_restore_target)" --in "$work/legacy/calling.dump"
expect_exit 0 "verify-restore.sh on the legacy restore" bash "$tool_dir/verify-restore.sh" --manifest "$work/legacy/calling.dump.manifest.json" \
  --database-url "$(admin_db_url legacy_restore_target)"
expect_exit 6 "restore.sh into a non-empty database is refused" bash "$tool_dir/restore.sh" --database-url "$(admin_db_url legacy_restore_target)" --in "$work/legacy/calling.dump"
echo corrupt >>"$work/legacy/calling.dump"
expect_exit 5 "restore.sh refuses a corrupted archive" bash "$tool_dir/restore.sh" --database-url "$(admin_db_url legacy_restore_target)" --in "$work/legacy/calling.dump"

echo "== the verifier is not a no-op"
expect_exit 0 "restore a drill to tamper with afterwards" bash "$tool_dir/restore-drill.sh" --in "$calling_set.age" \
  --age-identity-file "$work/owner/identity.txt" --target-database drill_mismatch --admin-url-file "$work/owner/admin.url"
dbq drill_mismatch "DELETE FROM calling_seaql_migrations WHERE version = (SELECT max(version) FROM calling_seaql_migrations)" >/dev/null
admin_db_url drill_mismatch >"$work/owner/drill_mismatch.url"
expect_exit 1 "a missing migration row is detected" bash "$tool_dir/verify-restore.sh" --manifest "$calling_set.manifest.json" --database-url-file "$work/owner/drill_mismatch.url"
check "the failure names the history table" grep -q "FAIL calling_seaql_migrations" "$work/last.out"
adm -c "DROP DATABASE drill_mismatch WITH (FORCE)" >/dev/null

echo "== pre-migration snapshot (SEC-011)"
printf 'CF_RUNTIME_ENVIRONMENT=production\nDATABASE_URL=%s\n' "$(cat "$work/host/urls/meeting-service.url")" >"$work/host/migration.private.env"
chmod 600 "$work/host/migration.private.env"
expect_exit 0 "snapshot meeting-service from a release's migration env file" bash "$tool_dir/pre-migration-snapshot.sh" --service meeting-service \
  --database-url-env-file "$work/host/migration.private.env" --label "run-$((RANDOM + 1000))" \
  --age-recipients-file "$work/host/age-recipients.txt" --out-dir "$work/host/backups/pre-migration"
snap="$(last_value PRE_MIGRATION_SNAPSHOT)"
check "snapshot evidence printed and archive exists" test -s "$snap"
check "snapshot checksum printed matches" [ "$(last_value PRE_MIGRATION_SNAPSHOT_SHA256)" = "$(cfb_sha256 "$snap")" ]
check "snapshot manifest labelled pre-migration" bash -c "python3 '$tool_dir/manifest.py' get '$(last_value PRE_MIGRATION_MANIFEST)' label | grep -q '^pre-migration:run-'"
expect_exit 0 "the snapshot restores and verifies" bash "$tool_dir/restore-drill.sh" --in "$snap" --age-identity-file "$work/owner/identity.txt" \
  --target-database restore_premigration --admin-url-file "$work/owner/admin.url" --drop-after
expect_exit 4 "snapshot of the wrong database fails non-zero" bash "$tool_dir/pre-migration-snapshot.sh" --service communications \
  --database-url-file "$work/host/urls/meeting-service.url" --label run-x --age-recipients-file "$work/host/age-recipients.txt" \
  --out-dir "$work/host/backups/pre-migration"
check "PRE_MIGRATION_SNAPSHOT=FAILED reported" grep -q '^PRE_MIGRATION_SNAPSHOT=FAILED$' "$work/last.out"
url_for cfmeet_backup "$backup_pw" "$(sample_db calling)" | sed 's/:5432\//:5999\//; s/127.0.0.1\//127.0.0.1:5999\//' >"$work/neg/unreachable.url"
expect_exit 6 "snapshot of an unreachable database fails non-zero" bash "$tool_dir/pre-migration-snapshot.sh" --service calling \
  --database-url-file "$work/neg/unreachable.url" --label run-y --age-recipients-file "$work/host/age-recipients.txt" \
  --out-dir "$work/host/backups/pre-migration"
check "failed snapshots leave no directory behind" [ -z "$(find "$work/host/backups/pre-migration" -maxdepth 2 -type d \( -name '*-run-x' -o -name '*-run-y' \) -print)" ]

echo "== client-container mode (the production path: pg_dump runs in postgres:16 via --pg-wrapper)"
if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  pg_wrapper="docker run --rm -i --network host -e PGHOST -e PGPORT -e PGUSER -e PGPASSWORD -e PGDATABASE postgres:16"
  mkdir -p "$work/wrapped"
  expect_exit 0 "backup.sh through the client container" bash "$tool_dir/backup.sh" --service meeting-service \
    --database-url-file "$work/host/urls/meeting-service.url" --out "$work/wrapped/meeting.dump" \
    --age-recipients-file "$work/host/age-recipients.txt" --pg-wrapper "$pg_wrapper" --label wrapped
  check "pg_dump version recorded from the container" bash -c "python3 '$tool_dir/manifest.py' get '$work/wrapped/meeting.dump.manifest.json' pg_dump_version | grep -q 'PostgreSQL) 16'"
  expect_exit 0 "restore drill through the client container (archive streamed on stdin)" bash "$tool_dir/restore-drill.sh" \
    --in "$work/wrapped/meeting.dump.age" --age-identity-file "$work/owner/identity.txt" --target-database drill_wrapped \
    --admin-url-file "$work/owner/admin.url" --pg-wrapper "$pg_wrapper" --drop-after
else
  fail "docker is not available for the client-container case"
fi

echo "== secrets never printed"
check "the backup role password appears in no output" bash -c "! grep -qF '$backup_pw' '$log'"
check "no URL with credentials appears in any output" bash -c "! grep -qE 'postgres(ql)?://[^ ]*:[^ ]*@' '$log'"
check "no manifest or summary contains the password" bash -c "! grep -rqF '$backup_pw' '$work/host/backups' '$work/offhost'"
check "the age identity never left the owner directory" bash -c "! grep -rq AGE-SECRET-KEY '$work/host/backups' '$work/offhost' '$work/rsync-target' '$work/rclone-target'"

echo "== evidence"
find "$work/host/backups" -name '*.manifest.json' -o -name 'run-summary.json' | while IFS= read -r f; do
  rel="${f#"$work/host/backups/"}"
  mkdir -p "$evidence/$(dirname "$rel")"
  cp "$f" "$evidence/$rel"
done
echo "manifests and run summaries copied to $evidence"

echo
if [ "$failures" -ne 0 ]; then
  echo "BACKUP_RESTORE_DRILL=FAILED ($failures check(s))"
  exit 1
fi
echo "BACKUP_RESTORE_DRILL=PASS"
