# CF Meet — Data Model and Architecture Gap Assessment

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.


## 1. Service and architecture inventory

The repository contains **12 backend services** (Rust/axum), **8 shared packages**, **2 client apps**, a certification harness and the infrastructure/CI definitions. No other backend service exists in the repository. The "Account and identity components", "Recording and voicemail" and "Notification infrastructure" listed in the audit brief are not separate services: they live inside Meeting Service (accounts), Calling (recording, voicemail), and Messaging / Communications / Calling (notifications). "Realtime/WebSocket infrastructure" is five WebSocket routes in three services plus the gateway's WebSocket proxy (§1.3).

### 1.1 Backend services

| # | Service | Path | Purpose | Canonical data ownership | Dependencies | Migrations | Tests | Documentation | Deployment status | Known restrictions |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | **Meeting Service** | `services/meeting-service` | Account and identity authority (users, auth, Google sign-in), legacy v1 meetings, v2 Meeting Spaces / Conferences / admission / recordings / transcripts, meeting organizations, legacy developer API (API keys, webhooks), first-party relays to Messaging and Communications | users, sessions (JWT), meetings (v1), meeting_spaces, conference_sessions, participants, invitations, organizations, organization_memberships, artifacts, account_communication_state | PostgreSQL, Redis, LiveKit, SMTP, Google OAuth, Communications, Messaging, Platform Access, Developer Platform (session bridge) | `migration/` 27 (3 history tables) | 61 test files, ~683 tests (`backend.yml`, real PostgreSQL; LiveKit mocked) | README, Swagger (utoipa) `src/swagger/mod.rs`, ADR 0002/0053 | PRODUCTION (`deploy-backend.yml`) | Legacy v1 writes retire behind `FEATURE_LEGACY_MEETING_WRITE_DISABLED` (off by default) |
| 2 | **Calling** | `services/calling` | Canonical call aggregate, endpoints and ringing, LiveKit media, signaling WS, enterprise calling (extensions, ring groups, queues, IVR, business hours, forwarding, voicemail, recording), webhooks authority (until Part VI cut-over), public Privacy Relay facade, usage/limits | calls, call_participants/sessions/legs/events, enterprise_*, voicemail_*, call_recordings, webhook_*, platform_* projection | PostgreSQL, LiveKit, object storage, Platform Access, Communications, Telephony, Numbering, Privacy Relay, Developer Platform | `migration/` 46 (+1, 2 histories) | 102 test files, ~730 tests (real PostgreSQL, real LiveKit, Asterisk/SIPp in CI) | README, ADRs 0003, 0006–0009, 0018–0027, 0030 | PRODUCTION (Enterprise Calling, recording, voicemail ON; public PSTN OFF) | No push transport (`NoPushTransport`); PSTN flags off |
| 3 | **Messaging** | `services/messaging` | Conversations, membership, MANAGED and MLS (PRIVATE_E2EE) messages, attachments with malware scanning, realtime gateway, search, moderation, compliance, key transparency, SMS bridge, backups | messaging_conversations, members, messages, revisions, attachments, devices, key_packages, mls_*, transparency_*, push_*, blocks, legal_holds | PostgreSQL, Redis, object storage, Security Scan Gateway, Platform Access, Communications, Calling, SMS Gateway, APNs/FCM (not configured) | `migration/` 29 (+1) | 38 test files, ~284 tests (real PostgreSQL/WS/Redis/MinIO/clamd) | README, ADRs 0045–0054, architecture/messaging-*.md | PRODUCTION (MANAGED); PRIVATE_E2EE blocked by production validator (#13) | E2EE device registration refused in production |
| 4 | **Communications Hub** | `services/communications` | Account-level communications: profiles, CF Meet ID/QR, contacts, requests, invites, blocks, privacy, devices, notification preferences, inbox/timeline, chat/contact-to-call, meeting share | communication_profiles, public_ids, connections, contact_*, devices, block_bridge, timeline_entries, outbox, audit | PostgreSQL, Calling, Messaging, Meeting Service, Platform Access (via signed service auth) | `migration/sql` 2 (2 histories) | 3 test files, ~38 tests (real PostgreSQL, simulated owners) + five-service certification job | docs/api/communications-unified-handoff.md, operations/communications-*.md | PRODUCTION (73 public ops served via Meeting relay; 22/22 release checks) | Phone operations deferred by owner |
| 5 | **Platform Access** | `services/platform-access` | Shared identity authority: tenants, applications, environments, service accounts, API credentials (peppered hashes), principals, scopes, delegation, audit | platform_tenants, applications, environments, service_accounts, api_credentials, principals, identity_changes, access_audit | PostgreSQL | `migration/` 4 | 14 test files, ~121 tests | README, ADR 0032, 0046 | PRODUCTION (image in closure report §3) | No public routes (internal/admin only); audit table write-only |
| 6 | **Developer Platform** | `services/developer-platform` | Developer/enterprise control plane: organizations, members, invitations, projects, environments, credentials, service accounts, webhooks (central), usage, quotas, plans, production access, operator API | devplat_organizations, workspace_members, invitations, billing, plan_*, usage_*, webhook_*, idempotency | PostgreSQL, Platform Access, Meeting Service (sessions), Calling (webhook migration) | `migration/` 8 | 9 test files, ~120 tests (real PostgreSQL, fake Platform Access) | README, ADR 0055, phase-06 docs | NOT DEPLOYED (no image/deploy job; PLT-005) | Console UI deferred (spec D-07) |
| 7 | **API Gateway** | `services/api-gateway` | Public customer ingress: manifest-driven routing of 572 operations, credential auth, quotas, WebSocket proxy, aggregate OpenAPI and discovery | stateless (Redis quotas) | Redis, all upstream services, Platform Access | — | 7 test files, ~44 tests | README, ADR 0055, infra/api-gateway/* | NOT DEPLOYED (Part VI compose "not wired into any workflow") | Central webhooks/usage flags off |
| 8 | **Telephony** | `services/telephony` | Executes PSTN calls: SIP/Asterisk ARI legs, bridges, provider callbacks, routes, captures | telecom_providers, routes, telephony_legs, bridges, provider_events, captures | PostgreSQL, Asterisk ARI, Calling, Numbering, Privacy Relay, carrier (none) | `migration/` 17 | 25 test files, ~165 tests (simulators, Asterisk/SIPp) | README, ADRs 0014–0017 | NOT DEPLOYED to production | No carrier contract (EXTERNAL) |
| 9 | **Numbering** | `services/numbering` | Number catalog, KYC/eligibility, inventory search/reservation, provisioning, ownership/assignment, routing grants, stewardship | number_resources, reservations, assignment_history, kyc_*, provider_*, routing_*, relay_leases | PostgreSQL, Platform Access, Developer Platform, number provider (synthetic only) | `migration/` 10 | 23 test files, ~215 tests | README, ADRs 0033–0040, 0043–0044 | No production deployment evidence in the repository; synthetic provider refused in protected environments | Real provider adapter not written |
| 10 | **SMS Gateway** | `services/sms-gateway` | SMS transport, delivery events, opt-out, Messaging SMS bridge | sms_transports, attempts, delivery_events, optout_blocks, bridge_* | PostgreSQL, Messaging, Numbering, Calling, Platform Access, SMS carrier (test provider only) | `migration/` 4 | 10 test files, ~87 tests | README, ADR 0042 | No production deployment evidence in the repository; test provider refused in protected environments | No real carrier adapter |
| 11 | **Privacy Relay** | `services/privacy-relay` | Communication contexts, relay sessions, protected contacts (Contact Vault), temporary relay numbers and cooldown | communication_contexts, privacy_relay_sessions, protected_contacts, contact_fingerprints, relay_number_* | PostgreSQL, Calling, Messaging, Numbering, Developer Platform | `migration/` 17 | 40 test files, ~180 tests | README, ADRs 0010–0012 | NOT DEPLOYED to production | Needs carrier and real numbers |
| 12 | **Security Scan Gateway** | `services/security-scan-gateway` | Shared authenticated malware-scan service (ClamAV) issuing signed, bound verdicts | stateless (in-memory replay cache) | ClamAV engines | — | 1 test file, ~20 tests | README, operations/shared-security-scanning.md | PRODUCTION (`scan-gateway-verify` PASS) | Single-instance replay cache |

### 1.2 Shared packages, clients and supporting components

| Package / app | Path | Role |
|---|---|---|
| contracts | `packages/contracts` | OpenAPI and JSON-Schema authority; `bundled/` Swagger bundles; manifest + validator |
| service-auth | `packages/service-auth` | Signed internal service-to-service authentication (ADR 0004) |
| messaging-crypto | `packages/messaging-crypto` | MLS / E2EE client core (Rust → WASM / FFI) |
| object-storage | `packages/object-storage` | Protected object storage and presigning (ADR 0023) |
| webhooks | `packages/webhooks` | Webhook signing and secret box |
| usage-facts | `packages/usage-facts` | Usage fact model shared by producers and Developer Platform |
| communications-policy | `packages/communications-policy` | Shared communications policy evaluation |
| scan-gateway-contract | `packages/scan-gateway-contract` | Scan verdict contract |
| SDKs | `sdks/typescript`, `sdks/dart` | Generated from the public gateway contracts (572 operations each); not used by either first-party app |
| Web app | `apps/web` | Next.js client: meetings v2, conference room, messaging (MANAGED + E2EE client), calling, organizations page, settings |
| Mobile app | `apps/mobile` | Expo / React Native client: auth and a legacy meeting flow (largely broken, see MTG-001) |
| Certification | `certification/` | Cross-service certification harness (Rust) |
| Infrastructure | `infra/` | API gateway manifests, deployment scripts, backup/restore, LiveKit, telephony edge, TLS |
| CI/CD | `.github/workflows` (75 workflows) | Per-service CI, certification, production release/verify workflows (deployments are GitHub Actions only) |

### 1.3 API surface per service (registered operations, independently recounted)

Counted by static extraction of every axum router, from `axum::serve` through `.route` / `.nest` / `.merge`, local bindings and router parameters. The Communications row counts the 73 operations dispatched by `services/communications/src/api.rs::dispatch_inner`, which clients reach through meeting-service's wildcard relay `ANY /api/v2/communications/*path`. Those two relay routes are counted in meeting-service's Public column. One row per operation is in [`cfmeet-api-inventory.csv`](cfmeet-api-inventory.csv).

| Service | Public | Realtime (WS) | Admin/operator | Internal service | Internal delegated mirror | Provider callback | Infrastructure (health/metrics/docs) | **Total** | Gateway-routed |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| api-gateway | 0 | 0 | 0 | 0 | 0 | 0 | 6 | **6** | 0 |
| calling | 142 | 3 | 0 | 24 | 0 | 1 | 6 | **176** | 141 |
| communications | 73 | 0 | 3 | 11 | 0 | 1 | 2 | **90** | 73 |
| developer-platform | 83 | 0 | 32 | 8 | 0 | 0 | 3 | **126** | 74 |
| meeting-service | 193 | 1 | 14 | 5 | 0 | 2 | 4 | **219** | 130 |
| messaging | 96 | 1 | 0 | 20 | 94 | 0 | 4 | **215** | 96 |
| numbering | 52 | 0 | 25 | 17 | 0 | 1 | 4 | **99** | 52 |
| platform-access | 0 | 0 | 26 | 38 | 0 | 0 | 3 | **67** | 0 |
| privacy-relay | 5 | 0 | 0 | 24 | 0 | 0 | 2 | **31** | 0 |
| security-scan-gateway | 0 | 0 | 0 | 1 | 0 | 0 | 3 | **4** | 0 |
| sms-gateway | 6 | 0 | 0 | 4 | 0 | 2 | 4 | **16** | 6 |
| telephony | 0 | 0 | 0 | 14 | 0 | 2 | 1 | **17** | 0 |
| **Total** | **650** | **5** | **100** | **166** | **94** | **9** | **42** | **1066** | **572** |

**Realtime interfaces (5 WebSocket routes):**
- Calling `GET /v1/calling/signaling`
- Calling `GET /v1/calling/media/rtc` (legacy) and `/v1/calling/media/rtc/v1`
- Meeting `GET /api/v2/conferences/:id/realtime`
- Messaging `GET /v1/realtime`

Three of them are gateway-routed. The two Calling media gateway routes are excluded from the gateway catalogue. There are also ticket endpoints (`POST /api/v2/conferences/:id/realtime/ticket`, `POST /v1/realtime/tickets`, `POST /v1/calling/signaling-credentials`), and the API Gateway proxies WebSockets (`services/api-gateway/src/ws.rs`).

**Event and webhook interfaces:**
- **Outbound customer webhooks:** Calling (authority today), Developer Platform (central, dormant), meeting-service legacy (stored, never delivered).
- **Inbound provider callbacks:** `POST /internal/rtc/livekit/webhook`, `POST /internal/transcription/events` (meeting), `POST /internal/provider/media/events` (calling), `POST /provider-callbacks/:provider_id[/:token]` (telephony), numbering/SMS provider callbacks, and `POST /internal/v1/communications/verification/callback`.

### 1.4 Cross-service authority map (who owns what, and where it is duplicated)

| Business entity | Canonical owner (per ADR / code) | Other copies or projections | Conflict? |
|---|---|---|---|
| User account / credentials | Meeting Service `users` | Platform Access `platform_principals` (NATIVE_CF_USER), Communications `communication_profiles`, `communication_principal_links` (meeting) | Projections are reconciled by account events, but events are dropped for users without a Hub profile (ACC-010). Deactivation never reaches Platform Access principals. |
| Organization / tenant | **None declared.** Meeting `organizations`, Developer Platform `devplat_organizations` (backed by a Platform Access tenant), Calling/Numbering tenant/app/env triple | — | **Yes (ORG-001 / DATA-004).** Three models, two membership/role systems, no mapping. |
| Organization membership / roles | Meeting `organization_memberships` (OWNER/ADMIN/MEMBER); DP `devplat_workspace_members` (OWNER/ADMIN/DEVELOPER/ANALYST/BILLING) | Messaging directory derives colleagues from meeting memberships | Yes |
| Devices | Messaging `messaging_devices` (MLS), Calling endpoints, Communications `communication_devices`, meeting `audio_video_devices` (legacy) | — | Yes (DATA-005). There is no account-level revocation across the four registries (Journey H FAIL). |
| Blocks | Calling `calling_block_rules`, Messaging `messaging_blocks`, Communications `communication_block_bridge` | — | Partially reconciled by the Hub. The `block-events` ingress has no producer (MSG-016). |
| Customer webhooks | Calling (today) → Developer Platform (planned) | meeting-service `webhooks` (legacy, not delivered) | Yes (PLT-003). See the [duplication report](cfmeet-api-duplication-report.md) C-01. |
| API credentials | Platform Access `platform_api_credentials` | meeting-service `api_keys` (legacy, plaintext) | Yes (PLT-002 / PLT-004) |
| Meetings | Meeting Service v2 (`meeting_spaces` + `conference_sessions`) | legacy `meetings` (v1), not mirrored to v2 | Yes (DATA-008 / MTG-017) |
| Meeting chat | Meeting `conference_messages` → Messaging (ADR 0053 cut-over, deferred) | legacy `chat_messages` | Migration pending (MSG-012) |
| Call history | Calling | Communications timeline call cards | Projection (by design) |

### 1.5 Architectural risks (summary)

1. **The account authority is the weakest service.** Meeting-service owns accounts, which every other service trusts, but carries the legacy auth model: symmetric JWT, no token type, revocation gaps, plaintext legacy secrets, naive timestamps, no audit table and no backup path. See SEC-001…SEC-013, ACC-001…ACC-004, DATA-001…DATA-003, DATA-015.
2. **No organization authority** (ORG-001). Enterprise Calling, the Developer Platform and Meetings each model a different "organization". The web runs all Calling traffic through one shared credential and scope (CAL-007).
3. **Part VI is undeployed** (PLT-005). The Developer Platform (126 registered operations) and the gateway (572 manifest routes, both SDKs, public docs) are built and CI-tested, but neither is deployed, and production first-party traffic bypasses them.
4. **Two generations of the meeting model are live** (v1 + v2). Mobile is the remaining v1 writer.
5. **Device and revocation fan-out spans four registries** without one account-level revocation event.

## 2. Data model and migration assessment (security, data and reliability audit, Part 2)



### Migration inventory

| Service | Migrations dir | # migrations (histories) | Framework / history table(s) | Key tables | Issues |
|---|---|---|---|---|---|
| api-gateway | — | 0 | stateless (Redis quota) | — | — |
| security-scan-gateway | — | 0 | stateless | — | in-memory replay cache (single instance) |
| meeting-service | `services/meeting-service/migration/src/` | 27 = 25 + 1 + 1 (3 histories) | SeaORM; `seaql_migrations` (default name), `account_communications_migrations`, `account_communications_facade_migrations` | legacy: `users, meetings, participants, chat_messages, session_logs, webhooks, api_keys, audio_video_devices`; v2: `meeting_spaces, conference_sessions, conference_participants, participant_sessions, meeting_invitations, meeting_members, conference_messages, conference_audit_events, outbox_events, organizations, organization_memberships, meeting_artifacts, transcript_*, webinar_registrations, breakout_*`; Hub: `account_communication_state/outbox, communication_principal_links, communication_instant_meetings` (44 raw-SQL tables + 8 SeaQuery) | 33 naive `TIMESTAMP` columns on legacy tables (DATA-001); case-sensitive email (DATA-002); plaintext api keys/webhook secrets (SEC-006, DATA-007); CASCADE vs RESTRICT conflict (DATA-009); dual legacy/v2 model (DATA-008); no retention (DATA-010); history fragmentation (DATA-011); historical destructive `m20260323_000001_migrate_to_redis` (drops columns/tables — already applied); oddly named `m20260205_110126_m20260205_000001_add_guest_role`; data backfill inside schema migration (`m20261003_000002_phase2_legacy_backfill`) |
| calling | `services/calling/migration/src/` | 48 = 47 + 1 (2 histories) | SeaORM + raw SQL; `calling_seaql_migrations`, `calling_communications_migrations` | `calls, call_participants, call_sessions, call_legs, call_events, call_recordings, voicemail_*, enterprise_* (ring groups, queues, IVR, SIP devices), webhook_*, platform_* projection, calling_outbox_events, calling_retention_*` (90) | number 000039 unused (documented); `platform_*` duplicate of Platform Access (projection, DATA-004); `calling_communication_revocations.credentials_not_before BIGINT` vs TIMESTAMPTZ elsewhere (DATA-012); `platform_auth_failure_windows` retained but unwritten |
| communications (Hub) | `services/communications/migration/sql/` (+`src/lib.rs`) | 2 (2 histories) | raw SQL via SeaORM; `communications_seaql_migrations`, `communications_unified_seaql_migrations` | `communication_profiles, communication_public_ids, communication_privacy, communication_connections, communication_contact_*, communication_devices, communication_block_bridge, communication_timeline_entries, communication_outbox, communication_audit, communication_service_nonces` (29) | Forward-only (good); one-migration-per-history pattern (DATA-011); duplicates users/devices/blocks (DATA-004/005/006) |
| developer-platform | `services/developer-platform/migration/src/` | 8 (numbers 1–5, 10, 11, 12) | SeaORM raw SQL; `developer_platform_seaql_migrations` | `devplat_organizations, devplat_workspace_members, devplat_invitations, devplat_billing_accounts, devplat_plan_*, devplat_usage_*, devplat_webhook_*, devplat_idempotency_records` (36) | Third "organization" concept (DATA-004); no retention for activity/usage/webhook attempts (DATA-010); future-dated names (DATA-014) |
| messaging | `services/messaging/migration/src/` | 30 = 29 + 1 (2 histories) | SeaORM raw SQL; `messaging_seaql_migrations`, `messaging_communications_migrations` | `messaging_conversations, messaging_conversation_members, messaging_messages, messaging_message_revisions, messaging_attachments(+derivatives), messaging_devices, messaging_key_packages, messaging_mls_*, messaging_transparency_*, messaging_push_*, messaging_blocks, messaging_legal_holds, messaging_outbox_events` (63) | E2EE-secret column guard (good); opaque principal refs without cross-service FK (by design, DATA-017); duplicate blocks/devices (DATA-005/006); migration names dated up to 2026-12-01 (DATA-014) |
| numbering | `services/numbering/migration/src/` | 10 | SeaORM raw SQL; `numbering_seaql_migrations` | `number_resources, number_reservations, number_assignment_history, number_kyc_*, number_provider_*, number_routing_*, number_relay_leases, numbering_outbox_events` (50) | `down()` drops (SEC-015) |
| platform-access | `services/platform-access/migration/src/` | 4 | SeaORM raw SQL; `platform_access_seaql_migrations` | `platform_tenants, platform_applications, platform_environments, platform_service_accounts, platform_api_credentials(+scopes, suspensions, restrictions), platform_principals, platform_identity_changes, platform_access_audit, platform_production_activations` (14) | Identity authority `down()` drops tables (SEC-015); no audit retention policy |
| privacy-relay | `services/privacy-relay/migration/src/` | 17 | SeaORM raw SQL; `privacy_relay_seaql_migrations` | `communication_contexts(+participants/directions/channels), privacy_relay_sessions, protected_contacts, contact_fingerprints (keyed HMAC), relay_number_*, privacy_relay_outbox, *_retention_*` (28) | Table name `communication_contexts` collides semantically with Hub `communication_*` prefix (separate DBs, naming confusion only) |
| sms-gateway | `services/sms-gateway/migration/src/` | 4 | SeaORM raw SQL; `sms_gateway_seaql_migrations` | `sms_transports, sms_attempts, sms_delivery_events, sms_optout_blocks, sms_bridge_*, sms_outbox_events, sms_usage_records` (12) | — |
| telephony | `services/telephony/migration/src/` | 17 | SeaORM raw SQL; `telephony_seaql_migrations` | `telecom_providers, telecom_routes, telephony_legs, telephony_bridges, provider_events, telephony_captures, telephony_outbox, telephony_retention_*` (20) | — |

Totals: 167 migrations across 10 services, 17 migration-history tables. TIMESTAMPTZ is used consistently everywhere except meeting-service legacy tables (33 naive `.timestamp()` columns; the two raw `TIMESTAMP` columns `dtstart_local`, `original_start_local` are intentional local wall-clock values paired with an IANA zone). `NaiveDateTime` appears only in meeting-service (8 files; `utils/datetime.rs:11` `now_utc()` returns naive UTC).

### DATA-001 — Legacy meeting-service tables use naive TIMESTAMP and NaiveDateTime
- Evidence: `grep -c ".timestamp()"` → users 4, meetings 4, participants 2, devices 1, chat 1, session_logs 1, webhooks 2, api_keys 3, … (33 total; defaults `Expr::current_timestamp()` e.g. `m20260323_000001…:96-98`). Models `models/users.rs:17-19`, `models/meetings.rs:16-20` use `DateTime` (naive); API DTOs `dto/api_keys.rs:23-39` expose `NaiveDateTime` (no offset in JSON); `utils/datetime.rs:11-13`.
- Risk: `DEFAULT CURRENT_TIMESTAMP` into a `timestamp` column stores the DB session's local time — wrong if the server TZ ≠ UTC; comparisons with v2 TIMESTAMPTZ (`legacy_meeting_id` dual-read) can drift; clients receive offset-less strings.
- Owner: BACKEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Remedy: `ALTER TABLE … ALTER COLUMN x TYPE timestamptz USING x AT TIME ZONE 'UTC'` per legacy table; models to `DateTimeUtc`; DTOs to RFC 3339 `Z`. Migration risk: full table rewrite + ACCESS EXCLUSIVE lock (small legacy tables → minutes at most); must assert `SHOW timezone = 'UTC'` historically, otherwise convert with the actual zone. Reversible. Complexity: MEDIUM. Confidence: CONFIRMED

### DATA-002 — `users.email` uniqueness and lookups are case-sensitive
- Evidence: `m20260116_000001_create_users.rs:47-50` `unique_key()` on raw email; register/login/OAuth filter `Column::Email.eq(&payload.email)` (`handlers/auth.rs:44,225,984-987`) without normalisation; rate limiter lowercases (`rate_limiter.rs:31`); v2 uses `to_ascii_lowercase()` for verified email (`actor.rs:337`), webinar index uses `lower(email)` (`m20261005_000005…:201`).
- Impact: duplicate accounts `Foo@x.com`/`foo@x.com`; both map to the same lowercase "verified email" in v2 invitation/organization matching; login failures for case variants; per-email throttle bypass.
- Severity: P1 (identity integrity). Owner: BACKEND.
- Remedy: normalise on write (trim + lowercase) and add `CREATE UNIQUE INDEX CONCURRENTLY users_email_lower ON users (lower(email))` after a duplicate report/merge. Migration risk: index creation fails if case-duplicates already exist → pre-check query and manual merge required. Complexity: MEDIUM. Confidence: CONFIRMED

### DATA-003 — Secrets at rest in meeting-service schema
- `api_keys.api_key` plaintext (SEC-006); `webhooks.secret` plaintext (`handlers/webhooks.rs:118-126`), contrasted with v2 `meeting_invitations.token_hash` / `conference_participants.credential_hash` (`m20261003_000001…:179,205`) and devplat sealed webhook secrets.
- Severity: P1. Remedy: hash API keys; seal or drop legacy webhook secrets (DATA-007). Migration risk: one-way hash backfill. Confidence: CONFIRMED

### DATA-004 — Identity and organization data duplicated across services without a single documented authority map
- Users/accounts: meeting-service `users` (credentials, status, username) ↔ Platform Access `platform_principals` ↔ Calling `platform_principals` (projection, `m…000044_platform_access_projection.rs:1-30`) ↔ Hub `communication_profiles` (`display_name`, `activation_state`, `credentials_not_before`) ↔ meeting `account_communication_state.credentials_not_before` ↔ Calling/Messaging `*_communication_revocations.credentials_not_before` (BIGINT epoch).
- Organizations: meeting-service `organizations`/`organization_memberships` (its migration header claims "No organization/membership source exists elsewhere in the monorepo", `m20261004_000001_organizations.rs:3-6` — no longer true), developer-platform `devplat_organizations`/`devplat_workspace_members` (roles OWNER/ADMIN/DEVELOPER/ANALYST/BILLING), Platform Access `platform_tenants`, Calling `platform_tenants` projection, Calling `enterprise_departments`.
- Impact: suspension/deactivation/display-name changes must be propagated by events; divergence produces inconsistent access (e.g., user suspended in meeting-service but Hub profile ACTIVE until event delivery). Role vocabularies differ (meeting OWNER/ADMIN/MEMBER vs devplat 5 roles).
- Owner: PRODUCT_DECISION + BACKEND. Classification: PARTIALLY_IMPLEMENTED. Severity: P2.
- Remedy: publish an ADR "authority map" (field → owning service → projections → propagation event → reconciliation job); add periodic reconciliation (Hub already has `/internal/v1/communications/reconciliation/range`); decide whether meeting `organizations` merges into Platform Access tenants or devplat organizations. No immediate schema change. Confidence: CONFIRMED

### DATA-005 — Four device registries
- `messaging_devices` (MLS), `messaging_push_registrations`, Calling `call_endpoints`, Hub `communication_devices` (links by `chat_device_ref`/`call_endpoint_ref`, terminal revocation trigger, `002_unified.sql:87-113`), legacy meeting `audio_video_devices` (model exists, no handler uses it — dead table).
- Risk: revocation must fan out; Hub device REVOKED while messaging device active (or vice-versa) until owner reconciliation. Closure §9 row 12 claims "owner reconciliation" verified in stage.
- Severity: P2 (consistency) / P3 (dead legacy table). Remedy: keep Hub as index but add reconciliation metric/alert; drop `audio_video_devices` after legacy retirement (destructive; take backup). Confidence: CONFIRMED (structure); reconciliation behaviour REQUIRES_VALIDATION.

### DATA-006 — Three block stores
- Hub `communication_block_bridge` (source column), `messaging_blocks`, Calling `calling_block_rules`. Hub-launch refusal after block is certified (closure §5), but direct-API paths rely on each owner's copy.
- Severity: P2. Remedy: document propagation (`/internal/v1/communications/block-events`), add drift reconciliation test. Confidence: CONFIRMED (tables); propagation REQUIRES_VALIDATION.

### DATA-007 — Legacy meeting webhooks: accepted and stored, never delivered
- `handlers/webhooks.rs` CRUD; `grep -rn "endpoint_url\|webhooks::" src` outside handlers/models/dto/swagger → none (no dispatcher). Swagger documents them (`swagger/mod.rs:66-69`).
- Classification: PARTIALLY_IMPLEMENTED (fields accepted but no behaviour). Severity: P2 (customers believe they are subscribed). Remedy: deprecate in Swagger and point to developer-platform webhooks (SSRF-safe, signed); later drop table. Confidence: CONFIRMED

### DATA-008 — Dual legacy/v2 meeting model still live
- `meetings`↔`meeting_spaces/conference_sessions` (`legacy_meeting_id UNIQUE … ON DELETE SET NULL`), `participants`↔`conference_participants`, `chat_messages`↔`conference_messages`↔Messaging conversations (`conference_chat_*` migration tables). Shadow-read compare (`handlers/meetings.rs:433-438`). Legacy writes still enabled by default (`FEATURE_LEGACY_MEETING_WRITE_DISABLED=false`).
- Severity: P2. Remedy: owner decision to set the flag in production, run final backfill/parity, then freeze legacy tables read-only (revoke INSERT/UPDATE from runtime role). Migration risk: none until drop; dropping is destructive (backup first). Confidence: CONFIRMED

### DATA-009 — Conflicting FK delete semantics and no account-deletion path
- `meetings.host_id → users ON DELETE CASCADE` (`m20260116_000002…:76-82`) and `participants.user_id SET NULL`, while v2 `meeting_spaces.owner_user_id … ON DELETE RESTRICT`, `meeting_invitations.inviter_user_id RESTRICT`, `conference_participants.user_id RESTRICT` (`m20261003_000001…:22,175,202`), `organization_memberships.user_id CASCADE`; `conference_audit_events` is append-only by trigger. Deleting a user therefore either silently cascades legacy history or fails on v2 rows. There is no delete/erase endpoint (only `POST /api/v1/users/me/deactivate`) and no PII-erasure job for meeting-service (contrast privacy-relay contact erasure, messaging deletion requests).
- Severity: P1 (privacy compliance / data integrity). Owner: PRODUCT_DECISION + BACKEND.
- Remedy: define account-deletion policy (pseudonymise `users` row: email → hash, username → "Deleted user", keep id for RESTRICT FKs); change `meetings.host_id` to RESTRICT; add `users.deleted_at`. Migration risk: FK change is metadata-only (`NOT VALID` + `VALIDATE`), low. Confidence: CONFIRMED

### DATA-010 — Retention gaps
- meeting-service purges nothing except artifacts (`grep -rn "DELETE FROM" src` → poll votes, recurrence cleanup, worker lease only): `outbox_events`, `rtc_webhook_events`, `rtc_drift_events`, `session_logs`, `chat_messages`, `conference_messages`, `account_communication_outbox` grow unbounded. developer-platform deletes only idempotency records and rollups (`ledger/mod.rs:760`, `control/idempotency.rs:128`) — `devplat_activity_events`, `devplat_usage_facts`, `devplat_webhook_attempts` unbounded. Platform Access `platform_access_audit`/`platform_identity_changes` no policy. Calling, telephony, privacy-relay, messaging, Hub (`events.rs:354-356` nonce/rate-window purge) do have retention.
- Severity: P2. Remedy: per-table retention policy (product decision for chat/audit), batched purge workers mirroring calling `calling_retention_*` incl. legal-hold table. Confidence: CONFIRMED

### DATA-011 — Migration-history fragmentation ("one new history table per additive migration")
- meeting-service: 3 histories; communications: 2; calling/messaging: 2 each. Readiness requires the **exact** applied set (`communications/migration/src/lib.rs:73-118` `rows.len() == 1 && version == …`; meeting `communications_facade_ready`), and comments say "Never merge the two histories" — so every future additive change needs yet another history table, binary and readiness check to keep old images bootable.
- Risk: operational complexity, easy to forget one history in a release (service panics at start, `main.rs:133-143`), backups/restores must include all history tables.
- Severity: P2. Remedy: switch readiness to "applied ⊇ required-by-this-binary and no unknown versions" (the `pending_migrations` helpers in calling/messaging/devplat already implement unknown-version fail-closed) so one history per service suffices; consolidate at the next maintenance window. Migration risk: metadata only (copy rows between history tables). Confidence: CONFIRMED

### DATA-012 — Inconsistent type for the same concept (`credentials_not_before`)
- TIMESTAMPTZ in meeting `account_communication_state` and Hub `communication_profiles`; BIGINT epoch seconds in `calling_communication_revocations` and `messaging_communication_revocations` (`calling/migration/src/communications.rs:11-14`, messaging `communications.rs:13`).
- Severity: P3. Remedy: document unit in contract; optionally add generated TIMESTAMPTZ column. Confidence: CONFIRMED

### DATA-013 — Migrations run by the runtime role without pre-migration snapshot (data side of SEC-011/SEC-018)
- Severity: P1. See SEC-011/SEC-018. Confidence: CONFIRMED

### DATA-014 — Migration numbering/dating anomalies
- calling 000039 missing (documented in `docs/architecture/part-j-…:90`); developer-platform jumps 5→10; migration names future-dated beyond today 2026-11-06 (`messaging m20261201_000029`, `calling m20261201_000047`, `developer-platform m20261201_000012`, `messaging m20261107…m20261130`, `calling m20261113_000046`). SeaORM orders by the `migrations()` vector, so execution is safe, but a migration generated today with `sea-orm-cli migrate generate` gets an earlier timestamp than already-applied ones → confusing ordering, merge-review mistakes. `m20260205_110126_m20260205_000001_add_guest_role` double-prefixed name.
- Severity: P3. Remedy: adopt sequence-only names going forward (`…_000048_…`), add a CI check that new migrations append to the vector and sort last. Confidence: CONFIRMED

### DATA-015 — Missing audit-history tables in the account authority
- No `account_audit_events`/history for `users` role/status/email/password changes, API key lifecycle, OAuth links (see SEC-013). Platform Access/Calling/Messaging have them. Proposed table: `account_audit_events(id uuid pk, occurred_at timestamptz not null, actor_user_id uuid null, subject_user_id uuid not null, action text check(...), request_id text, details jsonb)` + append-only trigger reused from `cf_audit_immutable()`. Migration risk: additive, none. Severity: P1. Confidence: CONFIRMED

### DATA-016 — Missing business/identity fields on `users`
- No `auth_provider_subject` (Google `sub` discarded, `handlers/auth.rs:971-974`), no `email_verified_at` (verification encoded only in `status = active`), no `deleted_at`, no `credentials_not_before` (lives in Hub-bridge table only), no `timezone`/`locale`/`avatar` (display data lives in Hub `communication_profiles.display_name/avatar_reference`, so meeting `users.username` and Hub `display_name` can diverge).
- Severity: P2. Remedy: additive nullable columns + unique `(auth_type, auth_provider_subject)`; backfill sub on next Google login. Migration risk: additive, low. Confidence: CONFIRMED (presence/absence); product need for timezone/locale REQUIRES_VALIDATION against the Phase 2 PDF.

### DATA-017 — Cross-service references are opaque (no FKs) — need reconciliation coverage
- By design (messaging/devplat/Hub lib.rs headers): principal/tenant/environment ids are opaque Platform Access ids. Orphans arise if a principal/environment is deleted/suspended and the event is missed. Calling has `calling_orphan_objects` (storage) and Hub has range reconciliation; messaging/devplat/numbering have no identity-orphan sweeper that I found.
- Severity: P3. Remedy: periodic identity reconciliation against `platform_identity_changes` feed (calling already consumes it via `platform_identity_projection_state`). Confidence: REQUIRES_VALIDATION

### Proposed data-model changes (summary, none applied)

| # | Change | Service | Type | Migration risk |
|---|---|---|---|---|
| 1 | `api_keys.api_key_hash`, drop plaintext | meeting | additive → destructive (2 steps) | low; one-way |
| 2 | `users` email normalisation + `UNIQUE (lower(email))` | meeting | data fix + index CONCURRENTLY | fails on existing dups; pre-merge needed |
| 3 | legacy TIMESTAMP → TIMESTAMPTZ (33 cols) | meeting | type change | table rewrite + exclusive lock; verify historical server TZ |
| 4 | `users.auth_provider_subject`, `email_verified_at`, `deleted_at`, `credentials_not_before` | meeting | additive nullable | low |
| 5 | `account_audit_events` append-only | meeting | additive | none |
| 6 | `meeting_idempotency_records` | meeting | additive | none |
| 7 | `meetings.host_id` FK → RESTRICT; deletion = pseudonymisation | meeting | FK swap NOT VALID/VALIDATE | low |
| 8 | retention purge jobs (+ legal-hold table) | meeting, devplat, platform-access | additive + workers | deletes data — owner policy first |
| 9 | single migration history per service (superset readiness) | meeting, communications, calling, messaging | metadata | low; coordinate with rollback images |
| 10 | migrator/runtime role split | meeting | ownership transfer | short maintenance |
| 11 | drop `audio_video_devices`, legacy `webhooks` | meeting | destructive | after legacy retirement + backup |
| 12 | partial unique index `participants(meeting_id) WHERE is_screen_sharing` | meeting | index | fails if current duplicates exist |

---


## 3. Data-model changes proposed by the domain audits (not applied)

| Proposal | Driver finding(s) | Storage change | API exposure change | Frontend change | Migration risk |
|---|---|---|---|---|---|
| Expose `created_by` (and the creator's display name) in `OrganizationResponse`; render `created_at` | ORG-003 | none (already stored) | add fields (additive) | render | LOW |
| Organization profile fields: `description`, `organization_type`, `registered_location` / `operating_location` (country, region, city), `website`, optional `legal_incorporation_date` (distinct from workspace `created_at`), `creation_source` | ORG-003, ORG-004 | additive nullable columns on `organizations` | new `GET /api/v2/organizations/:id` + `PATCH` | detail/edit page | LOW (additive, nullable) |
| Organization invitations table (`organization_invitations`: org, email/user, role, token hash, status, expiry, inviter) | ORG-005 | new table | invite/accept/decline/revoke/list | invite UI | LOW–MEDIUM |
| Organization audit events (`organization_audit_events`) and account security audit (`account_audit_events`) | ORG-007, SEC-013, DATA-015 | new append-only tables | admin read endpoints | admin views | LOW |
| Organization lifecycle: `archived_at`, `closed_at`, ownership-transfer support, last-owner guard | ORG-008 | additive columns + constraint | transfer/archive endpoints | UI | MEDIUM (constraint on existing data: verify every org has ≥1 OWNER first) |
| Cross-space agenda index: `conference_sessions(scheduled_start_at)`, plus membership/registration join paths | MTG-005, MTG-018 | indexes | `GET /api/v2/me/conferences?from&to&status` | calendar views | LOW (online index creation) |
| Recurrence: materialization horizon or virtual expansion; exceptions bound to the rule version | MTG-008, MTG-014 | rule version FK on exceptions | range query expands virtually | — | MEDIUM (backfill exceptions → rule version) |
| Reminder / notification scheduling (`meeting_reminders` or outbox-driven) | MTG-006 | new table or outbox events | none (worker) | preferences | MEDIUM |
| Case-insensitive email (`citext` or `lower(email)` unique index) | DATA-002, ACC-023 | index change | normalize on write | — | **MEDIUM–HIGH:** existing case-variant duplicates must be detected and merged before the unique index is created |
| Convert 33 legacy `TIMESTAMP` columns to `TIMESTAMPTZ` | DATA-001 | `ALTER COLUMN … TYPE timestamptz USING … AT TIME ZONE 'UTC'` | none | — | MEDIUM (table rewrite and locks on large legacy tables; needs a maintenance window or a batched shadow column) |
| Hash legacy API keys and webhook secrets (or retire them) | PLT-002, DATA-003 | hash column + backfill | lookup by hash | — | MEDIUM (one-way: existing plaintext must be hashed in place; keys shown once) |
| Session model (`account_sessions`: id, device, created, last_seen, revoked_at) with a token `sid` claim | ACC-007, ACC-002 | new table | list/revoke sessions | security settings UI | MEDIUM (token format change; dual-accept window) |
| Resolve FK delete conflict (`meetings.host_id` CASCADE vs v2 RESTRICT) and define account erasure | DATA-009, ACC-011 | FK change + erasure procedure | deletion/export endpoints | UI | HIGH (data-destructive path; needs legal/retention decision first) |
