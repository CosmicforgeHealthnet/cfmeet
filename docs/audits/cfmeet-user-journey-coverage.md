# CF Meet — End-to-End User Journey Coverage

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.

> **Status rules.** `PASS` requires end-to-end evidence through real services: a CI run against a real database and stack, or a recorded production verification. Mocks or isolated endpoint tests never count. A journey that works on the backend but cannot be completed in a shipped client is `PARTIAL` or `FAIL`. This audit **traced** every journey through the code; it did **not execute** any journey against a live environment. Where the status cites CI or production evidence, that evidence is quoted from the run records.


## 1. Journey dashboard

| ID | Journey | Status | Can it be completed today? | Blocking findings |
|---|---|---|---|---|
| A | New user: register → verify → profile → discover contacts → call / message | **PARTIAL** | Register, log in and message organization colleagues: yes. Discover personal contacts, use a CF Meet ID/QR or invites: no UI. Verification is not enforced. | ACC-009 (→MSG-006), ACC-016, ACC-021 |
| B | Organization: create → details → invite → permissions → org meeting | **PARTIAL** (final step FAIL in UI) | Only a platform admin can create (curl only). No detail view, no invitations, no org meeting in the UI. | ORG-002, ORG-003, ORG-004, ORG-005, MTG-016 |
| C | Scheduled meeting: space → date/time → schedule → calendar → invite → reschedule → join → end → history | **PARTIAL** | Schedule, reschedule, join and end: yes (web, backend CI-verified). Calendar, invitation emails, reminders and change notifications: no. The list shows the creation date. | MTG-004, MTG-005, MTG-006, MTG-012, MTG-013 |
| D | Instant meeting (web): create → join details → invite → admit → media → end | **PARTIAL** | Yes on web for open meetings (real media is not CI-verified: LiveKit is mocked in `backend.yml`). Consent-gated or invite-only joins fail. | FE-001, MTG-013 |
| E | Calling: contact → authorized call → ring → answer → media → reconnect → end → history | **PARTIAL** | Yes, if the callee has the Calls page open and enabled. The callee cannot end the call, and abandoned calls never terminate. | CAL-001, CAL-002, CAL-003, CAL-004, CAL-015 |
| F | Messaging: create → send → deliver → read → reconnect → sync → history | **PARTIAL** | DIRECT MANAGED chat works with 15 s polling. No realtime in production, REST re-sync is broken, receipts are not shown. | MSG-001, MSG-004, MSG-005 |
| G | Enterprise: identity → members/extensions → queue/ring group → internal call → admin review | **PARTIAL** | Backend yes (CI + production API acceptance). In the app: one shared tenant, admin by environment allowlist, raw-JSON editors. | CAL-007, ORG-012, CAL-017 |
| H | Device security: add device → authenticate → push → revoke → all sessions invalid | **FAIL** | Revoking a device leaves the account JWT valid. Push is never registered. | ACC-001, ACC-002, ACC-008, ACC-010, CAL-003 |
| U | Unified communications: chat → call / meeting link → timeline | **NOT_TESTED** | No client implements it, and no CI test joins the Hub with the real owners. | MSG-006, MSG-009, MSG-010 |
| P1 | Outbound app → PSTN | **BLOCKED_EXTERNAL** | No carrier contract; Telephony not deployed; flags off. Software passes against simulators. | CAL-010 |
| P2 | Inbound PSTN → app / extension | **BLOCKED_EXTERNAL** | Same, plus no real numbers | CAL-010, CAL-011 |
| V1 | Voicemail deposit | **PASS** | Yes: production acceptance plus CI `rtc_voicemail_through_egress` (real Egress) | — |
| V2 | Voicemail playback by an ordinary user (web) | **FAIL** | Backend PASS. The web restricts it to an admin allowlist and passes no principal binding. | CAL-006 |
| I | Guest joins a consent-gated or invite-only meeting from a link (web/mobile) | **FAIL** | Consent checkboxes never render, so join returns 403 `CONSENT_REQUIRED`. The invitation flag is read from a field the backend does not send. | FE-001 |
| J | Password recovery: forgot → email → verify token → reset → old sessions revoked | **PARTIAL** | The reset flow exists in code but `forgot-password`, `verify-reset-token` and `reset-password` have **no automated test** (API inventory `test_ref_count = 0`). Old tokens keep working on v1 routes for up to 7 days. | ACC-002, ACC-003, INV-002 |
| K | Google sign-up / sign-in | **PARTIAL** | Works for unique short names. Fails for repeated or long display names. Can take over a pending password account. Tokens are returned in the URL. No tests. | ACC-004, ACC-005, ACC-006 |
| L | Developer onboarding: org → project → environment → credential → API call via gateway → webhook | **PARTIAL** | Backend real-stack CI exists, but the gateway and Developer Platform are **not deployed** and there is no console UI. One gateway operation is always denied. | PLT-005, PLT-004, PLT-001 |
| M | Recording → artifact list → playback / download (web) | **PARTIAL** | Backend CI-verified. The web artifact view reads fields the backend does not send (file name, type, duration). | FE-007 |
| N | Mobile meeting: list → schedule → instant → join → in-room controls | **FAIL** | Every step fails: wrong v1 shapes, 35 non-existent v2 paths, a room shell with no media SDK, a hard-coded LAN API URL. | MTG-001, MTG-002, MTG-003, FE-004, FE-005, FE-006 |
| O | Account deactivation / deletion / data export | **FAIL** | No deletion or export. Deactivation does not propagate to Platform Access principals, or to Messaging/Calling for users without a Hub profile. | ACC-011, ACC-010, DATA-009 |

**Totals:** 20 journeys traced (0 executed live by this audit). PASS 1 · PARTIAL 11 · FAIL 5 · BLOCKED_EXTERNAL 2 · NOT_TESTED 1.

## 2. Cross-journey observations

1. **The backend usually works; the client path is what fails.** In 11 of the 20 journeys (A–G, I, L, M, V2), the backend steps have CI evidence against real PostgreSQL or a real stack, and the failure is in the client, the relay, the deployment or the configuration.
2. **There is no browser or mobile end-to-end test for any meeting, calling, organization or account journey.** The only browser journey in CI is the web Messaging E2EE journey, which is red because of frontend issue #37 (FE-016 / MSG-014). Web v2 meeting test suites are not run in CI (FE-015).
3. **Error and retry behaviour:**
   - Mobile re-POSTs join every 3 s while waiting (FE-005).
   - The web treats a 400 on event re-sync as "no events" (MSG-001).
   - Calling media does not auto-reconnect after LiveKit gives up (CAL-015).
   - Abandoned calls are re-reconciled forever (CAL-001).
4. **Authorization problems on journey paths:**
   - Tokens survive revocation (ACC-002).
   - A shared calling credential gives one enterprise namespace for every organization (CAL-007).
   - Organization membership is granted without consent (ORG-005).

## 3. Journey traces by domain

The step-by-step traces below come from the domain audits. Each step maps to its operation, handler and evidence.

### 3.1 Meetings and scheduling (Journeys C, D and the final step of B)


### Journey C — Create Meeting Space → choose date/time → schedule → calendar → invite → reschedule → join → end → history (web)
| # | Step | Operation / handler | Status |
|---|---|---|---|
| 1 | Create Meeting Space | POST /api/v2/meeting-spaces → `handlers/v2/spaces.rs:create_meeting_space` → `meeting_space.rs:441`; web `CreateMeetingSpaceContent.tsx:46-59` | PASS backend (e2e01, real DB); web IMPLEMENTED_UNVERIFIED |
| 2 | Choose date/time | Not part of create; web space page `ConferencesPanel.tsx` "Schedule occurrence" (browser TZ, not space TZ) | PARTIAL (MTG-012, MTG-021) |
| 3 | Schedule | POST /api/v2/meeting-spaces/{id}/conferences → `conferences.rs:create_conference` → `conference.rs:297` | PASS backend (e2e03, m_e2e_01) |
| 4 | Calendar | No calendar API/UI; `/dashboard/schedule` redirects; list cards show `createdAt` | FAIL (MTG-004, MTG-005, MTG-007) |
| 5 | Invite | POST /meeting-spaces/{id}/invitations (link token; no email) / POST …/members | PARTIAL (links PASS e2e12; email MISSING MTG-006) |
| 6 | Reschedule | POST /api/v2/conferences/{id}/reschedule → `conference.rs:768`; no attendee notification | PARTIAL (PASS e2e04_05 for API; notifications MISSING; end-wipe MTG-009) |
| 7 | Join | Host: dashboard Start (POST /conferences/{id}/start, window-checked) then pre-join POST /conferences/{id}/join → `admission.rs:552`; attendees blocked until started | PARTIAL (backend PASS m_e2e_01 with LiveKit mock; pre-join UX MTG-013) |
| 8 | End | POST /api/v2/conferences/{id}/end → `conference.rs:625` | PASS backend (e2e18, m_e2e_01) |
| 9 | History | GET /meeting-spaces/{id}/conferences (all statuses), GET /conferences/{id}/attendance, /recordings, /artifacts; web ConferenceWorkspace (attendance not shown) | PARTIAL (per-space only, MTG-005) |
Overall: **PARTIAL** — backend core (create/schedule/reschedule/cancel/start/join/end) is exercised against real PostgreSQL in CI (`tests/api_v2_e2e.rs`, `tests/system_e2e.rs`) with mock LiveKit; calendar, notifications and list display fail; no browser E2E exists for this journey.

### Journey D — Instant meeting: create → join details → invite → admit → connect media → end
| # | Step | Operation / handler | Status (web) | Status (mobile) |
|---|---|---|---|---|
| 1 | Create | POST /api/v2/instant-meetings → `spaces.rs:create_instant_meeting` → `meeting_space.rs:471` (LIVE immediately) | PASS backend; web `DashboardContent.tsx` | FAIL — mobile uses v1 `/meetings/instant` and misreads response (MTG-001) |
| 2 | Join details | Response `{meetingSpace{meetingCode}, conference}`; web routes to `/{meetingCode}` → GET /resolve/{code} | PASS | FAIL |
| 3 | Invite | Copy link `/{meetingCode}` (space card) or POST …/invitations | PASS (link) | n/a |
| 4 | Admit | GET /conferences/{id}/waiting-room, POST …/waiting-room/admit|deny (`handlers/v2/participants.rs`) | PASS backend (e2e07_08, e2e09); web ParticipantsPanel | FAIL — legacy v1 waiting endpoints with space id (MTG-002) |
| 5 | Connect media | Join response `rtc{url,credential}` → LiveKit | IMPLEMENTED_UNVERIFIED (CI mock; real LiveKit tests `#[ignore]`) | v2 join used, media likely connects; controls broken |
| 6 | End | POST /api/v2/conferences/{id}/end | PASS backend | FAIL — `POST /api/v1/meetings//end` (MTG-002) |
Overall: **PARTIAL** (web: backend-verified except real media; mobile: FAIL).

### Journey B — last step: schedule an organization meeting
| Step | Operation / handler | Status |
|---|---|---|
| Create org-scoped space | POST /api/v2/meeting-spaces with `organizationId` + `accessPolicy: ORGANIZATION_ONLY` (`meeting_space.rs:153,452` verify membership) | Backend PASS for access (`tests/organization_domain_access.rs`); web cannot send these fields → FAIL in UI |
| Schedule occurrence | POST …/conferences | PASS backend |
| Org members discover it | No org listing; members not auto-added | FAIL (MTG-016) |
Overall: **FAIL** from the product UI; backend capability exists.


### 3.2 Organizations and Enterprise (Journeys B and G)


### Journey B — Create organization → retrieve details → invite members → manage permissions → schedule an organization meeting

| Step | Operation (METHOD path → handler) | Status | Evidence / notes |
|---|---|---|---|
| B1 Create organization | `POST /api/v2/admin/organizations` → `handlers/v2/organizations.rs:48-59` → `services/organizations.rs:144-186` | PARTIAL | Platform-admin only (`:151-152`); end user 403 (test org03 L211-222). No web/admin UI (ORG-002). Alternative `POST /v1/organizations` (DP, self-service) creates a **different** entity not visible on the Organizations page (ORG-001). |
| B2 Retrieve details | `GET /api/v2/me/organizations` → `handlers/v2/organizations.rs:142-155` → `services/organizations.rs:318-341` | PARTIAL | No `GET /organizations/{id}` (ORG-004). Response: id, name, slug, status, createdAt + role; web renders name/slug/status/role only; createdAt not rendered; createdBy/location absent (ORG-003). |
| B3 Invite members | `PUT /api/v2/organizations/{id}/members/{userId}` → `handlers/v2/organizations.rs:119-131` → `services/organizations.rs:241-298` | PARTIAL | Direct add by UUID, no invitation/acceptance (ORG-005). Web form `OrganizationsContent.tsx:155-206`. |
| B4 Manage permissions | same PUT (role OWNER/ADMIN/MEMBER, status) + `GET /api/v2/organizations/{id}/members` → `services/organizations.rs:300-315` | IMPLEMENTED (backend verified in CI) | org03 asserts admin cannot mint admins/touch owners, no self-promotion, member cannot list (CI DB test). Fixed role set only; members shown as UUIDs (ORG-006). |
| B5 Schedule org meeting | `POST /api/v2/meeting-spaces` with `organizationId` + `accessPolicy: ORGANIZATION_ONLY` → `meeting_space.rs:452` (`verify_organization` `:305-314`), then conferences; admission `meeting_access.rs:331-341` | PARTIAL | Backend covered by org01 (CI DB: member creates & joins, non-member/foreign org 403). **Web cannot set organization or ORGANIZATION_ONLY** (ORG-010); no org calendar (ORG-011); mobile none. |

**Overall: PARTIAL.** Backend chain (admin-created org → direct add → roles → org-restricted meeting → join) is exercised by DB integration tests `org01`/`org03` (run in CI, not executed by this audit). The journey is not completable by an end user in web or mobile: B1 has no UI and is admin-only, B2 lacks a detail view/metadata, B3 lacks invitations, B5 has no UI. Not PASS — no end-to-end UI evidence.

### Journey G — Enterprise: configure enterprise identity → manage members/extensions → queue/ring group → internal call → history and admin review

| Step | Operation (METHOD path → handler) | Status | Evidence / notes |
|---|---|---|---|
| G1 Configure enterprise identity | Customer path: `POST /v1/organizations` → `POST /v1/organizations/{id}/projects` → environments → service accounts → credentials (DP `http/public.rs:45-216`, delegated to PA `delegation.rs:82-90`); Calling projects identity (ADR 0032 §6). Enterprise policy `POST /v1/enterprise/policies` (`enterprise/http.rs:61-62`, scope `enterprise.config.write` L356). CF Meet app path: fixed tenant/app/env from env vars (`lib/calling/server.ts:83-88`), users linked to principals via `GET /api/v2/messaging/me` (`communications.rs:136`, `unified_messaging/identity.rs:145`). | PARTIAL | DP flow backend-only, console deferred (ORG-017); DP tests use FakePlatformAccess. In-app: single shared tenant, not per organization (ORG-012). Meeting orgs play no role. |
| G2 Manage members/extensions | `POST/GET /v1/enterprise/extensions` (`enterprise/http.rs:85-87`), `PUT /v1/enterprise/departments/{id}/members` (`:80-81`) | IMPLEMENTED (Calling CI DB tests) | `tests/enterprise_extensions.rs:21,256` (namespace, cross-scope 404). "Members" are PA principals, not org members. Web: generic resource pages (`lib/calling/resource-routes.ts`) gated by `CALLING_ADMIN_PRINCIPAL_IDS` (`route.ts:58-68`). |
| G3 Queue / ring group | `POST /v1/enterprise/queues` (`:102`), `PUT …/queues/{id}/members` (`:108-109`), `POST /v1/enterprise/ring-groups` (`:89-90`), `PUT …/ring-groups/{id}/members` (`:99-100`) | IMPLEMENTED (Calling CI DB tests) | `enterprise_queues.rs:138-863` (FIFO, offers, overflow, races), `enterprise_ring_groups.rs:123-677`. Web resource page for ring groups (`resource-routes.ts:103-110`) and queues. |
| G4 Internal call | `POST /v1/calls` with `targetExtension` (`calls/http.rs`; test `enterprise_extensions.rs:335-380`: ENTERPRISE_INTERNAL, no telephony, idempotent) | IMPLEMENTED (Calling CI DB test) | Web dial-by-extension `CallsContent.tsx:182`; BFF forces `initiatorPrincipalId` and own endpoint (`route.ts:200-211`). Real-media end-to-end not evidenced here. |
| G5 History & admin review | `GET /v1/calls` (`calls/http.rs:49`, test `call_history.rs`), `GET /v1/enterprise/audit` (`enterprise/http.rs:83`), recordings access-audit | IMPLEMENTED_UNVERIFIED (UI) | Web: calls list (principal-filtered by BFF `route.ts:201-202`); audit via `resource-routes.ts:262` (admin allowlist only). Admin review is not organization-scoped. |

**Overall: PARTIAL.** Calling's enterprise backend (extensions, ring groups, queues, internal extension calls, audit) is well covered by DB-backed tests and is tenant-isolated, but (a) enterprise identity provisioning has no customer UI (developer console deferred; DP tests fake Platform Access), (b) in the CF Meet app all users share one fixed tenant/environment and enterprise administration is an env-var allowlist, and (c) there is no relationship between a CF Meet organization and an enterprise calling scope (ORG-012). No end-to-end evidence across DP → PA → Calling → web was found.


### 3.3 Accounts and device security (Journeys A and H)


### Journey A — Register → verify account → configure profile → discover contacts → initiate Calling/Messaging

| # | Step | Operation (handler) | Status | Notes |
|---|---|---|---|---|
| A1 | Register | POST /api/v1/auth/register (`handlers/auth.rs:33`) — web `auth.service.ts:20`, mobile `auth.api.ts` | IMPLEMENTED_UNVERIFIED | Exercised by the certification real stack. Enumeration (ACC-014); no rate limit (ACC-015); case-sensitive email (ACC-023) |
| A2 | Verify email | POST /verify-email (`auth.rs:145`), resend (`:459`) | IMPLEMENTED_UNVERIFIED | No automated test. Not enforced: the next steps work without it (ACC-016). Mobile routing is broken (ACC-012/022). Google path auto-verifies, with the takeover risk (ACC-004) |
| A3 | Login | POST /login (`auth.rs:211`) or Google (`:794-1049`) | IMPLEMENTED_UNVERIFIED | Google: tokens in URL (ACC-005), name collisions (ACC-006); mobile has no Google sign-in |
| A4 | Configure profile | GET/PATCH /api/v1/users/me (`handlers/users.rs`) | PARTIALLY_IMPLEMENTED | Username only, no avatar (ACC-021). Hub profile and privacy (`/api/v2/communications/me`, `/me/privacy`) have no UI (ACC-009) |
| A5a | Discover contacts (personal) | Hub GET /me/public-id, /me/qr, POST /discovery/public-id/resolve, POST /contact-requests, POST /contact-requests/:id/accept (`communications/src/api.rs:298-540`) via meeting relay `services/communications.rs:224-320` | FRONTEND_ONLY_GAP | Backend tested in the real 5-service stack (`personal_communications.rs:612-1139`, CI `communications-hub.yml:132`). Zero web/mobile consumption |
| A5b | Discover colleagues (web) | GET /api/v2/messaging/directory (`handlers/v2/unified_messaging.rs:264`) | IMPLEMENTED_UNVERIFIED | Works only for members of verified organizations. A user with no org finds nobody |
| A6a | Initiate Messaging | Device enrol: POST /api/v2/messaging/devices/registration-challenges, /devices, /key-packages, /client-tokens (`routes/v2.rs:429-452` → Messaging); conversation create via relay | IMPLEMENTED_UNVERIFIED | Web `lib/messaging/engine.ts`; real-stack test enrols devices (`personal_communications.rs:222-260`). Mobile: none |
| A6b | Initiate Calling from contact | Hub POST /contacts/:id/calls (`calls.rs:166`) → Calling launch | FRONTEND_ONLY_GAP | Web calls instead go through BFF `app/api/calling` with a platform credential and `/messaging/me` identity (`lib/calling/server.ts:31-90`), outside Hub policy. Mobile: none. Calling push missing (ACC-020) |

**Overall: PARTIAL.** The backend chain register → login → Hub activation → contact exchange → message/call policy has real-stack certification evidence. I did not verify that the CI job's latest run passed. In the shipped apps, the journey stops after A4 for personal contacts (no UI). Messaging works only between organization colleagues, and verification is not enforced. No end-to-end UI test covers A1–A6.

### Journey H — Add device → authenticate → register notifications → revoke device → confirm all related sessions invalid

| # | Step | Operation (handler) | Status | Notes |
|---|---|---|---|---|
| H1 | Add device | Messaging: POST /api/v2/messaging/devices (`unified_messaging.rs` → Messaging `http/devices.rs:184`). Calling: POST /v1/calling/endpoints (web BFF). Hub registry: POST /api/v2/communications/devices/link (`devices.rs:122`) | PARTIALLY_IMPLEMENTED | Owner registries work. The Hub unified registry is never populated by clients (ACC-008). There is no device concept for the CF Meet account session itself |
| H2 | Authenticate device | CF Meet JWT (not device-bound); Messaging POST /client-tokens (device-bound, TTL 900 s); Calling signaling credential (endpoint-bound, ≤3600 s) | PARTIALLY_IMPLEMENTED | Account session not bound to the device (ACC-007) |
| H3 | Register notifications | Hub PUT /devices/:id/push → Messaging internal `/communications/devices/push` (`devices.rs:227-289`); Messaging adapters | FRONTEND_ONLY_GAP / EXTERNAL_DEPENDENCY | No client registers push (no service worker or expo-notifications). Calling push not implemented (ACC-020). Provider live certification not done |
| H4 | Revoke device | Hub POST /devices/:id/revoke (`devices.rs:291`) → Messaging `/communications/devices/revoke`, Calling `/communications/endpoints/revoke`; or POST /api/v2/messaging/devices/:id/revoke | IMPLEMENTED_UNVERIFIED | Messaging revoke is thorough (`devices/mod.rs:809-905`: MLS removal, client sessions, push disabled, history transfer cancelled). Calling revoke cancels live ring offers. No UI except the web logout path |
| H5 | Confirm all related sessions invalid | — | **FAIL** | (a) Messaging client sessions and push for that device: revoked (code, plus `messaging/tests/devices.rs`). (b) Calling endpoint: REVOKED for HTTP. An open WebSocket stays until credential expiry (ACC-024). (c) **CF Meet access/refresh JWT: NOT revoked.** The Hub says so explicitly (`devices.rs:355`). The refresh token remains a 7-day bearer on v1 even after logout or password change (ACC-001/002). (d) Account-level revocation (password change, deactivation) does not reach Messaging or Calling for accounts without a Hub profile (ACC-010) |

**Overall: FAIL.** Revoking a device does not invalidate the account session, and there is no per-device account session to revoke. Cross-service gaps:
- Account → Messaging: only logout revokes sessions directly.
- Account → Hub → Messaging/Calling: dropped when no Hub profile exists.
- Account → Platform Access: no principal status sync.
- Hub → Account: no session-revoke callback.

The Messaging- and Calling-owned parts are individually sound.


### 3.4 Calling, enterprise calling, PSTN and voicemail (Journeys E, G-calling, P1, P2, V1, V2)


### Journey E — Discover contact → authorized call → ring → answer → media → reconnect → end → history (web)

| # | Step | Operation (handler) | Status |
|---|---|---|---|
| 1 | Sign in; resolve calling identity | Web GET `/api/calling/identity` → meeting-service GET `/api/v2/messaging/me` (`unified_messaging.rs` l.239); scope must equal `CALLING_*` env (`server.ts` l.83–88) | Implemented; single-scope (CAL-007) |
| 2 | Discover contact | GET `/api/v2/messaging/directory` (org colleagues only) + POST `/directory/resolve`; Hub contacts not used | PARTIAL (CAL-014) |
| 3 | Register device | POST `/v1/calling/endpoints` via adapter (`endpoints/http.rs`); manual button | PARTIAL (CAL-004) |
| 4 | Signaling online | POST `/v1/calling/signaling-credentials`; WS `/v1/calling/signaling` (`signaling/ws`) | Implemented, tested (`signaling.rs`) |
| 5 | Create authorized call | POST `/v1/calls` (`calls/app/http.rs` l.53 → `create.rs`, personal policy `communications.rs` l.69, concurrency `lifecycle.rs` l.290) | Backend COMPLETE_VERIFIED |
| 6 | Ring callee devices | `ringing.rs` signals; GET `/v1/calling/invitations` recovery; **no push** | Backend verified; delivery only to open `/calling` tab (CAL-003/004) |
| 7 | Answer (single winner) | POST `/v1/calling/invitations/{id}/accept` (signaling route) | COMPLETE_VERIFIED (`app_races.rs`) |
| 8 | Media | POST `/v1/calling/sessions/{sid}/media-credentials` → LiveKit via edge gateway `/v1/calling/media/rtc/v1` | COMPLETE_VERIFIED (rtc cert, production real media) |
| 9 | Reconnect | Signaling auto-reconnect (`useCallingDevice.ts`); media rejoin manual (`CallMedia.tsx` l.20) | Backend verified; web PARTIAL (CAL-015) |
| 10 | End | POST `/v1/calls/{id}/end` (`control.rs` l.162) — initiator only in web; callee "Leave" does not end call | FAIL for callee hang-up (CAL-001/002) |
| 11 | History / missed | GET `/v1/calls?principalId&missed` (`calls/http.rs` l.79) | COMPLETE_VERIFIED backend; web wired |

**Overall: PARTIAL.** Backend steps 4–8 and 11 are verified by real-PostgreSQL + real-LiveKit CI and by production API acceptance (run 41580265031). The web journey has never been executed end-to-end (CAL-022), and step 10 is functionally broken for the callee and for transferred calls (CAL-001/002); step 6 depends on the user sitting on the Calls page.

### Journey G (calling part) — Queue / ring group → internal call → history → admin review

| # | Step | Operation (handler) | Status |
|---|---|---|---|
| 1 | Admin configures extension, ring group, queue, IVR, hours | `/v1/enterprise/{extensions,ring-groups,queues,ivr-flows,business-hours}` (`enterprise/http.rs`) via JSON admin page; requires env admin allowlist | Backend COMPLETE_VERIFIED; web PARTIAL (CAL-007/017) |
| 2 | Caller dials extension | POST `/v1/calls` with `targetExtension` → enterprise router (`calls/app/enterprise/router.rs`) | COMPLETE_VERIFIED (`enterprise_extensions.rs`, rtc) |
| 3 | IVR keypad | POST `/v1/calls/{id}/ivr/input` (initiator) | COMPLETE_VERIFIED (production IVR PASS) |
| 4 | Ring group / queue offers to agents | `ring_groups.rs`, `queues.rs` l.817+ (`available_agents` l.948) | COMPLETE_VERIFIED; agents starve if they hold a ghost call (CAL-001) |
| 5 | Agent answers; media | same as Journey E 7–8 | COMPLETE_VERIFIED |
| 6 | Transfer to extension | POST `/v1/calls/{id}/transfer` (`transfer.rs`) | COMPLETE_VERIFIED; after transfer nobody can End in web (CAL-002) |
| 7 | Voicemail on no answer | `calls/app/enterprise/voicemail.rs` + Egress capture | COMPLETE_VERIFIED (production PASS) |
| 8 | History incl. "answered elsewhere"/missed for group members | GET `/v1/calls?principalId` | COMPLETE_VERIFIED (`call_history.rs::ring_group_members_see_answered_elsewhere_or_missed`) |
| 9 | Admin review: audit, queue entries, recordings | GET `/v1/enterprise/audit`, `…/queues/{id}/entries`, `/v1/enterprise/recordings/{id}` | PARTIAL: audit lacks human actor (CAL-007); recordings not discoverable (CAL-017) |

**Overall: PARTIAL.** Backend flow verified in CI (real PG, real LiveKit, SIPp for SIP) and production API acceptance run 41581639199 (ring groups, queues, IVR, transfer). Web admin is operator-allowlisted JSON editing over a single shared scope; no web E2E.

### PSTN journeys

| Journey | Path | Status |
|---|---|---|
| Outbound app → PSTN | POST `/v1/calls` (`targetContactReference`/public) → routing engine → spend reservation (`telephony/spend.rs`) → Telephony `POST /internal/v1/legs/originate` → Asterisk → carrier | **BLOCKED_EXTERNAL**: web adapter refuses (`route.ts` l.205–209), flags off, Telephony not deployed, no carrier (CAL-010). Simulated E2E passes (`public_app_to_phone.rs`, `communications_e2e` with SIPp). |
| Inbound PSTN → app/extension | Carrier → Asterisk → Telephony → Calling `POST /internal/v1/telephony/inbound-calls` / `dcn-inbound-calls` → ring | **BLOCKED_EXTERNAL** (same; also needs DCNs, CAL-011). Simulated: `public_phone_to_app.rs`, `dcn_calls.rs`. |
| Voicemail deposit (app caller) | Unanswered extension → voicemail route → LiveKit Egress caller-only capture → object storage (SSE) | **PASS** in production acceptance (ops doc l.33) and CI `rtc_voicemail_through_egress`. |
| Voicemail playback (owner) | GET `/v1/enterprise/voicemail/messages?mailboxId&principalId` → POST `…/media-access` (≤900 s URL) → PUT `…/read-state` | Backend **PASS** (production acceptance "owner playback; caller/other-tenant denial; READ state; deletion"); **web FAIL for ordinary users** (admin allowlist + no principal binding, CAL-006). |
| Voicemail deposit from PSTN/SIP caller | Asterisk capture → Telephony `capture.rs` → Calling `/internal/v1/telephony/captures` | BLOCKED_EXTERNAL / not deployed (CAL-010/016); CI `enterprise_capture_egress.rs`, telephony `enterprise_capture.rs`. |
| SMS send/receive | SMS Gateway → provider | BLOCKED_EXTERNAL (CAL-012). |
| Privacy relay masked call | Context → session → relay number → Telephony | BLOCKED_EXTERNAL (CAL-013, CAL-010/011). Simulated `privacy_relay_e2e.rs`. |


### 3.5 Messaging and unified communications (Journeys F and U)


### Journey F — Create conversation → send → deliver → read → reconnect → synchronize → retrieve history (first-party web, MANAGED and E2EE)

| # | Step | Operation (handler) | Status |
|---|---|---|---|
| 1 | Identity | `GET /api/v2/messaging/me` (Meeting `handlers/v2/unified_messaging.rs` → `relay.rs:663-680`) | IMPLEMENTED. Exercised by the CI web journey (steps pass) |
| 2 | Create DIRECT | `POST /api/v2/messaging/relay/v1/conversations` + Idempotency-Key → Messaging `conversations/http.rs:35` → `service.rs` create | PASS in CI for DIRECT E2EE and MANAGED (web journey steps "encrypted conversation created", "managed conversation labelled", run 42182739453) |
| 2b | Create GROUP + add members | `POST …/conversations` then `POST …/members` (`membership/mod.rs:470-540`) | BE-verified. Web realtime for added members broken (MSG-001) |
| 3 | Send | `POST …/messages` → `messages/mod.rs:445` → `managed.rs:252` / `e2ee.rs` | PASS (E2EE, CI). MANAGED send BE-verified (`messaging.rs:445`) |
| 4 | Deliver (live) | outbox → broker → WS `event`; client `ack` → `advance_delivery` (`connection.rs:593-740`, `messages/mod.rs:925`) | E2EE in CI: PASS for DIRECT. **Production: BLOCKED.** No WS for first-party users (MSG-004); delivery only via the 15 s poll |
| 5 | Delivery receipt shown | `delivery.cursor.updated` event | FAIL. Not rendered (MSG-005) |
| 6 | Read | `PUT …/read-cursor` (`messages/mod.rs:847`) on conversation open (`MessagesContent.tsx:281`) | Write IMPLEMENTED. Display of others' read state missing (FRONTEND_ONLY_GAP) |
| 7 | Reconnect | new client token → ticket → WS (`messaging-session.ts`, `messaging-realtime.ts`) | PASS in CI (step "reconnect") for E2EE staging config. BLOCKED in production (MSG-004) |
| 8 | Synchronize | WS `subscribe{afterEventSequence:0}` + REST `GET …/events?afterEventSequence=` | FAIL on the REST event path (400, MSG-001). The message-level sync by `afterSequence` (`engine.ts:882, 964`) works, so content converges; event cursor recovery doesn't |
| 9 | Retrieve history | `GET …/messages?afterSequence=…&limit` forward only; `beforeSequence` unused | PARTIAL. The whole history is loaded forward from 0, with no back-pagination (MSG-005) |

Overall: **PARTIAL**. The backend path is COMPLETE_VERIFIED in Messaging integration tests against real PostgreSQL/WebSockets/Redis (`tests/realtime.rs:288,416,836`, `messaging.rs:527`). The shipped web client passes create/send/receive/offline/reconnect for DIRECT conversations in CI before failing on #37. It is not PASS because (a) the REST event-sync step is broken (MSG-001), (b) receipts aren't shown, and (c) in the production configuration there is no realtime at all (MSG-004), so steps 4 and 7 are BLOCKED there.

### Journey U — Unified communications: chat → start call / share meeting link → timeline

| # | Step | Operation (handler) | Status |
|---|---|---|---|
| 1 | Open conversation in the unified app | client consumes Messaging relay + Hub `GET /api/v2/communications/conversations/{id}/context` | FAIL. No web/mobile consumer of the Hub (MSG-006) |
| 2 | Start call from chat | `POST /api/v2/communications/conversations/{id}/calls {kind, clientRequestId, endpointId}` → Meeting ingress → Hub `calls.rs:208` → Calling `/internal/v1/communications/calls` | Backend IMPLEMENTED: Hub tested with a simulated Calling (`unified.rs:406`), Calling owner route tested (`communications_owner_routes.rs`); production claim in closure doc :138 (not re-verified). No UI → NOT_TESTED end-to-end from a client |
| 3 | Call signaling/media | Calling signaling WS (another domain) | out of scope here |
| 4 | Share/instant meeting into chat | `POST …/conversations/{id}/meetings/instant|share` → Hub `meetings.rs:94-178` → Meeting `/internal/v1/communications/meetings/instant|resolve` | Backend IMPLEMENTED (Hub `unified.rs:819` simulated owner; Meeting side `tests/communications_meetings.rs`). Produces Hub cards only, no chat message (MSG-009) |
| 5 | Timeline | `GET …/conversations/{id}/timeline` (`calls.rs:453-495`); client merges with Messaging history by `anchorSequence` | Backend IMPLEMENTED (Hub `unified.rs:406,1343`). No realtime (poll `GET /sync`). No client merge implementation |
| 6 | Activity / inbox | `GET /activity`, `GET /inbox` (`calls.rs:497`, `sync.rs:134`) | IMPLEMENTED; inbox overlay-only (MSG-007/016) |

Overall: **NOT_TESTED** (no client implements it; no automated test joins Hub + real Calling + real Meeting + real Messaging). Server components are individually IMPLEMENTED_UNVERIFIED at the cross-service level (MSG-010). The production verification claim in the closure report covers Hub-launched calls only and couldn't be independently confirmed here.


### 3.6 Additional journeys traced by the lead auditor (I, J, K, L, M, N, O)

| Journey | Step → operation (handler) | Evidence |
|---|---|---|
| I — consent-gated guest join | `GET /api/v2/resolve/:identifier` → `GET /api/v2/conferences/:id/pre-join` (`services/collaboration/prejoin.rs:49-100`) → client renders consents from `consentRequirements` (absent) → `POST /api/v2/conferences/:id/join` → `admission.rs:775-786` returns `CONSENT_REQUIRED` | FE-001. The field mismatch was verified by the lead auditor. |
| J — password recovery | `POST /api/v1/auth/forgot-password` → email → `POST /api/v1/auth/verify-reset-token` → `POST /api/v1/auth/reset-password` (sets `credentials_not_before`) → old access/refresh token still accepted by `middleware/auth.rs` (blacklist-only check) | ACC-002 (verified: `middleware/auth.rs:38-39` consults only the token blacklist). The API inventory shows no test references for the three reset routes. |
| K — Google sign-in | `GET /api/v1/auth/oauth/google` → Google → `GET …/callback` (`handlers/auth.rs:838-1049`): code exchange at `oauth2.googleapis.com/token`, ID-token payload base64-decoded (no `aud`/`email_verified` check), a pending account matched by email is activated (`auth.rs:999-1004`), tokens are redirected in the query string | ACC-004 / ACC-005 (verified). The ID token comes straight from Google's token endpoint over TLS, so the missing signature check is lower risk than the missing `email_verified` check and the activation of a pending password account. |
| L — developer onboarding | DP `POST /v1/organizations` → `POST /v1/organizations/:id/projects` → `POST /v1/projects/:id/environments` → `POST /v1/environments/:id/credentials` → gateway `/v1/...` (manifest auth `PLATFORM_CREDENTIAL`) → DP webhooks | PLT-005: not deployed. PLT-001: `contact-access-grants` is always denied. PLT-004: no console. |
| M — recording artifacts | `POST /api/v2/conferences/:id/recordings` → `…/recordings/:rid/stop` → `GET /api/v2/conferences/:id/artifacts` → `POST …/artifacts/:aid/access` | FE-007 (web reads `filename`, `mimeType`, `readyAt`, `durationSeconds`; the backend sends `contentType`, `durationMs`, `finalizedAt`) |
| N — mobile meeting | mobile `/api/v1/meetings` (wrong body/response shape) and `/api/v2/spaces`, `/api/v2/instant`, … (non-existent) | MTG-001/002/003, FE-002/003/004. 35 non-existent paths were reproduced by the lead auditor's route cross-check. |
| O — deactivation / deletion | `POST /api/v1/users/me/deactivate` → account event → Hub drops it for users without a Hub profile → Messaging/Calling credentials remain → no deletion/export route exists in any service | ACC-010, ACC-011, DATA-009 |
