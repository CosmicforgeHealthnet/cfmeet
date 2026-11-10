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

| Operation | Changes production | What it does |
|---|---|---|
| `inspect` | no | MemAvailable, disk, memory use, every CF Meet container (state, image, image revision label (`revision`; the container's own, possibly stale, label is `containerLabelRevision`), limits, networks, ports, env **names**, non-secret flag values), migration histories of every database, mail credential presence (booleans), the release journal and a resource-gate preview. |
| `backup` | files only | `pg_dump -Fc` of every `cfmeet_*` database in `cfmeet-core-db` and of the Meeting database; sha256; `pg_restore --list` must list every migration history; isolated restore test (network-less throwaway PostgreSQL, histories compared row for row) of Meeting, Platform Access and Organizations (once it exists). Manifest: `/opt/cfmeet-core/backups/<run>/manifest.json`. Disk gate: 2 × database size + 5 GiB free. |
| `provision` | yes (idempotent, additive) | For the new services: database + `<svc>_migrator` / `<svc>` roles (the `cfmeet-core-db.yml` SQL), Ed25519 service keys (`/opt/cfmeet-core/secrets/<svc>-service-auth.pem`), SMS Gateway content/fingerprint keys, empty receiver trust bundles, private TLS routes 8450–8455 (`nginx -t`, reload, restore on failure). |
| `release` | yes | See below. |
| `verify` | no | Every container runs the manifest image of the SHA with revision label = SHA and is healthy; migration histories present; provider-dependent features off; public smoke (`/api-docs/swagger-config.json`, CORS preflight from `https://cfmeet.com`, `/internal/*` not public); Organizations outbox counts by state (`DEAD` must be 0); the existing production regression. |
| `certify` | no | Read-only live-production certification of the deployed topology, whatever revisions it runs (see below). `verify` is unchanged and stays strictly same-SHA. |
| `rollback` | yes | Restores the containers of the **latest** journal entry (a release or one activation), newest component first; new services are stopped and renamed `…-rolled-back-…`. Migrations and databases are kept (every migration is additive and the previous images start on the migrated schema). Refuses `org-cutover-authoritative`. Repeat to unwind further. |
| `activate-provisioning` | yes | Grant `platform-identity.provision-first-party` (Platform Access ← Organizations); `ORGANIZATIONS_PROVISIONING_ENABLED=true`. |
| `activate-org-events` | yes | Pre: Meeting has `organization_integration_migrations`, private route 8448. Grant `meeting-organization-events.apply` (Meeting ← Organizations) and `ORGANIZATION_EVENTS_ENABLED=true` on Meeting, then `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` + `CF_MEETING_SERVICE_BASE_URL` on Organizations. Post: no `DEAD` events. |
| `org-cutover-export` | file only | Runbook §1 inventory (counts only, read-only transaction) and §2 export with the Meeting image's `export-organizations`. Refuses rows the import would refuse; ownerless organizations need `confirm=ownerless-accepted`. Export counts must equal the inventory. |
| `org-cutover-import` | Organizations data | `confirm` must contain `offsite-backup-verified` (owner gate 1) and a backup ≤ 24 h with restore tests of Meeting **and** Organizations must exist. Import (`organizations-import`, export streamed on stdin), zero conflicts, reconciliation with zero mismatches and the export checksum, a repeated import fully unchanged; the export file is then deleted. |
| `org-cutover-shadow` | yes | Grant `organization.read/write/act-for-user/administer` (Organizations ← Meeting); `ORGANIZATIONS_AUTHORITY_MODE=shadow` + `CF_ORGANIZATIONS_BASE_URL` on Meeting. Reversible with `rollback`. |
| `org-cutover-authoritative` | yes, **irreversible** | Owner gates 1 and 6: `confirm=offsite-backup-verified,quiesced` after the legacy write routes are blocked at the ingress (runbook §6.2, manual) and the 7-day zero-mismatch criterion (§5) is accepted. Final export → import → reconciliation, then `ORGANIZATIONS_AUTHORITY_MODE=authoritative`. Never rolled back (runbook §7). |
| `activate-enterprise-calling` | yes | Requires authoritative. Grants `platform-principal.ensure` (Platform Access), `calling-request.act-for-organization` (Calling), `messaging-application.delegate` (Messaging), all ← Meeting; `CALLING_ORGANIZATION_FORWARDING_ENABLED=true`; `ORGANIZATION_CALLING_SCOPE=organization` (+ Calling/Messaging URLs if absent) on Meeting. The per-organization service bindings (runbook §4.4) remain operator API steps. |
| `activate-developer-platform` | yes | Grants (checklist §4): Platform Access ← API Gateway `platform-credential.authenticate`; Platform Access ← Developer Platform `platform-identity.read`, `platform-identity.delegate`; Developer Platform ← API Gateway `developer-request.forward`, `developer-entitlement.read`, `developer-usage.record`. `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED=true`; gateway `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true` + `CF_DEVELOPER_PLATFORM_BASE_URL`. Still not public. |
| `activate-gateway-routing` | yes (public) | Requires the previous step. A loopback-only relay (`cfmeet-gateway-ingress`, 127.0.0.1:18110 → `cfmeet-api-gateway:8099`, same confinement as the storage relay) and `/etc/nginx/snippets/cfmeet-api-gateway.conf`, included after the Calling routes, routing **only** the Developer Platform families (`/v1/developer-platform`, `/v1/organizations`, `/v1/projects`, `/v1/environments`, `/v1/scopes`, `/v1/invitations`, `/v1/production-access-requests`, `/v1/commercial-plans`). Refuses if any existing location already serves one of them. Meeting, Calling and Messaging keep their direct routes; `/internal`, operator and provider-callback paths never reach the gateway. `nginx -t`, reload, then: gateway answers 401 unauthenticated, CORS for `https://cfmeet.com`, public smoke, regression; any failure removes the include, snippet and relay. `rollback` undoes it the same way. |
| `activate-first-party-features` | yes | No prerequisites. Meeting: `FEATURE_CONFERENCE_REALTIME_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_QUALITY_TELEMETRY_ENABLED` = `true`, and `NOTIFICATIONS_EMAIL_ENABLED=true` **only** when the Meeting Service already carries SMTP host + credentials. Messaging: `MESSAGING_SESSION_DEVICES_ENABLED=true` (MANAGED realtime; refuses if PRIVATE_E2EE devices are on). The healthcare profile is never switched (compliance decision). Reversible with `rollback`. |

Activation order is enforced: provisioning → org events → export → import → shadow →
authoritative → enterprise calling; developer platform → gateway routing. Every activation
also requires that the completed release of the dispatched SHA is what is running. Each
activation recreates only the affected containers (same image, preserved configuration,
changed flags and/or a **new** trust-bundle file version); receivers get their grants before
callers switch on. A failed health check restores every container the step changed.

### Release

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
run activate-enterprise-calling
run activate-developer-platform
run activate-gateway-routing
run activate-first-party-features

run rollback                      # restores the latest release or activation
gh run watch                      # each run uploads its log as an artifact
```

Activation and verify runs act on the dispatched main head, which must be the released SHA:
if `main` moved, release the new head first. `rollback` (and `inspect`) may be dispatched
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
sessions, no image pull (`--pull never`; ephemeral `--rm --read-only` probe containers only),
no journal entry, no confirmation accepted. It takes the same production lock as every
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

- Off-host, encrypted backup copy and its restore drill (backup-and-restore.md): the release
  backup is a same-host 0600 copy; the cutover steps require the owner's explicit
  `offsite-backup-verified` confirmation.
- The shadow evidence (§5) and the quiesce block of the legacy organization write routes (§6.2).
- Per-organization service accounts, credentials and bindings for Enterprise Calling (§4.4).
- Provider-dependent features (SMS, PSTN, push, email without credentials, PRIVATE_E2EE).
- Staging activation (runbook §9) before each production activation step.
