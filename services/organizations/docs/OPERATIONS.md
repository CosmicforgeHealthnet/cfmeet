# Organizations Service operations

Nothing in this repository deploys the service yet. This page is the runbook for when a
release does; production steps need the release approvals of ADR 0056 §2 / §9.

## Processes and binaries

| Binary | Purpose |
|---|---|
| `organizations` | API (`ORGANIZATIONS_BIND_ADDRESS`, default `127.0.0.1:8101`) plus the in-process maintenance loop; `--check-config` validates configuration and the trust bundle and exits |
| `organizations-migration` | `up` / `down` / `status` / `reset` of the own schema (`organizations_seaql_migrations`), run as the migrator role; `down` / `reset` need `CF_MIGRATION_ALLOW_DESTRUCTIVE=<subcommand>` outside a non-production `APP_ENV` (SEC-015) |
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
| `ORGANIZATIONS_PROVISIONING_ENABLED` | no | `true` / `false` (default `false`): run the Section 7 provisioning saga (ADR 0058) |
| `CF_SERVICE_AUTH_KEY_ID` | when provisioning | `kid` of the Organizations signing key |
| `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | when provisioning | PKCS#8 PEM Ed25519 private key file |
| `CF_PLATFORM_ACCESS_BASE_URL` | when provisioning | Platform Access (https outside local / test) |
| `CF_SERVICE_TOKEN_TTL_SECONDS` | no | 10–300, default 120 |
| `CF_INTERNAL_HTTP_TIMEOUT_MS` | no | 100–30000, default 5000 |

With provisioning off (the default) the service calls nobody and holds no signing key;
organizations stay `PENDING` and `GET …/provisioning` reports `provisioningEnabled:
false`. With it on, Organizations calls only Platform Access, signing with its own key file
(inline keys are refused); Platform Access's trust bundle must grant
`cfmeet.organizations` the permission `platform-identity.provision-first-party`. Invalid
values fail startup without echoing the value.

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

## Provisioning saga (`src/provisioning.rs::run_once`, Section 7, ADR 0058)

Runs in the same loop when enabled: enqueue one workflow per ACTIVE `PENDING`
organization (stable key `org-scope:{id}:primary`), advance due workflows (Platform Access
`PUT` → checkpoint → `GET` + server-side verification → READY), apply desired tenant
statuses (suspension / resumption / compensation, retried forever with backoff) and
re-verify READY mappings every 6 h (drift → `FAILED`, `SCOPE_DRIFT`).

| Symptom (`GET /internal/v1/organizations/{id}/provisioning`) | Meaning | Action |
|---|---|---|
| `provisioningEnabled: false` | the saga is off in this deployment | none (organizations stay PENDING) |
| `workflowState: IN_PROGRESS`, `lastErrorClass: PLATFORM_ACCESS_UNAVAILABLE`, `nextAttemptAt` | Platform Access unreachable; capped backoff (5 s … 15 min), FAILED after 8 attempts | restore Platform Access |
| `FAILED` + `PLATFORM_ACCESS_REFUSED` | 401/403/4xx from Platform Access (trust bundle, key, permission) | fix the configuration, then retry |
| `FAILED` + `PLATFORM_SCOPE_CONFLICT` | the reserved tenant slug is held outside the scope registry | investigate in Platform Access (never adopted), then retry |
| `FAILED` + `SCOPE_VERIFICATION_FAILED` / `SCOPE_MISMATCH` / `SCOPE_DRIFT` | the triple does not verify, differs from the recorded one or vanished | investigate; any recorded tenant is being suspended (compensation) |
| `step: WAITING_FOR_ACTIVE_ORGANIZATION` | the organization is suspended; nothing is created | reactivate the organization |
| `scopes[0].lastSyncErrorClass` set | a tenant suspension / resumption is being retried | restore Platform Access; scope resolution refuses until applied |

Operator retry (idempotent; READY / in-flight workflows unchanged; archived refused):

```
POST /internal/v1/admin/organizations/{id}/provisioning/retry     (organization.administer)
```

## Calling service binding (Section 12 follow-up, contract §8.5)

Per-organization Enterprise Calling needs, inside the organization's READY triple, a
Platform Access service account with a credential and the Calling scope grants — an explicit
operator decision made in Platform Access (ADR 0058 §1: provisioning grants nothing). Once it
exists, record its ids (never the credential value) here:

```
PUT    /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/calling
       {"serviceAccountId": "<uuid>", "credentialId": "<uuid>", "expectedVersion": <current or 0>}
GET    …/service-bindings/calling        (404 NOT_FOUND = none)
DELETE …/service-bindings/calling        (idempotent; stops organization calling forwarding at once)
```

All three need `organization.administer`; changes are audited (`scope.service_binding_set` /
`scope.service_binding_cleared`, actor `OPERATOR`, relayed operator as `boundBy`). Refusals:
`PROVISIONING_PENDING` (no recorded triple), `ORGANIZATION_ARCHIVED`, `VERSION_CONFLICT`.
Rotation = create the new credential in Platform Access, `PUT` the new `credentialId` with the
current `expectedVersion`, then revoke the old credential. A wrong id cannot widen access:
Platform Access refuses a credential outside the organization's triple. Rollback of the
release that adds this: `CF_MIGRATION_ALLOW_DESTRUCTIVE=down organizations-migration down -n 1` drops
`organization_scope_service_bindings` (bindings are lost; organization calling forwarding is
off without them).

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
