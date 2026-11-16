# CF Meet — production activation and backend closure

**Date:** 2026-11-16 · **Environment:** production (`https://api.cfmeet.com`, `https://www.cfmeet.com`)
**Supersedes** the activation and closure tables of: `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md` (2026-11-12), `CFMEET_FINAL_PRODUCTION_COMPLETION.md` (2026-11-13), `CFMEET_ORGANIZATIONS_AND_MESSAGING_PRODUCTION_COMPLETION.md` (2026-11-13), `CFMEET_FIRST_PARTY_ACTIVATION_FINAL_REPORT.md` (2026-11-14) and `CFMEET_FIRST_PARTY_BACKEND_ENGINEERING_CLOSURE.md` (2026-11-15). Those documents remain as history; where they disagree with this one, this one reflects production today.
**Notification Service** status is owned by [Document 10](../audits/notification-service/10-engineering-report-n2-n15.md) and is only summarised here.

<!-- EVIDENCE: filled in after the release (see §5) -->

## 0. Verdict

* **First-party backend engineering is closed.**
  * Every implemented, tested and internally provisioned first-party feature is active in production (§2).
  * The Organizations Service is authoritative. Its public API now has its own Swagger document (§3).
* **Nothing safely activatable remains switched off.** Every switch that is still off has a stated reason (§2.3, §4):
  * an external provider or a contract;
  * an owner decision or owner credential;
  * a client release;
  * a regulated-data assessment.
* **Not claimed:**
  * Resend activation (suspended by the owner);
  * any external telecom, APNs or FCM delivery;
  * an independent cryptographic assessment;
  * Notification Service cutovers or operational acceptance;
  * any time-based observation that has not elapsed.

## 1. Services and production status

All 15 service containers are certified by `certify` (§5). Each one:
* runs its journaled revision;
* is ready and connected to its database;
* has every migration applied;
* refuses anonymous requests;
* holds its service identity and grants.

| Service | Container | Production status | Notes |
|---|---|---|---|
| Meeting Service (accounts, sessions, devices, meetings, scheduling, conferences, first-party API host, Swagger host) | `cfmeet-production-backend` | ACTIVE | serves `/api/v1`, `/api/v2`, `/swagger-ui/` |
| Organizations | `cfmeet-organizations` | ACTIVE, **authoritative** (`ORGANIZATIONS_AUTHORITY_MODE=authoritative`) | provisioning, events, automatic bindings on |
| Platform Access | `cfmeet-platform-access` | ACTIVE | application identities, credentials, scopes |
| Communications Hub | `cfmeet-communications` | ACTIVE | contacts, relationships, policy, timeline |
| Calling | `cfmeet-calling` | ACTIVE | personal and enterprise calling, LiveKit media, recording, voicemail |
| Messaging | `cfmeet-messaging` | ACTIVE | personal E2EE (owner release record), organization MANAGED, realtime, Web Push |
| Developer Platform | `cfmeet-developer-platform`, `cfmeet-developer-platform-worker` | ACTIVE | console, credentials, webhooks (active delivery), usage |
| API Gateway | `cfmeet-api-gateway` | ACTIVE | public routing for the Developer Platform, Calling and Messaging families, central webhooks, documentation |
| Notification Service | `cfmeet-notifications`, `cfmeet-notifications-2` | **DEPLOYED DARK** (in development, §6) | no traffic: routing OFF, providers `none` |
| Numbering | `cfmeet-numbering` | DEPLOYED, provider-disabled | needs number providers (§4) |
| Telephony | `cfmeet-telephony` | DEPLOYED, provider-disabled | needs a SIP/PSTN carrier (§4) |
| SMS Gateway | `cfmeet-sms-gateway` | DEPLOYED, provider-disabled | needs an SMS carrier (§4) |
| Privacy Relay | `cfmeet-privacy-relay` | DEPLOYED, provider-disabled | needs relay numbers (§4) |

Supporting runtime:
* LiveKit (media and egress), PostgreSQL 16 and Redis are running.
* The security scan gateway is deployed; MANAGED attachments are READY only after a CLEAN scan.
* Object storage is private, with AES256 server-side encryption.
* Edge TLS is renewed by the certificate monitor.
* The web app is the Vercel production build of `main`.

## 2. Features and whether they are active

### 2.1 Active in production

The evidence column cites the activation run or the acceptance run that proves the feature live.

| Area | Feature | Switch / state | Evidence |
|---|---|---|---|
| Accounts | Registration, sign-in (incl. Google), sessions, sign-out everywhere, profile | on | certify; every acceptance suite signs in |
| Accounts | Linked devices: one primary mobile, several web devices, web-only accounts, replacement and revocation | on | E2EE 16/16 (43702913188) |
| Accounts | Structured `VALIDATION_ERROR` details, field-level password errors | on | released 43873042221, probed in production |
| Meetings | Meetings v2, meeting spaces, scheduling and RFC 5545 recurrence, calendar | `FEATURE_MEETING_V2_ENABLED=true` | Meetings 22/22 (43710752927) |
| Meetings | Conference realtime, waiting room, webinar mode, collaboration, quality telemetry, RTC provider control | `FEATURE_CONFERENCE_REALTIME_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_QUALITY_TELEMETRY_ENABLED`, `FEATURE_RTC_PROVIDER_CONTROL_V2` = true | first-party features 42730700547; Meetings 22/22 |
| Meetings | Meeting and organization notification email (ledger, categories, idempotency) | `NOTIFICATIONS_EMAIL_ENABLED=true`, provider SMTP | delivery currently fails at the SMTP provider (§4, A1) |
| Organizations | Authority, provisioning, event delivery to the Meeting Service, automatic Calling and Messaging bindings | authoritative; `ORGANIZATIONS_PROVISIONING_ENABLED`, `ORGANIZATIONS_SERVICE_BINDINGS_ENABLED` = true | `activate-provisioning` 42709421203, `activate-org-events` 42710192324, `org-cutover-first-activation` 43479482331, `activate-organization-bindings` (journaled); certify `organizationsEventDelivery` PASS |
| Organizations | Organizations, memberships, invitations, teams, policies, lifecycle, audit, workspaces | on | Organization acceptance (§5) |
| Organizations | Organization Enterprise Calling and Messaging | `ENTERPRISE_CALLING_ENABLED=true`, `ORGANIZATION_CALLING_SCOPE=organization` | `activate-enterprise-calling` 43482739979; Enterprise Calling 21/21 (43702221217); Organizations and Messaging 18/18 (43708792961) |
| Calling | Personal app calling, LiveKit media, offers acknowledged by devices, cancelled-call history, media-admission retry | `CALLING_APP_CALLING_ENABLED`, `CALLING_NEW_CALLS_ENABLED` = true | personal calling 24/24 (43709750585); issue verification 43947118829 |
| Calling | Voice-to-video upgrade, screen sharing, add participants to a call | on (`CALLING_SCREEN_SHARE_ENABLED` default on) | issue verification 43947118829: C5, C6 and C7 PASS |
| Calling | Call recording, voicemail, organization forwarding | `CALL_RECORDING_ENABLED`, `VOICEMAIL_ENABLED`, `CALLING_ORGANIZATION_FORWARDING_ENABLED` = true | recording 12/12 (43713116133) |
| Messaging | Personal E2EE (MLS, device keys, key transparency `UNWITNESSED`) | `MESSAGING_E2EE_DEVICES_ENABLED=true` under the owner's release record | `activate-personal-encryption` 43663108367; E2EE 16/16; real-website journey 11/11 (43705489163) |
| Messaging | Organization MANAGED conversations, scanned attachments, realtime | `MESSAGING_SESSION_DEVICES_ENABLED=true` | Organizations and Messaging 18/18 |
| Messaging | CF Meet attachment policy: 25 MiB; IMAGE, DOCUMENT, FILE, VOICE_NOTE, VIDEO | `CFMEET_FIRST_PARTY` baseline | capability verified live in 43947118829 |
| Messaging | Web Push (VAPID key generated on the VPS) | adapter on | `activate-web-push` 43706099426 |
| Communications Hub | Contacts, relationships, blocks, policy, timeline, capabilities, meeting-notice preferences | on | Communications acceptance; `activate-meeting-notice-preferences` 43788212474 |
| Developer Platform | Console, applications, credentials, customer webhooks (active delivery), usage export | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`, `DEVELOPER_PLATFORM_WEBHOOKS_ENABLED` = true, `DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE=active`, `CFMEET_USAGE_EXPORT_ENABLED=true` | `activate-developer-console` 43169453546, `activate-developer-webhooks` 43173688215, `activate-usage-export` 43192643956; Developer acceptance 30/30 (43709293595) |
| API Gateway | Public routing, Messaging and Calling account families, central webhooks, CORS for both web origins, documentation | `API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED` = true | `activate-gateway-families` 43174233820, `activate-gateway-docs` 43630780918; certify |
| Security | Public host refuses Meeting operator routes; internal routes never public | on | `activate-public-internal-denial` 43788404790; certify: 25 of 25 internal routes refused |

### 2.2 Activated or completed in this closure

| Item | Before | Now |
|---|---|---|
| Organizations public API documentation | Organization operations sat inside the Meetings document under a stale "FEATURE_DISABLED until authoritative" description | dedicated **Organizations** entry in the Swagger selector (`/api-docs/organizations.json`), 44 operations in 11 sections, documented once (§3) |
| Backup and restore drill | failing on every push since the Notification Service joined the backup catalog | covers every catalog service, including the Notification Service and both notification-fence migrations |

The audit of every production switch (certify feature flags, §5) found no further first-party switch that was off while its feature was complete, tested and provisioned. §2.3 lists the switches that are deliberately off.

### 2.3 Implemented but deliberately not switched on

| Switch | What turning it on would do | Why it stays off | Classification |
|---|---|---|---|
| `API_GATEWAY_WEBSOCKETS_ENABLED` | an alternative WebSocket path through the gateway | every WebSocket channel (messaging realtime, calling signaling, conference realtime) is already live and authenticated on its direct route; moving live clients changes the transport of every connected call and chat for no functional gain | not needed (alternative transport, no feature behind it) |
| `MEETING_PLATFORM_ACCESS_ENABLED` | machine (platform-credential) access to 21 Meeting operations | the operations use the same `/api/v2` paths as the live web and mobile app, so they can only reach the Meeting Service if `/api/v2` moves behind the gateway | owner decision on the edge topology |
| `DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED` | customer applications request production access | needs at least one named reviewer (`CFMEET_PRODUCTION_ACCESS_REVIEWERS`) | owner configuration (§4) |
| `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM` | refuse sign-ins that never declared a device | the mobile app does not declare its device yet; enforcing would sign out every mobile user | client release (mobile) |
| Email verification enforcement | refuse unverified accounts | email delivery is failing (A1), and it would lock out existing unverified accounts | owner policy plus A1 |
| `FEATURE_RECORDING_V2_ENABLED` | Meeting composite recording | needs a Meeting artefact bucket isolated from Calling's (not provisioned) and the owner's retention decision (DATA-010) | owner decision and provisioning |
| Transcription and intelligence v2 | captions, transcripts, AI summaries | no speech-to-text or AI provider is contracted | external provider |
| `FEATURE_HEALTHCARE_PROFILE_ENABLED` | healthcare spaces and appointment access | needs the HealthNet appointment-authorization endpoint, a data classification and DPIA, and the retention decision | regulated-data assessment and owner |
| `CALLING_SYNTHETIC_INTERCONNECT_ENABLED`, `CALLING_WEBHOOK_DELIVERY_ENABLED` | test interconnect; Calling's own legacy webhook sender | test-only; webhooks are delivered by the Developer Platform (`CALLING_WEBHOOK_AUTHORITY=retired`) | not applicable in production |

## 3. APIs and their Swagger locations

### 3.1 Public Swagger: `https://api.cfmeet.com/swagger-ui/`

The documentation is read-only (Try-it-out is disabled), served with pinned assets, and filtered on the server.

| Selector | Document | Operations | Served at (unchanged URLs) |
|---|---|---|---|
| Meetings | `/api-docs/openapi.json` | 224 | `/api/v1`, `/api/v2` (account bearer session) |
| **Organizations** | `/api-docs/organizations.json` | 44 | `/api/v2/organizations…`, `/api/v2/me/organizations`, `/api/v2/me/workspaces…`, `/api/v2/organization-invitations…` |
| Communications | `/api-docs/communications.json` | 73 | Communications Hub |
| Calling | `/api-docs/calling.json` | 131 | `/v1/calls`, `/v1/calling`, `/v1/enterprise` |
| Numbering / DCNs | `/api-docs/numbering.json` | 52 | documented; not routed until a provider exists |
| Privacy Relay | `/api-docs/privacy-relay.json` | 17 | documented; not routed |
| SMS Gateway | `/api-docs/sms-gateway.json` | 6 | documented; not routed |
| Messaging | `/api-docs/messaging.json` | 97 | `/v1/conversations`, `/v1/messaging`, native first-party paths (`x-cfmeet-native-path`) |
| Developer Platform | `/api-docs/developer-platform.json` | 73 | `/v1/developer-platform…`, `/v1/organizations`, `/v1/environments` … |
| API Gateway | `/api-docs/api-gateway.json` | 1 | gateway context |

**Organizations ownership:**
* The Organizations Service is authoritative for organization data.
* The `/api/v2` organization routes relay to it with the signed-in user as the verified acting user.
* Organization Calling and Messaging relay allowlisted paths to the Calling and Messaging services, which keep their own data.
* Developer Platform "organizations" (`/v1/organizations` in its document) are API workspaces, a different resource.

### 3.2 Gateway documentation

The API Gateway serves these at `https://api.cfmeet.com`:
* `/docs`: reference UI;
* `/openapi.json`: aggregate;
* `/openapi/{domain}.json`: one document per domain;
* `/discovery`: family states.

Disabled telecom families are reported as disabled.

### 3.3 Protected internal documentation

The private engineering catalogue has 21 documents (`scripts/swagger/build-private-docs.py`), built offline and served only behind authentication (`infra/deployment/private-api-docs.nginx.conf.example`). It includes:
* `Organizations (internal)`;
* Platform Access, Messaging, Communications, Developer Platform and Calling internal;
* Telephony, SMS and Privacy Relay internal.

None of these is reachable on the public host: certify probes 25 internal, operator and callback routes, and none answers 2xx. `/api-docs/organizations-internal.json` answers 404 (tested).

### 3.4 Realtime

| Channel | Route | Authentication |
|---|---|---|
| Messaging realtime | `wss://api.cfmeet.com/v1/realtime` | single-use subprotocol ticket |
| Calling signaling | `wss://api.cfmeet.com/v1/calling/signaling` | signaling credential |
| Conference realtime | `wss://api.cfmeet.com/api/v2/conferences/{id}/realtime` | session |

## 4. External integrations that remain blocked

| Integration | Needed from | State |
|---|---|---|
| Resend email (E1) | owner: Resend credentials and activation | **SUSPENDED by owner decision**; not activated (PR #88 belongs to the Notification Service track) |
| Current SMTP transport | owner: valid SMTP credentials | **failing** (SMTP 535 authentication rejected); configuration untouched |
| PSTN / SIP telephony, public inbound/outbound calling, callback relay, enterprise SIP | a contracted carrier | disabled (`TELEPHONY_ENABLED=false` and related) |
| SMS | an SMS carrier | disabled |
| Phone numbers and porting | number inventory providers | disabled |
| Privacy Relay, Contact Vault, relay number pool | relay numbers (depend on the providers above) | disabled |
| APNs (iOS) and FCM (Android) push | Apple `.p8` key and team/key ids; Firebase service account | not configured; Web Push active |
| Speech-to-text and AI summaries | a contracted provider | not contracted |
| HealthNet appointment authorization | HealthNet endpoint and token, plus DPIA | not configured |
| Developer production-access reviewers | owner: `CFMEET_PRODUCTION_ACCESS_REVIEWERS` | not set (§5) |
| Key-transparency witness; independent cryptographic assessment | external parties | not performed (owner release decision records both) |

## 5. CI, deployment and certification evidence

<!-- EVIDENCE TABLE -->

## 6. Work still under development (not complete)

| Work | Owner | State |
|---|---|---|
| Unified Notification Service N3–N15 | Notification Service track (Document 10) | deployed dark: N2 foundation, N6 owner APIs and the N8 relay are verified. Still to come:<ul><li>N7 producer publications (only the fence foundation is built);</li><li>client integrations FE2–FE5;</li><li>N9 to N12 cutovers;</li><li>N13 monitoring deployment;</li><li>N14 canary.</li></ul>Routing is OFF and providers are `none`. **No notification migration is complete, and no operational acceptance window has passed.** |
| Notification inbox UI (web and mobile) | frontend | not built |
| Mobile calling client (offer acknowledgement, upgrade, screen share, add people) | mobile | not built; the server APIs are live |
| Mobile attachment upload, groups in common, shared-media screens | mobile | not built |
| Web shared-media thumbnails; Hub contacts in "Add people" | web | data available; UI not built |
| Web Push subscription (service worker) | web | key published; no client subscribes yet |
| Mobile device declaration (unblocks `ACCOUNT_DEVICE_DECLARATION_REQUIRED_FROM`) | mobile | not built |
| Time-based gates | — | not claimed: the Notification Service 24-hour dark observation and 30/60-day acceptance windows; certificate renewal is monitored continuously |
