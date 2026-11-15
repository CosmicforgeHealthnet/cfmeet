# 05 — Developer Platform, API Gateway, Webhooks, Scan Gateway, Infrastructure, Production Evidence

Auditor: read-only inventory for the centralized "Notification Service" design.
Repo: `/home/user/cfmeet` at `6f3743f` (2026-11-15, "Messaging CI: authorization-matrix ... push configuration").
Method: static reading of code/config/docs + unauthenticated GET probes of public health/doc paths on `https://api.cfmeet.com` (2026-11-15 ~09:07 UTC). No repo files modified, no state-changing git, no secrets copied.

Classification vocabulary used below:
`implemented` (code exists + tests) · `configured` (deployment config/flags exist) · `activated` (turned on in production per recorded run) · `production-tested` (a recorded production run exercised it) · `disabled` (shipped off / safe default off) · `broken` · `deprecated` · `absent`.

---

## 0. Executive findings (most decision-relevant first)

1. **Two distinct "notification" systems exist and must not be conflated:**
   - **Customer (machine) webhooks** = Developer Platform Section 53 central webhook platform (`services/developer-platform/src/webhooks/*`, tables `devplat_webhook_*`, primitives in `packages/webhooks`). Status: **implemented, activated (mode `active`), production-tested** (signed delivery SUCCEEDED HTTP 204 in developer-acceptance runs 43184898911, 43337892242, 43488534526).
   - **Human-facing developer notifications** (invitations, credential lifecycle, production-access decisions, security alerts): the Developer Platform itself **sends no email, push or in-app notification**. Invitation tokens are returned to the inviting caller only. Credential-lifecycle notices are emitted **only as webhooks** (`credential.expiring_soon` / `credential.expired`). Production-access approve/reject emails are sent by an **ops script** (`infra/calling/production/developer_access.py::notify_applicant`) calling the Meeting Service's internal email endpoint — only when the decision is made through the coordinated-release workflow, and that workflow is **not activated** (`DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false`, no reviewers named). Security alerts to developers: **absent**.
2. **The only outbound email transport in the platform is the Meeting Service** (`email_deliveries` ledger, SMTP today; Resend adapter implemented but **not activated**). Push (APNs/FCM/Web Push) lives in Messaging and is **not configured in production** (`MESSAGING_PUSH_PROVIDER=none`); an `activate-web-push` operation was added 2026-11-15 (commit `42b509c`) with **no recorded run**.
3. **Operator alerting is effectively absent in production:** Prometheus rules + Alertmanager routing exist as code and are CI-validated, but **nothing is deployed and every receiver is empty** (`infra/monitoring/alertmanager/alertmanager.yml:3-14`). The only working operator notification is the TLS certificate monitor posting comments to GitHub issue #38. No alert rule covers Developer Platform webhook dead letters (the DP exports no metrics at all).
4. **Production is a single shared Contabo VPS** (6 vCPU, 11.7 GiB RAM, 4 GiB swap measured 2026-11-05) that also hosts other CosmicForge products; CF Meet uses <1 GiB real memory; ~4.7–5.5 GiB MemAvailable is the recurring measured headroom. New services get `--memory` limits (192/256/128 MiB) via `coordinated_release.py`; `docker-compose.prod.yml` is a **legacy/stale** template with **no memory/CPU limits** and does not describe the real topology.
5. **Adding a new service** touches ~15 registries (service-auth identity + permissions, contracts manifest, public-routes allow-list → generated route manifest, gateway `CUSTOMER_UPSTREAMS` + `TARGETS`, nginx family prefixes in `coordinated_release.py`, `SERVICES/ORDER/DEPENDS/IMAGES`, core-images matrix, release gate `required` list, DB provisioning case list, backup catalog, operations registry, boundary checker). Details in §7.

---

## 1. Developer Platform

### 1.1 Service shape

| Item | Evidence | Status |
|---|---|---|
| Binaries `developer-platform` (API), `developer-platform-worker`, `developer-platform-operator`, `developer-platform-reconcile-references`, `developer-platform-migration` | `services/developer-platform/Cargo.toml:16-31`; `Dockerfile:37-41` | implemented |
| Own DB `cfmeet_developer_platform`, `devplat_` tables, history `developer_platform_seaql_migrations` | `migration/src/lib.rs:1-29`; `infra/calling/production/coordinated_release.py:128-133` | implemented, activated |
| Probes `GET /health`, `GET /ready`, `GET /openapi.json` | `src/http/mod.rs:38-40` | implemented (private only; not public) |
| No Prometheus metrics endpoint | `grep metrics|prometheus src/` → none | absent |
| Production container `cfmeet-developer-platform` (256 MiB) + `cfmeet-developer-platform-worker` (128 MiB, instance `worker-production-1`) | `coordinated_release.py:128-142`; `docs/operations/coordinated-production-release.md:33` | activated |
| Stale docs claim "not deployed" | `services/developer-platform/README.md:4` ("functional Developer Platform (Sections 50+) is not implemented"), `Dockerfile:15` ("Not deployed by any workflow yet"), ADR 0055 status line | **stale documentation** (contradicted by §5 evidence) |

### 1.2 Customer webhooks (machine-to-machine) — Section 53

**Pipeline** (`src/webhooks/mod.rs:1-21`):
producer outbox → `POST /internal/v1/platform/events` (`developer-event.publish`; envelope producer must equal authenticated service) → `catalog::project` (allow-listed safe keys only) → `devplat_webhook_events` (dedupe on `(producer, source_event_id)`) → one delivery per ACTIVE matching subscription in the same environment → worker (SKIP LOCKED + lease) → SSRF-safe signed POST → SUCCEEDED | RETRY_SCHEDULED | FAILED (dead letter); replay = new delivery; shadow mode records SHADOW.

#### Endpoint registration (routes)

`src/webhooks/http.rs:66-107`

| Method | Path | Caller | Notes |
|---|---|---|---|
| POST/GET | `/v1/webhooks` | platform credential (Calling-compatible contract) | re-pointed from Calling by gateway `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` |
| GET/PATCH/DELETE | `/v1/webhooks/:webhook_id` | platform credential | soft delete only |
| POST | `/v1/webhooks/:webhook_id/rotate-secret` | platform credential | secret shown once |
| GET | `/v1/webhooks/:webhook_id/deliveries` | platform credential | |
| POST | `/v1/webhook-deliveries/:delivery_id/replay` | platform credential | |
| GET/POST | `/v1/environments/:environment_id/webhooks` | DEVELOPER_SESSION (console) | |
| GET/PATCH/DELETE | `/v1/environments/:environment_id/webhooks/:webhook_id` | console | |
| POST | `/v1/environments/:environment_id/webhooks/:webhook_id/rotate-secret` | console | |
| GET | `/v1/environments/:environment_id/webhooks/:webhook_id/deliveries` | console | |
| GET | `/v1/environments/:environment_id/webhook-deliveries/:delivery_id/attempts` | console | |
| POST | `/v1/environments/:environment_id/webhook-deliveries/:delivery_id/replay` | console | |
| POST | `/internal/v1/platform/events` | service token, `developer-event.publish` | ingestion; max envelope 16 KiB (`ingest.rs:29`) |

Operator: `/operator/webhook-migration/status`, `/operator/webhook-migration/inventory` (`src/webhooks/migration/http.rs:50-51`).
Subscribing requires `webhooks.manage` + the event family's domain read scope (`catalog.rs:69-73`, `mod.rs::domain_scopes`).

#### Signing scheme (`packages/webhooks/src/signing.rs:1-55`; ADR 0028 §68 lines 95-103)

```
CF-Webhook-Id:        <delivery id>
CF-Webhook-Event-Id:  <event id>   (stable across retries/replays: dedupe key)
CF-Webhook-Timestamp: <unix seconds>
CF-Webhook-Signature: v1=<hex HMAC-SHA256(secret, "v1:" + ts + ":" + delivery id + ":" + body)>
```
Receiver tolerance default 300 s (`DEFAULT_TOLERANCE_SECONDS`), secret prefix `whsec_` + 256-bit (`generate_secret`). Reference verifier `cf_webhooks::signing::verify`. SDK vectors test: `packages/webhooks/tests/sdk_vectors.rs`. Status: implemented, production-tested.

#### Secrets at rest (`packages/webhooks/src/secret_box.rs:1-27`)
AES-256-GCM (`ring`), stored `v2.<kid>.<b64(nonce||ct||tag)>`, AAD = kid + owner (subscription id); legacy `v1.` readable; current + previous key ring; weak keys refused. DB CHECK forbids raw `whsec_` (`m20261104_000004_webhooks.rs:63-67`). Production key ring `prod-1`, file `/opt/cfmeet-core/secrets/developer-platform-webhook-secret.key` (0400, uid 10001) (`coordinated-production-release.md:74`; `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:73`).

#### SSRF policy (`packages/webhooks/src/endpoint.rs:1-22, 82, 125, 181-257`)
https only (http only when `*_ALLOW_PRIVATE_ENDPOINTS=true` AND `CF_RUNTIME_ENVIRONMENT=local`), no credentials/fragment, port 443 or ≥1024, no local/internal/metadata names, prohibited-IP filter enforced in `GuardedResolver` at connect time (DNS-rebinding safe), no redirects, no proxy. In production, only the worker is attached to egress bridge `cfmeet-webhook-egress`; `cfmeet-core-private` is `--internal` (`coordinated-production-release.md:74`).

#### Delivery tables (migration `services/developer-platform/migration/src/m20261104_000004_webhooks.rs`, extended by `m20261105_000005_webhook_migration.rs`)

**`devplat_webhook_subscriptions`** (lines 30-75)
| Column | Type / constraint |
|---|---|
| id | UUID PK |
| tenant_id, application_id, environment_id | UUID NOT NULL (Platform Access chain, never client-supplied) |
| environment_kind | VARCHAR(16) CHECK IN (SANDBOX, TEST, PRODUCTION) |
| url | TEXT, CHECK ≤2048 bytes, regex `^https?://[^@/\s]+(/[^\s]*)?$` |
| description | VARCHAR(200) |
| event_types | TEXT[] 1..64, no NULLs |
| status | CHECK IN (ACTIVE, DISABLED, DELETED); DELETED ⇔ deleted_at NOT NULL |
| secret_sealed, secret_key_id | sealed only; NULL allowed only for `secret_state='ROTATION_REQUIRED'` (migrated, never ACTIVE) (`m20261105...:36-55`) |
| secret_version, version | ≥1, never decrease (trigger) |
| consecutive_failures, last_success_at, last_failure_at | endpoint health |
| disabled_reason | upper-snake, only when status ≠ ACTIVE |
| origin | NATIVE / MIGRATED_FROM_CALLING |
| secret_state (added 000005) | VALID / ROTATION_REQUIRED |
| created_by, created_at, updated_at, deleted_at | |
Unique `(tenant_id, application_id, environment_id, id)`; index `(environment_id, status, created_at, id)`; trigger `devplat_webhook_subscriptions_guard` (scope immutable, DELETED terminal); no-delete trigger.

**`devplat_webhook_events`** (lines 97-126) — public normalized events, append-only (update/delete/truncate refused).
`event_id` `whe_[0-9a-f]{32}` PK; `producer` CHECK IN (meeting-service, calling, privacy-relay, messaging, numbering, sms-gateway, developer-platform); `source_event_id`; `event_type` regex; scope ids; `occurred_at`; `payload` JSONB (must contain `id`, `type`, object `data`, ≤16384 bytes); `received_at`; `origin` (000005). Dedupe unique per `(producer, source_event_id)` — per origin after 000005.

**`devplat_webhook_deliveries`** (lines 128-200)
`id`, `subscription_id`, `event_id`, scope ids (composite FKs force same environment for subscription and event), `replay_of` (self-FK), `status` CHECK IN (PENDING, DELIVERING, RETRY_SCHEDULED, SUCCEEDED, FAILED, CANCELLED, SHADOW), `attempt_count` 0..64, `next_attempt_at`, `lease_until` (non-NULL ⇔ DELIVERING), `last_http_status`, `failure_category`, `created_at`, `updated_at`, `completed_at` (⇔ terminal), plus `origin`, `migration_hold` (000005). Unique `(subscription_id, event_id) WHERE replay_of IS NULL`; partial index on due rows; state-machine trigger whitelists transitions (lines 187-191); never deleted.

**`devplat_webhook_attempts`** (lines 202-221) — append-only: `(delivery_id, attempt_number)` PK, started/finished, `http_status`, `outcome` (SUCCEEDED/RETRY/FAILED), `error_class`. Never stores response bodies.

Migration-support tables (000005): `devplat_webhook_migration_runs`, `_subscriptions`, `_state` (single row PREPARING/CUT_OVER/ROLLED_BACK), `_audit`.

#### Retry / backoff / dead letter
- `packages/webhooks/src/retry.rs:11-14`: `MAX_ATTEMPTS = 8`, base 30 s, cap 6 h; `backoff()` exponential with upper-half jitter (`:50-55`); honours `Retry-After` only when longer (`:73-79`).
- Classification (`:26-46`): 2xx delivered; 408/429/5xx/timeout/connection retried; 3xx, other 4xx, SSRF block permanent.
- DP settings defaults (`src/webhooks/mod.rs:42-90`): mode `Shadow`, timeout 10 s, `max_attempts` 8, `auto_disable_after` 50. Env: `DEVELOPER_PLATFORM_WEBHOOK_MAX_ATTEMPTS` (1–20), `_AUTO_DISABLE_AFTER` (5–10000), `_REQUEST_TIMEOUT_MS` (500–30000) (`src/config/webhooks.rs:5-17`).
- Worker (`src/webhooks/worker.rs`): `claim_due` (`:83`), `dead_letter_expired_at_cap` (`:108`), `shadow_due` (`:130`), `deliver_due_once` (`:149`), auto-disable on consecutive failures sets `status='DISABLED'`, `disabled_reason='CONSECUTIVE_FAILURES'` (`:502-519`). FAILED = dead letter.
- Dead-letter visibility: customer/console delivery lists + attempts only. The operator ingestion-health view (`src/ledger/mod.rs:~800-830`) reports usage/event **rejections**, not webhook dead letters. **No metric, no alert, no operator notification** for webhook dead letters → absent.

#### Replay
`WebhookService::replay` (`src/webhooks/mod.rs:822-880`): new delivery row with `replay_of`, same event id, to an ACTIVE subscription; audit action `webhook.delivery_replayed`; recorded SHADOW in shadow mode. Status: implemented, exposed publicly.

#### Event catalog (`src/webhooks/catalog.rs`)
72 public event types (`:84-164`), each with required scope. Producers and allowed families (`:29-52`):

| Producer | Families |
|---|---|
| `cfmeet.meeting-service` | meeting, participant, recording, artifact |
| `cfmeet.calling` | call, recording, voicemail, usage, limit |
| `cfmeet.privacy-relay` | relay |
| `cfmeet.messaging` | message, conversation |
| `cfmeet.numbering` | number |
| `cfmeet.sms-gateway` | sms |
| `cfmeet.developer-platform` | credential, production_access, usage, limit |

Families: `call.*` (21 types), `voicemail.*` (3), `recording.*` (2), `relay.*` (5), `usage.recorded`, `limit.warning`, `limit.exceeded`, `sms.*` (7), `conversation.*` (4), `message.*` (4), `meeting.created/started/ended`, `participant.joined/left`, `artifact.ready/deleted`, `number.*` (8), `credential.created/rotated/revoked/expiring_soon/expired`, `production_access.requested/approved/rejected`.
Projection keeps only UUIDs, upper-case codes and whitelisted integers (`catalog.rs:1-12, 225-300`).

**Gap:** `credential.created/rotated/revoked` and `production_access.*` are in the catalog and have projections (`project_developer_platform`, `catalog.rs:561-582`), but **no code publishes** `developer-platform.credential.created|rotated|revoked` or `developer-platform.production-access.*` envelopes: `control/credentials.rs:218-241` and `control/production.rs:304-341` write only activity rows (`devplat_activity_events`). Only `credential_lifecycle.rs:406-418` calls `ingest_public_event`. → catalog entries **implemented-in-catalog but never emitted (dead)**.

#### Flags & production state
| Flag | Default | Production |
|---|---|---|
| `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` | false | true (run 43173688215) |
| `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE` | shadow | active (run 43173688215) |
| `DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN` | false | — |
| Calling `CALLING_WEBHOOK_AUTHORITY` | — | `retired`, `CALLING_WEBHOOK_DELIVERY_ENABLED=false` |
| Producers `*_PUBLIC_EVENTS_DESTINATION` | none | `developer-platform` for Calling, Messaging, Meeting |
| Gateway `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` | false | true (run 43174233820; live `/discovery` reports `"centralWebhooks":true`) |

Sources: `docs/operations/coordinated-production-release.md:74-75`; `docs/operations/CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:52-53,73`.

Classification: **implemented, configured, activated, production-tested.** Legacy Calling webhook system (`services/calling/src/webhooks/*`) still in code: **disabled/retired** in production; operations registry still lists it as `webhook-delivery-worker` owner `calling` (`scripts/service-boundaries/operations_registry.json`).

### 1.3 Credential lifecycle notices (Blueprint §15, PLT-008)

- `src/control/credential_lifecycle.rs`: `sweep_once` (`:262`), `notify` (`:367-440`) → builds `developer-platform.credential.expiring-soon|expired` envelope → `catalog::project` → `ingest_public_event` → inserts `devplat_credential_lifecycle_notices` (`ON CONFLICT DO NOTHING`).
- Table `devplat_credential_lifecycle_notices` (`m20261203_000014_credential_lifecycle_notices.rs:22-52`): PK `(credential_id, event_type, threshold_days, expires_at)`, `event_type` IN (credential.expiring_soon, credential.expired), threshold 0 for expired / 1..365, `source_event_id` `evt_…`, `webhook_event_id` `whe_…`, `deliveries`, `notified_at`; append-only. `devplat_worker_schedules` (`:54-63`) gives cross-instance at-most-once per interval.
- Defaults: interval 3600 s, warning days `14,7,1`, expired lookback 7 d, `DEVELOPER_PLATFORM_CREDENTIAL_LIFECYCLE_ENABLED` on unless set false (`README.md:33-44`).
- **Channel: webhook only.** No email to the credential owner. Production-tested: developer-acceptance subscribes to `credential.expiring_soon`/`credential.expired` and gets a real delivery (`infra/calling/production/developer_acceptance.py:15,35,178-185`; runs 43184898911, 43337892242, 43488534526).
- Classification: implemented, activated (as webhook), production-tested; **human channel absent**.

### 1.4 Human-facing developer notifications (keep distinct from webhooks)

| Notification | Mechanism found | Channel | Status | Evidence |
|---|---|---|---|---|
| Workspace invitation | `POST /v1/organizations/{id}/invitations` returns a one-time token **to the inviter** in the response; only SHA-256 digest stored; acceptance requires verified email match | none (out-of-band: inviter must share token) | invitation implemented; **notification absent** | `src/control/members.rs:8-10, 324-400` (`invite`, `v["token"] = token`), table `devplat_invitations` |
| Invitation accepted/revoked/expired | activity events only (`invitation.created`, etc.) | in DB activity log | no notification | `members.rs:386-397`, `control/activity.rs` |
| Credential created / rotated / revoked | activity event `credential.created` etc. | DB only | no notification; webhook types exist but never emitted | `control/credentials.rs:218-241, 306-314, 478` |
| Credential expiring / expired | webhook (`credential.expiring_soon` / `.expired`) | customer webhook | activated, production-tested | §1.3 |
| Production access requested | activity `production_access.requested` | DB only | no notification to operators | `control/production.rs:455` |
| Production access approved / rejected | DP: activity only (`:487, :1009, :1035`). Email: **ops script** `infra/calling/production/developer_access.py:150-165` `notify_applicant` → `POST http://127.0.0.1:18081/internal/operations/email/developer-access-decision` (X-Internal-Token) on the Meeting Service → fixed-template email via `email_service.enqueue_template(category::DEVELOPER_PLATFORM, idempotency "devplat-access:<req>:<decision>")` (`services/meeting-service/src/handlers/email.rs:11-14, 194-260`; route `src/routes/mod.rs:105`) | email (Meeting Service email ledger; SMTP or Resend) | **implemented, not activated**: called only from `developer-access-review` workflow op; `activate-production-access-review` never run; `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED=false` | `docs/operations/coordinated-production-release.md:82-83`; `CFMEET_FINAL_PRODUCTION_COMPLETION.md:17`; `CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:82,269` |
| Decisions made via DP operator HTTP API directly (not via the workflow op) | no email path | none | gap | `src/http/operator.rs` (no email call) |
| Suspension / reinstatement, approval expiry | activity only | none | absent | `production.rs:823, 1128-1130` |
| Security alerts (leaked credential, anomalous use, auth failures) | none found | none | **absent** | grep `security|anomal|leak` in `src/` → only report-only reference reconciliation (`src/reference_reconciliation.rs`) which logs |
| Quota/limit warnings | `limit.warning`/`limit.exceeded` webhook types (Calling producer; DP producer mapping exists) | webhook | Calling-originated; DP-originated not emitted | `catalog.rs:117-119, 573-575` |

### 1.5 Legacy meeting webhooks (meeting-service) — deprecated

- Routes `GET/POST /api/v1/webhooks`, `PATCH/DELETE /api/v1/webhooks/{id}` (`services/meeting-service/src/handlers/webhooks.rs:120,195,247,320`; mounted `src/routes/mod.rs:83`), table created by `migration/src/m20260116_000007_create_webhooks.rs`.
- **Never delivered by any code, ever** (`docs/api/legacy-meeting-webhooks-deprecation.md:12-16`). Responses carry `delivery: NOT_DELIVERED` (`handlers/webhooks.rs:40,173`) and RFC 9745/8594 headers `Deprecation: @1794009600`, `Sunset: Sat, 01 May 2027 00:00:00 GMT` (`src/middleware/deprecation.rs:19-22`).
- Gateway classifies them `EXCLUDED / LEGACY_DIRECT_INGRESS` (public-routes.json); served only on the direct Meeting ingress.
- Migration path → DP environment webhooks; legacy `meeting_start/end`, `participant_join/leave` map to `meeting-service.meeting.*`/`participant.*` envelopes (deprecation doc `:60-66`). Meeting public events only for spaces owned by a Platform Access application environment (`services/meeting-service/src/services/platform/events.rs`; destination flag `MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION`, default `none`, `:36-42, 85`).
- Sunset preconditions (doc §5, `:76-83`): frontend notice OPEN; DP reachable OPEN (stale — now activated); measured zero use OPEN; drop table NOT STARTED. Contract `cfmeet.meeting-service.http-api` 1.4.0 says "legacy webhook secrets never stored" (packages/contracts/manifest.json), whereas the doc still lists dropping a plaintext `secret` column as precondition 4 — minor doc drift.
- Classification: **deprecated** (stored, never delivered).

---

## 2. API Gateway

### 2.1 How routing and auth work
- `services/api-gateway/src/routing.rs:1-41`: default-deny table from `infra/api-gateway/route-manifest.json`, embedded at compile time (`src/catalogue.rs:16` `include_str!`). `CUSTOMER_UPSTREAMS` = developer-platform, calling, messaging, numbering, sms-gateway, meeting-service. Forbidden segments: internal, admin, operator, provider-callbacks, callbacks, metrics. Public patterns only under `/v1/` and `/api/v2/`.
- Auth modes (`routing.rs:50-88`): PLATFORM_CREDENTIAL (verified at Platform Access, `platform-credential.authenticate`), DEVELOPER_SESSION (DP verifies via Meeting session bridge), CLIENT_TOKEN, REALTIME_TICKET, SIGNALING_CREDENTIAL, MEETING_SESSION, PARTICIPANT_TOKEN, ANONYMOUS.
- Forwarding: fresh 60 s Ed25519 service token with `DelegatedPlatformContext`, one audience; client authority headers dropped (`services/api-gateway/README.md:6-22`; ADR 0055 §3-4).
- Central re-pointing: when `central_webhooks` and route is calling `/v1/webhooks*` or `/v1/webhook-deliveries*`, upstream switched to DP (`catalogue.rs:99-103`; `routing.rs:441-445`). Same for usage (`central_usage`, live `false`).
- Quotas: Redis-shared when `API_GATEWAY_REDIS_URL` set; optional per-token ceiling (`README.md:24-34`).
- Upstream base URLs (`src/config/service_auth.rs:111-122`): `CF_PLATFORM_ACCESS_BASE_URL`, `CF_DEVELOPER_PLATFORM_BASE_URL`, `CF_CALLING_BASE_URL`, `CF_MESSAGING_BASE_URL`, `CF_NUMBERING_BASE_URL`, `CF_SMS_GATEWAY_BASE_URL`, `CF_MEETING_SERVICE_BASE_URL` (https required outside local/test).
- Other env flags: `API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `_WEBSOCKETS_ENABLED`, `_CENTRAL_WEBHOOKS_ENABLED`, `_CENTRAL_USAGE_ENABLED`, `_DISABLED_DOMAINS`, `_CORS_ALLOWED_ORIGINS`, `_MAX_BODY_BYTES`, `_UPSTREAM_TIMEOUT_MS`, `_TOKEN_RATE_LIMIT_PER_MINUTE`, `_REDIS_*` (`src/config/mod.rs`).

Manifest stats (`route-manifest.json`): 578 PUBLIC_CUSTOMER routes (meeting-service 207, calling 142, messaging 97, developer-platform 74, numbering 52, sms-gateway 6); classes EXCLUDED 136, INTERNAL_SERVICE 33, OPERATOR_ADMIN 38, PROVIDER_CALLBACK 4, PUBLIC_INFRASTRUCTURE 17; 3 WebSocket routes.

### 2.2 Notification-related routes in the manifest (grep: notification|push|preference|device|email|webhook|inbox|reminder)

PUBLIC_CUSTOMER (routable by gateway when its domain is enabled):

| Method | Pattern | Upstream | Auth | Route id |
|---|---|---|---|---|
| GET/PUT | `/api/v2/communications/me/notification-preferences` | cfmeet.meeting-service | MEETING_SESSION | communications.getNotificationPreferences / updateNotificationPreferences (manifest lines ~745-761) |
| GET | `/api/v2/communications/me/notification-delivery-status` | meeting-service | MEETING_SESSION | communications.getNotificationDeliveryStatus |
| GET/PUT | `/api/v2/communications/conversations/{conversationId}/preferences` | meeting-service | MEETING_SESSION | communications.get/updateConversationPreferences |
| GET | `/api/v2/communications/inbox` | meeting-service | MEETING_SESSION | communications.getCommunicationsInboxOverlay |
| GET | `/api/v2/communications/devices` | meeting-service | MEETING_SESSION | listCommunicationsDevices |
| POST | `/api/v2/communications/devices/link` | meeting-service | MEETING_SESSION | linkCommunicationsDevice |
| PUT/DELETE | `/api/v2/communications/devices/{deviceId}/push` | meeting-service | MEETING_SESSION | register/removeCommunicationsDevicePush |
| POST | `/api/v2/communications/devices/{deviceId}/revoke` | meeting-service | MEETING_SESSION | revokeCommunicationsDevice |
| GET/PUT | `/api/v2/communications/devices/{deviceId}/sync-state` | meeting-service | MEETING_SESSION | get/acknowledgeCommunicationsDeviceSync |
| GET | `/v1/messaging/push/configuration` | cfmeet.messaging | PLATFORM_CREDENTIAL | messaging.getPushConfiguration (line ~6712) |
| PUT/DELETE | `/v1/messaging/devices/{deviceId}/push-registration` | messaging | PLATFORM_CREDENTIAL, CLIENT_TOKEN | messaging.registerPush / unregisterPush |
| POST | `/v1/messaging/devices`, `/v1/messaging/devices/registration-challenges`, `/{deviceId}/key-packages`, `/{deviceId}/revoke`; GET `/{deviceId}/welcomes`, POST `/welcomes/{welcomeId}/consume`; GET `/v1/messaging/principals/{principalId}/devices` | messaging | PLATFORM_CREDENTIAL (some +CLIENT_TOKEN) | messaging.* device ops |
| GET/POST/GET/PATCH/DELETE | `/v1/webhooks`, `/v1/webhooks/{webhook_id}`; POST `/rotate-secret`; GET `/deliveries`; POST `/v1/webhook-deliveries/{delivery_id}/replay` | manifest upstream cfmeet.calling → **re-pointed to cfmeet.developer-platform** at runtime (central webhooks on) | PLATFORM_CREDENTIAL | calling.*Webhook* |
| GET/POST/GET/PATCH/DELETE | `/v1/environments/{environmentId}/webhooks[/{webhookId}]`; POST `/rotate-secret`; GET `/deliveries`; GET `/webhook-deliveries/{deliveryId}/attempts`; POST `/replay` | cfmeet.developer-platform | DEVELOPER_SESSION | developer-platform.*EnvironmentWebhook* (line ~5849) |
| (non-notification matches) | `/v1/enterprise/sip-devices*`, `/v1/enterprise/voicemail/*` | calling | PLATFORM_CREDENTIAL | keyword hits only |

EXCLUDED (not gateway-routed, reason in `public-routes.json`): `/api/v1/webhooks*` (LEGACY_DIRECT_INGRESS), `/api/v1/auth/verify-email` (LEGACY_DIRECT_INGRESS), `/api/v2/me/devices*`, `/api/v2/me/device-links*` incl. `/email-code` (FIRST_PARTY_APP_ACCOUNT), `/api/v2/messaging/devices*` (FIRST_PARTY_APP_BACKEND), `/v1/messaging/session-devices*` (FIRST_PARTY_SESSION_DEVICES).
INTERNAL_SERVICE (never public): Calling `/internal/v1/webhook-migration/*` (8 ops).
**No route** named reminder, email-send, notification-inbox, or a generic notifications resource exists. No `/v1/notifications` family.

**Edge reality vs manifest:** in production nginx sends to the gateway only the DP families (`/v1/commercial-plans`, `/v1/developer-platform`, `/v1/environments`, `/v1/invitations`, `/v1/organizations`, `/v1/production-access-requests`, `/v1/projects`, `/v1/scopes`) plus `FAMILY_PREFIXES` (`/v1/abuse-reports`, `/v1/blocks`, `/v1/conversations`, `/v1/limits`, `/v1/messaging`, `/v1/usage`, `/v1/webhook-deliveries`, `/v1/webhooks`) and docs paths (`coordinated_release.py:2047-2048, 2142-2177`). `/api/v2/communications/*` and `/api/v1|v2/*` meeting routes stay on the **direct Meeting Service route** (host nginx → 127.0.0.1:18081). WebSockets off. Disabled domains `numbering,sms,privacy-relay`.

### 2.3 How a new service is added to the gateway and nginx (evidence-based checklist)
1. Service identity in `packages/service-auth/src/identity.rs` (`services::*`, e.g. `:10-46`).
2. Forward permission `<svc>-request.forward` in `packages/service-auth/src/permissions.rs` (pattern `:254-280` enum, `:770-810` definitions: receiver = new service, `grantable_to: [API_GATEWAY]`); registry contract `cfmeet.platform.common` bump.
3. Contract(s) in `packages/contracts/<svc>/…` registered in `packages/contracts/manifest.json`.
4. Add to `SOURCES` + `PUBLIC_DOMAINS` + `CUSTOMER_UPSTREAMS` in `scripts/contracts/public_api.py:56-75, 466`; allow-list every operation in `infra/api-gateway/public-routes.json` (PUBLIC_CUSTOMER or EXCLUDED+reason) → regenerate `route-manifest.json` (CI `--check` in `contracts.yml:101`, `api-gateway.yml:79`); `scripts/contracts/check_gateway_upstreams.py` proves each upstream handler exists.
5. Add to `CUSTOMER_UPSTREAMS` (`services/api-gateway/src/routing.rs:25-32`) and `TARGETS` `CF_<SVC>_BASE_URL` (`src/config/service_auth.rs:111-122`).
6. Production: receiver trust-bundle grant, gateway env URL (https via private TLS proxy `cfmeet-core-tls`), and extend nginx gateway snippet prefixes (`GATEWAY_PREFIXES`/`FAMILY_PREFIXES` in `infra/calling/production/coordinated_release.py`; snippet `/etc/nginx/snippets/cfmeet-api-gateway.conf`, relay `cfmeet-gateway-ingress` 127.0.0.1:18110 → gateway:8099; collision check `route_collisions`). Each is an activation step in the coordinated release.

---

## 3. Security scan gateway (`services/security-scan-gateway`)

- Internal-only `POST /internal/v1/scans`, `GET /health`, `GET /ready` (`src/lib.rs:137-145`; README `:15-64`). Signed verdicts (`packages/scan-gateway-contract`).
- Metrics on separate private listener (`SCAN_GATEWAY_METRICS_ADDRESS`): `cosmicforge_scan_requests_total`, `_auth_failures_total`, `_backend_up`, `_signature_age_seconds`, etc. (README `:66-75`; `src/metrics.rs`).
- **Incident/alert notifications: absent.** No webhook/email/alert code (`grep alert|notif|incident|webhook|email src/` → none). No Prometheus rule exists for it (`docs/operations/monitoring-alerts.md:128-130`). Callers (Messaging) treat non-signed outcomes as no-answer and raise their own incidents.
- Production: container `cosmicforge-scan-gateway`, 64 MiB / 0.5 CPU, port 8449, deployed by `scan-gateway-configure` run 42102547990, verified 42113919022 (26/26, peak 8 MiB); engine = mail product's clamd (`docs/operations/shared-security-scanning.md:35-54`). Classification: implemented, activated, production-tested; notifications absent.

---

## 4. Infrastructure

### 4.1 Production hosting topology (actual)

Single **Contabo VPS**, `api.cfmeet.com` (`docs/operations/CFMEET_FINAL_PRODUCTION_COMPLETION.md:3`; `cfmeet-final-production-certification.md:3`; `deploy-backend.yml:93` "Contabo Rust release build"). Shared with other CosmicForge products (`cosmicforge-backend`, `cosmicforge-db`, `cosmicforge-redis`, KarryGo, mail stack) which the release tooling never touches (`coordinated-production-release.md:45-49`).

CF Meet containers (13 backend + helpers) — `coordinated-production-release.md:21-35`, `coordinated_release.py:87-147`:

| Service | Container | Internal port | Private TLS port | Memory limit | DB |
|---|---|---|---|---|---|
| Platform Access | cfmeet-platform-access | 8094 | 8443 | kept (pre-existing) | cfmeet_platform_access |
| Organizations | cfmeet-organizations | 8101 | 8450 | 192 MiB | cfmeet_organizations |
| Messaging | cfmeet-messaging | 8097 (metrics 9197) | 8444 | kept | cfmeet_messaging |
| Calling | cfmeet-calling | 8090 | 8445 | kept | cfmeet_calling |
| Communications Hub | cfmeet-communications | 8080 | 8447 | kept | cfmeet_comm_production_hub |
| Numbering | cfmeet-numbering | 8095 | 8451 | 192 MiB | cfmeet_numbering |
| Privacy Relay | cfmeet-privacy-relay | 8091 | 8452 | 192 MiB | cfmeet_privacy_relay |
| SMS Gateway | cfmeet-sms-gateway | 8096 | 8453 | 192 MiB | cfmeet_sms_gateway |
| Telephony | cfmeet-telephony | 8092 | 8454 | 192 MiB | cfmeet_telephony |
| Meeting Service | cfmeet-production-backend | 8080 | 8448 (public via host nginx → 127.0.0.1:18081) | kept | legacy `cosmicforge-db` container (name/version REQUIRES_VALIDATION) |
| Developer Platform | cfmeet-developer-platform | 8098 | 8455 | 256 MiB | cfmeet_developer_platform |
| DP worker | cfmeet-developer-platform-worker | — | — | 128 MiB | same |
| API Gateway | cfmeet-api-gateway | 8099 | — (public via relay 18110) | 192 MiB | none |
| Scan gateway | cosmicforge-scan-gateway | — | 8449 | 64 MiB | none |

Supporting: `lk-server` (LiveKit), `cfmeet-calling-egress`, `cfmeet-calling-artifacts`, `cfmeet-calling-redis`, `cfmeet-core-db` (postgres:16-alpine, internal network `cfmeet-core-private`, no host port), `cfmeet-core-tls` (private TLS proxy), `cfmeet-gateway-ingress` (32 MiB), `cfmeet-storage-ingress`, `cfmeet-messaging-ingress` (`CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:71`; `docs/operations/backup-and-restore.md:14-28`).
New containers: `--restart unless-stopped`, read-only root, `/tmp` tmpfs, cap-drop ALL, no-new-privileges, bounded logs, env file 0600 under `/opt/cfmeet-core/coordinated/env/` (`coordinated-production-release.md:37-43`). CPU limits: not set by the release tooling except relay (`--pids-limit`) and scan gateway (0.5 CPU); Calling's artifact helper "2 CPUs and 2 GiB" (`calling-production-2026-11-02.md:40`).

**PostgreSQL:** one shared instance `cfmeet-core-db` with one database per service (`cfmeet_*`) + Meeting DB inside legacy `cosmicforge-db`. Per-service roles (`<svc>_migrator` owner + runtime role, CONNECT revoked from PUBLIC) via `infra/deployment/postgres/provision-service-database.sh` (allowed `--service` list at `:17,44`), isolation certified by `verify-service-isolation.sh`.
**Redis:** `cfmeet-calling-redis` (Calling/Messaging); Meeting uses Redis (legacy `email:queue` list, now replaced by DB ledger, `CFMEET_FINAL_PRODUCTION_COMPLETION.md:28-30`); gateway Redis `API_GATEWAY_REDIS_URL` — production value not recorded (REQUIRES_VALIDATION).

**`docker-compose.prod.yml` (legacy template, not the live topology):** services postgres (postgres:16-alpine, `:20-36`), redis (`--maxmemory 256mb allkeys-lru`, `:41-54`), livekit (`:59-76`), meeting-service (`:81-127`, SMTP env `:101-107`, healthcheck curl `/health` `:122-127`), nginx (`:132-148`), certbot (`:153-158`). **No `mem_limit`/`cpus`/`deploy.resources` anywhere.** Container name `cfmeet-api` ≠ live `cfmeet-production-backend`. `docker-compose.yml` is local-only (127.0.0.1 ports; postgres:15-alpine; frontend under `app` profile).
`infra/deployment/part-vi/compose.part-vi.yml` is an alternative Part VI compose with readiness risk R1 (http URL refused outside local) per `developer-platform-gateway-activation-checklist.md:126` — resolved in practice by `cfmeet-core-tls`.

### 4.2 Recorded production resource measurements

| Date | Measurement | Source |
|---|---|---|
| 2026-11-05 | Host 6 vCPU, 11.7 GiB RAM, 4 GiB swap (~0.3 used), cgroup v2, Docker 29.1.3; MemAvailable 5.0 GiB (min 4.72); load 4–5 (≥54% idle); disk / 84.7% used, 29.5 GiB free; images 144 GB (45.6 reclaimable); 0 OOM | `docs/operations/messaging-section-8-delivery.md:292-300` |
| 2026-11-05 | CF Meet real container memory 0.71 GiB; other products 4.51 GiB (ClamAV daemons 0.93+0.92+0.67); MemAvailable 5.02 GiB; load 4.5–5.7 | same `:590-598` |
| 2026-11-05 | Recommendation: +2 GiB minimum, +4 GiB (16 GiB total) recommended | same `:624-628` |
| ~2026-11-0x | `resource-inventory` run 42192839669: 4.74 of 11.68 GiB available (CF Meet 0.91, other 4.61), disk free 23.14 GiB | `cfmeet-sections-8-23-final-closure.md:117-118` |
| 2026-11-10 | 5,316 MiB available, 151.2 GiB free disk (disk apparently grown since 11-05) | `cfmeet-final-production-certification.md:130` |
| — | Preflight "6.8 GB" (~6485 MiB) used in resource-gate example | `coordinated-production-release.md:114-117` |
| 2026-11-12 | memAvailable 5272 MiB, 4440 after release | `CFMEET_FINAL_PRODUCTION_COMPLETION.md:33` |
| 2026-11-13 | 5517 MiB available | `CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:72` |
| — | Organizations release: MemAvailable 5365 MiB, 4533 remaining | `CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md:227` |
| — | Scan gateway peak 8 MiB; mail clamd 1846 MiB peak / 3072 limit | `shared-security-scanning.md:35-54` |

Resource gate: `MemAvailable − (new container limits + 832 MiB transient) ≥ 1024 MiB` (`coordinated_release.py:166-190`; `SAFETY_MARGIN_MIB=1024`, `TRANSIENT_MIB=256+64+512`). A new notification service (~128–256 MiB) fits the measured headroom; a push/email worker with its own Redis or broker would need re-measurement.
`docs/INFRASTRUCTURE.md:50-58` gives only generic minimums (2 vCPU/4 GB/40 GB; recommended 4 vCPU/8 GB/80 GB) — template, not measurement.

### 4.3 How services deploy

| Workflow | Trigger | Role | Status |
|---|---|---|---|
| `cfmeet-coordinated-release.yml` | workflow_dispatch, main only | **single production path** for all 13 services; ops: inspect, backup, provision, release, verify, rollback, certify, 20+ `activate-*` incl. `activate-web-push`, `activate-resend-email`, `activate-developer-webhooks`, `email-acceptance`, `developer-access-review` (`:14-44`); gate waits for green push CI of exact SHA with `required` list (`:85-90`) and advisory list (`:96-99`); calls `cfmeet-core-images.yml`; VPS side `infra/calling/production/coordinated_release.py` (2522 lines) | activated, production-tested |
| `cfmeet-core-images.yml` | push main/staging, dispatch, workflow_call | builds 13 images to `ghcr.io/<owner>/cfmeet-<svc>:<sha>` with revision label; matrix `:31-33` | activated |
| `deploy-backend.yml` | push (tests only), dispatch (emergency Meeting-only deploy, SSH build on VPS + `infra/deployment/backend_runtime.py`) | concurrency `deploy-backend-production` shared | configured (emergency) |
| `production-v2-cutover.yml` | push/dispatch, environment production | API cutover script `scripts/production-v2-cutover.sh` | configured (historic) |
| `cfmeet-core-platform-access.yml`, `cfmeet-core-db.yml`, `cfmeet-core-messaging.yml`, `cfmeet-core-calling.yml`, `cfmeet-core-private-tls.yml`, `cfmeet-core-identities.yml`, `cfmeet-core-vps-preflight.yml`, `cfmeet-calling-production.yml`, `cfmeet-capture-*`, `cfmeet-enterprise-*`, `messaging-ingress.yml`, `private-docs-vps.yml` | various | earlier per-component bootstrap / ops workflows | configured |
| `cfmeet-certificate-monitor.yml` | daily 04:23 UTC | TLS monitor + GitHub issue notification | activated |
| `cfmeet-offsite-backup.yml` | daily 02:17 UTC + dispatch | age-encrypted off-host backup + drill | configured, **not activated** (no age keys; "no backup taken yet", `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:133`) |
| `backup-restore-drill.yml` | push | CI drill | implemented (advisory in gate) |
| Service CI: `developer-platform.yml`, `api-gateway.yml`, `webhooks.yml`, `security-scan-gateway.yml`, `usage-facts.yml`, `contracts.yml`, `service-boundaries.yml`, `migration-guard.yml` | push/PR | lint/test/build/docker; "nothing here deploys" | implemented |

Release mechanics: gate → images → backup (pg_dump every DB, restore-test Meeting/PA/Org) → provision → per-service migrations then swap in dependency order (`ORDER`/`DEPENDS`, `coordinated_release.py:150-163`) → health `/ready` or `/health` via `nsenter` → `verify`. Release scope keeps unchanged services (`release_scope.py`). VPS records: `/opt/cfmeet-core/coordinated/journal.json`, `registry-<sha>.json`, `env/*.env`, `trust/`, `/opt/cfmeet-core/backups/<run>/` (`coordinated-production-release.md:236-247`).

### 4.4 Monitoring: Prometheus rules & Alertmanager

- `infra/monitoring/prometheus/rules/meeting-service.rules.yml`: 15 alerts — critical: MeetingServiceUnavailable, MeetingJoinSuccessRateLow, MeetingRtcProviderUnavailableForJoins, MeetingConferenceTerminationFailing, **MeetingOutboxDeadLettersIncreasing** (`sum(rate(cfmeet_rtc_outbox_dead_lettered_total[5m])) > 0` for 10m, `:98-109`), MeetingOutboxRetriesWithoutProgress (`:111-124`), MeetingRecordingFailureRatioHigh; warning: JoinLatencyP95High, RtcDriftEscalated, ShadowMismatches, RealtimeDegraded, ArtifactFinalizationStuck; info: RtcDriftRepaired, LegacyWritesRejected.
- `messaging.rules.yml`: 6 warnings — **MessagingOutboxDeadLetters** (`messaging_outbox_events{status="DEAD_LETTER"} > 0`, `:16-27`), **MessagingObjectDeletionDeadLetters** (`:29-40`), MessagingRetentionBacklog, MessagingOutboxLagHigh, MessagingWebhookRelayLagHigh (`>600 s`, approval required, `:69-80`), MessagingMetricsUnavailable.
- No rules for email delivery, push dispatch, DP webhook dead letters, Calling, Platform Access, scan gateway, etc. (`monitoring-alerts.md:128-130`).
- `prometheus.example.yml`: EXAMPLE ONLY, `.invalid` hosts, scrapes meeting-service `/internal/metrics` (X-Internal-Token from file) and messaging `:9197/metrics`.
- `alertmanager.yml:19-66`: routes critical → `unconfigured-pager`, warning → `unconfigured-ticket`, info → `unconfigured-info`; **all receivers empty** (commented templates for PagerDuty / webhook / email). Inhibit rules critical ⊃ warning/info.
- CI: `service-boundaries.yml` job runs `scripts/monitoring/check_alert_rules.py`, `promtool check/test rules`, `amtool check-config` (pinned prometheus v3.5.0 / alertmanager v0.28.1).
- Classification: rules **implemented + CI-validated**; Prometheus/Alertmanager **absent in production**; paging **not configured** (`monitoring-alerts.md:3-5, 132-136, 165-166`).
- Only live operator notification: **TLS certificate monitor** → GitHub issue "Operations: api.cfmeet.com TLS certificate monitoring" (#38), comment on state change / daily while ALERT, read back (`.github/workflows/cfmeet-certificate-monitor.yml:1-30`; `infra/calling/production/certificate_notify.py:1-10`); delivery verified run 42189603354, comment 6680863924 (`cfmeet-sections-8-23-final-closure.md:126`). Email/push to a person depends on GitHub notification settings (not observable).
- Incidents (ADR 0031 §4): Calling/Telephony/Privacy Relay/Messaging persist incident rows (OPEN→RESOLVED via admin CLI) — **no outbound notification**.

### 4.5 Health checks
- Per service `/health` (DB check) and/or `/ready`; services refuse to start with pending migrations (`infra/deployment/README.md:51-52`). Note Meeting `/health` is static and does not touch DB (`monitoring-alerts.md:119`; `cfmeet-final-production-certification.md:120-121`).
- Release/certify probes from inside container netns (`coordinated-production-release.md:122, 201-217`).
- `docker-compose.prod.yml:32-36,50-54,122-127` compose healthchecks (legacy template).

### 4.6 Backups
- Per-service `infra/deployment/backup/backup.sh` / `restore.sh` / `verify-restore.sh` / `offsite.sh`; catalog `infra/deployment/backup/catalog.conf` (11 services incl. developer-platform); systemd timer template `backup/systemd/cfmeet-db-backup.timer`.
- Coordinated `backup` op: pg_dump of 16 databases with restore tests (release 43309857124, `CFMEET_FINAL_PRODUCTION_COMPLETION.md:309`).
- Off-host: implemented, not activated (owner age keys missing; owner decision 2026-11-13: no backup prerequisite for first activation).

---

## 5. Production evidence

### 5.1 Recorded production runs relevant to notifications / DP / gateway

| Run | Operation | Date/SHA context | Result | Source |
|---|---|---|---|---|
| 42102547990 | scan-gateway-configure | 2026-11-05 | deployed | shared-security-scanning.md:35 |
| 42113919022 | scan-gateway-verify | | 26/26 | same |
| 42189603354 | certificate monitor delivery test | | comment 6680863924 on #38 | cfmeet-sections-8-23-final-closure.md:126 |
| 42193577972 | messaging-verify | | 15/15; APNs, FCM, Web Push NOT configured | same:115 |
| 42713082843 | activate-gateway-routing | | DP families public | CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:80 |
| 42909534672 | certify | 2026-11-10 | CERTIFIED_READ_ONLY | cfmeet-final-production-certification.md:7-9 |
| 43169453546 | activate-developer-console | base main 4d90b9b6 ≡ release 4904ee33 | regression PASS | CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md:51 |
| **43173688215** | **activate-developer-webhooks** | | webhooks enabled, mode active, key ring mounted, worker egress | same:52, :73 |
| 43174233820 | activate-gateway-families | | 18/18 post-checks; central webhooks on | same:53 |
| 43176010902 | certify | | CERTIFIED_READ_ONLY; developer-platform 33 routes auth-enforced | same:54 |
| **43184898911** | developer-acceptance | | 30/30; webhook deliveries | same:55 |
| 43192643956 | activate-usage-export | base 58cc5eb8 | ledger acknowledged Calling streams | same:56 |
| 43291592091 / 43302325314 | inspect / diagnose | 2026-11-12 | SMTP (Gmail, `smtp.gmail.com:587`) implicit; 0 SMTP sends/535 in 168 h window; no Resend | CFMEET_FINAL_PRODUCTION_COMPLETION.md:22-30 |
| **43309857124** | release (Meeting + DP; `lifecycle_delivery_migrations` = email ledger) | `f2f9693489bd1289935b695d5d83e6db4d22eb2f` | 30/30 | same:13 |
| 43331884110 | release | `3e11958b`; DP kept at f2f96934 | 30/30 | same:318 |
| 43337109332 | certify | `0bcdd137e2b9402fbc52c66ba4e74ed6769ba342` | CERTIFIED_READ_ONLY | same:20 |
| **43337892242** | developer-acceptance | `0bcdd137` | 30/30, signed webhook SUCCEEDED | same:17, 197 |
| 43479482331 | org-cutover-first-activation | 2026-11-13 | authoritative | CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:30 |
| **43488534526** | developer-acceptance | release b9dc15be | 30/30, webhook HTTP 204 | same:34, 177-184 |
| 43490164383 | certify | | 13/13 PASS | same:35 |
| 43630780918 | activate-gateway-docs | | 18/18 (gateway `/health` and `/ready` not public) | same:301 |

Notification-specific status:
- **Email**: Meeting Service SMTP configured and `NOTIFICATIONS_EMAIL_ENABLED=true` (CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md:76; certify 43176010902). Durable ledger `email_deliveries`/`email_delivery_events` deployed (release 43309857124; `services/meeting-service/migration/src/m20261112_000001_email_delivery_and_presence.rs:1-74`) — "ledger exists and is empty" at 2026-11-12. **Resend: implemented, not activated** (`activate-resend-email` never run; `CFMEET_FINAL_PRODUCTION_COMPLETION.md:15,121,317`). OTP delivery "not yet recovered" (`:121`).
- **Push**: `MESSAGING_PUSH_PROVIDER=none` (cfmeet-final-production-certification.md:169; CFMEET_BACKEND_FINAL_CLOSURE.md:124); APNs/FCM/Web Push `NOT_CONFIGURED` / BLOCKED_EXTERNAL (cfmeet-sections-8-23-final-closure.md:86,158); `physicalDevicePush: NOT_RUN_NO_PROVIDER_CREDENTIALS_OR_DEVICE_EVIDENCE` (FIRST_PARTY report :206). `activate-web-push` (VAPID key generated on VPS, `coordinated_release.py:1878-1926, 2410-2411`) exists since `42b509c` (2026-11-15) — **no recorded run** → configured, not activated. Note the release "safe defaults" text still says `MESSAGING_PUSH_PROVIDER` must be unset/none (`coordinated-production-release.md:139`).
- **Customer webhooks**: activated + production-tested (above).
- **DP human notifications**: absent / not activated (§1.4).

### 5.2 Live probes (unauthenticated GET, 2026-11-15 ~09:07 UTC)

| Path | HTTP | Content | Interpretation |
|---|---|---|---|
| `/health` | 200 | `{"service":"CosmicForge RTC","status":"healthy","timestamp":"2026-11-15T09:07:43Z","version":"0.1.0"}` (116 B, 0.68 s) | Meeting Service (direct route) liveness |
| `/ready` | 404 | — | not routed publicly (gateway /ready private by design) |
| `/discovery` | 200 | JSON 3783 B; begins `{"centralUsage":false,"centralWebhooks":true,"documentation":{"aggregate":"/openapi.json","reference":"/docs"},"families":[{"domain":"meetings","enabled":true,…"operations":133,…"version":"1.5.0"…` | API Gateway catalogue; confirms central webhooks live |
| `/openapi.json` | 200 | 2.84 MB OpenAPI 3.1 "CF Meet public API" | gateway aggregate |
| `/docs` | 200 | Swagger UI HTML | gateway docs |
| `/api-docs/swagger-config.json` | 200 | 565 B; Meetings, Communications, Calling, Numbering, Privacy Relay… | Meeting Service Swagger index |
| `/api/v1/health`, `/v1/health` | 404 | — | not exposed |

(No authenticated or non-health customer routes were probed.)

---

## 6. Shared packages useful for a Notification Service

| Package | Provides | Key symbols | Status |
|---|---|---|---|
| `packages/service-auth` (`cf-service-auth`) | Ed25519 JWS service tokens (≤300 s, one audience, request/correlation binding), receiver-authoritative `TrustPolicy` (trust bundle), permission registry, `DelegatedPlatformContext`, replay guard, outbound `InternalServiceClient` (feature `client`), service identity registry | `src/lib.rs:1-70`; `identity.rs` `services::*`; `permissions.rs` `InternalPermission`, `PermissionDefinition {name, receiver, grantable_to, sensitive}` (e.g. `developer-event.publish` `:849-860`) | implemented, used by all services |
| `packages/webhooks` (`cf-webhooks`) | HMAC signing/verify (`signing`), SSRF endpoint policy + guarded resolver + client (`endpoint`), retry classification/backoff/Retry-After (`retry`), AES-GCM secret box with key ring (`secret_box`) — no business vocabulary | `src/lib.rs:1-18`; `retry.rs:11-79`; `signing.rs:18-55`; `endpoint.rs:22-257`; `secret_box.rs:24-242` | implemented; reused by Calling + DP; CI `webhooks.yml` |
| `packages/usage-facts` (`cf-usage-facts`) | billing-neutral `UsageFact` model, units, producers, digest; feature `exporter`: durable watermark-based exporter to DP (`GET /internal/v1/usage/streams/{stream}`, `POST /internal/v1/usage/facts`, reconciliations) | `src/lib.rs:1-40`; `src/exporter.rs:1-30` | implemented, activated (run 43192643956) |
| `packages/migration-guard` (`cf-migration-guard`) | refuses `down/fresh/refresh/reset` unless `APP_ENV` non-prod or `CF_MIGRATION_ALLOW_DESTRUCTIVE=<subcmd>`; call `enforce_from_env()` first in migration `main` | `src/lib.rs:1-30`; usage `services/developer-platform/migration/src/main.rs:5-7`; CI `scripts/migrations/assert_destructive_guard.sh` | implemented |
| `packages/contracts` | contract authority: `manifest.json` (98 entries; fields id, version, kind, path, owner, visibility, lifecycle, authority, generated_by, description), schema `manifest.schema.json`, offline `validator/` (`make contracts-validate`, CI `contracts.yml`); relevant: `cfmeet.platform.webhooks` 1.3.0, `cfmeet.developer-platform.webhooks` 1.1.0, `cfmeet.platform.event-envelope`, `cfmeet.calling.push-invitation` 1.0.0 (experimental) | `packages/contracts/README.md:1-40` | implemented |
| `packages/scan-gateway-contract`, `packages/object-storage`, `packages/communications-policy`, `packages/messaging-crypto` | scan wire contract; S3 SigV4 private objects; comms policy; MLS crypto | Cargo descriptions | implemented (less relevant) |
| Scripts | `scripts/contracts/public_api.py` (route manifest generator, default deny), `check_gateway_upstreams.py`, `bundle_openapi.py`; `scripts/service-boundaries/check_service_boundaries.py` (≈25 rule codes, `:1-80`), `check_operations_registry.py` + `operations_registry.json` (owner of each durable worker, e.g. `messaging-push-dispatch` → `messaging src/realtime/push.rs dispatch_due`, `developer-platform-webhook-delivery-worker`); `scripts/monitoring/check_alert_rules.py`; `scripts/migrations/check_migration_order.py`; `scripts/secret-scan/` | | implemented |

---

## 7. How a new Rust service is structured and added (pattern from developer-platform)

1. **Layout:** `services/<svc>/` own Cargo workspace (`[workspace] members = [".", "migration"]`, `Cargo.toml:1-2`), lib + bins (`src/main.rs`, `src/bin/<svc>-worker.rs`), `migration/` crate (`<svc>-migration`, `sea-orm-migration` cli, `cf-migration-guard` dep; `migration/Cargo.toml`), `tests/`, `.env.example`, `README.md`, `Dockerfile`, `Dockerfile.dockerignore`. Path deps only into `packages/*` (boundary rule `CARGO_CROSS_SERVICE_DEPENDENCY`).
2. **Migrations:** raw-SQL `UP`/`DOWN` consts per file `mYYYYMMDD_00000N_<name>.rs`, own history table `<svc>_seaql_migrations` (`migration/src/lib.rs:28`), prefixed tables; append-only/no-delete triggers common; server refuses to start with pending migrations; guard first in `main` (`migration/src/main.rs`).
3. **Dockerfile:** context = repo root; builder `rust:1-bookworm` copies only listed packages + service; `cargo build --release --locked --workspace --bins`; runtime `debian:bookworm-slim` + security upgrades, uid 10001 non-root, CA certs, binaries in `/app`, `ENV <SVC>_BIND_ADDRESS`, `LOG_FORMAT=json` (`services/developer-platform/Dockerfile:17-46`). `.dockerignore` whitelists exactly those paths and excludes `.env`, keys.
4. **CI workflow** (`.github/workflows/developer-platform.yml`): path filters incl. shared packages; jobs `lint` (fmt, clippy -D warnings, repo guards: no other service paths, only prefixed tables), `test` (postgres:16 service; foundation/architecture/startup/worker tests), `build` (destructive-guard assertion, migration up/reset/up smoke, table-prefix check, `verify-service-isolation.sh` with real migrations), `docker` (build, assert `/app` contents, no private key material).
5. **Registries to touch to ship to production:**
   - `packages/service-auth/src/identity.rs` (ServiceId) and `permissions.rs` (any new permission, e.g. `notification.publish` receiver = new svc, grantable_to producers); bump `cfmeet.platform.common`.
   - `packages/contracts/<svc>/…` + `manifest.json`; generated bundles.
   - Gateway (if public): §2.3.
   - `scripts/service-boundaries/operations_registry.json` (each durable worker: owner, source, entry, tests); boundary checker sets if domain-specific rules needed.
   - `infra/deployment/postgres/provision-service-database.sh:17,44` service list; `infra/deployment/backup/catalog.conf`.
   - `.github/workflows/cfmeet-core-images.yml:33` matrix; `cfmeet-coordinated-release.yml:85-90` `required` CI list.
   - `infra/calling/production/coordinated_release.py`: `SERVICES` entry (`_service(container, port, health, tls=, db=, role=, migrations=, history=, memory=, env=)`, `:87-147`), `ORDER`, `DEPENDS`, `IMAGES` (`:150-170`), safe-default flag checks (`:1219+`), private TLS route (port allocation 84xx), an `activate-<x>` operation for grants/flags, plus tests in `test_coordinated_release.py`.
   - Trust bundles of receivers (grants are per activation step).
   - Docs: runbook table in `docs/operations/coordinated-production-release.md:21-35`.

---

## 8. Classification summary table

| Capability | Classification | Key evidence |
|---|---|---|
| DP customer webhooks (subscriptions, signing, delivery, retry, DLQ, replay, auto-disable) | implemented · configured · activated · production-tested | §1.2; runs 43173688215, 43337892242, 43488534526 |
| DP webhook event catalog (72 types) | implemented; credential.created/rotated/revoked & production_access.* never emitted | `catalog.rs:84-164`; no publisher |
| Credential lifecycle webhooks | implemented · activated · production-tested | `credential_lifecycle.rs:262-440` |
| Human developer notifications (invite, credential, security) | absent | §1.4 |
| Production-access decision email | implemented (ops script + Meeting endpoint) · not activated | `developer_access.py:150-200`; `email.rs:194-260` |
| Calling legacy customer webhooks | implemented · disabled (authority retired) | `coordinated-production-release.md:74` |
| Meeting legacy webhooks | deprecated (never delivered) | `legacy-meeting-webhooks-deprecation.md` |
| Gateway central webhooks re-point | activated (live `/discovery`) | `catalogue.rs:99-103` |
| Gateway notification routes (communications prefs/devices/push, messaging push) | in manifest; edge routes only DP + messaging families via gateway; `/api/v2/communications` direct to Meeting | §2.2 |
| Meeting email (SMTP) | configured · activated (`NOTIFICATIONS_EMAIL_ENABLED=true`) | FIRST_PARTY report :76 |
| Email ledger + Resend | ledger activated; Resend implemented, not activated | CFMEET_FINAL_PRODUCTION_COMPLETION.md:15 |
| Push (APNs/FCM/Web Push) | implemented in Messaging; disabled in prod (`none`); `activate-web-push` configured, never run | §5.1 |
| Scan gateway alerts | absent | §3 |
| Prometheus/Alertmanager | rules implemented + CI-validated; not deployed; receivers empty | `alertmanager.yml:3-14` |
| DP webhook dead-letter alerting / metrics | absent | no metrics in DP |
| TLS cert monitor → GitHub issue | activated · production-tested | run 42189603354 |
| Off-host backup | implemented · not activated | ALL_FEATURES closure :133 |
| `docker-compose.prod.yml` | stale template (no limits, not live topology) | file :1-168 |
| DP README / Dockerfile / ADR 0055 / activation checklist "not deployed / OPEN" | stale documentation | §1.1, checklist statuses |
