# CF Meet — Frontend ↔ Backend Contract Coverage

**Audit phase:** Phase 1 — audit only, no implementation · **Repository:** `CosmicforgeHealthnet/cfmeet` · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` (`staging` = `main`) · **Date:** 2026-11-06

**Related:** [executive report](cfmeet-platform-comprehensive-audit.md) · [feature gap register](cfmeet-feature-gap-register.md) · [API inventory](cfmeet-api-inventory.csv) · [implementation plan (proposal)](cfmeet-recommended-implementation-plan.md)

> **Method and evidence rules.** Every finding was established by reading the code at the baseline commit. The file and line references are to that commit. No application code, schema, contract, configuration or deployment was changed, and no production host was contacted. No test suites were run by this audit: the sandbox has no service databases, and building would write into the repository. A feature is `COMPLETE_VERIFIED` only where an automated test exercises it against a real database or real stack **in CI**. Those CI runs were read, not re-executed. The latest backend CI evidence is on `e717d018`, an ancestor of the baseline whose six later commits change only docs, infra and workflows. The lead auditor re-checked the highest-severity claims directly; see "Lead-auditor verification" below.

## Lead-auditor verification and cross-checks

- **The call inventory was cross-checked independently.** The lead auditor joined all 388 frontend call sites to the 1,066 statically extracted backend operations and the 572 gateway routes. Results:
  - **Web:** every concrete REST path resolves to a registered backend route. The only unmatched web rows are annotations for WebSocket URLs, server-side proxies and presigned-storage URLs, and each of those was traced by hand.
  - **Mobile:** 35 distinct `/api/v2` call sites resolve to no route at all (listed in the [meetings audit](cfmeet-meetings-calendar-audit.md#mobile-v2-calls-that-hit-non-existent-backend-routes)). The mobile v1 calls resolve to real routes but use request and response shapes the backend does not have (FE-002 / MTG-001).
  - The per-operation join is the `frontend_refs` / `frontend_consumed` columns of [`cfmeet-api-inventory.csv`](cfmeet-api-inventory.csv). 266 operations have at least one frontend consumer: 261 public (meeting-service 181, Calling 51, Messaging 27 via the meeting relay, Developer Platform 2) and 5 admin. 394 public or realtime operations have no frontend consumer.
- **FE-001: verified.** `services/meeting-service/src/services/collaboration/prejoin.rs:49-100` serializes `access{accessPolicy, authenticationRequired, invitationRequired, passcodeRequired, …}` and `consents{…}`. The web reads `requiresInvitation` and `consentRequirements` (`apps/web/src/types/v2/index.ts:772-774`, `components/room/pre-join-screen.tsx:172-173`). As a result, `admission.rs:775-786` refuses join with `CONSENT_REQUIRED`.
- **Issue #37: confirmed FRONTEND.** The GitHub issue body records that the backend jobs of the same workflow pass. Commit `ebf28ce` moved `DevicePanel`, which holds the `recover-history` button, into a collapsed toggle. No backend defect is implicated, and this audit does not reclassify it.
- **Ownership summary.** Of the frontend findings, 15 are FRONTEND, 4 PRODUCT_DECISION, 2 SHARED_CONTRACT and 1 INFRASTRUCTURE. The missing backend data that the screens need, such as a cross-space agenda, organization details and an inbox model, is recorded as BACKEND findings in the respective domain audits (MTG-005, ORG-004, MSG-007).

### Severity reconciliation in the master register
- **FE-002** — consolidated into **MTG-001** in the register
- **FE-003** — consolidated into **MTG-003** in the register
- **FE-004** — consolidated into **MTG-002** in the register
- **FE-008** — consolidated into **CAL-006** in the register
- **FE-009** — consolidated into **CAL-005** in the register
- **FE-010** — consolidated into **CAL-008** in the register
- **FE-011** — consolidated into **ACC-004** in the register
- **FE-013** — consolidated into **PLT-004** in the register
- **FE-016** — consolidated into **MSG-014** in the register
- **FE-020** — consolidated into **MSG-006** in the register
- **FE-022** — consolidated into **MTG-001** in the register



Prefix: FE-. Audit only; no repository files were changed. Repo the repository at `586b88c` (staging == origin/main).
Raw call inventory: `the frontend call inventory (joined into `cfmeet-api-inventory.csv`, column `frontend_refs`)`
(388 rows: 285 web + 96 mobile from the static extractor, plus 10 hand-added WebSocket / server-side / presigned-URL rows).

## Scope inspected

**Frontends**
- Web (Next.js 16): every `apps/web/src/app/**/page.tsx` (27 pages) and the two Next route handlers `app/api/calling/[...path]/route.ts` and `app/api/calling-signaling/[...path]/route.ts`. Also all of `src/services/**` (v1 `api.ts`, `auth/user/meeting/api-keys/webhook.service.ts`, `v2/*.service.ts`, `calling/*.service.ts`, `messaging-*.ts`), `src/lib/calling/server.ts`, `src/lib/calling/resource-routes.ts`, `src/lib/messaging/{routes,engine}.ts`, `src/types/**` (including all 1,582 lines of `types/v2/index.ts`), the key components (`components/room/{pre-join-screen,live-room,footer-controls,sidebar}.tsx`, `components/conferences/*`, `components/messaging/MessagesContent.tsx`, `components/calling/*`, `components/dashboard/*`), `hooks/**`, `store/**`, `apps/web/tests/*.mjs` and `.github/workflows/frontend.yml`.
- Mobile (Expo 57 / RN 0.86): all 21 files under `apps/mobile/app/**`, `features/{auth,meetings,meetings/v2,room,room/v2,user}`, `lib/{constants,api/client}.ts`, `app.json`, `package.json` and `.github/workflows/mobile.yml`.

**Backend and contracts**
- meeting-service: `src/routes/{mod,auth,meetings,participants,users,api_keys,webhooks,admin,api,v2}.rs`; DTOs `src/dto/{auth,meetings,users,common,v2}.rs`; `services/collaboration/{prejoin,polls}.rs`, `services/artifacts/{mod,consent}.rs`, `services/quality/service.rs`, `services/admission.rs` (join path, lines 600-790), `services/unified_messaging/relay.rs`, `middleware/legacy_write.rs`, `config/features.rs`, `handlers/auth.rs` (OAuth callback), `handlers/v2/{conferences,realtime}.rs`.
- calling: `src/routes.rs`, `calls/{app/http,http}.rs`, `endpoints/http.rs`, `signaling/http.rs`, `usage/http.rs`, `webhooks/http.rs`, `enterprise/http.rs`, `media/gateway.rs`.
- `packages/contracts/openapi/meeting-service.json` (v1.2.0, 205 operations); `infra/api-gateway/{route-manifest.json,public-routes.json,README.md}`.
- Product spec PDF `CFMeet_Phase2_Frontend_Product_Design_Requirements_v1.0.pdf`, pages 1-14 (TOC, sections 1-12; Appendix A was not needed because I matched against `routes/v2.rs` and the OpenAPI file directly).
- Issue #37 evidence: `docs/operations/cfmeet-sections-8-23-final-closure.md`, `docs/phases/phase-05/evidence/web-messaging-journey-2026-10-20.txt`, and a read-only look at branch `origin/diagnostics/production-browser-crash-20261020` (`git show --stat` and `git show <sha>:file`).

**Method.** A Python extractor (regex over axios/`call(...)`/`fetch(...)` call sites, with template params normalized to `:param`) built the call list. A second script parsed every axum `.route("…", get(..).post(..))` with balanced parentheses, applied the `nest` prefixes from `routes/mod.rs`, and matched each frontend call by method and path against meeting-service, the calling service and the gateway manifest. A third script compared TypeScript interfaces field by field with Rust structs of the same name, applying `serde(rename_all="camelCase")` and `rename`. Every mismatch reported below was then confirmed by reading the code by hand. No unit test was run.

### Call-verification summary
| App / target | Calls | Route exists, method OK | No such route | Notes |
|---|---|---|---|---|
| web → meeting-service `/api/v1` | 37 | 37 | 0 | the v1 chat, media and meeting hooks are reachable only from dead or legacy code (FE-014) |
| web → meeting-service `/api/v2` | 119 | 119 | 0 | paths are all correct; **response shapes do not match** for pre-join and artefacts (FE-001, FE-007) |
| web → messaging relay `/api/v2/messaging/relay/v1/*` | 40 | allowlisted in `lib/messaging/routes.ts` | — | mirrors `services/unified_messaging/relay.rs` ROUTES (spot-checked) |
| web → calling (through the Next adapter `/api/calling/*`) | 52 | backend 52/52 | 0 | **the Next adapter blocks 9** of them (FE-008, FE-009) |
| mobile → `/api/v1` | 30 | 28 | 2* | *false positives from the first parser pass; fixed. Shapes are badly mismatched (FE-002) |
| mobile → `/api/v2` | 64 | 23 | **41 → 35 unique wrong paths** | FE-003 |
| Gateway manifest | — | 129 of 188 web v1/v2 calls are present | 59 absent | absent by design (`LEGACY_DIRECT_INGRESS`, `FIRST_PARTY_APP_BACKEND`). The first-party apps use direct ingress (FE-018) |

## Screen inventory

### Web (`apps/web/src/app`)
| Route | Component(s) | What it does | Data source |
|---|---|---|---|
| `/` | Navbar, Hero | Marketing landing | static |
| `/login`, `/signup`, `/verify`, `/forgot-password{,/verify-token,/reset}` | auth modals | Account auth and recovery | v1 `/auth/*` |
| `/google/callback` | `useGoogleCallback` | Reads tokens from the query string | v1 OAuth redirect (FE-011) |
| `/dashboard` | DashboardContent | Lists spaces; instant meeting | v2 `/meeting-spaces`, `/instant-meetings` |
| `/dashboard/meetings` | MeetingsContent | Space list, filters, actions | v2 |
| `/dashboard/meetings/new` | CreateMeetingSpaceContent | Create wizard | v2 |
| `/dashboard/meetings/[id]` | MeetingSpaceWorkspace (Members, Invitations, Recurrence, Registration, Conferences panels) | Space detail | v2 |
| `/dashboard/meetings/[id]/edit` | EditMeetingContent | PATCH space | v2 (profile-policy PATCH not wired, FE-017) |
| `/dashboard/meetings/[id]/conferences/[conferenceId]` | ConferenceWorkspace (Participants, Recordings, Artifacts, CollaborationPolicy) | Occurrence detail / summary | v2 |
| `/dashboard/schedule` | redirect → `/dashboard/meetings` | — | — |
| `/dashboard/messages` | MessagesContent | E2EE Unified Messaging | v2 messaging bridge + relay |
| `/dashboard/organizations` | OrganizationsContent | My organizations and members | v2 |
| `/dashboard/settings` | SettingsContent (+ApiKeys, Webhooks) | Profile, password, deactivate, v1 API keys and webhooks | v1 |
| `/dashboard/developers` | DevelopersContent (1,057 lines) | **Static docs** for the legacy v1 third-party API | none (FE-013) |
| `/dashboard/calls`, `/calling`, `/calling/[...segments]` | CallsContent, CallingDevicesPage, CallingSettingsPage, UsageDashboard, CallingResourcePage, CallingEnterpriseHome | App calling, devices, history, settings, enterprise and privacy admin | calling service through the Next adapter |
| `/[id]` (room) | PreJoinScreen → LiveRoom | Resolve code, pre-join, join, waiting room, live LiveKit room | v2 (+ a dead v1 branch) |
| `/join` | JoinMeetingInput | Enter code → `/[id]` | — |
| `/invite` | InvitationLanding | Resolve an invitation token | v2 |
| `/register/[id]` | PublicRegistrationForm | Webinar registration | v2 |
| `/server-error` | static | Error page | — |

### Mobile (`apps/mobile/app`)
| Route | What it does | Data source | Works? |
|---|---|---|---|
| `(auth)/login, signup, verify, forgot-password` | Email auth; the Google button shows a "coming soon" toast | v1 `/auth/*` | yes (shapes match) |
| `(tabs)/index` (Home) | Recent meetings, instant meeting, join by code | v1 `/meetings`, `/meetings/instant` | **no** (FE-002) |
| `(tabs)/meetings` | Meeting list, delete | v1 `/meetings` | **no** (envelope mismatch) |
| `(tabs)/schedule` | Schedule meeting (v1 form) | v1 `POST /meetings` | **no** (wrong field names) |
| `(tabs)/settings` | Theme, profile, password, logout; Help, Terms and Privacy are no-ops | — | partial (FE-012) |
| `(meetings)/[id]/edit` | Edit v1 meeting | v1 `PUT /meetings/:id` | **no** |
| `(settings)/edit-profile` | Edit username | v1 `/users/me` | **no** (expects `{user}`) |
| `(settings)/change-password` | Change password | v1 `/auth/change-password` | yes |
| `join` | Code input → room | — | yes |
| `(room)/[id]/index` | Pre-join: v2 resolve and v2 join, polls the join while waiting | v2 `/resolve`, `/conferences/:id/join` | partial (FE-005, FE-001) |
| `(room)/[id]/live` | "Live room" | v1 hooks with an empty id | **non-functional shell** (FE-004) |
| Components not mounted anywhere | `features/meetings/v2/components/*`, `features/room/v2/components/{polls,qna,breakout,room-sidebar}` | mobile v2 module | dead and broken (FE-003) |

## Feature matrix

Columns follow the task's requested set plus the brief's Classification. "Backend status" is from route and DTO inspection only; **no frontend feature is COMPLETE_VERIFIED**, because no automated test drives these screens against a real stack. The exception is the messaging journey, and that one currently fails (#37).

| Screen/feature | App | API operations used | Backend status | Missing fields/APIs | Owner class | Readiness | Classification |
|---|---|---|---|---|---|---|---|
| Email/password auth and recovery | web | POST `/api/v1/auth/{register,verify-email,resend-verification,login,refresh,logout,forgot-password,verify-reset-token,reset-password}` | routes `routes/auth.rs:8-25`; DTOs match (snake_case both sides) | — | FRONTEND | Usable, unverified | IMPLEMENTED_UNVERIFIED |
| Google OAuth | web | GET `/api/v1/auth/oauth/google` (redirect); callback reads `?access_token&refresh_token…` | `handlers/auth.rs:873-880` puts tokens in the query string | Token handoff violates spec §F-01 ("No raw tokens in URLs"); error branch of the callback page is an empty placeholder | SHARED_CONTRACT | Works on the happy path; error UX broken | PARTIALLY_IMPLEMENTED |
| Profile / password / deactivate | web | GET/PATCH `/api/v1/users/me`, POST `/api/v1/auth/change-password`, POST `/api/v1/users/me/deactivate` | `routes/users.rs`, `dto/users.rs:8-26` match `types/user.ts` | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| v1 API keys and webhooks (Settings tabs) | web | `/api/v1/api-keys` GET/POST/DELETE; `/api/v1/webhooks` GET/POST/PATCH/DELETE | routes exist | Spec D-07 is still open (does this belong in the meeting app?) | PRODUCT_DECISION | Usable | IMPLEMENTED_UNVERIFIED |
| Developers docs page | web | none (static) | Documents legacy `/api/v1/api/meetings` on `api.cosmicforge.io` | No UI for developer-platform (orgs, projects, credentials) | PRODUCT_DECISION | Static / misleading | PARTIALLY_IMPLEMENTED |
| Dashboard and space list | web | GET `/api/v2/meeting-spaces`, POST `/api/v2/instant-meetings`, GET `/meeting-spaces/:id/conferences` | `routes/v2.rs:40-72` | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Create space / instant | web | POST `/meeting-spaces`, POST `/meeting-spaces/:id/conferences`, POST `/instant-meetings` | exists; types match | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Space detail: members, invitations, recurrence, registrations, conferences | web | `/meeting-spaces/:id/{members,invitations,recurrence,recurrence/exceptions,registrations,conferences}`, `/invitations/:token/resolve` | exists; types match | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Space settings / edit, duplicate, archive, owner transfer | web | PATCH `/meeting-spaces/:id`, POST `/archive`, `/duplicate`, `/owner-transfer`; GET `/profile-policy` | exists | PATCH `/meeting-spaces/:id/profile-policy` has a service method (`profiles.service.ts:36`) but no UI (hook `useUpdateProfilePolicy` unused) | FRONTEND | Partial | PARTIALLY_IMPLEMENTED |
| Link resolve | web | GET `/api/v2/resolve/:identifier` | exists; `ResolveResponse` matches | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| **Pre-join lobby (passcode, invitation, auth, consent, waiting-room notice)** | web | GET `/conferences/:id/pre-join`, POST `/conferences/:id/join` | `prejoin.rs:49-63` returns `access.*`, `consents.{required,version}`; join enforces consents (`admission.rs:778-784`) | Web reads the non-existent `requiresAuth`, `requiresInvitation`, `waitingRoomEnabled`, `consentRequirements` (`types/v2/index.ts:767-775`) | FRONTEND | **Broken for consent-required, invite-only and auth-required meetings** | CONTRACT_MISMATCH |
| Waiting room (attendee) and host admission console | web | GET `/participants/me`, `/waiting-room`; POST `/waiting-room/{admit,deny}`, `/participants/:pid/return-to-waiting`; POST `/realtime/ticket`; WS `/realtime` | exists | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Live room media (LiveKit) | web | POST `/conferences/:id/media-credential`, LiveKit SDK | exists; depends on the LiveKit provider | — | EXTERNAL_DEPENDENCY | Usable when the provider is up | IMPLEMENTED_UNVERIFIED |
| Host controls: role, mute, remove, ban, locks, spotlight, stop share | web | PATCH `/participants/:pid/role`; POST `/mute`, `/remove`, `/ban`; POST/DELETE `/{audio,video,screen-share}-lock`; POST/DELETE `/spotlights`; POST `/screen-share/:pid/stop` | exists | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| In-meeting chat, hands, reactions | web | GET/POST `/messages`, DELETE `/messages/:mid`, PATCH `/chat-policy`, `/chat-restriction`, `/hand-raise`, `/reactions` | exists; `MessageView` matches | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Breakouts, polls, Q&A | web | `/breakouts/**`, `/polls/**`, `/questions/**` | exists; `PollView`/`PollQuestion` and Q&A types match | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Recording and consent (in room) | web | `/recordings` GET/POST, `/recordings/:rid/stop`, `/consents/requirements`, `/consents`, `/consents/:type/withdraw` | exists; consent DTOs match | — | FRONTEND | Usable when the provider is enabled | IMPLEMENTED_UNVERIFIED |
| Artefacts, attendance, transcripts, reports, intelligence | web | `/artifacts`, `/artifacts/:aid{,/access}`, `/artifacts/reports`, `/attendance`, `/transcripts/**`, `/transcription/**`, `/intelligence/requests` | exists | UI reads `filename`, `mimeType`, `readyAt`, `durationSeconds`, `deletedAt`; backend sends `contentType`, `durationMs`, `finalizedAt`, `deletionPending`, and no filename | FRONTEND | Displays wrong or "Not available" values | CONTRACT_MISMATCH |
| Quality | web | POST `/quality/samples`, GET `/quality` | exists; `QualitySampleInput` matches | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Webinar registration / backstage | web | `/meeting-spaces/:id/registrations/**`, `/conferences/:id/backstage` | exists | — | FRONTEND | Usable when the webinar flag is on | IMPLEMENTED_UNVERIFIED |
| Organizations | web | GET `/me/organizations`, GET `/organizations/:id/members`, PUT `/organizations/:id/members/:uid` | exists | admin create/update service methods have no UI (operator scope) | PRODUCT_DECISION (D-07) | Usable | IMPLEMENTED_UNVERIFIED |
| Unified Messaging (E2EE) | web | 10 bridge ops `/api/v2/messaging/*` + 30 relay ops `/api/v2/messaging/relay/v1/*`; messaging origin `/v1/realtime{,/tickets}` | exists | Production E2EE is blocked externally (#13); web journey fails (#37, FE-016) | EXTERNAL_DEPENDENCY + FRONTEND | Not production-available | EXTERNAL_DEPENDENCY |
| App calling: calls, devices, history, signaling | web | `/api/calling/v1/calls/**`, `/v1/calling/{endpoints,signaling-credentials}`, `/api/calling-signaling/v1/calling/{invitations,history,…}`, WS `/v1/calling/signaling` | calling routes exist | Needs `CALLING_PLATFORM_CREDENTIAL` / tenant env and messaging identity | EXTERNAL_DEPENDENCY | Config-dependent | IMPLEMENTED_UNVERIFIED |
| Calling settings: blocking | web | `/v1/blocks` GET/POST/DELETE | exists | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Calling settings: forwarding, voicemail, usage | web | `/v1/enterprise/forwarding-rules/**`, `/v1/enterprise/voicemail/**`, `/v1/calling/{usage,limits}` | exist (enterprise/admin API) | Next adapter admin-gates them, so ordinary users get 403 | PRODUCT_DECISION | Broken for end users | PARTIALLY_IMPLEMENTED |
| Call recording / consent, abuse report, platform usage | web | `/v1/calls/:id/{recordings,recording-consent,recording-authorizations,abuse-reports}`, `/v1/{usage,limits}` | backend exists (`enterprise/http.rs:217-231`, `calls/app/http.rs:74`) | Next adapter has no allow rule (returns 404); components `CallRecordingControls` and `AbuseReportDialog` are never mounted | FRONTEND | Absent | FRONTEND_ONLY_GAP |
| Enterprise / privacy-relay admin pages | web | generic `CallingResourcePage` over `/v1/enterprise/*` and `/v1/privacy-relay/*` | exist | Raw resource UI only | FRONTEND | Admin-only, generic | IMPLEMENTED_UNVERIFIED |
| Communications Hub (contacts, contact requests, favourites, activity, conversation context) | none | — | 73 routes via `/api/v2/communications/*` relay (`services/communications.rs:145-148`) | No frontend consumer at all | PRODUCT_DECISION | Absent | FRONTEND_ONLY_GAP |
| Calendar view / external calendar sync | none | — | No backend calendar or iCal surface (grep found none) | Spec §10 marks it optional | PRODUCT_DECISION | Absent | NOT_REQUIRED |
| Operator admin (v1 admin users/meetings/logs, v2 compat, chat migration) | none | `chatMigrationService` exists, unused | exists | — | PRODUCT_DECISION | n/a | NOT_REQUIRED |
| Auth (email) | mobile | POST `/api/v1/auth/*` | DTOs match (`dto/auth.rs:26-188`) | Google sign-in is "coming soon" (`login-form.tsx:164-165`) | FRONTEND | Usable; Google missing | PARTIALLY_IMPLEMENTED |
| Profile | mobile | GET/PATCH `/api/v1/users/me` | returns a flat `UserMeResponse` (`dto/users.rs:8-15`) | Mobile expects `{ user: User }` (`features/user/api/user.api.ts:5-8`) | FRONTEND | Broken | CONTRACT_MISMATCH |
| Change password | mobile | POST `/api/v1/auth/change-password` | matches | — | FRONTEND | Usable | IMPLEMENTED_UNVERIFIED |
| Home / meetings list / schedule / edit / instant | mobile | v1 GET/POST/PUT/DELETE `/api/v1/meetings{,/:id,/instant}` | v1 legacy; `{success,data,meta}` envelopes; `start_time`/`end_time`/`timezone` | Mobile uses `meetings`, `scheduled_at`, `duration_minutes`, `{meeting}`, `{meeting,join_data}` | FRONTEND | Broken | CONTRACT_MISMATCH |
| Join by code / pre-join | mobile | GET `/api/v2/resolve/:id`, POST `/api/v2/conferences/:id/join` | exists | No consent/passcode/invite handling; `participantToken` never stored; polling re-joins | FRONTEND | Partially works for public meetings | PARTIALLY_IMPLEMENTED |
| Live room | mobile | v1 participants/chat/end with an empty id | — | No RTC SDK; no v2 room APIs | FRONTEND | Non-functional | FRONTEND_ONLY_GAP |
| In-meeting v2 features (chat, polls, Q&A, breakouts, locks, spotlight) | mobile | 35 wrong v2 paths (`/spaces`, `/qna`, `/hand/raise`, `/lock/audio`…) | correct routes exist under other names | Paths and DTOs wrong; components never mounted | FRONTEND | Dead code | CONTRACT_MISMATCH |
| Spaces management, messaging, calling, organizations, registrations | mobile | none | backend exists | No mobile screens | PRODUCT_DECISION | Absent | FRONTEND_ONLY_GAP |
| Settings: Help / Terms / Privacy | mobile | — | — | Items have no `onPress` (`(tabs)/settings.tsx:179-183`) | FRONTEND | Placeholder | FRONTEND_ONLY_GAP |

### Spec (PDR v1.0) screen → implementation map
| Spec item | Proposed route | Existing screen | Backend APIs | Status |
|---|---|---|---|---|
| F-01 Auth and recovery | `/login`, `/register`… | web `(auth)/*`; mobile `(auth)/*` | v1 auth | web OK; tokens-in-URL issue (FE-011); mobile has no OAuth |
| F-02 Dashboard | `/dashboard` | `/dashboard`, `/dashboard/meetings` | v2 spaces, me/organizations | web OK; mobile on v1 and broken |
| F-03 Create / instant wizard | `/meetings/new` | `/dashboard/meetings/new` | v2 | web OK |
| F-04 Space details / lifecycle | `/meetings/{spaceId}` | `/dashboard/meetings/[id]` | v2 | web OK |
| F-05 Schedule / recurrence | — | RecurrencePanel | v2 recurrence, reschedule | web OK; mobile schedule broken |
| F-06 Membership / invitations | — | MembersPanel, InvitationsPanel, `/invite` | v2 | web OK |
| F-07 Link resolution and pre-join | `/join/{identifier}` | `/join` → `/[id]` PreJoinScreen | v2 resolve, pre-join, consents | **web CONTRACT_MISMATCH (FE-001)** |
| F-08 Attendee waiting room | `/conferences/{id}/waiting` | in-page state inside `/[id]` | v2 participants/me, realtime | web OK, but the route shape differs (FE-017) |
| F-09 Host admission | — | room sidebar | v2 waiting-room | web OK |
| F-10/11/12 Live core, media, host controls | `/conferences/{id}/live` | `/[id]` LiveRoom (keyed by meeting code + `?conference=`) | v2 + LiveKit | web OK; spec wants the conferenceId route (FE-017); mobile absent (FE-004) |
| F-13/14/15/16/17 Chat, hands, breakouts, polls, Q&A | — | room sidebar panels | v2 | web OK; mobile broken (FE-003) |
| F-18/19/20 Recording, artefacts, transcription | `/conferences/{id}/summary` | `/dashboard/meetings/[id]/conferences/[conferenceId]` and room sidebar | v2 | web partial (FE-007) |
| F-21 Webinar | `/webinars/{spaceId}/register` | `/register/[id]` | v2 registrations, backstage | web OK |
| F-22 Healthcare / profile policy | `/meetings/{spaceId}/settings` | `/dashboard/meetings/[id]/edit` (no profile-policy editor) | v2 profile-policy | **partial**; regulated joins are blocked by FE-001 |
| F-23 Quality | — | RoomQualityPanel | v2 quality | web OK |
| F-24 End / leave | — | LiveRoom | v2 leave/end | web OK; mobile leave never calls v2 `/leave` |
| Org members | `/organizations/{id}/members` | `/dashboard/organizations` | v2 | web OK |
| §7.1 / §11.2 "generated typed client, contract parity gate" | — | handwritten types; v2 web tests not in CI | OpenAPI 1.2.0 exists and is correct | **not met (FE-015)** |

## Findings

### FE-001 — Web (and mobile) pre-join reads a PreJoinResponse shape the backend does not return; consent-required joins cannot succeed
- Service/module: apps/web `components/room/pre-join-screen.tsx`, `types/v2/index.ts`; apps/mobile `features/meetings/v2/types.ts`; meeting-service `services/collaboration/prejoin.rs`, `services/admission.rs`.
- Expected: Per OpenAPI 1.2.0 (`components.schemas.PreJoinResponse`) and spec §7.3, `GET /api/v2/conferences/{id}/pre-join` returns `meetingSpaceId, conferenceId, title, meetingType, conferenceStatus, scheduledStartAt, joinable, organizer, organization, access{accessPolicy, authenticationRequired, invitationRequired, passcodeRequired, externalAuthorization, waitingRoomExpected, alreadyAuthorized}, notices, consents{required[], version, granted[]}, media, displayName, devices`. The UI should drive passcode, invitation, sign-in and consent inputs from these fields, then send `consents:[{type, version}]` in join.
- Actual: The web type `PreJoinResponse` (`types/v2/index.ts:767-775`) declares `meetingSpace, conference, requiresAuth, requiresPasscode, requiresInvitation, waitingRoomEnabled, consentRequirements`. None of these exist in the response. `pre-join-screen.tsx:172-184` builds consent checkboxes from `preJoin.data?.consentRequirements`, which is always `undefined`, so the list is always empty and `grantedConsents` is always `[]`. Lines 526-528 never show the invitation-token input, the "Sign in to join" notice or the waiting-room notice. Line 614 never disables Join for auth-required meetings. The backend join enforces consents: `admission.rs:778-784` returns `CONSENT_REQUIRED` (403, `error.rs:248`) when `required` consents are missing. The passcode input still works, but only through the fallback `resolveData.meetingSpace.requiresPasscode` (line 525). Mobile copies the same wrong type (`mobile/features/meetings/v2/types.ts:424-432`) and has no consent UI at all.
- Evidence: `services/meeting-service/src/services/collaboration/prejoin.rs:49-63` (struct), `:84-92` (`PreJoinAccess`), `:107-111` (`PreJoinConsents`), `:362-379` (`missing_consents`); `python3 -c "json.load(open('packages/contracts/openapi/meeting-service.json'))['components']['schemas']['PreJoinResponse']"` lists the backend property names. My field-by-field comparison script flagged `PreJoinResponse` as entirely mismatched. `grep -rn CONSENT_REQUIRED apps/web/src` → no handler.
- API/Swagger refs: `GET /api/v2/conferences/{id}/pre-join` (`routes/v2.rs:208`), `POST /api/v2/conferences/{id}/join`.
- Affected frontend workflow: J-01 guest via link, J-08 recording with consent, J-10 regulated appointment, AT-05/06/24/29. Any conference whose profile requires explicit consent (healthcare / high-security profiles, `profiles.rs:307-310`) fails at Join with 403 and offers the user no way to give consent. Invite-only meetings without a token in the URL cannot be joined, because the token field is never shown. Guests on auth-required meetings get a backend error instead of a sign-in prompt.
- Owner class: FRONTEND (the backend matches its published OpenAPI)
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Regulated and consent-gated meetings, a headline Phase 2 capability, cannot be joined from web or mobile.
- Recommended remedy: Reuse the existing endpoint and change only the frontend. Regenerate or rewrite `PreJoinResponse` from OpenAPI. Map `access.passcodeRequired`, `access.invitationRequired`, `access.authenticationRequired` and `access.waitingRoomExpected`. Render `consents.required` with `consents.version` and send `consents:[{type, version: consents.version}]`. Add a contract test that loads the OpenAPI schema. No backend or DB change.
- Dependencies: FE-015 (generated client).
- Complexity: LOW
- Regression risk: Low (UI-only); test passcode, invite and consent flows.
- Confidence: CONFIRMED

### FE-002 — Mobile v1 meeting and profile screens use response/request shapes the backend never had
- Service/module: apps/mobile `features/meetings/{api/meetings.api.ts,types.ts,components/*}`, `app/(tabs)/{index,meetings,schedule}.tsx`, `app/(meetings)/[id]/edit.tsx`, `features/user/api/user.api.ts`.
- Expected: The v1 contract per `dto/common.rs:10-28` and `dto/meetings.rs`. List → `{success, data: Meeting[], meta:{page,limit,total,total_pages}}`. Get/create/update → `{success, data}`. Create requires `title, start_time (NaiveDateTime), timezone` (`dto/meetings.rs:58-84`). Instant → `{success, data:{id, meeting_identifier, …, participant_id, join_token, livekit_url, room_name}}` (`dto/meetings.rs:26-55`). `GET /users/me` → a flat `UserMeResponse` (`dto/users.rs:8-15`). Chat send requires `participant_id` (`dto/meetings.rs:367-377`).
- Actual: `meetings.api.ts:21-31` reads `response.data.meetings`, `total`, `total_pages` and `{meeting}`. `types.ts:3-31` uses `scheduled_at` and `duration_minutes` (the backend has `start_time`/`end_time`), plus `description` (no such field). The schedule form posts `scheduled_at` and `duration_minutes` without `start_time` or `timezone`, so serde rejects it (missing field). `(tabs)/index.tsx:65` navigates with `response.join_data`, which is `undefined`, so the live screen shows "Invalid Room". `meeting-list.tsx:32-34` always renders an empty list. `user.api.ts:5-8` returns `response.data.user`, which is `undefined`. `meetings.api.ts:108-113` sends `{message}` only, without `participant_id`, so chat send is rejected.
- Evidence: the files and lines above; the comparison script output ("CreateMeetingRequest TS-only:[description, scheduled_at, duration_minutes] RS-only:[start_time, end_time, timezone, metadata]", "InstantMeetingResponse TS-only:[meeting, join_data]", "JoinMeetingResponse TS-only:[message, data]").
- API/Swagger refs: `/api/v1/meetings` GET/POST, `/api/v1/meetings/:id` GET/PUT/DELETE, `/api/v1/meetings/instant`, `/api/v1/users/me`, `/api/v1/meetings/:id/chat`.
- Affected frontend workflow: Mobile Home, Meetings, Schedule, Edit meeting and Edit profile. Every mobile meeting-management screen fails or shows nothing.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: The shipped mobile app (EAS build and OTA pipeline in `mobile.yml`) cannot schedule, list or start meetings.
- Recommended remedy: Do not fix this against v1. Migrate the mobile meeting screens to v2 (`/meeting-spaces`, `/instant-meetings`, `/meeting-spaces/:id/conferences`) using the corrected v2 module (see FE-003), and fix `user.api.ts` to read the flat response. This needs no backend change.
- Dependencies: FE-003, FE-022.
- Complexity: MEDIUM
- Regression risk: Medium (mobile UX rewrite).
- Confidence: CONFIRMED

### FE-003 — Mobile v2 API module targets 35 non-existent paths and wrong DTOs
- Service/module: apps/mobile `features/meetings/v2/{api.ts,types.ts,hooks.ts}`, `features/room/v2/components/*`, `features/meetings/v2/components/*`.
- Expected: `routes/v2.rs` paths: `/meeting-spaces`, `/instant-meetings`, `/conferences/:id` (not nested under the space), `/waiting-room{,/admit,/deny}`, `/messages`, `/hand-raise`, `/breakouts/announcement`, `/polls/:pid/responses`, `/questions/**`, `/spotlights`, `/participants/:pid/audio-lock`.
- Actual: `api.ts:89-615` calls `/spaces`, `/spaces/:id`, `/spaces/:id/archive`, `/instant`, `/spaces/:sid/conferences/:cid`, `/participants/:pid` (GET), `/waiting`, `/chat`, `/hand/raise`, `/hand/lower`, `/hand`, `/breakouts/announce`, `/polls/:id/respond`, `/qna/**`, `/spotlight`, `/participants/:pid/lock/{audio,video}`. All of these fall through to the v2 fallback `ENDPOINT_NOT_FOUND`. Only 23 of 64 calls are valid, among them `resolve`, `conferences/:id/{start,end,lock,unlock,pre-join,join,leave}`, `participants`, `role`, `remove`, `mute`, `reactions` and parts of breakouts and polls. The DTOs also diverge: `CreatePollRequest`, `PollView`, `QuestionView`, `SendMessageRequest {content}` vs `{body}`, `MessagePage {cursor, hasMore}` vs `{nextCursor}`, `MediaContextView`, `RolePolicy`. Only `useResolve` and `useJoinConference` are imported by a screen (`pre-join-screen.tsx:14`). The room/v2 and meetings/v2 components are mounted by no route.
- Evidence: matcher output (41 NO_ROUTE rows for mobile, also in `frontend-calls.csv`, feature `mobile-v2-meetings`); `grep -rn "meetings/v2\|room/v2" apps/mobile/app` → only pre-join.
- API/Swagger refs: `routes/v2.rs:40-460`.
- Affected frontend workflow: No mobile in-meeting collaboration. The module cannot be adopted as-is for FE-002.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P2 (dead code today; blocks the mobile v2 migration)
- Business impact: Any developer wiring these hooks gets a uniformly broken mobile meeting experience.
- Recommended remedy: Replace the hand-written mobile v2 client with a generated client from `packages/contracts/openapi/meeting-service.json`, or share the web `services/v2` modules, which are path-correct. Delete or rewrite the room/v2 components.
- Dependencies: FE-015.
- Complexity: MEDIUM
- Regression risk: Low (unused today).
- Confidence: CONFIRMED

### FE-004 — Mobile live room is a non-functional shell (no media SDK, broken id mapping, v1 endpoints with v2 ids)
- Service/module: apps/mobile `app/(room)/[id]/live.tsx`, `features/room/{providers/room-provider.tsx, components/*, hooks/*}`, `features/room/components/pre-join-screen.tsx`.
- Expected: Spec F-10/11/24: connect to LiveKit with `rtc.url` and `rtc.credential`, render the roster and chat from v2 conference APIs, and call `/leave` or `/end` on v2.
- Actual: `package.json` has no LiveKit or WebRTC dependency (`grep livekit|webrtc` → none). `pre-join-screen.tsx:39-51` maps the v2 join into `{meeting_id, participant_id, role, join_token, livekit_url, room_name}`, but every room component reads `joinData?.id` (`video-grid.tsx:50`, `chat-panel.tsx:15`, `participants-panel.tsx:21`, `room-controls.tsx:22`), which is always `""`. The participant, chat and end queries are therefore disabled or target `/meetings//…`, and the grid permanently shows "Waiting for participants…". Even with an id they would call v1 `/meetings/:id/{participants,chat,end}` with a v2 `meetingSpaceId`. `room-provider.tsx:32-42` mic, camera and screen toggles only flip local state. `isHost = role === "host"`, so OWNER and CO_HOST get no host controls. Leaving never calls `POST /conferences/:id/leave`. When `rtcStatus = PROVIDER_CONTROL_DISABLED` (`rtc` null) the pre-join treats the user as waiting and polls forever.
- Evidence: the files and lines above; `apps/mobile/package.json` dependencies.
- API/Swagger refs: `POST /api/v2/conferences/{id}/join|leave|end`, `GET /participants`, `GET /messages`.
- Affected frontend workflow: Any mobile meeting attendance (AT-01, AT-09, AT-32).
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P1
- Business impact: Mobile users "join" but get no audio, video, roster or chat.
- Recommended remedy: Add the LiveKit React Native SDK (`@livekit/react-native` with an Expo config plugin and a dev build). Key the room on `conferenceId`. Use the v2 participant, message and leave APIs. No backend change.
- Dependencies: FE-003, FE-005, the PRODUCT_DECISION on mobile scope (FE-021).
- Complexity: HIGH
- Regression risk: Medium.
- Confidence: CONFIRMED

### FE-005 — Mobile guest join never keeps the participant credential and re-POSTs join every 3 s while waiting
- Service/module: apps/mobile `features/meetings/v2/hooks.ts:173-195`, `api.ts:182-195`, `lib/api/client.ts`, `features/room/components/pre-join-screen.tsx:86-118`.
- Expected: Spec J-01 / §7.4 item 44: store `participantToken`, send `X-Participant-Token` on conference-scoped calls, follow admission through `GET /participants/me` or the SELF_ONLY realtime stream, and never re-join blindly.
- Actual: `participantToken` appears only in the type (`types.ts:339`). It is never read, stored or sent (`grep -rn "participantToken\|X-Participant-Token" apps/mobile` → only the type). A waiting guest re-calls `POST /conferences/:id/join` anonymously through `publicApiV2` every 3 s (`pre-join-screen.tsx:97-118`). In the backend, an anonymous actor has no existing participant (`admission.rs:617-623`, `own_participant` resolves only from the actor credential), so each poll appears to create a new guest participant in the waiting room. The guest can never be admitted as "themselves".
- Evidence: the files above; `services/meeting-service/src/services/admission.rs:600-640`.
- API/Swagger refs: `POST /api/v2/conferences/{id}/join`, `GET /participants/me`, `POST /realtime/ticket`.
- Affected frontend workflow: Mobile guest admission (AT-09, AT-11, AT-33). The host's waiting queue fills with duplicates.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P1
- Business impact: Guests on mobile may never get in, and hosts see a spammed waiting room.
- Recommended remedy: Persist `participantToken` (SecureStore or memory), attach `X-Participant-Token` in an `apiV2` interceptor (mirroring web `services/v2/api.ts:30-50`), poll `GET /participants/me` instead of re-joining, and treat `rtcStatus` explicitly.
- Dependencies: FE-004.
- Complexity: LOW
- Regression risk: Low.
- Confidence: REQUIRES_VALIDATION (the duplicate-participant creation is inferred from `admission.rs`; an integration test would confirm it)

### FE-006 — Mobile build config hard-codes a private LAN IP over cleartext HTTP, overriding env
- Service/module: apps/mobile `app.json:33,56-57`, `lib/constants.ts:5-6`; no `eas.json` in the repo.
- Expected: API base from the build profile or env (`EXPO_PUBLIC_API_URL`), HTTPS only in release.
- Actual: `app.json` sets `extra.apiUrl = "http://10.209.6.52:8080/api/v1"`, `extra.socketUrl = "http://10.209.6.52:8080"` and `android.usesCleartextTraffic: true`. `constants.ts:5` gives precedence to `extra.apiUrl` over `EXPO_PUBLIC_API_URL`. `mobile.yml:107-139` runs `eas build --profile preview|production` and an OTA update, but no `eas.json` exists, so no profile overrides these values (as far as the repo shows).
- Evidence: `grep -n 'apiUrl\|usesCleartextTraffic' apps/mobile/app.json`; `ls apps/mobile/eas.json` → not found.
- API/Swagger refs: n/a
- Affected frontend workflow: Every mobile API call in any store or preview build.
- Owner class: FRONTEND (build config) / INFRASTRUCTURE
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P1 (release builds would target an unreachable LAN host with cleartext auth traffic)
- Business impact: The mobile app is unusable outside the developer LAN, and bearer tokens and passwords go over HTTP.
- Recommended remedy: Move to `app.config.ts` that reads env per profile, add `eas.json` profiles, and disable cleartext in release.
- Dependencies: none
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED (that CI builds really pick this up is REQUIRES_VALIDATION, because EAS may fail without `eas.json`)

### FE-007 — Web artefact views read fields the backend does not send
- Service/module: apps/web `types/v2/index.ts:1116-1128`, `components/conferences/ConferenceArtifactsPanel.tsx:197,242-243`, `ConferenceRecordingsPanel.tsx:147`.
- Expected: `ArtifactView` (`services/artifacts/mod.rs:219-244`) has `contentType, sizeBytes, durationMs, layout, checksumSha256, metadata, retentionAt, createdAt, updatedAt, startedAt, endedAt, finalizedAt, failureCode, sourceArtifactId, deletionPending`.
- Actual: The web type and UI use `filename`, `mimeType`, `durationSeconds`, `readyAt` and `deletedAt`. The UI shows "MIME type: Not available" and "Ready at: Not ready" for READY artefacts. Duration, failure code and deletion-pending are never shown. `ArtifactAccess` also omits `expiresInSeconds`, `contentType` and `sizeBytes`.
- Evidence: comparison script output; the lines above.
- API/Swagger refs: `GET /api/v2/conferences/{id}/artifacts`, `/artifacts/{aid}`, `/recordings`.
- Affected frontend workflow: F-19 post-meeting outputs, AT-24/26.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P2
- Business impact: Misleading artefact state in the post-meeting summary.
- Recommended remedy: Align the type with OpenAPI. If a display filename is wanted, derive it from `artifactType` + `createdAt`, or add it to `metadata` server-side (a backend product decision; no DB change needed).
- Dependencies: FE-015
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-008 — Calling "Settings" exposes forwarding, voicemail and usage tabs that the Next adapter admin-gates (403 for normal users)
- Service/module: apps/web `components/calling/CallingSettingsPage.tsx:22-27,73-76`, `app/api/calling/[...path]/route.ts:8-37,61-71,236-239`.
- Expected: A user-facing settings page should call only operations the signed-in user is authorized for, or hide tabs.
- Actual: The tabs Forwarding (`/v1/enterprise/forwarding-rules`), Voicemail (`/v1/enterprise/voicemail/*`) and Usage (`/v1/calling/usage|limits`) are routed by `isAllowedResourceRoute` → `requireResourceAdministrator`. Unless the principal is listed in `CALLING_ADMIN_PRINCIPAL_IDS`, the adapter returns `403 INSUFFICIENT_SCOPE "This Calling administration surface is not enabled for your account."` The backend routes exist (`calling/src/enterprise/http.rs:174-205`, `usage/http.rs:31-36`).
- Evidence: the lines above.
- API/Swagger refs: calling `/v1/enterprise/forwarding-rules`, `/v1/enterprise/voicemail/{mailboxes,messages}`, `/v1/calling/{usage,limits}`.
- Affected frontend workflow: `/calling/settings`, `/calling/usage`.
- Owner class: PRODUCT_DECISION (end-user forwarding and voicemail vs enterprise admin) with FRONTEND execution
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Three of four settings tabs error for ordinary users.
- Recommended remedy: Decide whether end users own their forwarding and voicemail. If they do, add a principal-scoped adapter rule (as for `/v1/blocks`, which injects `principalId`) and verify that backend scoping exists. If they do not, hide the tabs for non-admins.
- Dependencies: Calling backend authorization model (other auditors).
- Complexity: MEDIUM
- Regression risk: Medium (authorization boundary).
- Confidence: CONFIRMED

### FE-009 — Call recording/consent, abuse reporting and platform usage clients exist but are unreachable (adapter has no rule; components unmounted)
- Service/module: apps/web `services/calling/{recording,abuse,usage}.service.ts`, `hooks/calling/{useRecording,useAbuse,useUsage}.ts`, `components/calling/{CallRecordingControls,AbuseReportDialog}.tsx`, `app/api/calling/[...path]/route.ts:197-232`.
- Expected: Either a working path to the backend capability or no dead client code.
- Actual: The adapter's `/v1/calls/:id/*` branch allows only `GET`, `actions`, `cancel|end|hold|resume|transfer|accept|decline`, `ivr/input` and `sessions/:sid/{media-credentials,remove}`. `recordings`, `recording-consent`, `recording-authorizations` and `abuse-reports` all fall through to `404 "Calling operation not found."` `/v1/usage` and `/v1/limits` have no rule either. `CallRecordingControls` and `AbuseReportDialog` are imported nowhere (`grep -rn "<CallRecordingControls\|<AbuseReportDialog"` → none). `usePlatformLimits` and `usePlatformUsage` are unused. Backend: `calling/src/enterprise/http.rs:217-231`, `calls/app/http.rs:74-77`, `usage/http.rs:31-32`.
- Evidence: as above.
- API/Swagger refs: calling `POST/GET /v1/calls/{id}/recordings`, `/recording-consent`, `/recording-authorizations`, `POST /v1/calls/{id}/abuse-reports`.
- Affected frontend workflow: In-call recording consent and abuse reporting (safety).
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: Backend safety and recording capabilities are invisible to users. Abuse reporting matters for trust and safety.
- Recommended remedy: Add participant-scoped adapter rules (ownership via `ownCall`) and mount the components in the call UI, or remove the dead code.
- Dependencies: Calling authorization review.
- Complexity: MEDIUM
- Regression risk: Medium.
- Confidence: CONFIRMED

### FE-010 — CALLING_DEV_MODE mock identity and mock data have no production guard in the Next server routes
- Service/module: apps/web `lib/calling/server.ts:19-45`, `app/api/calling/[...path]/route.ts:89-158`.
- Expected: Dev bypasses are refused when `NODE_ENV=production` (as `callingFetch` already does for TLS, `server.ts:95-99`).
- Actual: `isDevMode()` checks only `process.env.CALLING_DEV_MODE === "true"`. When it is set, any bearer string is accepted and mapped to the fixed principal `CALLING_DEV_PRINCIPAL_ID || "dev-user-001"`. All non-mocked routes are still proxied to the real Calling service with the platform credential under that shared principal.
- Evidence: the lines above.
- API/Swagger refs: n/a (Next adapter)
- Affected frontend workflow: All calling.
- Owner class: FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2 (configuration-dependent auth bypass)
- Business impact: A single mis-set environment variable on Vercel collapses every user into one calling identity.
- Recommended remedy: Refuse dev mode when `NODE_ENV === "production"` or a Vercel production env is detected, and add a test.
- Dependencies: none
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-011 — Google OAuth hands tokens to the web app in the URL query; callback error UI is an empty placeholder
- Service/module: meeting-service `handlers/auth.rs:868-880`; apps/web `hooks/useGoogleCallback.ts:26-80`, `app/(auth)/google/callback/page.tsx:7-12`.
- Expected: Spec F-01 acceptance: "No raw tokens in URLs/logs". A visible error state.
- Actual: The backend redirects to `{frontend}/google/callback?access_token=…&refresh_token=…&user_id=…&username=…&role=…&status=…`. The page reads them from `useSearchParams`, so they reach Vercel request logs, browser history and any referrer. When `error` is set, `page.tsx` hits `if (error) { // ... (keep error handling) }` and renders the spinner forever.
- Evidence: the lines above.
- API/Swagger refs: `GET /api/v1/auth/oauth/google`, `/oauth/google/callback`.
- Affected frontend workflow: Google sign-in.
- Owner class: SHARED_CONTRACT
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Refresh-token exposure in logs; users stuck on a spinner after an OAuth failure.
- Recommended remedy: Backend issues a one-time code (or fragment/POST handoff) that the web exchanges via a new `POST /api/v1/auth/oauth/exchange`. The new endpoint is justified because no existing operation exchanges a code. Frontend renders the error.
- Dependencies: backend auth owner.
- Complexity: MEDIUM
- Regression risk: Medium (auth).
- Confidence: CONFIRMED

### FE-012 — Mobile placeholders: Google sign-in "coming soon", Help/Terms/Privacy rows inert, space edit TODO
- Service/module: apps/mobile `features/auth/components/login-form.tsx:164-165`, `signup-form.tsx:214-215`, `app/(tabs)/settings.tsx:179-183`, `features/meetings/v2/components/space-list.tsx:43`.
- Expected: Working actions or hidden rows.
- Actual: A toast says "Google sign-in coming soon". The settings rows have no `onPress`. There is a `// TODO: Implement edit functionality`.
- Evidence: `grep -rniE 'coming soon|todo' apps/mobile/{app,features}`.
- API/Swagger refs: `/api/v1/auth/oauth/google` (exists).
- Affected frontend workflow: Mobile onboarding and settings.
- Owner class: FRONTEND
- Classification: FRONTEND_ONLY_GAP
- Severity: P3
- Business impact: Polish; store review risk for missing privacy-policy link.
- Recommended remedy: Implement OAuth through `expo-auth-session` (needs an FE-011-style code exchange) and link the policy URLs.
- Dependencies: FE-011
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-013 — Developers page is static documentation for the legacy v1 third-party API on a different domain; developer-platform has no UI
- Service/module: apps/web `components/dashboard/DevelopersContent.tsx:291,313,390-393,471,553,570`.
- Expected: Docs pointing at current public APIs (gateway or `/api/v2`, developer-platform credentials), or a decision (D-07) that this belongs elsewhere.
- Actual: Hard-coded `https://api.cosmicforge.io/api/v1/api/meetings` and `wss://api.cosmicforge.io/ws/meeting/...`. These are legacy routes whose writes are slated for retirement (`FEATURE_LEGACY_MEETING_WRITE_DISABLED`), and the WS path does not exist in meeting-service routes. The developer-platform service (73 gateway routes: organizations, projects, credentials) has no frontend.
- Evidence: the lines above; `python3` count of `route-manifest.json` by domain → developer-platform 73.
- API/Swagger refs: `/api/v1/api/meetings` (`routes/api.rs`), developer-platform `/v1/organizations/**`.
- Affected frontend workflow: Developer onboarding.
- Owner class: PRODUCT_DECISION
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: External developers are directed to a deprecated surface and an unverified domain.
- Recommended remedy: Decide D-07, then replace the static page with links to `/api-docs` and the SDKs, or build a developer-platform console separately.
- Dependencies: PRODUCT_DECISION D-07
- Complexity: LOW (docs) / HIGH (console)
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-014 — Dead legacy v1 meeting code in the web room and hooks
- Service/module: apps/web `components/room/live-room.tsx:99-101,163-170`, `components/room/ChatProvider.tsx`, `footer-controls.tsx:49,64-78`, `hooks/{useCreateMeeting,useUpdateMeeting,useDeleteMeeting,useEndMeeting,useJoinMeeting,useMeetings,useMeeting,useWaitingRoom}.ts`, `store/storage.store.ts:36` (`setInstantJoinData` is never called).
- Expected: Spec §1.1 "Do not send v2 IDs to legacy meeting/chat/media endpoints".
- Actual: The live room still branches on `joinData.conference_id`. Without it, it uses v1 public meeting, v1 chat and v1 media PATCH. Pre-join always sets `conference_id` (`pre-join-screen.tsx:264`), and instant join data is never written, so the branch is unreachable. Eight v1 hooks are imported by no component.
- Evidence: `grep -rn setInstantJoinData apps/web/src` → definition only; the hook usage scan.
- API/Swagger refs: `/api/v1/meetings/**`, `/api/v1/participants/**`.
- Affected frontend workflow: none at runtime; maintenance hazard.
- Owner class: FRONTEND
- Classification: NOT_REQUIRED (cleanup)
- Severity: P3
- Business impact: Confusion; risk of re-activation.
- Recommended remedy: Delete the legacy branch and the v1 meeting hooks and service methods.
- Dependencies: FE-022
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-015 — No contract-parity gate: handwritten frontend types, v2 web tests not in CI, mobile has no tests
- Service/module: apps/web `types/v2/index.ts`, `apps/web/tests/*-v2.test.mjs`, `.github/workflows/frontend.yml:46-90`, `.github/workflows/mobile.yml:55-80`.
- Expected: Spec §7.1 ("typed operation clients generated from pinned meeting OpenAPI") and §11.2 shipping gate "Contract parity".
- Actual: Types are hand-maintained, which produced FE-001, FE-007 and FE-003. `frontend.yml` runs only `messaging-realtime`, `messaging-session`, `csp`, `build-assets` and `browser-security`. The six v2 suites (`meeting-spaces-v2`, `scheduling-v2`, `collaboration-v2`, `invitations-participants-v2`, `specialized-v2`, `messaging-v2`; 46 tests) and `calling-adapter` are not run in CI, and they test helpers with mocked axios, not the contract. Mobile CI runs lint and `tsc` only, and typecheck cannot catch wrong paths or shapes.
- Evidence: `grep -n 'run:' .github/workflows/frontend.yml`; `grep -c 'test(' apps/web/tests/*.mjs`.
- API/Swagger refs: `packages/contracts/openapi/meeting-service.json` 1.2.0.
- Affected frontend workflow: all.
- Owner class: FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Contract drift goes undetected; regressions ship.
- Recommended remedy: Generate TS types (`openapi-typescript`) for web and mobile from the pinned spec, fail CI on drift, and run all `apps/web/tests/*.test.mjs` in `frontend.yml`.
- Dependencies: none
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-016 — Issue #37 (Chromium web messaging journey): the history-recovery and transfer-approval UI only renders inside a collapsed panel
- Service/module: apps/web `components/messaging/MessagesContent.tsx:64-160,396,505-512`; `apps/web/tests/messaging-e2e.browser.mjs:295`.
- Expected: Per the closure doc, the journey `web_messaging_client_in_chromium_across_real_services` passes. The last recorded pass was 6/6 at commit 8999ac6 (`docs/phases/phase-05/evidence/web-messaging-journey-2026-10-20.txt:1-4`).
- Actual: `DevicePanel` (which holds `recover-history`, `remove-device` and the incoming-transfer approval `approve-transfer`) is rendered only when `securityOpen` is true. That state defaults to `false` (line 396) and is toggled by "Device & security settings" (lines 508-511). The browser test clicks `[data-testid=recover-history]` without expanding (line 295), so the journey fails. This matches the closure doc: "#37: the collapsed device panel hides 'Recover history' from the test" (`docs/operations/cfmeet-sections-8-23-final-closure.md:23,61,163`). There is also a real UX consequence beyond the test: on the source device, an incoming history-transfer request with its comparison code is invisible unless the user happens to open the collapsed panel. The panel was introduced by commit `ebf28ce` "Improve messaging layout and conversation navigation", after the passing run. Separately, the unmerged branch `origin/diagnostics/production-browser-crash-20261020` (commits `5b157a6`, `14b4b1d`) adds only a read-only Playwright smoke (`apps/web/tests/live-production-smoke.mjs`) that looks for "Application error: a client-side exception" on `www.cfmeet.com`. No result artefact is in the repo, so a production *crash* is unproven from repo evidence.
- Evidence: the lines above; `git show --stat origin/diagnostics/production-browser-crash-20261020`; `git merge-base --is-ancestor` → not merged.
- API/Swagger refs: messaging relay `history-transfers/**` (backend jobs reported success in run 42177519023 per the closure doc; not re-run here).
- Affected frontend workflow: Multi-device E2EE history recovery; the CI journey `unified-messaging-e2e.yml`.
- Owner class: FRONTEND (no backend evidence; do not reclassify)
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: The E2EE messaging certification stays red. Users miss transfer approvals.
- Recommended remedy: Surface incoming transfers and "Recover history" outside the collapsed section (or auto-expand on pending transfers) and update the test to open the panel. Separately, run the diagnostic smoke and record its output to confirm or close the "crash" hypothesis.
- Dependencies: none
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED (cause of the journey failure); REQUIRES_VALIDATION (any production Chromium crash)

### FE-017 — Spec information-architecture gaps in web: live room keyed by meeting code, no profile-policy editor, waiting room not a route
- Service/module: apps/web `app/(room)/[id]/page.tsx`, `components/room/pre-join-screen.tsx:123-125`, `components/dashboard/EditMeetingContent.tsx`, `hooks/v2/*` (`useUpdateProfilePolicy`, `useRaisedHands`, `useEvents`, `useRealtimeTicket`, `useLeaveConference` exported but unused).
- Expected: Spec §4: `/conferences/{conferenceId}/{waiting,live,summary}` and `/meetings/{spaceId}/settings` with profile policies. "The live room route must use conferenceId; meetingCode is a navigational entry identifier."
- Actual: The room lives at `/{meetingCode}` with an optional `?conference=`. Waiting and live are component state, so a refresh during waiting re-enters pre-join (`previousSession` handling exists, lines 128-131). No UI calls `PATCH /meeting-spaces/{id}/profile-policy`, so F-22 healthcare and high-security configuration cannot be done from the web.
- Evidence: the files above; unused-hook scan.
- API/Swagger refs: `PATCH /api/v2/meeting-spaces/{id}/profile-policy`.
- Affected frontend workflow: F-08, F-10, F-22, AT-29, AT-36.
- Owner class: FRONTEND
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: Organizers cannot set regulated profiles; deep links and refresh behaviour diverge from the spec.
- Recommended remedy: Add a profile-policy editor to the edit page and conferenceId-keyed routes (keep `/{code}` as a resolver redirect).
- Dependencies: FE-001
- Complexity: MEDIUM
- Regression risk: Medium (routing).
- Confidence: CONFIRMED

### FE-018 — First-party apps bypass the API Gateway; 59 web operations are intentionally absent from the gateway manifest
- Service/module: `infra/api-gateway/{route-manifest.json,public-routes.json,README.md}`; `docs/ENVIRONMENT.md:200-203`; `.github/workflows/cfmeet-web-build-diagnostic.yml:32`.
- Expected: Spec D-01/D-02: confirm the browser API origin and the gateway vs meeting-service contract.
- Actual: The web and mobile call `https://api.cfmeet.com/api/v1|v2` directly. The gateway README says "LOCAL ONLY". `public-routes.json` excludes v1 auth, users and meetings (`LEGACY_DIRECT_INGRESS`), the v2 messaging bridge (`FIRST_PARTY_APP_BACKEND`) and org directory admin (`MEETING_DIRECTORY_NOT_PUBLIC_FAMILY`). All 119 web v2 meeting operations are in the manifest. The 58 absent web operations (list in the scratch matcher) are all in those excluded classes.
- Evidence: the matcher (`gateway` column); `public-routes.json` `reasons`.
- API/Swagger refs: n/a
- Affected frontend workflow: none today; a future gateway cutover would break auth and messaging if the web were pointed at the gateway.
- Owner class: INFRASTRUCTURE
- Classification: NOT_REQUIRED (by design; documented)
- Severity: P3
- Business impact: Low; only a risk if the ingress changes.
- Recommended remedy: Record D-01 in docs. Keep `api.cfmeet.com` direct ingress for the first-party apps.
- Dependencies: D-01/D-02
- Complexity: LOW
- Regression risk: n/a
- Confidence: CONFIRMED

### FE-019 — Mobile error decoding ignores the v2 error envelope and mislabels every 401
- Service/module: apps/mobile `lib/api/client.ts:175-195` (`getStatusMessage`, `getApiError`).
- Expected: Spec §7.5: interpret `{success:false, error:{code,message,requestId,retryAfterSeconds}}` by `error.code`.
- Actual: The client reads `response.data.message` (a v1 shape), so v2 errors fall back to generic texts. Every 401 says "Invalid email or password."
- Evidence: the lines above.
- API/Swagger refs: v2 `ErrorEnvelope`.
- Affected frontend workflow: all mobile error states.
- Owner class: FRONTEND
- Classification: CONTRACT_MISMATCH
- Severity: P3
- Business impact: Confusing errors (for example, a passcode error shown as a login error).
- Recommended remedy: Decode `error.code` and `message` and map codes per §7.5.
- Dependencies: FE-003
- Complexity: LOW
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-020 — Communications Hub (73 operations) has no frontend consumer
- Service/module: meeting-service `services/communications.rs:145-148` (`/api/v2/communications/*` account-authenticated relay); gateway manifest domain `communications` = 73 routes (contacts, favourites, contact requests, activity, blocks, conversation context and meetings, calls history).
- Expected: A product surface (contacts and activity) if Phase 2 includes it.
- Actual: `grep -rn communications apps/web/src apps/mobile` → only a static mention in DevelopersContent. No screen, service or hook.
- Evidence: `python3` domain count of `route-manifest.json`.
- API/Swagger refs: `/api/v2/communications/**`; `docs/api/communications-unified-handoff.md`.
- Affected frontend workflow: contacts, contact requests, unified activity feed, "start meeting from conversation".
- Owner class: PRODUCT_DECISION
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: A built backend domain delivers no user value.
- Recommended remedy: Decide scope. If in scope, build it on the generated TS SDK (`client.communications.*`).
- Dependencies: product scope; the spec §2.1 excludes "full general-purpose messaging" from the Phase 2 meeting frontend.
- Complexity: HIGH
- Regression risk: Low.
- Confidence: CONFIRMED

### FE-021 — Mobile covers only auth plus a broken legacy meeting flow; no spaces, conferences, messaging, calling or organizations
- Service/module: apps/mobile `app/**` (21 files).
- Expected: The spec targets web first, with "responsive/mobile integration requirements" (§9.3, D-06 device matrix). The intended native scope is not specified.
- Actual: No mobile screens for meeting spaces, conferences, invitations, registrations, recordings or artefacts, Unified Messaging, calling or organizations.
- Evidence: the screen inventory above.
- API/Swagger refs: n/a
- Affected frontend workflow: all mobile.
- Owner class: PRODUCT_DECISION
- Classification: FRONTEND_ONLY_GAP
- Severity: P2
- Business impact: The mobile app published through the EAS pipeline is not a usable product.
- Recommended remedy: Decide the native scope (D-06). Minimum viable: v2 dashboard, join, waiting, live (FE-002 to FE-005).
- Dependencies: FE-002 to FE-006
- Complexity: HIGH
- Regression risk: n/a
- Confidence: CONFIRMED

### FE-022 — Mobile is the remaining v1 meeting-write client, which blocks legacy-write retirement
- Service/module: apps/mobile `features/meetings/api/meetings.api.ts`; meeting-service `middleware/legacy_write.rs:1-45`, `config/features.rs:64-78`; `docs/phase2-certification/12-rollout-plan.md:36,72,118`.
- Expected: Rollout step 8 enables `FEATURE_LEGACY_MEETING_WRITE_DISABLED` once "v2 consumers migrated".
- Actual: Web meeting writes are all v2 (FE-014 notes only dead v1 code). Mobile still issues v1 `POST /meetings`, `PUT /meetings/:id`, `DELETE`, `/instant`, `/end` and `/chat`. These are already broken by FE-002, and they would answer 410 once the flag is on.
- Evidence: the files above.
- API/Swagger refs: `/api/v1/meetings/**`.
- Affected frontend workflow: Mobile meetings.
- Owner class: SHARED_CONTRACT (rollout coordination)
- Classification: PARTIALLY_IMPLEMENTED
- Severity: P2
- Business impact: The legacy surface (and its security exposure) cannot be retired.
- Recommended remedy: Migrate mobile to v2 first, then enable the flag.
- Dependencies: FE-002, FE-003
- Complexity: MEDIUM
- Regression risk: Low.
- Confidence: CONFIRMED

## Backend capabilities not consumed by any frontend (by domain)
| Domain | Unconsumed operations | Note |
|---|---|---|
| Meeting v1 admin | GET `/api/v1/admin/{users,meetings,logs}`, PATCH `/admin/users/:id` | Operator; NOT_REQUIRED for the end-user app |
| Meeting v1 misc | POST `/api/v1/participants/:id/kick`, GET `/api-keys/:id`, POST `/meetings/:id/join` | legacy |
| Meeting v1 third-party | POST `/api/v1/api/meetings`, `/api/v1/api/meetings/:id/join` | API-key surface, documented statically only (FE-013) |
| Meeting v2 | PATCH `/meeting-spaces/:id/profile-policy` (FE-017); POST `/admin/organizations`, PATCH `/admin/organizations/:id` (service methods exist, no UI); `/admin/compat/**`; `/admin/chat-migration/**` (hooks exist, no screen) | — |
| Messaging relay | Message reactions (`Reaction` segment in `relay.rs` ROUTES; not in web `routes.ts`); `GET conversations/:id/unread` allowlisted in web but unused | minor |
| Communications Hub | all 73 `/api/v2/communications/*` | FE-020 |
| Calling | `/v1/calls/:id/{recordings,recording-consent,recording-authorizations,abuse-reports}`, GET `/v1/abuse-reports`, `/v1/{usage,limits}` | FE-009; enterprise and privacy-relay are covered only by the generic admin JSON pages |
| Developer platform | all (73 gateway routes) | FE-013 |
| Numbering (52), SMS (6), telephony | none | Excluded from the Phase 2 meeting frontend by spec §2.1 |

## Counts

**Feature-matrix rows by classification** (39 rows)
| Classification | Count |
|---|---|
| COMPLETE_VERIFIED | 0 |
| IMPLEMENTED_UNVERIFIED | 20 |
| PARTIALLY_IMPLEMENTED | 6 |
| CONTRACT_MISMATCH | 5 |
| FRONTEND_ONLY_GAP | 5 |
| EXTERNAL_DEPENDENCY | 1 |
| NOT_REQUIRED | 2 |
| MISSING / DEFERRED_BY_OWNER | 0 |

**Findings by severity** (22 findings)
| Severity | Count | IDs |
|---|---|---|
| P0 | 0 | — |
| P1 | 5 | FE-001, FE-002, FE-004, FE-005, FE-006 |
| P2 | 13 | FE-003, FE-007, FE-008, FE-009, FE-010, FE-011, FE-013, FE-015, FE-016, FE-017, FE-020, FE-021, FE-022 |
| P3 | 4 | FE-012, FE-014, FE-018, FE-019 |

**Findings by owner class**: FRONTEND 15 (FE-001, 002, 003, 004, 005, 006*, 007, 009, 010, 012, 014, 015, 016, 017, 019), PRODUCT_DECISION 4 (FE-008, 013, 020, 021), SHARED_CONTRACT 2 (FE-011, 022), INFRASTRUCTURE 1 (FE-018). *FE-006 is also INFRASTRUCTURE.
