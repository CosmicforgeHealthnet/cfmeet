# Hosted-staging certification kit

Part K gates 16 (capacity) and 30 (hosted staging deployment). This kit is **prepared, not
run against hosted staging**: no hosted staging environment exists or has been authorized.
Nothing here provisions, deploys, migrates or reconfigures anything. It drives an
environment that an owner has already deployed and authorized, and it records evidence.

| Tool | What it does | Side effects on the target |
|---|---|---|
| `preflight.mjs` | target safety, `/ready` of each service, credential accepted, principals well-formed; `--smoke`: one MANAGED conversation written and read back in order | `--smoke` only: one conversation, two messages |
| `capacity-runner.mjs` | managed send throughput, history paging, realtime fan-out latency (WebSocket), reconnection storm, search, attachment pipeline (presigned upload, finalize, scan, grant, byte-identical download); correctness asserted, no performance threshold invented | one device per realtime receiver (revoked at the end); conversations, messages, attachments (left to the environment's retention); environment policy widened (GROUP MANAGED, attachments) and **restored** |
| `report.mjs` | Markdown evidence; `MEASURED` without owner thresholds, `PASS` / `FAIL` with an owner-approved thresholds file | none |
| `.github/workflows/staging-capacity-certification.yml` | the three tools in CI, manual dispatch from `staging` only, evidence uploaded as an artifact | as above |

Rehearsal: the Messaging CI job runs the unit tests (`test/staging-kit.test.mjs`) and
`services/messaging/tests/platform_access_e2e.rs::hosted_staging_kit_rehearsal_against_real_services`,
which drives the kit against the real Platform Access, the Messaging binary, MinIO and clamd
on loopback. A rehearsal is not hosted-staging evidence.

## Safety rules (enforced in `lib.mjs`)

* CF Meet production hosts (`cfmeet.com`, `www.cfmeet.com`, `api.cfmeet.com`) are refused,
  even if listed in the allowlist (an allowlist that names one is itself refused).
* A non-loopback target must use `https` and be listed in `CFMEET_STAGING_ALLOWED_HOSTS`.
* A hosted run needs `--authorization <owner approval reference>`; it is written into the report.
* Credentials are read from files that other users cannot read (mode 600); never from the
  command line, never printed. Only a credential of a **SANDBOX environment of a test
  application** may be used. Production credentials must never be copied into staging.

## 1. Owner prerequisites (not done in this repository)

1. Authorization to create hosted staging (gate 30) and to run load against it (gate 16).
2. A staging deployment separate from production in every respect: its own hosts / DNS
   names, PostgreSQL databases (one per service, `infra/deployment/postgres`), Redis,
   object-storage buckets, scanner, LiveKit project, service-auth signing keys and trust
   bundles, Contact Vault keyrings, API credential pepper and webhook secrets. No value is
   copied from production. `CF_RUNTIME_ENVIRONMENT=staging`.
3. Hardware recorded for the report (instance types, CPU, memory, PostgreSQL / Redis /
   storage tiers): capacity numbers describe that hardware only.
4. In Platform Access (operator CLI / admin API): a test tenant, application with the
   scopes `messages.send messages.read conversations.create conversations.manage
   attachments.write attachments.read`, a **SANDBOX** environment, a service-account
   credential with those scopes, and at least `--members` principals
   (`EXTERNAL_APPLICATION_PRINCIPAL`). Environment policy: GROUP allows MANAGED; for the
   attachment workload attachments enabled (and `managedScanRequired` as staging intends).
5. Owner-approved thresholds, if a PASS / FAIL verdict is wanted (`report.mjs --thresholds`).
   This kit supplies none.

## 2. Deployment validation (owner deploys; the kit verifies)

Per service, in the release order of `infra/deployment/README.md`:

1. **Backup first**: `infra/deployment/backup/backup.sh --service <name> --database-url <staging url> --out F`
   (also before any re-run that changes schema).
2. **Migration preflight**: `<service>-migration status` against the staging database as
   the migrator role; record the pending list. Apply with `<service>-migration up`; a second
   `status` must show nothing pending. Each service refuses to start with pending
   migrations. Messaging migration 19 (`m20261121_000019_derivative_reservations`) is
   additive (new nullable columns, a status value, replaced trigger functions) and its
   `down` restores the previous functions; see the Part K final evidence §H.
3. Roll out the image; then `node scripts/staging/preflight.mjs --messaging-url <url>
   --service platform-access=<url> --service meeting=<url> ...` (every `/ready`).
4. Smoke: `preflight.mjs ... --credential-file F --principals-file F --smoke`.

## 3. Capacity run

```bash
export CFMEET_STAGING_ALLOWED_HOSTS=<staging messaging host>
node scripts/staging/capacity-runner.mjs --messaging-url https://<host> \
  --credential-file cred --principals-file principals.json \
  --authorization <approval ref> --label "<hardware / release>" --out capacity.json
node scripts/staging/report.mjs --in capacity.json [--thresholds owner-thresholds.json] --out capacity.md
```

Or dispatch **Hosted staging capacity certification** from the `staging` branch after a
repository admin has: created the GitHub environment `hosted-staging` with required
reviewers; added its secrets `STAGING_MESSAGING_API_CREDENTIAL` and `STAGING_PRINCIPALS_JSON`
(a JSON array of principal ids); set the repository variables `STAGING_MESSAGING_URL`,
`CFMEET_STAGING_ALLOWED_HOSTS`, optionally `STAGING_CAPACITY_THRESHOLDS_JSON`, and finally
`STAGING_CERTIFICATION_ENABLED=true`.

Scale with the parameters (`--members`, `--messages-per-sender`, `--fanout-messages`,
`--attachments`, `--attachment-bytes`, `--concurrency`), repeating at increasing sizes; the
report states each run's parameters. Rate limits configured in staging appear as `429`
counts in `statuses`. E2EE send throughput with real MLS is measured by
`services/messaging/tests/load.rs` (in process); this kit drives MANAGED traffic, device
registration and realtime only.

## 4. Rollback

1. Turn off the flag that enabled the change (state is preserved).
2. Roll back the image to the previous release.
3. Schema rollback only as a last resort, after a backup: `<service>-migration down -n 1`
   (Messaging 19 `down` is tested in `tests/attachments.rs`), or restore the backup into an
   empty database with `infra/deployment/backup/restore.sh`, then run the migrator for that
   release and confirm nothing is pending.
4. Re-run preflight and smoke.

## 5. Evidence

Keep `preflight.json`, `capacity.json`, `capacity.md`, the hardware description, the
release SHAs of every service and the owner authorization reference together, and add them
to `docs/phases/phase-05/evidence/`. Gate 16 can move from PARTIAL only with such a hosted
run on authorized staging hardware.
