# Meeting-service database roles (SEC-018): migrator and runtime separation

**Finding:** SEC-018 (P2). The meeting-service runtime database role can run DDL, and meeting-service was left out of least-privilege provisioning. **Scope:** `services/meeting-service` (the account authority and Meetings service), `infra/deployment/postgres`, and the release tooling that runs its migrations. **Production is NOT changed by this work.** Every production step below is an **OWNER ACTION**. The repository only supplies the tooling, the SQL and the CI proof.

Related: [backup-and-restore.md](backup-and-restore.md) (pre-migration snapshots), [account-security-section-2.md](account-security-section-2.md) (migration order).

## 1. Current state (production, as the release tooling shows it)

- `infra/deployment/backend_runtime.py` `release()` writes `migration.private.env` from the **running container's environment** and runs every history binary with it: `migration`, `account-security-migration`, `meetings-calendar-migration`, `profile-notifications-migration`, then `account-communications-migration` where Communications is configured. `infra/calling/production/communications_release.py` `feature_migrate()` does the same for meeting-service: it uses the runtime `DATABASE_URL`. The other services in that function already use a `<service>_migrator` login.
- So the role in the runtime `DATABASE_URL` applied every migration. It therefore **owns** the tables, sequences, enum types (`user_status`, `user_role`, `auth_type`, `event_type`, `email_job_status`, `api_key_status`, ...) and trigger functions. It can run any DDL at runtime: `DROP`, `TRUNCATE`, `ALTER TABLE`, `DISABLE TRIGGER` on the append-only audit log, and so on.
- The database lives in the legacy `cosmicforge-db` container. Its database name, role names and PostgreSQL major version are **not recorded in the repository** (REQUIRES_VALIDATION; see backup-and-restore.md). `services/meeting-service/.env.example` names the historical local database `cosmicforge_rtc`. **None of these names is assumed here.** The pre-checks in section 4 find them out.
- Two shapes are possible, and the pre-checks tell them apart:
  - **Branch A:** the runtime login is a dedicated non-superuser role that owns the objects.
  - **Branch B:** the runtime login is a superuser (for example the container's `postgres`). In that case the service bypasses every privilege check, and least privilege needs a **new** runtime role plus a change to the container's `DATABASE_URL`.

## 2. Target state

| Role | Owns | Can do | Used by |
|---|---|---|---|
| `<migrator>` (default name `meeting_service_migrator`) | the database and every object in schema `public` | DDL; applies all histories | release migration containers only |
| `<runtime>` (default name `meeting_service`) | nothing | `CONNECT`; `SELECT/INSERT/UPDATE/DELETE` on all tables, `USAGE/SELECT/UPDATE` on sequences, `EXECUTE` on functions, plus default privileges for everything the migrator creates later; **no** `CREATE` on the schema, **no** `CREATE`/`TEMPORARY` on the database, no `TRUNCATE`/`REFERENCES`/`TRIGGER` | the running service |

Triggers keep working. The trigger functions are `SECURITY INVOKER`, so they run as the runtime role and need only the DML it already has. Examples: `trg_account_security_credentials_changed` writes `account_security_state` and revokes `account_sessions`; `account_communication_status_outbox` writes the Communications outbox. The append-only audit log `account_security_events` stays append-only for the runtime role, and the runtime role can no longer disable or drop that log's triggers.

New databases get this shape from `infra/deployment/postgres/provision-service-database.sh --service meeting-service` (`--database` / `--role-prefix` override the default names).

The migration histories, in release order, are listed in one place per tool:

- CI: the step "Meeting-service migration histories (release order)" in `.github/workflows/backend.yml`.
- Release: the binary list in `backend_runtime.py` and `communications_release.py`.

To add a history (for example `account-hardening-migration`), append one entry to each list. The CI list already ends with `?…/account-hardening-migration`. The `?` marks the entry optional, so it is skipped until the binary is built. Drop the `?` to make it required.

**No superuser is needed by any migration.** The histories use only `CREATE TABLE/INDEX/TYPE/FUNCTION/TRIGGER`, `ALTER TYPE … ADD VALUE`, plpgsql (which is installed by default) and the built-in `gen_random_uuid()` (PostgreSQL ≥ 13). They create no extensions, `SECURITY DEFINER` functions, roles or grants. The owner of the objects can therefore apply all of them.

## 3. What proves it (CI)

| Workflow / job / step | Proves |
|---|---|
| `service-boundaries.yml` › `database-isolation` › "verify service database isolation" | meeting-service is in the throwaway set with the other 10 services. Its runtime role has no DDL. No role of meeting-service can connect to another service's database, and no other service's role can connect to meeting-service's. |
| `backend.yml` › `build` › "Meeting-service database roles (migrator applies all histories, runtime DML only)" | On a new database from the provisioning script: the runtime role **cannot** apply migrations, every history applies (and re-applies) as the migrator, and the runtime role owns nothing and has no `CREATE`/`TEMP`. It reads every table (history tables included), writes `users` / `account_sessions`, can only append to `account_security_events`, uses the serial and standalone (`conference_chat_sync_version_seq`) sequences, and fires the credentials-changed and Communications outbox triggers. It is refused `CREATE TABLE/INDEX/FUNCTION`, `CREATE TEMP TABLE`, `ALTER TABLE … ADD COLUMN`, `ALTER TYPE … ADD VALUE`, `DISABLE TRIGGER`, `DROP TRIGGER`, `DROP TABLE` and `TRUNCATE`, each with a privilege error. |
| `backend.yml` › `build` › "Meeting-service database role cutover and rollback rehearsal" | Branch A and branch B from the current production shape: the database and objects are owned by the runtime role (A) or by the superuser (B), and every history is applied as that owner. Then **the SQL in this runbook** runs. The dry run changes nothing. The script refuses a superuser runtime role and the wrong database. The real run moves everything, and a re-run is a no-op. The full certification above then passes. The rollback returns everything, and the old owner can migrate and run DDL again. |
| `backend.yml` › `build` › "Service database isolation with the real meeting-service migrations" | The cross-service certification again, with meeting-service's real histories applied as its migrator. |
| `messaging-ingress.yml` › "Production tooling unit tests" (`test_communications_release.MeetingServiceMigrationRole`) | Release tooling: without the secret file, migrations use the runtime URL, byte for byte as today. With the file, both `backend_runtime.release()` and `feature_migrate()` migrate as the migrator. A URL that targets another host or database, or that reuses the runtime role, is refused. |

Scripts: `infra/deployment/postgres/verify-meeting-service-roles.sh`, `meeting-service-roles-cutover.sql`, `meeting-service-roles-rollback.sql`.

## 4. Production cutover: OWNER ACTION, in a maintenance window

All commands run on the VPS. Never put a password on a command line, in shell history or in the repository. Replace `<db>`, `<runtime>`, `<migrator>` with the names found in 4.1. `PSQL` stands for the superuser `psql` inside the database container, for example `docker exec -i -u postgres cosmicforge-db psql -X -v ON_ERROR_STOP=1`. Confirm the container and the superuser login first.

### 4.1 Pre-checks (read-only) — OWNER ACTION

1. **Runtime connection, without printing secrets:**

   ```bash
   docker inspect cfmeet-production-backend --format '{{range .Config.Env}}{{println .}}{{end}}' \
     | sed -n 's#^DATABASE_URL=[a-z]*://\([^:@/]*\)[:@].*@\([^/]*\)/\([^?]*\).*#user=\1 host=\2 db=\3#p'
   ```

   This gives `<runtime>` and `<db>`.
2. **Server version, role attributes and memberships:**

   ```sql
   SHOW server_version;
   SELECT rolname, rolsuper, rolcreatedb, rolcreaterole, rolbypassrls, rolcanlogin
     FROM pg_roles WHERE rolname = '<runtime>';
   SELECT r.rolname AS member, g.rolname AS member_of
     FROM pg_auth_members m JOIN pg_roles r ON r.oid = m.member JOIN pg_roles g ON g.oid = m.roleid
    WHERE r.rolname = '<runtime>';
   ```

   `rolsuper = t` means **branch B**.
3. **Owners, recorded for rollback** (connect to `<db>`):

   ```sql
   SELECT datname, datdba::regrole AS database_owner FROM pg_database WHERE datname = '<db>';
   SELECT nspname, nspowner::regrole FROM pg_namespace WHERE nspname NOT LIKE 'pg\_%' AND nspname <> 'information_schema';
   SELECT relowner::regrole AS owner, relkind, count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE n.nspname = 'public' GROUP BY 1, 2 ORDER BY 1, 2;          -- tables, sequences, indexes, views
   SELECT proowner::regrole, count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
    WHERE n.nspname = 'public' GROUP BY 1;                             -- functions / trigger functions
   SELECT typowner::regrole, typtype, string_agg(typname, ', ') FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
    WHERE n.nspname = 'public' AND typtype IN ('e', 'd', 'r') GROUP BY 1, 2; -- enum types: user_status, ...
   SELECT extname, extowner::regrole, extnamespace::regnamespace FROM pg_extension;
   SELECT defaclrole::regrole, defaclnamespace::regnamespace, defaclobjtype FROM pg_default_acl;
   ```

   Keep the output. The database owner and the object owner are the rollback inputs `database_owner` and `owner`. Objects in schemas other than `public`, or objects owned by roles other than the expected owner, stop the plan until they are understood. The cutover commits only if the runtime role then owns nothing in this database.
4. **Who connects, and what else the old owner holds:**

   ```sql
   SELECT usename, application_name, client_addr, count(*) FROM pg_stat_activity WHERE datname = '<db>' GROUP BY 1, 2, 3;
   SELECT datname FROM pg_database WHERE datdba = '<runtime>'::regrole;   -- other databases the role owns
   SELECT datname, datacl FROM pg_database WHERE datname = '<db>';
   SHOW log_statement;   -- 'ddl' or 'all' would log ALTER ROLE ... PASSWORD (see 4.3)
   ```

5. **pg_hba.conf:** confirm that a new login role may connect to `<db>` from the backend network. If the file lists users explicitly, add `<migrator>` (and, in branch B, the new `<runtime>`) first. Check with `SELECT * FROM pg_hba_file_rules;`.

### 4.2 Encrypted pre-migration snapshot — OWNER ACTION

Take a verified, encrypted snapshot of `<db>` immediately before the cutover. Use the same command as a release (backup-and-restore.md, "Pre-migration snapshot"). Put the URL in a temporary 0600 env file built from the container env, and delete it afterwards:

```bash
umask 077; tmp=$(mktemp)
docker inspect cfmeet-production-backend --format '{{range .Config.Env}}{{println .}}{{end}}' | grep '^DATABASE_URL=' > "$tmp"
bash /opt/cfmeet-backup/pre-migration-snapshot.sh --service meeting-service \
  --database-url-env-file "$tmp" --label "sec018-$(date -u +%Y%m%d)" \
  --age-recipients-file /etc/cfmeet-backup/age-recipients.txt \
  --pg-wrapper "docker run --rm -i --network container:cfmeet-production-backend -e PGHOST -e PGPORT -e PGUSER -e PGPASSWORD -e PGDATABASE postgres:16-alpine"
rm -f "$tmp"
```

Record `PRE_MIGRATION_SNAPSHOT_SHA256`. Do not continue if the script exits non-zero. After the cutover, take snapshots with the migrator URL instead: `--database-url-env-file /opt/cfmeet-core/secrets/meeting-service-migration.private.env --url-var MEETING_SERVICE_MIGRATION_DATABASE_URL`.

### 4.3 Create the migrator role (and, in branch B, the new runtime role) — OWNER ACTION

Generate the password into a root-only file. Send it to PostgreSQL on standard input only, so it never appears in an argument list. Disable statement logging for that session:

```bash
umask 077
openssl rand -hex 32 > /opt/cfmeet-core/secrets/meeting-service-migrator.password
{ echo "SET log_statement = 'none';"
  printf "CREATE ROLE %s LOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS PASSWORD '%s';\n" \
    '<migrator>' "$(cat /opt/cfmeet-core/secrets/meeting-service-migrator.password)"
} | PSQL -d postgres
```

In branch B, create the new `<runtime>` role the same way, with its own password file. Do not reuse the superuser login.

### 4.4 Ownership transfer, grants and revokes — OWNER ACTION

`infra/deployment/postgres/meeting-service-roles-cutover.sql` does all of the following in **one transaction**:

- `ALTER DATABASE <db> OWNER TO <migrator>`.
- `public` to the migrator, if the old owner owned it explicitly (PostgreSQL < 15). On 15 and later, `pg_database_owner` owns it.
- Per-object `ALTER TABLE / VIEW / SEQUENCE / ROUTINE / TYPE / DOMAIN … OWNER TO <migrator>` for every object the old owner holds in `public`. That covers tables (indexes and serial/identity sequences follow), standalone sequences (`conference_chat_sync_version_seq`), trigger and other functions, and the enum types (`user_status`, …). Extension members are skipped.
- Explicit runtime grants: DML on all tables, `USAGE, SELECT, UPDATE` on sequences, `EXECUTE` on functions, and `ALTER DEFAULT PRIVILEGES FOR ROLE <migrator>` for all three.
- `REVOKE CREATE ON SCHEMA public FROM PUBLIC, <runtime>` and `REVOKE CREATE, TEMPORARY ON DATABASE <db> FROM PUBLIC, <runtime>`.
- A guard that **refuses to commit** if any table, view, sequence, function or type in `public` is not owned by the migrator, if the runtime role still owns anything in the database, or if it still has `CREATE`/`TEMPORARY`.

It also refuses a superuser runtime role, the wrong database, and a migrator that is the same role as the runtime or the old owner.

**Why per-object `ALTER … OWNER` and not `REASSIGN OWNED BY <old> TO <migrator>`:**

- `REASSIGN OWNED` also reassigns every **shared** object the old role owns, which means **every database it owns in the cluster**, not only `<db>`.
- It acts on all object types without listing them.
- In branch B it cannot be used at all, because objects of the bootstrap superuser cannot be reassigned.

The per-object form touches only schema `public` of `<db>` and the database itself, prints each statement, and is checked before commit. Use `REASSIGN OWNED` only if 4.1 shows that the old role owns nothing outside `<db>`, and even then the script is preferred.

```bash
# 1. Dry run: everything is executed and rolled back; review the echoed statements.
PSQL -d <db> -v runtime=<runtime> -v migrator=<migrator> -v database=<db> -v dry_run=1 \
  < infra/deployment/postgres/meeting-service-roles-cutover.sql
# 2. Real run (branch B: add -v current_owner=<the superuser that owns the objects today>).
PSQL -d <db> -v runtime=<runtime> -v migrator=<migrator> -v database=<db> \
  < infra/deployment/postgres/meeting-service-roles-cutover.sql
```

(`psql -f` with a file on the host works too; with `docker exec -i`, use the `<` redirection shown.)

**Locks:**

- `ALTER … OWNER` takes a brief `ACCESS EXCLUSIVE` lock on each table. In one transaction, all of those locks are held until commit. The changes are catalog-only, so this is normally well under a second.
- The script sets `lock_timeout = 5s`. If a long-running query holds a table, the whole transaction aborts with nothing changed. Retry when the database is quiet.
- Requests that arrive during the commit wait briefly. Run it in a low-traffic window. The service does not need to stop in branch A.

### 4.5 Verification — OWNER ACTION

Connected to `<db>`:

```sql
SELECT datdba::regrole FROM pg_database WHERE datname = current_database();              -- <migrator>
SELECT count(*) FROM pg_shdepend WHERE deptype = 'o' AND refobjid = '<runtime>'::regrole
   AND dbid = (SELECT oid FROM pg_database WHERE datname = current_database());          -- 0
SELECT has_schema_privilege('<runtime>', 'public', 'CREATE'),
       has_database_privilege('<runtime>', current_database(), 'CREATE'),
       has_database_privilege('<runtime>', current_database(), 'TEMPORARY');            -- f, f, f
SELECT has_table_privilege('<runtime>', 'users', 'SELECT,INSERT,UPDATE,DELETE'),
       has_sequence_privilege('<runtime>', 'conference_chat_sync_version_seq', 'USAGE'); -- t, t
```

Then check the service: `/health`, a sign-in, and the meeting list (the release acceptance checks). The running container needs no restart in branch A, because its role, its URL and its effective data access are unchanged.

To prove the migrator path without changing anything, run `status` with the current release image:

```bash
docker run --rm --network container:cfmeet-production-backend \
  --env-file <(sed 's/^MEETING_SERVICE_MIGRATION_DATABASE_URL=/DATABASE_URL=/' \
    /opt/cfmeet-core/secrets/meeting-service-migration.private.env) \
  --entrypoint /app/migration <image> status
```

Run it after 4.6.

### 4.6 Wire the separate migration URL into releases — OWNER ACTION

The release tooling is opt-in and unchanged by default. Both `backend_runtime.release()` and `communications_release.feature_migrate()` use `/opt/cfmeet-core/secrets/meeting-service-migration.private.env` when it exists. That file holds one line:

```
MEETING_SERVICE_MIGRATION_DATABASE_URL=postgres://<migrator>:<password>@<same host:port as the runtime URL>/<db>
```

Create it 0600, owned by root, from the runtime URL and the password file, without printing it:

```bash
umask 077
docker inspect cfmeet-production-backend --format '{{range .Config.Env}}{{println .}}{{end}}' | grep '^DATABASE_URL=' \
 | python3 -c 'import sys,urllib.parse as u; r=u.urlsplit(sys.stdin.read().strip().split("=",1)[1]); pw=open("/opt/cfmeet-core/secrets/meeting-service-migrator.password").read().strip(); h=r.hostname+(":"+str(r.port) if r.port else ""); print("MEETING_SERVICE_MIGRATION_DATABASE_URL="+u.urlunsplit((r.scheme, sys.argv[1]+":"+u.quote(pw, safe="")+"@"+h, r.path, r.query, "")))' '<migrator>' \
 > /opt/cfmeet-core/secrets/meeting-service-migration.private.env
```

The tooling refuses a URL that points at another host, port or database, or that uses the runtime role. Place this file **only after 4.4 has committed**:

- Before 4.4, the migrator owns nothing and a release with a new migration would stop.
- After 4.4 without the file, a release with new DDL would also stop, because the runtime role can no longer run DDL.

Both cases fail safe: the migration runs before any container is replaced.

**Branch B only:** the running container must also switch its `DATABASE_URL` to the new runtime role. The current release tooling deliberately preserves the running environment byte for byte (`clone(c, NAME, image, {}, False)`) and has **no** path to change it. That switch is a separate production configuration change for the owner to decide on and schedule (open item, section 6).

## 5. Rollback (reversible) — OWNER ACTION

1. Delete `/opt/cfmeet-core/secrets/meeting-service-migration.private.env`. Releases then migrate with the runtime URL again.
2. Return ownership. Use the database owner and object owner recorded in 4.1; in branch A the object owner is `<runtime>`:

   ```bash
   PSQL -d <db> -v runtime=<runtime> -v migrator=<migrator> -v database=<db> \
     -v database_owner=<recorded database owner> -v owner=<recorded object owner> [-v dry_run=1] \
     < infra/deployment/postgres/meeting-service-roles-rollback.sql
   ```

   This moves every migrator-owned object in `public` and the database back, gives the old owner `CREATE` on `public` back, and removes the migrator's default privileges and schema grants. It refuses to commit while the migrator still owns anything in `<db>`. CI rehearses it for both branches.
3. Optional: `DROP ROLE <migrator>`. It now owns nothing; run `DROP OWNED BY <migrator>` in `<db>` first to clear any remaining grants. Remove its password file.
4. The `CREATE`/`TEMPORARY` revocations for `PUBLIC` stay in place. They do not affect the service. If 4.1 showed another consumer relying on them, re-grant to that role explicitly.
5. If data were damaged (the transfer itself changes no data), restore the 4.2 snapshot with the restore runbook in backup-and-restore.md.

## 6. Open items and owner inputs

- **Production names and version:** the database name, the runtime role, branch A or B, the PostgreSQL major version and pg_hba rules for `cosmicforge-db` (4.1). These are REQUIRES_VALIDATION and nothing here assumes them.
- **Branch B runtime switch:** a new runtime role needs the container's `DATABASE_URL` changed. The release tooling has no environment-override path for meeting-service. This needs an owner decision and a separately reviewed change.
- **`CONNECT` from `PUBLIC`:** new databases revoke it (`provision-service-database.sh`). The cutover does not, because other consumers (backup role, monitoring) may rely on it. After 4.1 step 4, grant `CONNECT` explicitly to each legitimate role, then run `REVOKE CONNECT ON DATABASE <db> FROM PUBLIC`.
- **Sixth history `account-hardening-migration`:**
  - CI: drop the `?` from the CI list entry once the binary is merged.
  - Release: the release lists are edited with that change.
  - Backup: the catalog's history list in `infra/deployment/backup/catalog.conf` likewise.
- **No superuser requirement** was found in any meeting-service migration (section 2).
