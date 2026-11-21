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
| `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED` | no | `true` / `false` (default `false`; requires provisioning): the saga also creates, verifies, renews and (after archive) revokes each organization's Calling and Messaging service bindings (ADR 0058 A2; Platform Access must grant `platform-identity.bind-organization-services`) |
| `ORGANIZATIONS_EVENT_DESTINATIONS` | no | comma-separated organization-event consumers (today only `cfmeet.meeting-service`, with `CF_MEETING_SERVICE_BASE_URL`); empty: events stay PENDING |
| `CF_NOTIFICATIONS_BASE_URL` | no | Notification Service internal base URL (https outside local / test). When set, the catalogue events `organizations.*` are published to its I1 (see below); unset (default): nothing is published and nothing is written |
| `ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED` | no | `true` / `false` (default `false`; gap audit G19): with `CF_NOTIFICATIONS_BASE_URL`, also publish `organizations.organization.{archived, reactivated}` to the organization's ACTIVE members. The Notification Service catalogue carries both types since notification event contract 1.4.0 (#39 / #40; before it an enabled event was recorded `SKIPPED NOT_IN_CATALOGUE`, never sent). Turning it on also needs the pending migration `migration/pending/m20261118_000007_lifecycle_notification_types.rs` released first (kept out of the history for rollback safety); without it the service refuses to start |
| `ORGANIZATIONS_EXPORT_DELIVERY_ENABLED` | no | `true` / `false` (default `false`; ADR 0068 A4): owner-delivered exports for the admin bridge (see below). Needs `CF_NOTIFICATIONS_BASE_URL`, `CF_NOTIFICATIONS_SEALING_KEY`, `ORGANIZATIONS_EXPORT_KEYS_FILE` and the Notification Service catalogue type `organizations.organization.export_ready` (not published yet): the service refuses to start otherwise. Off: the bridge's export command answers 409 `EXPORT_DELIVERY_UNAVAILABLE` |
| `CF_NOTIFICATIONS_SEALING_KEY` | with export delivery | the Notification Service's public HPKE key `<kid>:<base64url>` (each export link's token is sealed to it) |
| `ORGANIZATIONS_EXPORT_KEYS_FILE` | with export delivery | file (0400) of `<key id>:<base64url of 32 random bytes>` lines: the at-rest keys of exports (XChaCha20-Poly1305). The first line seals, every line opens: rotate by adding the new key first and keep the old one 7 days |
| `CF_SERVICE_AUTH_KEY_ID` | when provisioning, event delivery or notification publication | `kid` of the Organizations signing key |
| `CF_SERVICE_AUTH_PRIVATE_KEY_FILE` | when provisioning, event delivery or notification publication | PKCS#8 PEM Ed25519 private key file |
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

## Notification Service publications (`src/notification_publications.rs`, roadmap N7)

Runs in the same loop only when `CF_NOTIFICATIONS_BASE_URL` is set; unset, the step does
not run and the table `organization_notification_publications` stays untouched.
Organizations never notified users itself, so there is no old path and no routing fence:
everything it publishes is new-only.

* **Source.** The committed `cfmeet.reconciliation` record rows of the outbox (written in
  the transaction of every change) of the ten source types `invitation.{created, accepted,
  declined, revoked, expired}`, `membership.{role_changed, suspended, removed}`,
  `ownership.transferred` and `organization.suspended`, younger than 24 h. Nothing is added
  to the business transaction; on first activation, the last 24 h of events are published.
* **Envelope.** Built once with `packages/notification-client` and stored (re-posts are
  byte-identical): ids only (`invitationId` / `membershipId`, `organizationId`, the role
  when the contract can carry it: never `MANAGER`), no name, email, token or free text.
  `eventId` / `route.decisionId` derive from the source event id and the recipient chunk
  (redelivery = `DUPLICATE`); `dedupeKey` = `<type>:<source event id>`.
* **Recipients** (`cfmeet-user:<id>`, never the acting user, only within the event's
  organization): invitee account for `invitation.created` (unknown for an email-addressed
  pending invitation: `SKIPPED NO_RECIPIENTS`); inviter for accepted / declined; inviter
  and known invitee for revoked / expired; the member for membership events; the new and
  previous owner for `ownership.transferred`; ACTIVE members (chunks of 40) for
  `organization.suspended`.
* **Lifecycle (gap audit G19, off by default).** With
  `ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED=true`, `organization.archived` and
  `organization.reactivated` are discovered too (`organizations.organization.archived` /
  `.reactivated`, data `organizationId`, the organization's ACTIVE members in chunks of
  40, never the actor). Notification Service owner prerequisite: both catalogue types
  (category `ORGANIZATION_UPDATES`, subject `ORGANIZATION`, required data
  `organizationId`, templates and the `OPEN_ORGANIZATION` action like
  `organizations.organization.suspended`) in `packages/notification-client`, the
  notification event contracts and the service catalogue — **met** by notification
  event contract 1.4.0 / common 1.3.0 (catalogue #39 / #40, templates
  `org.organization.archived` / `org.organization.reactivated`, in-app + push).
  Remaining Organizations owner steps: move
  `migration/pending/m20261118_000007_lifecycle_notification_types.rs` into
  `migration/src/`, register it in `Migrator::migrations()`, release it through the
  coordinated release, then set `ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED=true`
  (the service refuses to start with the switch on before that migration). The
  Notification Service delivers them once `ORGANIZATION_UPDATES` IN_APP / PUSH are ACTIVE.
* **Relay.** Batches of ≤ 100 to `POST /internal/v1/notification-events`
  (audience `cfmeet.notifications`, permission `notifications-event.publish`; the
  Notification Service trust bundle must grant it to `cfmeet.organizations`). ACCEPTED /
  DUPLICATE / HELD → `ACKED`; REJECTED or a 4xx contract refusal → `DEAD`; 5xx, 401, 403,
  408, 429 and transport errors retry with backoff (2^n s, ≤ 5 min, at most 16 attempts,
  then `DEAD ATTEMPTS_EXHAUSTED`); unacknowledged after 24 h → `EXPIRED`. Settled rows are
  purged after 30 days.

```sql
SELECT event_type, state, last_error, count(*) FROM organization_notification_publications
 GROUP BY 1, 2, 3 ORDER BY 1, 2;
```

## Owner-delivered exports (`src/exports.rs`, ADR 0068 A4)

The admin bridge's R4 export never returns the document to HQ. The command builds it, keeps an
encrypted copy (`organization_exports`) for 7 days and queues one single-use link notice per
ACTIVE OWNER (`organization_export_deliveries`: token SHA-256 only; the HPKE-sealed notice is
dropped once the Notification Service acknowledged it). Every worker tick expires links and
exports, purges encrypted copies (at expiry, on `FAILED`, or once every link is settled) and
deletes rows after 90 days; with `CF_NOTIFICATIONS_BASE_URL` it also relays pending notices to
I1 (same classification, backoff and attempt limit as the publications). Logs carry export and
delivery ids only. A link that cannot open (`owner-delivered export does not open`) means a
retired key was removed too early: the owner gets 503 and HQ issues the command again.

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

### Automatic service bindings (ADR 0058 Amendment A2, `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED`)

With the flag on, a verified scope does not make the organization READY by itself: the
workflow records `step: SCOPE_VERIFIED`, then for `calling` and `messaging` in turn calls
Platform Access `PUT …/organization-scopes/{id}/primary/service-bindings/{service}` (create
or return; the credential secret never leaves Platform Access), reads it back with `GET`,
verifies it server-side (same triple, service account and credential ACTIVE, exact scope set:
Calling `calls.read`, `enterprise.config.read`, `enterprise.config.write`; Messaging
`messages.read`) and records the ids (`source = AUTOMATIC`, `step: BINDING_<SERVICE>_VERIFIED`).
Only then is the organization READY (`organization.provisioned`). Every step is idempotent: a
lost lease or a retry gets the same service account from Platform Access.

| Symptom | Meaning | Action |
|---|---|---|
| `IN_PROGRESS`, `step: SCOPE_VERIFIED` / `BINDING_CALLING_VERIFIED`, `lastErrorClass: PLATFORM_ACCESS_UNAVAILABLE` | bindings being retried with backoff (FAILED after 8 attempts) | restore Platform Access |
| `FAILED` + `SERVICE_BINDING_CONFLICT` | Platform Access refused (409): tenant not active, a foreign `org-<service>` account, or a binding revoked earlier | investigate in Platform Access, then retry |
| `FAILED` + `SERVICE_BINDING_VERIFICATION_FAILED` | the binding's read-back does not verify (wrong triple, scopes or status) | investigate, then retry |
| `FAILED` + `SERVICE_BINDING_DRIFT` | reconciliation found a recorded automatic binding no longer verifies; it was removed locally (fail closed); the tenant is NOT suspended | fix in Platform Access, then retry |
| `FAILED` + `PLATFORM_ACCESS_REFUSED` | 401/403 (trust bundle / missing `platform-identity.bind-organization-services`) | fix the grant, then retry |

Reconciliation (every 6 h per READY organization, `organization_platform_scopes.bindings_*`):
backfills missing bindings of organizations that were READY before the flag was enabled,
re-`PUT`s automatic bindings (Platform Access renews a credential close to expiry; the new
credential id is recorded, version + 1, audited `scope.service_binding_rotated`) and
re-verifies them. Failures back off (`bindings_last_error_class`). An `OPERATOR` binding is
never touched; a binding revoked locally is never recreated.

Archive marks every automatic binding revoked in the archive transaction (resolution stops at
once; audited `scope.service_binding_revoked`); the worker then revokes the credential and
service account at Platform Access (`DELETE`), retried with backoff until applied
(`scope.service_binding_revocation_applied`). Suspension needs nothing extra: the tenant is
suspended, so every credential fails, and resolution refuses a suspended organization;
reactivation resumes the tenant and resolution returns the same bindings.

Operator rotation of an automatic binding (Platform Access issues a successor credential and
revokes the predecessor; the answer is verified before it is recorded):

```
POST /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/{calling|messaging}/rotate   (organization.administer)
```

Operator retry (idempotent; READY / in-flight workflows unchanged; archived refused):

```
POST /internal/v1/admin/organizations/{id}/provisioning/retry     (organization.administer)
```

## Operator (manual) service bindings (Section 12 follow-up, contract §8.5)

With automatic service bindings enabled (above) nothing here is routine: this is the
break-glass path. A binding recorded with `PUT` is `source = OPERATOR` and automation never
replaces it; `DELETE` of an AUTOMATIC binding revokes it at Platform Access instead of just
forgetting it, and automation does not recreate it. `PUT` is refused (`VERSION_CONFLICT`)
while such a revocation is still being applied. The same routes exist for `messaging`.

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
