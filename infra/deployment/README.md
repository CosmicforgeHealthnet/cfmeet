# Deployment

Each application/service deploys independently from its own subdirectory.

## Web app: `apps/web` (Vercel)

| Setting | Value |
|---|---|
| Git repository | `CosmicforgeHealthnet/cfmeet` |
| Root Directory | `apps/web` |
| Framework | Next.js |
| Environment | `NEXT_PUBLIC_API_URL` = public Meeting Service API base URL, e.g. `https://<api-host>/api/v1` |

## Meeting service: `services/meeting-service` (Docker)

The image is built from `services/meeting-service/Dockerfile` with **`services/meeting-service/` as the build context**:

```bash
docker build -t cfmeet-meeting-service ./services/meeting-service
# or
cd services/meeting-service && docker build -t cfmeet-meeting-service .
```

The image contains the API server and SeaORM migration binary. Runtime configuration remains environment-driven; see `services/meeting-service/.env.example`.

`services/meeting-service/docker-compose.yml` remains available for service-local workflows. The root `docker-compose.yml` is the preferred local platform orchestration file.

## Calling, Privacy Relay and Telephony (Docker, one image each)

Three independent deployables. Each scales, restarts and fails on its own; none shares a
database, a secret or source with another (enforced by `scripts/service-boundaries`).

| Service | Image (context = repository root) | Listener | Database | Secrets it alone receives |
|---|---|---|---|---|
| Calling | `docker build -f services/calling/Dockerfile .` → `calling`, `calling-migration`, `platform_admin` | `CALLING_BIND_ADDRESS` (8090): public API + gateway; `/internal/v1` cluster-only | `cfmeet_calling` | API credential pepper, signaling / media-edge signing keys, LiveKit API secret, webhook secret-box keys, contact-delegation key, object-storage credentials |
| Privacy Relay | `docker build -f services/privacy-relay/Dockerfile .` → `privacy-relay`, `privacy-relay-migration`, `privacy-relay-admin` | `PRIVACY_RELAY_BIND_ADDRESS` (8091), cluster-internal only | `cfmeet_privacy_relay` | Contact Vault and fingerprint keyrings |
| Telephony | `docker build -f services/telephony/Dockerfile .` → `telephony`, `telephony-migration`, `telephony-admin` | `TELEPHONY_BIND_ADDRESS` (8092): `/internal/v1` cluster-only, `/provider-callbacks/*` path-filtered public ingress | `cfmeet_telephony` | provider secrets directory, ARI password |

Every service also has its own service-auth signing key. Public SIP, carrier signalling and
provider callbacks terminate only in Telephony and its SIP / media edge (`infra/telephony`);
neither Calling nor the Meeting Service proxies telecom ingress. Network planes and secret
delivery: [`security-deployment-checklist.md`](../../docs/architecture/security-deployment-checklist.md).

**Configuration and feature flags** are environment-driven and server-side; each
`services/<service>/.env.example` and README documents every variable, including the kill
switches (public outbound / inbound, callback relay, enterprise, voicemail, recording,
Privacy Relay, Contact Vault, relay numbers) that disable a capability without touching
authoritative state. Provider / route enable-disable and circuits are operator actions
(`telephony-admin`, operator-audited), not flags.

**Health.** `GET /health` on each service checks its database. Each service refuses to start
while its migrations are pending, so a release runs the migration binary first.

**Release order.** For each service independently: back up its database → run its
`*-migration up` as the migrator role → roll out the new image → enable any new flag. All
migrations are additive and can be deployed before the flag that uses them. Rollback: turn
the flag off (state is preserved; safety operations such as ending calls, cleanup and
artifact deletion keep working), then roll back the image; schema rollback (`down`) is a
last resort after a backup.

### Database roles (least privilege)

`infra/deployment/postgres/provision-service-database.sh --service <name> --admin-url <url>
--migrator-password-file F --runtime-password-file F` creates the service's database, a
`<name>_migrator` role that owns it (used only by the migration binary) and the runtime
role the service connects with (`calling`, `privacy_relay`, `telephony`, as in each
`.env.example`): CONNECT to its own database only, data access only (no DDL, no TRUNCATE,
no temporary tables). CONNECT is revoked from PUBLIC, so no role of one service can open
another service's database. `verify-service-isolation.sh --admin-url <url>` certifies this
on throwaway databases (CI: Service boundaries → per-service database roles; with
`*_MIGRATION_BIN` set it also applies each service's real migrations as the migrator role).

### Backup and restore (per service)

* `infra/deployment/backup/backup.sh --service <name> --database-url <url> --out F` —
  `pg_dump` custom format of exactly one service database (refuses a database that is not
  that service's), plus `F.sha256`. There is no shared platform dump.
* `infra/deployment/backup/restore.sh --database-url <empty db url> --in F` — verifies the
  checksum, refuses a non-empty target, restores in one transaction.
* After a restore: run the service's migrator (it must report nothing pending for the same
  release), then start the service and its workers. Leases in the restored rows have
  expired, so pending durable work (outbox events, Telephony commands, enforcement jobs,
  webhook deliveries) is reclaimed exactly as after a crash; paid Telephony actions keep
  their idempotency keys, and a command past the transmit fence is never re-sent (it goes
  to reconciliation).
* **Key material is never in a database backup.** Contact Vault keyrings, provider
  secrets and signing keys are backed up through the secret store. Restoring Privacy Relay
  without the keyring versions its rows reference makes it refuse to start / fail closed
  (no plaintext fallback). Protected objects (voicemail, recordings) live in the private
  bucket with its own versioning / lifecycle; the restored Calling rows keep their
  authorization, deletion and retention state.
* Each service's `tests/disaster_recovery.rs` exercises this end to end against
  PostgreSQL (`DISASTER_RECOVERY_CERT_REQUIRED=1` makes a missing `pg_dump` a failure).

## Environment-driven configuration

| Variable | Component | Notes |
|---|---|---|
| `DATABASE_URL` | meeting-service | PostgreSQL |
| `REDIS_URL` | meeting-service | Redis |
| `LIVEKIT_URL` | meeting-service | LiveKit URL returned/used for meeting media |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | meeting-service | LiveKit token/control credentials |
| `FRONTEND_URLS` | meeting-service | CORS allow-list |
| `APP_URL` | meeting-service | App/join URLs used by backend flows |
| `GOOGLE_REDIRECT_URL` | meeting-service | OAuth callback |
| `NEXT_PUBLIC_API_URL` | web | Meeting Service API base URL including `/api/v1` |

## Monorepo deployment roots

- Web: `apps/web`
- Meeting Service: `services/meeting-service/`

The earlier `services/core-api/` name was replaced before Phase 2 so the service's domain responsibility is explicit.

Before retiring the legacy `rtc_backend` / `rtc_frontend` repositories, deploy both the web app and meeting service successfully from `cfmeet` and verify `/health` plus a full meeting join.

## Security, secrets and retention

Before any environment goes live read, in this order:

* [`docs/architecture/security-deployment-checklist.md`](../../docs/architecture/security-deployment-checklist.md) — network segmentation per plane, secret delivery (`*_FILE`), backup / keyring separation, administrative interfaces, go-live gates.
* [`docs/architecture/secret-rotation-runbook.md`](../../docs/architecture/secret-rotation-runbook.md) — rotation steps for every secret class, including which rotations are not seamless.
* `infra/telephony/README.md` — SIP / media edge planes, carrier peer configuration, certification.
* `make secret-scan` — repository scan; `python3 scripts/secret-scan/scan_secrets.py --tree <exported rootfs>` scans a built image.

