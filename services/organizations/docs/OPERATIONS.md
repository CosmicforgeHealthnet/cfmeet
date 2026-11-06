# Organizations Service operations

Nothing in this repository deploys the service yet. This page is the runbook for when a
release does; production steps need the release approvals of ADR 0056 §2 / §9.

## Processes and binaries

| Binary | Purpose |
|---|---|
| `organizations` | API (`ORGANIZATIONS_BIND_ADDRESS`, default `127.0.0.1:8101`) plus the in-process maintenance loop; `--check-config` validates configuration and the trust bundle and exits |
| `organizations-migration` | `up` / `down` / `status` / `reset` of the own schema (`organizations_seaql_migrations`), run as the migrator role |
| `organizations-import` | `organizations-import <export.json>` imports a Meeting Service export; `--reconciliation` prints the reconciliation |

## Configuration

| Variable | Required | Meaning |
|---|---|---|
| `DATABASE_URL` | yes | the Organizations database (runtime role) |
| `ORGANIZATIONS_DATABASE_MAX_CONNECTIONS` | no | 1–200, default 10 |
| `ORGANIZATIONS_BIND_ADDRESS` | no | default `127.0.0.1:8101` |
| `ORGANIZATIONS_WORKER_INTERVAL_MS` | no | 1000–600000, default 30000 |
| `LOG_FORMAT` | no | `pretty` / `json` |
| `CF_SERVICE_ID` | no | must be `cfmeet.organizations` if set |
| `CF_RUNTIME_ENVIRONMENT` | yes | `local`, `test`, `staging`, `production` |
| `CF_SERVICE_TRUST_BUNDLE_FILE` | yes | receiver-authoritative trust bundle (`receiver: cfmeet.organizations`) |

The service calls no other service yet, so it holds no signing key (inline keys are
refused). Invalid values fail startup without echoing the value.

## Database roles

```bash
infra/deployment/postgres/provision-service-database.sh --service organizations \
  --admin-url "$ADMIN_URL" --migrator-password-file m.txt --runtime-password-file r.txt
DATABASE_URL=<migrator url> organizations-migration up
```

The runtime role has data access only (no DDL); the API refuses to start while migrations
are pending (`/ready` reports `migrations: pending`). `verify-service-isolation.sh`
certifies the separation (CI: `organizations.yml`, `service-boundaries.yml`).

## Maintenance loop (`src/worker.rs::run_once`)

Every interval, idempotent and multi-instance safe: sweep expired service-token nonces,
sweep idempotency records older than 24 h, expire PENDING invitations past `expires_at`
(`invitation.expired` event + audit, `FOR UPDATE SKIP LOCKED`), and deliver outbox rows
when a dispatcher is enabled. **Delivery is disabled in Section 5**: events are recorded
for destination `cfmeet.reconciliation` and stay PENDING; `/ready` shows the backlog.

## Migration import runbook (Section 4 / 5)

1. Meeting Service produces the read-only export (`export_organizations`; no source write).
2. Import (either path; both are one transaction and idempotent):
   * `organizations-import export.json` with `DATABASE_URL` set, or
   * `POST /internal/v1/migration/import` with the export as body (`organization.migrate`).
3. Exit status / HTTP: refused exports (schemaVersion, source, counts, checksum, invalid
   values, orphan memberships) write nothing (CLI exit 1, HTTP 400). Otherwise the report
   lists `imported`, `unchanged` and `conflicts`.
4. Conflicts are never overwritten. Resolve them at the source (or wait for the explicit
   cutover re-sync decision) and re-import.
5. Reconcile: `organizations-import --reconciliation` or
   `GET /internal/v1/migration/reconciliation`. `checksum` is computed over the imported
   rows in their current state with the export algorithm; it must equal the checksum of a
   fresh Meeting Service export before the cutover gate. `mismatches` must be empty.
6. Re-running the import with an unchanged export is a no-op (`imported: 0`).

The shared checksum test vector is in the contract §5.

## Observability

Structured logs (`LOG_FORMAT=json`) carry route patterns, caller identity and request /
correlation ids — never tokens, emails or bodies. Audit events
(`organization_audit_events`) and membership history are append-only (trigger-enforced).
