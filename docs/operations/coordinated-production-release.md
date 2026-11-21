# Coordinated production release (all CF Meet backend services)

**One path to production.** Every CF Meet backend service is released together from one
`main` SHA by `.github/workflows/cfmeet-coordinated-release.yml`; the VPS side is
`infra/calling/production/coordinated_release.py` (offline tests:
`test_coordinated_release.py`). This document orders and automates the steps of
[final-production-activation-runbook.md](final-production-activation-runbook.md); the
[organizations-cutover-runbook.md](organizations-cutover-runbook.md) and the
[developer-platform-gateway-activation-checklist.md](developer-platform-gateway-activation-checklist.md)
stay authoritative for their own scope.

`deploy-backend.yml` no longer deploys on a `main` push (it still runs the Meeting Service
tests); its manual dispatch remains as an emergency Meeting-only path. Both workflows, and
every mutating `cfmeet-calling-production.yml` operation, take the same
`deploy-backend-production` concurrency lock (`cancel-in-progress: false`), so two
production changes never overlap. GitHub keeps only one *pending* run per group: a third
dispatch while one runs and one waits cancels the waiting one; dispatch again after it.

## Services, order and runtime

| Order | Service | Container | Private HTTPS (`api.cfmeet.com:<port>` on `cfmeet-core-private`) | New in production | Memory limit |
|---|---|---|---|---|---|
| 1 | Platform Access | `cfmeet-platform-access` | 8443 | no | kept |
| 2 | Organizations | `cfmeet-organizations` | 8450 | yes | 192 MiB |
| 3 | Messaging | `cfmeet-messaging` | 8444 | no | kept |
| 3 | Calling | `cfmeet-calling` | 8445 | no | kept |
| 3 | Communications | `cfmeet-communications` | 8447 | no | kept |
| 3 | Numbering | `cfmeet-numbering` | 8451 | yes | 192 MiB |
| 3 | Privacy Relay | `cfmeet-privacy-relay` | 8452 | yes | 192 MiB |
| 3 | SMS Gateway | `cfmeet-sms-gateway` | 8453 | yes | 192 MiB |
| 3 | Telephony | `cfmeet-telephony` | 8454 | yes | 192 MiB |
| 4 | Meeting Service | `cfmeet-production-backend` | 8448 (public: host nginx → 127.0.0.1:18081) | no | kept |
| 5 | Developer Platform (+ worker) | `cfmeet-developer-platform`, `cfmeet-developer-platform-worker` | 8455 | yes | 256 + 128 MiB |
| 6 | API Gateway | `cfmeet-api-gateway` | none (public only after `activate-gateway-routing`) | yes | 192 MiB |
| – | Security scan gateway | `cosmicforge-scan-gateway` | 8449 | upgraded only if already deployed (`scan_gateway.configure`); otherwise use `scan-gateway-configure` | 64 MiB |

New services run on `cfmeet-core-private` only: no published port, `--restart unless-stopped`,
read-only root, `/tmp` tmpfs, no capabilities, `no-new-privileges`, bounded logs, the memory
limit above, and an env file (mode 0600) under `/opt/cfmeet-core/coordinated/env/`. Existing
services are replaced with their exact configuration (environment, mounts, networks, static
addresses, loopback ports, limits) and only the image changes (`communications_release.clone`).
Internal URLs use HTTPS through the private TLS proxy (`cfmeet-core-tls`), which `provision`
extends with one route per new service (the R1 blocker of the activation checklist).

Never touched: `cosmicforge-backend`, `cosmicforge-db`, `cosmicforge-redis`, any
`cfmeet-staging-*`, LiveKit, the ingress/egress/storage helpers. The code refuses to stop,
start, rename, create or remove any container outside its own list (`owned()`); the Meeting
database (wherever the Meeting container's `DATABASE_URL` points) is only *read* by
`pg_dump`/`psql` from inside the Meeting container's network namespace.

## Operations

Server-audit follow-up: `activate-database-capacity` prepares the core PostgreSQL server
for `activate-database-pools`. Its `confirm=restart-core-database` acknowledges a brief
connection interruption. It rounds the live pool plan plus reserved/operator connections
to the next 32, refuses more than 256 connections, and requires 16 MiB of available memory
per additional backend plus 2 GiB of headroom. A health failure restores the previous
setting. Roll application pools back before rolling capacity back. This operation changes
only `cfmeet-core-db`; it never restarts the Meeting Service's separate database.

Meeting Service releases and activation clones mount `/opt/cfmeet-core/logs/meeting-service`
at `/var/log/cfmeet` and set `CFMEET_LOG_DIRECTORY`. The image retains fifteen daily log
files, covering today and the previous fourteen days; replacement containers keep the same
host directory. Existing logs inside old containers are retained with those rollback
containers; the new retention window begins when this release starts writing to the mount.

| Operation | Changes production | What it does |
|---|---|---|
| `inspect` | no | MemAvailable, disk, memory use, every CF Meet container (state, image, image revision label (`revision`; the container's own, possibly stale, label is `containerLabelRevision`), limits, networks, ports, env **names**, non-secret flag values), migration histories of every database, mail credential presence (booleans), the release journal and a resource-gate preview. |
| `backup` | files only | `pg_dump -Fc` of every `cfmeet_*` database in `cfmeet-core-db` and of the Meeting database; sha256; `pg_restore --list` must list every migration history; isolated restore test (network-less throwaway PostgreSQL, histories compared row for row) of Meeting, Platform Access, Organizations and Developer Platform. Manifest: `/opt/cfmeet-core/backups/<run>/manifest.json`. Disk gate: 2 × database size + 5 GiB free. |
| `provision` | yes (idempotent, additive) | For the new services: database + `<svc>_migrator` / `<svc>` roles (the `cfmeet-core-db.yml` SQL), Ed25519 service keys (`/opt/cfmeet-core/secrets/<svc>-service-auth.pem`), SMS Gateway content/fingerprint keys, empty receiver trust bundles, private TLS routes 8450–8455 (`nginx -t`, reload, restore on failure). |
| `release` | yes | See below. After a successful release the monitoring stack is converged as in `deploy-monitoring`; a monitoring failure is reported (`monitoringStack: NOT_UPDATED`) and never fails the release. |
| `deploy-monitoring` | monitoring only | Prometheus, Alertmanager, blackbox and heartbeat exporters (`monitoring_stack.py`, [monitoring-alerts.md](monitoring-alerts.md#production-monitoring-stack)); creates the read-only database role `cfmeet_monitor`; installs or removes the Alertmanager receiver URLs from the environment secrets `CFMEET_ALERT_WEBHOOK_URL` / `CFMEET_ALERT_SLACK_WEBHOOK_URL`. No service container changes. |
| `verify` | no | Every container runs the manifest image of the SHA with revision label = SHA and is healthy; migration histories present; provider-dependent features off; public smoke (`/api-docs/swagger-config.json`, CORS preflight from `https://cfmeet.com`, `/internal/*` not public); Organizations event delivery by destination and event type (`organizations_event_delivery`: the never-dispatched `cfmeet.reconciliation` records are reported as event history, never as failures; a consumer row DEAD, undelivered for 15 minutes to a configured destination, blocked behind a DEAD event, or without its record fails); the existing production regression. |
| `certify` | no | Read-only live-production certification of the deployed topology, whatever revisions it runs (see below). `verify` is unchanged and stays strictly same-SHA. |
| `rollback` | yes | Restores the containers of the **latest** journal entry (a release or one activation), newest component first; new services are stopped and renamed `…-rolled-back-…`. Migrations and databases are kept (every migration is additive and the previous images start on the migrated schema). Refuses `org-cutover-authoritative`. Repeat to unwind further. |
| `activate-provisioning` | yes | Grant `platform-identity.provision-first-party` (Platform Access ← Organizations); `ORGANIZATIONS_PROVISIONING_ENABLED=true`. |
| `activate-organization-bindings` | yes | ADR 0058 A2 (after `activate-provisioning`): grant `platform-identity.bind-organization-services` (Platform Access ← Organizations); `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED=true`. Refuses unless Organizations migration `m20261113_000005_automatic_service_bindings` is applied; fails (and restores) if any provisioning workflow is FAILED afterwards. Required by `activate-enterprise-calling`. |
| `activate-org-events` | yes | Pre: Meeting has `organization_integration_migrations`, private route 8448. Grant `meeting-organization-events.apply` (Meeting ← Organizations) and `ORGANIZATION_EVENTS_ENABLED=true` on Meeting, then `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` + `CF_MEETING_SERVICE_BASE_URL` on Organizations. Post: no `DEAD` events. |
| `org-cutover-export` | file only | Runbook §1 inventory (counts only, read-only transaction) and §2 export with the Meeting image's `export-organizations`. Refuses rows the import would refuse; ownerless organizations need `confirm=ownerless-accepted`. Export counts must equal the inventory. |
| `org-cutover-import` | Organizations data | `confirm` must contain `offsite-backup-verified` (owner gate 1) and a backup ≤ 24 h with restore tests of Meeting **and** Organizations must exist. Import (`organizations-import`, export streamed on stdin), zero conflicts, reconciliation with zero mismatches and the export checksum, a repeated import fully unchanged; the export file is then deleted. |
| `org-cutover-shadow` | yes | Grant `organization.read/write/act-for-user/administer` (Organizations ← Meeting); `ORGANIZATIONS_AUTHORITY_MODE=shadow` + `CF_ORGANIZATIONS_BASE_URL` on Meeting. Reversible with `rollback`. |
| `org-cutover-authoritative` | yes, **irreversible** | Owner gates 1 and 6: `confirm=offsite-backup-verified,quiesced` after the legacy write routes are blocked at the ingress (runbook §6.2, manual) and the 7-day zero-mismatch criterion (§5) is accepted. Final export → import → reconciliation, then `ORGANIZATIONS_AUTHORITY_MODE=authoritative`. Never rolled back (runbook §7). |
| `org-cutover-first-activation` | yes, **irreversible** | Empty-dataset first activation (ADR 0059 + Amendment 1): `confirm=empty-dataset`. **No backup prerequisite** (owner decision 2026-11-13; no off-host record, local backup, drill or age key is checked; the run records `backupGate.required=false` and never claims a verified backup). Refuses unless no cutover step ran, the Meeting Service is `legacy`, and every organization dataset (Meeting, Organizations, organization references in every CF Meet database) is empty. Then empty export → import / reconciliation (`backup_gate=False`) → inventory again → shadow grants + `authoritative` in one step → startup-log and quiescence check (returns to `legacy` automatically while Organizations is still empty). Never rolled back. |
| `activate-enterprise-calling` | yes | Requires authoritative. Grants `platform-principal.ensure` (Platform Access), `calling-request.act-for-organization` (Calling), `messaging-application.delegate` (Messaging), all ← Meeting; `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`; `ORGANIZATION_CALLING_SCOPE=organization` (+ Calling/Messaging URLs if absent) on Meeting. Requires `activate-organization-bindings`: per-organization service bindings are provisioned automatically (ADR 0058 A2). |
| `activate-developer-platform` | yes | Grants (checklist §4): Platform Access ← API Gateway `platform-credential.authenticate`; Platform Access ← Developer Platform `platform-identity.read`, `platform-identity.delegate`; Developer Platform ← API Gateway `developer-request.forward`, `developer-entitlement.read`, `developer-usage.record`. `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true`; gateway `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true` + `CF_DEVELOPER_PLATFORM_BASE_URL`. Still not public. |
| `activate-gateway-routing` | yes (public) | Requires the previous step. A loopback-only relay (`cfmeet-gateway-ingress`, 127.0.0.1:18110 → `cfmeet-api-gateway:8099`, same confinement as the storage relay) and `/etc/nginx/snippets/cfmeet-api-gateway.conf`, included after the Calling routes, routing **only** the Developer Platform families (`/v1/developer-platform`, `/v1/organizations`, `/v1/projects`, `/v1/environments`, `/v1/scopes`, `/v1/invitations`, `/v1/production-access-requests`, `/v1/commercial-plans`). Refuses if any existing location already serves one of them. Meeting, Calling and Messaging keep their direct routes; `/internal`, operator and provider-callback paths never reach the gateway. `nginx -t`, reload, then: gateway answers 401 unauthenticated, CORS for `https://cfmeet.com`, public smoke, regression; any failure removes the include, snippet and relay. `rollback` undoes it the same way. |
| `activate-developer-console` | yes | Requires `activate-developer-platform`. Grant `user-session.verify` (Meeting ← Developer Platform); Meeting `DEVELOPER_SESSION_BRIDGE_ENABLED=true`; Developer Platform `CF_MEETING_SERVICE_BASE_URL`. Developer Console (DEVELOPER_SESSION) requests stop being refused; the bridge fails closed (revocation store required; guests never authenticate). |
| `activate-developer-webhooks` | yes | Requires the console step. Refuses unless Calling holds **no** webhook subscriptions or open deliveries and no Calling→DP migration was ever recorded (otherwise: phase-06 webhook migration runbook). Generates the 32-byte sealing key ring (`/opt/cfmeet-core/secrets/developer-platform-webhook-secret.key`, 0400, uid 10001) and mounts it read-only into the Developer Platform API and worker; `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED=true`, `…_DELIVERY_MODE=active`. Attaches **only the worker** to `cfmeet-webhook-egress` (non-internal bridge, inter-container traffic off; `cfmeet-core-private` is `--internal`); the worker's STRICT endpoint policy allows https only and refuses private, loopback, link-local and metadata addresses at connect time. Calling `CALLING_WEBHOOK_AUTHORITY=retired`, `CALLING_WEBHOOK_DELIVERY_ENABLED=false`; Calling, Messaging and Meeting publish to the central ingestion (`…_PUBLIC_EVENTS_DESTINATION=developer-platform`, grants `developer-event.publish`). `SAFE_DEFAULTS` accept `active` only once this step is recorded; auto-approval stays refused. |
| `activate-gateway-families` | yes (public) | Requires gateway routing and webhooks. Grants `calling-request.forward` (Calling ← gateway) and `messaging-request.forward` (Messaging ← gateway); gateway `CF_CALLING_BASE_URL`, `CF_MESSAGING_BASE_URL`, `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED=true`, `API_GATEWAY_DISABLED_DOMAINS=numbering,sms,privacy-relay`. Extends the gateway snippet with `/v1/conversations`, `/v1/messaging`, `/v1/abuse-reports`, `/v1/blocks`, `/v1/limits`, `/v1/usage`, `/v1/webhooks`, `/v1/webhook-deliveries` (refuses existing locations). Calling's `/v1/calls`, `/v1/calling`, `/v1/enterprise` and Messaging's `/v1/realtime` keep their direct routes. Post-checks: every family refuses anonymous and forged credentials (401/403), CORS only for `https://cfmeet.com`, `/v1/numbers`, `/v1/sms`, `/v1/privacy-relay` stay unrouted, internal paths private; any failure restores the snippet and the containers. `rollback` removes the families from the edge first. |
| `activate-gateway-docs` | yes (public, edge only) | Requires gateway routing. Adds the API Gateway's public documentation to the gateway snippet: exact `location = /discovery`, `/openapi.json`, `/docs` and `location ^~ /openapi/` (the per-domain documents the reference and the catalogue link to), GET/OPTIONS only (`limit_except`). No container, flag or grant changes; refuses to shadow an existing location. Post-checks: discovery served by `cfmeet.api-gateway` with numbering / sms / privacy-relay reported `enabled: false`, aggregate OpenAPI 3.x without `/internal` paths, the Swagger UI reference, per-domain documents, CORS on `GET /discovery` for both web origins and never a foreign one, POST refused, the gateway's `/ready` and `/health` still not public, existing families and public smoke unchanged; any failure restores the previous snippet. `rollback` removes only the documentation locations. For the Developer Console Docs page. |
| `activate-edge-rate-limits` | yes (public, edge only) | P7. Per-client-IP throttling of the public api.cfmeet.com server block (host nginx): `conf.d/cfmeet-edge-rate-limits.conf` declares the zones (`limit_req_zone … rate=50r/s`, `limit_conn_zone`, status 429), `snippets/cfmeet-edge-limits.conf` applies `limit_req … burst=200 nodelay` and `limit_conn … 300` at server level (inherited by every location; body size limits unchanged, so uploads, the web app's page-load bursts and Messaging / signaling WebSockets — one request each, long-lived connections — stay well inside). Refuses a site that already declares zones; nginx -t and the public smoke must pass or the previous files are restored. **Opt-out:** `rollback` removes exactly this include and both files. Tune `EDGE_RATE` / `EDGE_BURST` / `EDGE_CONNECTIONS` in `coordinated_release.py`. |
| `activate-token-ceiling` | yes | P7. Requires gateway routing. Sets the API Gateway's per-token ceiling of end-user / device traffic `API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE=600` (meeting sessions, participant tokens, developer sessions, client tokens, signaling credentials, realtime tickets; shared quota store; 429 `x-cf-quota-level: GATEWAY_TOKEN`). Platform-credential traffic keeps its plan quota. **Opt-out:** `rollback` (restores the previous container, ceiling off). Services have no per-token ceiling of their own. |
| `activate-client-address-limits` | yes (public, edge + Meeting Service) | S1. The Meeting Service's per-visitor sign-in limits (20 code requests and 30 code checks per address per 15 minutes) need the visitor's address. Host nginx terminates TLS for api.cfmeet.com itself (no CDN; the edge limits already key on `$binary_remote_addr`), so `$remote_addr` is the visitor: every proxying location of the public site gets `proxy_set_header X-Real-IP $remote_addr;` (overwriting anything the visitor sent; a location that sets X-Real-IP to anything else is refused), then the Meeting Service is recreated with `AUTH_CLIENT_IP_HEADER=x-real-ip`, `AUTH_TRUSTED_PROXY_HOPS=1`. nginx -t, the public smoke and the Meeting health must pass or the site and container are restored. **Opt-out:** `rollback` restores the Meeting Service (limits off); the header line stays (harmless). |
| `activate-account-privacy` | yes | S2. Grants `calling-account.privacy` (Calling ← Meeting Service): account deletion erases the person's call history and the data download includes it. Messaging (`messaging-application.delegate`) and the Hub (`communications-account.apply`) already hold their grants. Requires `CF_CALLING_BASE_URL` on the Meeting Service. Until it runs, the Calling erasure jobs stay pending and are retried (nothing is lost). |
| `activate-database-pools` | yes | B6 / C5. The only way the bigger database pools take effect (code defaults stay 10, the Notification Service template 8): sets `MESSAGING_DATABASE_MAX_CONNECTIONS=25`, `CALLING_DATABASE_MAX_CONNECTIONS=20`, the Meeting Service's `DATABASE_MAX_CONNECTIONS=20` and `NOTIFICATIONS_DATABASE_MAX_CONNECTIONS=16` on both Notification replicas (its code is not changed), after checking that every service's pool (these plus every other service's current one) fits `SHOW max_connections` of cfmeet-core-db minus reserved connections and 15 headroom; refuses otherwise (raise max_connections first). **Opt-out:** `rollback` (restores the previous containers and pools). |
| `activate-service-key-publish` | yes (`confirm: service=<name>`) | P16, service key rotation step 1 of 3 (`key_rotation.py`). Generates the service's next Ed25519 signing key on the VPS (`<identity>-service-auth.<kid>.pem`, kid `<identity>-production-<n+1>`, owned by the service, 0400) and adds its public key **next to** the current one for that issuer in every receiver's trust bundle (permissions unchanged; new bundle versions, old files kept). Receivers are recreated with the same image and configuration. The service still signs with the old key. |
| `activate-service-key-switch` | yes (`confirm: service=<name>`) | Step 2. Requires step 1 for that service and that it still signs with the published old kid. Recreates the service with `CF_SERVICE_AUTH_KEY_ID=<new kid>` and the new key mounted at `/run/secrets/service-auth.pem`. Both keys are trusted, so nothing fails during the switch; later releases clone the running container, so the new key stays. |
| `activate-service-key-retire` | yes (`confirm: service=<name>`) | Step 3. Requires step 2 and at least 15 minutes since it (every old-key token, ≤ 300 s, has expired), and that the service signs with the new kid. Removes the old kid from every receiver's bundle (never the last key). The old key file stays on disk. Each step is journaled and undone by `rollback` (previous containers and bundle files); a failed step restores itself. No key material is printed or journaled. |
| `activate-usage-export` | yes | Requires the webhooks step (its `developer-event.publish` grants and Developer Platform targets). `CFMEET_USAGE_EXPORT_ENABLED=true` on Calling, Messaging and the Meeting Service: billing-neutral usage facts (safe refs only; platform application environments only) to the unified ledger. Post: the ledger's acknowledged usage streams are reported. Customer `/v1/usage` and `/v1/limits` stay with Calling. |
| `developer-acceptance` | acceptance data only | Requires the three developer/gateway activations. Public API only (`developer_acceptance.py`): two fresh acceptance accounts, each with a developer organization, project, SANDBOX environment, granted scopes and a credential; credential calls to Calling, Messaging and central webhooks through the gateway; scope denial; forged/revoked credentials refused; tenant B gets 404 for tenant A; a console webhook receives a real `credential.expiring_soon` delivery (SUCCEEDED, attempt recorded) at `ACCEPTANCE_WEBHOOK_RECEIVER`. Cleanup always runs (webhook deleted, credentials revoked, projects archived, organizations closed). |
| `diagnose` | no | Read-only (no CI wait, any SHA like `inspect`). The Meeting Service's mail configuration (non-secret values; presence for secrets), mail failure signatures in its logs (counts and first/last timestamps only: `535`, `BadCredentials`, dropped jobs, provider outcomes), the legacy Redis `email:queue` length, the email ledger (status / category / error-class counts, provider events) and the conference lifecycle (LIVE conferences by kind, ENDING, LIVE instant conferences without an active session for more than 10 minutes, LIVE conferences of archived spaces, the reported meeting codes). |
| `activate-resend-email` | yes | Requires the durable email ledger (`lifecycle-delivery-migration`). Verifies over SSH, without reading them out, the VPS-only credential files `/opt/cfmeet-core/secrets/resend-api-key` (required) and `resend-webhook-secret` (optional): present, root-owned, no group/other access, expected form (owner decision 2026-11-16: nothing comes from GitHub secrets). It then recreates **only** the Meeting Service with `EMAIL_PROVIDER=resend`, `RESEND_API_KEY_FILE`, sender `CF Meet <no-reply@cfmeet.com>`; post-check: a controlled acceptance email to `vars.CFMEET_EMAIL_ACCEPTANCE_RECIPIENT` (default Resend's test inbox) must be ACCEPTED by Resend (and DELIVERED when the webhook secret is set). Any failure restores the previous container (SMTP). Owner procedure: [resend-email-owner-handoff.md](resend-email-owner-handoff.md). |
| `email-acceptance` | acceptance data only | Requires `activate-resend-email`. The acceptance email again; with `confirm=otp` also the public OTP journey (register a labelled acceptance account → Resend accepts the verification email → wrong / right / reused code → sign in → profile → deactivate). Prints statuses only. |
| `activate-production-access-review` | yes | Requires `activate-developer-console` and the owner's `vars.CFMEET_PRODUCTION_ACCESS_REVIEWERS` (`github-<login>=<CF Meet user id>`, …). Creates the `cfmeet.operator-console` Ed25519 identity (secret store), grants it `developer-platform.administer` on the Developer Platform, sets `DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS` and only then `…_PRODUCTION_ACCESS_WORKFLOW_ENABLED=true` (API + worker). Automatic approval stays refused. Post: the review tool lists the queue. |
| `developer-access-review` | Developer Platform data | One operator command (`confirm` = `list` / `get` / `review` / `approve` / `reject`, `request_id`, `reason_code` for reject) by the dispatching GitHub account, which must be a named reviewer; run with the Developer Platform image's `developer-platform-operator` (signed as `cfmeet.operator-console`, `x-cf-operator` = `github-<login>`, self-approval refused by the Developer Platform). Approve / reject email the applicant through the Meeting Service's unified email delivery. |
| `activate-first-party-features` | yes | No prerequisites. Meeting: `FEATURE_CONFERENCE_REALTIME_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_QUALITY_TELEMETRY_ENABLED` = `true`, and `NOTIFICATIONS_EMAIL_ENABLED=true` **only** when the Meeting Service already carries SMTP host + credentials. Messaging: `MESSAGING_SESSION_DEVICES_ENABLED=true` (MANAGED realtime; refuses if PRIVATE_E2EE devices are on). The healthcare profile is never switched (compliance decision). Reversible with `rollback`. |

Activation order is enforced: provisioning → org events → export → import → shadow →
authoritative → enterprise calling; developer platform → gateway routing. Every activation
also requires that the completed release of the dispatched SHA is what is running. Each
activation recreates only the affected containers (same image, preserved configuration,
changed flags and/or a **new** trust-bundle file version); receivers get their grants before
callers switch on. A failed health check restores every container the step changed.

### Release

**Release scope (final completion).** The runner (`release_scope.py`, full history) compares
every image's build inputs (Dockerfile `COPY` sources) between the latest completed release
and the release SHA. On the VPS, a service whose image inputs did not change is **kept**:
it must run exactly the image recorded for the previous release (re-checked), it is not
migrated or swapped, and the journal entry records it under `kept`. Only changed services
are migrated and replaced (a Meeting-only change no longer restarts all 13 containers).
`verify`, activations (`require_release`) and `rollback` follow the kept images. Without a
previous release, or a scope document that does not match, the release is a full release.

1. Workflow gates: `refs/heads/main`; `release_sha` = the dispatched main head; every push
   workflow of that SHA green (a required service CI that did not run because its paths did
   not change must have a green latest `main` run); main must not advance while waiting.
2. Images: `cfmeet-core-images.yml` (called) builds — or reuses when the tag already carries
   the revision label — all 13 images for the SHA. The production job pulls each, checks
   `org.opencontainers.image.revision` = SHA, records the image IDs in
   `/opt/cfmeet-core/coordinated/registry-<sha>.json`, transfers only images the VPS does not
   already hold, and re-checks the ID after `docker load`.
3. VPS gates: the production identity containers run; every local image matches the recorded
   ID and label; provider-dependent features are at their safe defaults (below); the
   **resource gate**: `MemAvailable − (new container limits + 832 MiB transient) ≥ 1024 MiB`,
   otherwise it stops before any change and prints the exact shortfall. With the 6.8 GB
   measured by the preflight (≈ 6485 MiB) and all eight new containers:
   6485 − (1536 + 832) ≈ 4117 MiB left, well above the margin.
4. `backup` (above) — the release stops if it fails.
5. `provision` (above).
6. In dependency order, per service: the migration binaries (from the image, `up`, as the
   migrator role; Meeting with its SEC-018 migrator URL when provisioned) **before** the
   swap; then swap (Calling waits for zero active calls), health (`/ready` or `/health` from
   inside the container's network namespace; the worker must stay running without restarts),
   running image = manifest image and revision label = SHA. A failure restores that one
   component and stops the release; services already released stay on the new SHA (owners
   before callers, additive migrations) and `rollback` can restore them. A migration failure
   stops before the component is touched; migrations are never rolled back.
7. The shared scan gateway is upgraded with `scan_gateway.configure` only if it is deployed.
8. `verify`. A verification failure leaves the release in place and fails the run: decide,
   then dispatch `rollback` if needed.

### Safe defaults kept by every release

The release refuses to run (and `verify` fails) unless: SMS Gateway has no provider
(`SMS_GATEWAY_PROVIDER` unset/`none`, so no Africa's Talking / SMS OTP / external sending);
Numbering has no providers and porting off; Telephony, public inbound/outbound, callback
relay and enterprise SIP are off with `TELEPHONY_SIP_EDGE=none` (no PSTN carriers); Privacy
Relay, Contact Vault and the relay-number pool are off; Calling public outbound/inbound are
off; Messaging `PRIVATE_E2EE` devices are off and `MESSAGING_PUSH_PROVIDER` is unset/`none`
(no APNs/FCM/WebPush); Developer Platform auto-approval is off and webhooks stay `shadow`;
Meeting email notifications only with mail credentials present (detected, never printed).
New services start with every flag of runbook §3 at its default (provisioning off, no event
destinations, gateway API and public routing off, webhooks off) and **no grants**: every
grant belongs to an activation step.

## How to dispatch

Prerequisites: the release commit was promoted from `staging` with its full matrix green
(runbook §0 P1) and merged to `main`; `main`'s push CI for that commit is green.

```bash
SHA=$(git rev-parse origin/main)
run() { gh workflow run cfmeet-coordinated-release.yml --ref main -f operation="$1" -f release_sha="$SHA" ${2:+-f confirm="$2"}; }

run inspect                       # read-only; check the resource-gate preview and flags
run backup                        # optional dry run of the backup and restore tests
run release                       # gates → backup → provision → ordered release → verify
run verify                        # any time later (only a SHA with a completed release)
run certify                       # read-only live certification of what is actually deployed

# Activation steps: one owner approval each, in this order, re-running verify between steps.
run activate-provisioning
run activate-org-events
run org-cutover-export            # add: ownerless-accepted  only if the owner decided so
run backup                        # fresh backup incl. Organizations restore test (≤ 24 h)
run org-cutover-import  offsite-backup-verified
run org-cutover-shadow            # then ≥ 7 days of shadow evidence (runbook §5)
run backup
run org-cutover-authoritative offsite-backup-verified,quiesced   # after the §6.2 ingress block
# Empty dataset (ADR 0059 + Amendment 1), instead of export/backup/import/shadow/authoritative above:
run org-cutover-first-activation empty-dataset   # no backup prerequisite (owner decision 2026-11-13)
run activate-enterprise-calling
run activate-developer-platform
run activate-gateway-routing
run activate-first-party-features

run rollback                      # restores the latest release or activation
gh run watch                      # each run uploads its log as an artifact
```

`verify` acts on the dispatched main head, which must be the released SHA. Activations
act on the deployed release: when the head has no completed release of its own, the workflow
proves on the runner (`activation_base.py`) that no build input of any of the 13 images
(Dockerfile `COPY` sources, as `certify` defines them) changed between the latest completed
release and the head, and the VPS still checks that every container runs exactly that
release's recorded image. A head that changed any build input must be released first. `rollback` (and `inspect`) may be dispatched
with any SHA; rollback always restores the latest recorded entry.

## Live-production certification (`certify`)

`verify` certifies one coordinated release: every container must run the image of exactly
the dispatched SHA, and the journal must record a completed release of it. After a release,
`main` usually advances (docs, tooling), and other operations can redeploy single services,
so `verify` on the latest head correctly refuses: *No completed coordinated release of this
SHA is recorded*. `certify` answers the different question "is what is deployed right now
healthy, authentic and current?" without weakening `verify`.

It changes nothing: no container lifecycle or configuration change, no file written on the
host (credentials reach `psql` only through the docker CLI's environment), read-only SQL
sessions, no image pull (`--pull never`; ephemeral `--rm --read-only` psql containers only;
HTTP probes run the host's `curl` inside each service's network namespace via `nsenter -n`,
with names pinned to Docker's own addresses so TLS is verified), no journal entry, no
confirmation accepted. It takes the same production lock as every
production operation, so it never runs during a release.

Per service (all 13 containers, `live_certification.py` on the VPS and
`certification_report.py` on the runner):

| Check | Passes when |
|---|---|
| container | running, not restarting, not OOM-killed; restart count unchanged during the run |
| image | the image's own `org.opencontainers.image.revision` is a SHA, the image is the local `ghcr.io/cosmicforgehealthnet/cfmeet-<svc>:<revision>` tag, source label is this repository |
| revision on main | that revision is an ancestor of the dispatched main head |
| source equals main | none of the image's build inputs (Dockerfile `COPY` sources) changed since that revision |
| readiness | `/ready` or `/health` answers 200 from inside the service's network namespace |
| database | `psql` with the service's own `DATABASE_URL`, from its own namespace, read-only session; database matches the catalogue |
| migrations | history tables present and every migration defined at the running revision applied |
| authentication | a credential-free GET is rejected by the auth layer (401; the gateway answers 404 while public routing is off) |
| service auth | signing key mounted; trust bundle is the production bundle of this receiver; every grant of a completed activation is present with the issuer's current public key |
| service-to-service | every configured `CF_*_BASE_URL` upstream answers its readiness route from the caller's namespace (private TLS verified) |

Platform: provider-dependent features at safe defaults, external integrations reported as
enabled/disabled, Organizations outbox without `DEAD` events, the VPS public smoke. From the
runner (`public_certification.py`, unauthenticated GET/OPTIONS only): `/health`, Swagger and
every OpenAPI document, CORS preflight, TLS certificate ≥ 14 days, every public GET route of
`infra/api-gateway/route-manifest.json` (401/403 or unrouted; activated gateway families must
route; any 5xx or unauthenticated 2xx fails), and every internal/operator/provider-callback GET
route of the source contracts must not answer on the public host.

Verdict: `CERTIFIED_READ_ONLY` only when every applicable check passes; otherwise
`NOT_CERTIFIED` (a check failed) or `INCOMPLETE` (collection did not finish). The run fails
unless certified and uploads `certification.md` / `certification.json`. Authenticated
end-to-end journeys are outside its scope (they write sessions and data) and are always listed
as an outstanding requirement for full production readiness.

## Records on the VPS

| Path | Content |
|---|---|
| `/opt/cfmeet-core/coordinated/journal.json` | every release / activation with the replaced and rollback container names and status |
| `/opt/cfmeet-core/coordinated/registry-<sha>.json` | the workflow-verified image IDs and digests |
| `/opt/cfmeet-core/coordinated/env/*.env` (0600) | runtime environment of the new services |
| `/opt/cfmeet-core/coordinated/trust/` | trust bundle versions (`<svc>.<run>.trust.json`) and `backup/` copies of every superseded bundle |
| `/opt/cfmeet-core/backups/<run>/` (0700) | dumps (0600) and `manifest.json` |
| `/opt/cfmeet-core/coordinated/org-cutover/` | export / import summaries (counts and checksums; the export itself is deleted after import) |

Rollback containers (`<name>-rollback-<run>`) are kept stopped; remove old ones only after
the release they protect is accepted.

## Not automated (owner decisions)

- The owner's age key pair for the off-host copy (backup-and-restore.md). Once the public
  recipient (`vars.CFMEET_BACKUP_AGE_RECIPIENTS`) and the drill identity
  (`secrets.CFMEET_BACKUP_AGE_IDENTITY`) exist, `cfmeet-offsite-backup.yml` encrypts, stores
  off-host, restore-drills on a separate runner and records the result on the VPS daily;
  The standard `org-cutover-import` requires that record **and** the owner's `offsite-backup-verified`;
  the empty-dataset `org-cutover-first-activation` does not (ADR 0059 Amendment 1).
- The shadow evidence (§5) and the quiesce block of the legacy organization write routes (§6.2).
- Per-organization service accounts, credentials and bindings for Enterprise Calling (§4.4).
- Provider-dependent features (SMS, PSTN, push, email without credentials, PRIVATE_E2EE). Their CI
  (`telephony.yml`, `telephony-sip-certification.yml`, `sms-gateway.yml`, `numbering.yml`,
  `privacy-relay.yml`, `communications-e2e.yml`) still runs on every push but is **advisory** in the
  release gate: it is reported as a warning and never blocks a first-party release (owner decision
  2026-11-13). The same holds for `backup-restore-drill.yml` and `cfmeet-offsite-backup.yml`.
- Staging activation (runbook §9) before each production activation step.
## Service log archive (M6)

After the reviewed release is live, dispatch `activate-service-log-collector` with
`confirm=retain-14-days`. It installs the owned `cfmeet-service-logs.service` host unit.
The collector reads only running CF Meet containers from the release catalogue and
known CF Meet infrastructure names; other applications, staging and old release
containers are excluded. Logs stay under `/opt/cfmeet-core/logs/services/`, separated
by container and UTC date, with private file permissions. Fourteen complete UTC days
plus the current day are retained, independently of container replacement.

The cursor is checkpointed only after archived bytes are durable. Restart may repeat
the final timestamp boundary, but does not skip unflushed bytes. The collector reconnects
to the current container name after replacement. Its systemd unit has no network
address family, a read-only host filesystem apart from its archive, no capabilities,
and a 512 MiB memory ceiling. Activation requires 2 GiB free disk and a fresh healthy
collector heartbeat. Rollback restores the prior unit/configuration and preserves logs.

This starts durable collection at activation; it can recover only history still held
by Docker. It cannot reconstruct logs already removed before installation. Inspect
`health.json` for collector failures and monitor host disk usage as part of operations.

## Server audit follow-up: approval and activation order

The owner instructed on 21 November 2026 that this work must stay off `main` until
explicit approval. Keep the review pull request a draft. A feature branch and local
validation do not authorize merging `main` or deploying the new images. Once approval
arrives, retain the normal exact-main-SHA and green-CI release gate.

### Local message block checks (B2)

The independently versioned `messaging-send-policy-projection-migration` must run
before the new Messaging image. It is included in the image, coordinated migrations,
certification harness and backup catalogue. It never alters another owner's database.

After the approved release, dispatch `activate-local-send-policy` twice, first with
`confirm=prepare`, then `confirm=enable`. Preparation starts Messaging's receiver with
the fast path off and a fresh rollout UUID, then configures Communications and Calling
to synchronously acknowledge mutations in that same generation. Enabling refuses
unless all three report protocol 1 and the matching generation. The prepared generation
is consumed; after disabling or a source-image rollback, prepare a new one.

Every warmed direct send checks the local block projection and invalidation sequence.
The first eligibility check and checks after the 30-second eligibility expiry still
ask Communications; pre-existing native/Hub blocks therefore cannot become a cached
allow while a snapshot is missing. Block updates take exclusive principal locks in
Messaging, while direct sends hold shared locks through their transaction. A successful
block ACK therefore waits for an earlier send and a later send sees the local refusal.

Hub updates and native Calling principal/application rules reach that projection before
their public mutation returns success. A failed ACK leaves durable source data/outbox
work and returns unavailable; an idempotent retry must deliver before confirming.
Per-rule versions preserve unblock tombstones; contradictory equal versions are refused.
Rollout UUIDs keep delayed events from an older activation from restoring stale blocks.

Before releasing or rolling back a source image, dispatch this operation with
`confirm=disable`. Source-image release/rollback refuses while the local fast path is
on. Disabling restores fresh direct eligibility checks while preserving synchronous
source ACKs. The normal background relay remains available for retry.

### Meeting notification email cutover (M4)

The current in-app-only path keeps email with the legacy Meeting sender. To move email
as well, dispatch `activate-meeting-in-app-notices` with `confirm=disable`, keeping the
producer fence in SHADOW. Then dispatch `activate-notifications-routing` with
`confirm=meeting-notification-email-active`. The new gate checks that old non-shadow
Meeting publications are settled and their receiving inbox has no PENDING/HELD rows;
otherwise it refuses before changing routing. DEAD producer publications count as a
backlog because a later replay must not turn one into a second email.

Once drained and EMAIL/IN_APP are ACTIVE, dispatch `activate-notification-fence` with
`confirm=meeting-service-meeting-notification-fenced-new`. It repeats the drain check.
The existing per-notice routing decision then makes Notification Service authoritative
and skips the legacy email path for that same notice. Existing OLD decisions retain
their recorded path. Do not replay obsolete in-app-only events after this cutover.
Verify one reminder, cancellation and time change in the bell and exactly one email
for each controlled notice.

### Meeting recording (M7)

Dispatch `activate-meeting-recording` with `confirm=prepare`. This creates the separate
private `cfmeet-meeting-artifacts` bucket on the existing MinIO/KMS, enables default
SSE-S3 encryption, creates a bucket/prefix-scoped service identity and keeps its keys in
root-protected files/recovery material on the VPS. The bucket has a 10 GiB hard quota;
preparation requires 12 GiB free. It installs a signed GET/HEAD-only HTTPS route through
the durable storage relay, verifies unsigned reads fail and a scoped signed round trip
succeeds, and mounts the credentials read-only with recording still disabled. No root
storage credential enters the Meeting container or test client.

Meeting explicitly supplies its own S3 destination to each Egress request. It cannot
silently use the shared worker's Calling bucket. Standard meeting policy retains a
recording for 30 days; stricter meeting/profile rules continue to apply through the
existing artifact retention worker. No bucket lifecycle rule bypasses that authority.
Storage and its signed-read route survive an image rollback so old recording bytes
remain accessible through authorized application access.

Then dispatch `activate-meeting-recording` with `confirm=enable-30-days`. This requires
prepared storage and 2 GiB available RAM and runs the controlled real API/RTC acceptance
inside the activation's rollback boundary. A disposable client publishes generated
audio/video, starts and stops its own meeting recording through the public API, waits
for READY/object verification, checks default retention, downloads through an authorized
signed URL, verifies the digest and playable audio/video, refuses another account and
an unsigned storage request, deletes the recording and confirms byte absence. It ends
and archives its own meeting. Only short-lived controlled user tokens enter that client;
no LiveKit admin or MinIO key is mounted. The token file is removed even after failure.
The protected receipt is under `/opt/cfmeet-core/meeting-recording/acceptance/<run>/`.
Any failure restores the previous recording-disabled container; storage is preserved.

This acceptance is prepared code, not evidence that production recording has passed it.
It must actually pass on the approved release before recording is reported as live.

References: [LiveKit storage outputs](https://docs.livekit.io/transport/media/ingress-egress/egress/outputs/),
[MinIO bucket quotas](https://docs.min.io/aistor/administration/bucket-quotas/).

### Email evidence and deferred external setup

On 21 November, the authenticated Resend console showed SPF and DKIM verified and
successful delivery to a real mailbox. Recent delayed acceptance messages used
`example.invalid`; a delay event reported SMTP `421 4.4.1 Unable to connect to remote
host`. They do not prove that all real recipients are undeliverable. A missing DMARC TXT
record was added at Hostinger as `_dmarc = v=DMARC1; p=none;`, TTL 3600, and read back
from the authoritative nameserver. The Resend adapters now permanently refuse `.invalid`
recipients before a provider request, preventing fresh acceptance fixtures from generating
hours of meaningless provider retries. Recording/test transports remain usable locally.

The owner explicitly chose to leave M8 transcripts and AI notes disabled. No AI key was
created and no provider was enabled. Independent witnessing and native phone push still
need the external operator/keys and actual acceptance listed in
[server-audit-external-setup.md](server-audit-external-setup.md).
