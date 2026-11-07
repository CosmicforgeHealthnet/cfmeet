# Database backup and restore

Section 2 (data protection). This runbook closes the repository side of **SEC-010** (P0: no backup path for the Meeting Service, Communications and Developer Platform databases; no scheduled backups) and prepares **SEC-011 / DATA-013** (no snapshot before production migrations). See [the feature gap register](../audits/cfmeet-feature-gap-register.md).

The short version:

* Every CF Meet service database can now be backed up, encrypted to the owner's public key, copied off the host, restored into an isolated database and verified, with one set of scripts in [`infra/deployment/backup/`](../../infra/deployment/backup/).
* The [Backup and restore drill](../../.github/workflows/backup-restore-drill.yml) workflow runs all of this against a throwaway PostgreSQL 16 on every change to the tooling. That workflow is the execution proof.
* **Nothing runs in production yet.** The owner must create the key pair, choose the off-host location and approve the first run (see [Status](#status)).
* A backup with no off-host copy is reported as `LOCAL_ONLY — not disaster recovery` everywhere it appears: in the manifest, in the run summary and in the script output. It still protects against a bad migration or an operator mistake. It does not protect against losing the VPS.

## What is covered

[`catalog.conf`](../../infra/deployment/backup/catalog.conf) lists every service database and its migration-history tables. A backup is refused if the required history (and identity) tables are missing. This stops the tooling from backing up the wrong database by mistake.

| Service | Required history | Optional (additive) histories, recorded when present | Production database (as far as the repository shows) |
|---|---|---|---|
| meeting-service (account authority) | `seaql_migrations` + tables `users`, `meetings` | `account_communications_migrations`, `account_communications_facade_migrations`, `account_security_migrations`, `meetings_calendar_migrations` | inside the legacy `cosmicforge-db` container; name and version REQUIRES_VALIDATION |
| communications (Hub) | `communications_seaql_migrations` + table `communication_profiles` | `communications_unified_seaql_migrations` | `cfmeet_comm_production_hub` on `cfmeet-core-db` |
| platform-access | `platform_access_seaql_migrations` | — | `cfmeet_platform_access` on `cfmeet-core-db` |
| calling | `calling_seaql_migrations` | `calling_communications_migrations` | `cfmeet_calling` on `cfmeet-core-db` |
| messaging | `messaging_seaql_migrations` | `messaging_communications_migrations` | `cfmeet_messaging` on `cfmeet-core-db` |
| developer-platform | `developer_platform_seaql_migrations` | — | `cfmeet_developer_platform`; production deployment REQUIRES_VALIDATION |
| numbering, privacy-relay, telephony, sms-gateway | `<service>_seaql_migrations` | — | `cfmeet_<service>`; production deployment REQUIRES_VALIDATION |
| organizations (future) | reserved slot | — | every script refuses it until the line is switched to `active` |

`cfmeet-core-db` is `postgres:16-alpine` on the internal network `cfmeet-core-private`. It publishes no host port, so the tooling reaches it through a throwaway client container (`--pg-wrapper`, below).

**Not covered here**, and backed up or re-provisioned by other means:

* Key material: service signing keys, Contact Vault keyrings, provider secrets and the Hub keys. These live outside PostgreSQL and go through the secret store (`communications-runtime-recovery.md`).
* Object storage (recordings, voicemail, attachments).
* Redis, which is a cache and lease store and is rebuilt.
* Host configuration.
* LiveKit Cloud.

There is no point-in-time recovery: no WAL archiving. With the proposed daily schedule, the recovery point is up to 24 hours. A pre-migration snapshot narrows this for releases.

## Tooling

| File | Purpose |
|---|---|
| `backup.sh` | Dumps one database (`pg_dump --format=custom --no-owner --no-privileges`), writes a SHA-256 checksum and a manifest, and with `--age-recipients-file` encrypts the dump and deletes the plaintext. The interface the services' disaster-recovery tests use (`--service S --database-url URL --out F`) still works unchanged. |
| `restore.sh` | Restores a plain or `.age` archive into an **empty** database in one transaction. It verifies the `.age` checksum before decrypting and the dump checksum after. |
| `restore-drill.sh` | Creates a fresh `drill_…` / `restore_…` database, restores into it and verifies it against the manifest. It refuses names without that prefix, and refuses an existing database unless `--replace-existing-drill` is passed. Checksums are verified before anything is created. |
| `verify-restore.sh` | Checks a restored database against its manifest: at least one table, the same table count as the source, required histories present and non-empty, and the same row count and latest version for every recorded history. |
| `backup-all.sh` | The scheduled job. It backs up every configured database, writes `run-summary.json`, copies the run off-host, records the outcome in every manifest and applies retention. |
| `pre-migration-snapshot.sh` | SEC-011: an encrypted, checksummed, re-verified snapshot of one database. It exits non-zero if it fails, and the release must then stop. |
| `offsite.sh` | Pluggable off-host copy (`none`, `rsync`, `rclone` or `dir`) with verification after the copy. It refuses to send anything that is not encrypted or metadata. |
| `prune.sh` | Local retention. It only touches stamped directories that hold this tooling's manifests. |
| `provision-backup-role.sh` | Creates a read-only `cfmeet_backup` login: membership of `pg_read_all_data` plus CONNECT on the listed databases. It has no write, DDL or superuser rights. |
| `manifest.py` | Writes and reads manifests and run summaries (JSON). |
| `backup.env.example`, `systemd/`, `scheduled-backup.workflow.yml.template` | Configuration and schedule **templates** (proposals; nothing installs them). |
| `tests/unit.sh`, `tests/ci-drill.sh` | Unit tests that need no database, and the end-to-end PostgreSQL drill run by CI. |

Layout of one scheduled run (all files mode 0600 and directories 0700, under `CFMEET_BACKUP_ROOT`, default `/var/backups/cfmeet`):

```
runs/20261107T023112Z/run-summary.json
runs/20261107T023112Z/calling/calling-20261107T023112Z.dump.age          encrypted archive
runs/20261107T023112Z/calling/calling-20261107T023112Z.dump.age.sha256   checksum of the archive
runs/20261107T023112Z/calling/calling-20261107T023112Z.dump.sha256       checksum of the decrypted dump
runs/20261107T023112Z/calling/calling-20261107T023112Z.dump.manifest.json
pre-migration/<service>/<stamp>-<release>/...                             same four files
```

A manifest contains no row data and no credentials. It records:

* the service, database, label and creation time
* the `pg_dump` and server versions
* the database size and the public table count
* each history table: present or not, row count and latest version
* the dump size and checksum, and the encrypted size and checksum
* the public age recipients
* whether plaintext was retained
* the `offsite` block: status `NOT_CONFIGURED`, `PENDING`, `COPIED` or `FAILED`, with the classification `LOCAL_ONLY — not disaster recovery` or `OFF_HOST_COPY_VERIFIED`

### Secrets handling

* A database URL is read from a root-only file (`--database-url-file`, or `CFMEET_BACKUP_<SERVICE>_URL_FILE`), from a docker env file (`--database-url-env-file`), or from `DATABASE_URL`. It is split into libpq environment variables on stdin and is never placed on a child process's command line, written to a manifest or printed. `--database-url URL` remains for the CI tests only: avoid it in production, because it is visible in the process list.
* Client containers receive the connection through `-e PGPASSWORD`-style forwarding (name only, value taken from the environment).
* The production host holds only the age **public** recipient. Every script refuses a recipients file that contains `AGE-SECRET-KEY-` or a `PRIVATE KEY`.
* The CI drill asserts that the backup role's password never appears in any output, manifest or copy, and that no credential-bearing URL is printed.

## Owner setup

### 1. Age key pair (owner, never on the VPS)

On the owner's own machine, ideally offline:

```bash
age-keygen -o cfmeet-backup-identity.txt     # prints "Public key: age1..."
age-keygen -y cfmeet-backup-identity.txt     # the public recipient, safe to share
```

Keep `cfmeet-backup-identity.txt` in two independent safe places, for example a password manager plus an offline copy. Losing it makes **every** backup unreadable. Anyone who has it can read every backup. A second recipient (for example a sealed escrow key held by another trusted person) can be added as a second line in the recipients file. Either private key then decrypts.

On the VPS: `/etc/cfmeet-backup/age-recipients.txt` (root, 0600) containing only the `age1…` line(s).

### 2. Read-only backup role and URL files

For `cfmeet-core-db`, run once as root on the VPS. This needs an admin URL file built from `/opt/cfmeet-core/secrets/postgres.password`, created and deleted by the operator; the URL is not printed:

```bash
openssl rand -hex 32 > /etc/cfmeet-backup/backup-role.password; chmod 600 /etc/cfmeet-backup/backup-role.password
CFMEET_PG_WRAPPER="docker run --rm -i --network cfmeet-core-private -e PGHOST -e PGPORT -e PGUSER -e PGPASSWORD -e PGDATABASE postgres:16-alpine" \
  bash /opt/cfmeet-backup/provision-backup-role.sh --admin-url-file /root/core-admin.url \
  --password-file /etc/cfmeet-backup/backup-role.password \
  --databases "cfmeet_platform_access cfmeet_calling cfmeet_messaging cfmeet_comm_production_hub"
```

Then write one URL file per database: `postgres://cfmeet_backup:<password>@cfmeet-core-db:5432/<db>`, in `/etc/cfmeet-backup/urls/<service>.url` (0600). Repeat for the Meeting Service database inside `cosmicforge-db`, using the wrapper from `backup.env.example`. Before that, confirm its database name and PostgreSQL major version (REQUIRES_VALIDATION).

### 3. Off-host target (owner decision, no AWS)

Set `CFMEET_BACKUP_OFFSITE_KIND` in `/etc/cfmeet-backup/backup.env`:

* **`rsync`** to a host the owner controls, in a different provider or location: `CFMEET_BACKUP_OFFSITE_RSYNC_DEST=user@host:/path`, a dedicated key in `CFMEET_BACKUP_OFFSITE_SSH_KEY`, and a pinned `CFMEET_BACKUP_OFFSITE_SSH_KNOWN_HOSTS`. On the target, restrict the key with `command="rrsync -wo /srv/cfmeet-backups",restrict` in `authorized_keys`. The VPS can then add backups but never read or delete them, so a compromised VPS cannot erase its own history. Verification is a checksum dry run that must find nothing to transfer.
* **`rclone`** to any non-AWS backend the owner picks (SFTP, WebDAV, Backblaze B2, a non-AWS S3-compatible provider): `CFMEET_BACKUP_OFFSITE_RCLONE_REMOTE=remote:path` and `CFMEET_BACKUP_OFFSITE_RCLONE_CONFIG=/etc/cfmeet-backup/rclone.conf`. Verification is `rclone check --one-way`. Prefer credentials without delete rights.
* **`dir`**: a mounted path. This only counts as off-host if the mount is on independent hardware. CI uses it to simulate a remote.
* **`none`** (the default): backups still run, and every report says `OFFSITE_STATUS=NOT_CONFIGURED` / `LOCAL_ONLY — not disaster recovery`.

Only `*.age`, `*.sha256`, `*.manifest.json` and `run-summary.json` ever leave the host. Remote retention belongs to the target, for example lifecycle rules or a periodic cleanup there.

### 4. Install (proposal, after owner approval)

```bash
install -d -m 0700 /opt/cfmeet-backup /etc/cfmeet-backup /etc/cfmeet-backup/urls /var/backups/cfmeet
# copy infra/deployment/backup/* to /opt/cfmeet-backup (root-owned, not writable by others)
cp /opt/cfmeet-backup/backup.env.example /etc/cfmeet-backup/backup.env && chmod 600 /etc/cfmeet-backup/backup.env
# edit backup.env, then the FIRST run is manual and watched:
bash /opt/cfmeet-backup/backup-all.sh --config /etc/cfmeet-backup/backup.env
```

The VPS needs `age`, `python3`, `flock` (util-linux) and `rsync` or `rclone` as chosen. The PostgreSQL client tools run inside the `postgres:16-alpine` wrapper image.

## Schedule

Two equivalent proposals. Neither is active, and nothing in this change contacts production:

* **systemd timer** ([`systemd/cfmeet-db-backup.service`](../../infra/deployment/backup/systemd/cfmeet-db-backup.service) and [`.timer`](../../infra/deployment/backup/systemd/cfmeet-db-backup.timer)): daily at 02:30 UTC (±20 min), `Persistent=true`, with nice/idle I/O and `CPUQuota=50%` so live meetings are not affected. Install with `systemctl enable --now cfmeet-db-backup.timer`. This is the most robust option: no dependency on GitHub or SSH.
* **Scheduled GitHub workflow** ([`scheduled-backup.workflow.yml.template`](../../infra/deployment/backup/scheduled-backup.workflow.yml.template)): matches the repository's "production only through workflows on main over SSH" model. It ships the tooling at the workflow revision, runs `backup-all.sh` and uploads only the evidence lines. To activate, copy it to `.github/workflows/` after approval.

Recommendation: use the systemd timer for the backup itself. Optionally add a scheduled read-only workflow later that alerts when `run-summary.json` is older than 26 hours or reports anything other than `OK` / `COPIED`, which ties in with SEC-016 alerting.

### Exit status of `backup-all.sh`

| Exit | Meaning | Local backups |
|---|---|---|
| 0 | Every database was dumped, verified and encrypted. Off-host status is `COPIED`, or `NOT_CONFIGURED` while `CFMEET_BACKUP_REQUIRE_OFFSITE` is not 1. Read `BACKUP_CLASSIFICATION`: `LOCAL_ONLY — not disaster recovery` unless an off-host copy was verified. | complete |
| 1 | At least one database failed. The others were still backed up and copied. Retention is skipped. | partial |
| 2 | Configuration error, including a missing, empty or private-key recipients file. Nothing was written. | none |
| 3 | A required tool is missing. | none |
| 4 | Every database succeeded locally, but the configured off-host copy failed. | complete |
| 5 | Every database succeeded locally, but there is no off-host target while `CFMEET_BACKUP_REQUIRE_OFFSITE=1`. | complete |

Set `CFMEET_BACKUP_REQUIRE_OFFSITE=1` as soon as a target exists. From then on, a run without a verified off-host copy fails the unit and becomes visible.

`backup.sh` exits with 2 (usage), 3 (tool missing or `pg_dump` older than the server), 4 (not this service's database), 5 (encryption refused), 6 (unreachable, dump failed, or the migration history changed during the dump) or 7 (manifest). `restore-drill.sh` exits with 4 (file missing), 5 (checksum or decryption), 8 (target name refused), 9 (target exists) or 10 (restore or verification failed).

## Retention

* Scheduled runs: always keep the newest `CFMEET_BACKUP_KEEP_MIN` (7). Delete runs older than `CFMEET_BACKUP_KEEP_DAYS` (14).
* Pre-migration snapshots, per service: keep 5 and delete after 30 days.
* Pruning happens only after a fully successful run, so failing backups never cause good old ones to be deleted. Off-host copies are never pruned from the VPS.

These are proposals for the current data volume. The owner may lengthen them, for example by keeping weekly copies for 3 months on the off-host target.

## Pre-migration snapshot (SEC-011)

```bash
bash /opt/cfmeet-backup/pre-migration-snapshot.sh --service meeting-service \
  --database-url-env-file "$MIGRATION_ENV_FILE" --label "$RUN_ID" \
  --age-recipients-file /etc/cfmeet-backup/age-recipients.txt \
  --pg-wrapper "docker run --rm -i --network container:cfmeet-production-backend -e PGHOST -e PGPORT -e PGUSER -e PGPASSWORD -e PGDATABASE postgres:16-alpine"
```

The script prints `PRE_MIGRATION_SNAPSHOT=<path>`, `PRE_MIGRATION_SNAPSHOT_SHA256=…` and `PRE_MIGRATION_MANIFEST=…` for the release evidence, and exits non-zero (the migration must not run) if any step fails.

No existing release workflow was changed. These are the places where production migrations run today, and where the snapshot step belongs. Insert it immediately before each migration call and abort the release if it fails:

| Workflow (step) | Code that runs `migration up` | Database(s) | Insert |
|---|---|---|---|
| `deploy-backend.yml`, step "Deploy verified main commit to VPS (rollback on failure)", which calls `infra/deployment/backend_runtime.py` | `release()`: `migration_env=env_file(folder/'migration.private.env', …)`, then the `for binary in ([...'migration', 'account-security-migration', 'account-communications-migration'...])` loop of `docker run … --entrypoint /app/<binary> … up` | meeting-service | In `release()`, after `migration_env` is written and before the loop. Run the snapshot with `--database-url-env-file <migration_env>` and the `--network container:cfmeet-production-backend` wrapper (the same network the migration uses). |
| `cfmeet-calling-production.yml`, operation `communications-release`, step "Execute versioned production operation", which calls `communications_release.py` | `release()` → `for service in ('meeting-service','calling','messaging'): feature_migrate(service, sha)` → `migrate(…, feature_only=True, network=…)` | meeting-service, calling, messaging | At the top of `feature_migrate()`, before `migrate(...)`: snapshot `service` with the URL it just built and the same `network`. The new Hub database (`database('cfmeet_comm_production_hub')`) is created fresh with a collision check, so it needs no snapshot. `communications-stage` migrates only fresh `cfmeet_comm_stage_*` databases, so none is needed there either. |
| `cfmeet-core-calling.yml`, step "Migrate and start private Calling" | `docker run --rm --network "$NET" -e "DATABASE_URL=postgres://calling_migrator:…/cfmeet_calling" --entrypoint /app/calling-migration "$IMAGE" up` | calling | Directly before that `docker run`, inside the same SSH heredoc. |
| `cfmeet-core-messaging.yml`, step "Migrate and start private Messaging" | `… --entrypoint /app/messaging-migration "$IMAGE" up` | messaging | Directly before that `docker run`. |
| `cfmeet-core-platform-access.yml`, step "Create dedicated keys, migrate and start Platform Access" | `… --entrypoint /app/platform-access-migration "$IMAGE" up` | platform-access | Directly before that `docker run`. |
| `infra/deployment/part-vi/compose.part-vi.yml` (not a workflow): service `developer-platform-migrate` | `./developer-platform-migration up` | developer-platform | As a one-shot service the migrate service `depends_on` (`condition: service_completed_successfully`), or a step in whoever runs the compose release. |

The release host needs the tooling at `/opt/cfmeet-backup` (or shipped with the release, as `production.py` already ships `infra/calling/production`) and the recipients file.

## Restore drill procedure

The private key is needed to decrypt, so drills run where the identity lives: the owner's machine or a dedicated drill host. **Never on the production VPS.**

```bash
# 1. fetch one run from the off-host target (or the VPS) — encrypted files only
rsync -a backup-host:/srv/cfmeet-backups/cfmeet-vps/runs/20261107T023112Z/ ./run/
# 2. disposable PostgreSQL of the same major version
docker run -d --name cfmeet-drill -e POSTGRES_PASSWORD=drill -p 127.0.0.1:55432:5432 postgres:16
printf 'postgres://postgres:drill@127.0.0.1:55432/postgres\n' > drill-admin.url
# 3. restore + verify each service
bash infra/deployment/backup/restore-drill.sh --in run/meeting-service/meeting-service-20261107T023112Z.dump.age \
  --age-identity-file ~/secure/cfmeet-backup-identity.txt --target-database drill_meeting_service \
  --admin-url-file drill-admin.url --drop-after
# 4. record the RESTORE_DRILL=PASS line (service, restore_seconds) in the operations log; remove the container
docker rm -f cfmeet-drill
```

Run a drill monthly, after every release that changes a schema, and whenever the tooling changes. CI runs it on every change to the tooling. A drill passes only with `RESTORE_DRILL=PASS`: both checksums verified, decryption done, the restore completed in one transaction, and every manifest check passed.

### Real recovery (outline)

1. Choose the run, using the pre-migration snapshot for a bad release. Verify it with `restore-drill.sh` into `restore_<service>` on the target server, without `--drop-after`. The target server needs the identity temporarily: bring it in for the recovery only and remove it afterwards.
2. Stop the affected service, using the existing workflows. Point its `DATABASE_URL` at the restored database, or rename the databases inside a maintenance window. Re-provision roles and grants with `provision-service-database.sh`, because dumps carry no owners or privileges.
3. Run the service's migrator. It must report nothing pending for the same release. Then start the service and its workers. Leases have expired, so durable work is reclaimed exactly as after a crash (`infra/deployment/README.md`, "Backup and restore").

## Status

| Item | State |
|---|---|
| Backup / encrypt / checksum / manifest for every catalog database, including meeting-service, communications and developer-platform | IMPLEMENTED: proven in CI on PostgreSQL 16 (Backup and restore drill) |
| Restore drill into an isolated database with refusal rules, plus verification | IMPLEMENTED: proven in CI, including tamper, wrong-target and wrong-database refusals |
| Off-host copy (rsync, rclone, dir) with verification after copy | IMPLEMENTED: proven in CI against local targets. An SSH or remote rclone target is untested until the owner provides one. |
| Honest `LOCAL_ONLY` reporting when no target is configured | IMPLEMENTED: proven in CI |
| Pre-migration snapshot helper (SEC-011) | IMPLEMENTED: proven in CI. **Not yet wired** into the release workflows (insertion points above). |
| Schedule (systemd timer / scheduled workflow) | TEMPLATE ONLY: owner approval required |
| Age key pair: generation and custody of the private key | **OWNER**: see setup step 1 |
| Off-host target: host or provider, credentials, append-only access | **OWNER**: see setup step 3. Until then, every backup is `LOCAL_ONLY — not disaster recovery`, and total VPS loss is **not** recoverable. |
| Meeting Service database facts (name, PostgreSQL version, where the URL lives) | REQUIRES_VALIDATION on the VPS (read-only inspection) |
| Read-only `cfmeet_backup` role in production | **OWNER / operator** approval (setup step 2) |
| First production run | **OWNER** approval, run manually and watched; then enable the schedule and set `CFMEET_BACKUP_REQUIRE_OFFSITE=1` once a target exists |
| First owner-side drill from a production backup | **OWNER** (needs the private key) |
| Point-in-time recovery (WAL archiving) | NOT IMPLEMENTED: recovery point is the last daily or pre-migration dump |
