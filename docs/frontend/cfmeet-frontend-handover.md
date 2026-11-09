# CF Meet — frontend implementation handover

Audience: the web (Next.js, `apps/web`), mobile (Expo / React Native, `apps/mobile`) and Developer
Portal teams. Scope: the whole normal CF Meet application (personal and organization workspaces) and
the Developer Portal. Backend state: the 18-section architecture is under **controlled feature freeze**
on `staging` (final closure, 2026-11-09). Changes from now on are integration-defect fixes, security
fixes or owner-approved changes only.

How to read this guide:

* This file explains **how** to consume the backend (sessions, scopes, roles, realtime, errors, media,
  journeys, flags, gates).
* **What** exactly each endpoint takes and returns is in the generated
  [`cfmeet-frontend-endpoint-reference.md`](cfmeet-frontend-endpoint-reference.md) (618 operations; one
  row per operation with auth, summary, statuses, error codes and the canonical contract entry). It is
  generated from the served OpenAPI contracts and CI fails when it is stale, so it cannot drift.
* Request / response schemas and examples are in the contracts themselves (Swagger UI renders each
  bundle): `packages/contracts/bundled/public/*/v1.openapi.json` (self-contained) and the per-service
  sources listed in §2. Generated client types: `apps/web/src/types/generated/meeting-service.ts`,
  the mobile equivalent, and the SDKs in `sdks/typescript` and `sdks/dart`.
* Per-area traceability tables that already map screens → endpoints → errors (kept, not duplicated):
  `docs/frontend/section-17/web-account-organizations.md`, `web-calling.md`, `web-communications.md`,
  and the Hub journey map `docs/api/communications-unified-handoff.md` §11.

Nothing in this guide is a secret; no credential value appears anywhere in the frontend docs.

---

## 0. Production status (live, verified 2026-11-09)

This section records what is **actually running** in production, verified on the server and through
the public host. Where it disagrees with older wording elsewhere in this guide, this section wins.

**Release.** Every CF Meet backend service runs the images built from `main` `bde67a8b` (all 13
services: Platform Access, Organizations, Communications Hub, Messaging, Calling, Meeting Service,
Developer Platform + worker, API Gateway, Numbering, Privacy Relay, SMS Gateway, Telephony; plus the
security scan gateway). The coordinated release verified each running container's image id and
source revision, applied every migration, and passed its post-release checks (public Swagger, CORS
from `https://cfmeet.com`, internal routes not public, provider features off).

**Hosts and Swagger** (the only hosts a client uses; no internal credential is ever needed):

| What | URL |
|---|---|
| Production API | `https://api.cfmeet.com` |
| Swagger UI (all public contracts) | `https://api.cfmeet.com/swagger-ui/` |
| Swagger index | `https://api.cfmeet.com/api-docs/swagger-config.json` |
| Meetings / accounts / organizations relay | `/api-docs/openapi.json` (1.5.0, 188 paths) |
| Communications Hub | `/api-docs/communications.json` (1.3.0) |
| Calling | `/api-docs/calling.json` (1.16.0) |
| Messaging | `/api-docs/messaging.json` (1.16.0) |
| Developer Platform | `/api-docs/developer-platform.json` (1.6.0) |
| API Gateway | `/api-docs/api-gateway.json` (1.1.0) |
| Messaging realtime (WebSocket) | `wss://api.cfmeet.com/v1/realtime` (ticket from `POST /v1/realtime/tickets`) |
| Media (LiveKit) | `wss://livekit.cfmeet.com` (token issued by the API, never by the client) |

**Feature availability in production:**

| Capability | State | Evidence / reason |
|---|---|---|
| Accounts, sessions, profile, session revocation | **Active** | live acceptance (login, `/api/v2/communications/me`, logout → 401) |
| Meetings, calendar, guests, waiting room, LiveKit media, recordings | **Active** | Meeting Service health + regression on every step |
| Conference realtime, webinar mode, quality telemetry | **Active** | `activate-first-party-features` |
| Meeting notification email | **Active** | SMTP credentials present; enabled by `activate-first-party-features` |
| Healthcare profile | Off | compliance decision for the owner |
| Communications Hub: CF Meet ID, QR, contacts, requests, blocking, privacy, timeline | **Active** | live Messaging acceptance 15/15 (contacts, requests, blocking) |
| Timeline realtime (`timeline.changed` on the Messaging stream) | **Active** | released (MSG-009); subscribe to the conversation stream |
| App-to-app Calling (voice/video), call history, transfers | **Active** | Calling regression, both LiveKit keys PASS |
| Call recording with consent | **Active** | `test-recording` passed in production |
| Voicemail | **Active** | `test-voicemail` passed in production |
| Enterprise Calling (shared developer scope) | **Active** | `test-enterprise` passed in production |
| Per-organization Enterprise Calling | Off | needs the Organizations authority cutover (below) |
| MANAGED Messaging + session-device realtime, read receipts (coalesced) | **Active** | `activate-first-party-features`; receipts released (MSG-018) |
| PRIVATE_E2EE personal messaging | Off | independent cryptographic assessment + owner sign-off required |
| Organizations service: provisioning, signed event delivery | **Active** | `activate-provisioning`, `activate-org-events`; dead-letter count 0 |
| Organizations **authority** (routes answer from the Organizations service) | Off (`legacy`) | import step needs the owner's verified **off-site backup**; production holds 0 legacy organizations |
| Developer Platform + public API Gateway | **Active** | gateway answers `401` unauthenticated on `/v1/environments/*`, `/v1/scopes`, CORS OK |
| Credential expiry webhooks (`credential.expiring_soon`, `credential.expired`) | **Active** | released (PLT-008) |
| Push (APNs / FCM / Web Push) | Off | no provider credentials |
| SMS / phone OTP (Africa's Talking), SMS delivery callbacks | Off | no provider credentials or onboarding |
| PSTN calling, real numbers, carrier interconnect | Off | no carrier agreement |

---

## 1. System map for client developers

| Product area | Owner service (authority) | How a first-party client reaches it | Contract (version) |
|---|---|---|---|
| Accounts, sessions, profile | Meeting Service (account authority) | direct, `NEXT_PUBLIC_API_URL` | `openapi/meeting-service.json` (1.5.0) |
| Meetings, conferences, calendar, guests | Meeting Service | direct | same |
| Organizations, workspaces, teams, policies | Organizations Service (authority), relayed by the Meeting Service with the signed-in user as acting user | direct to Meeting Service `/api/v2/organizations/*`, `/api/v2/me/workspaces*` | same |
| Contacts, CF Meet ID / QR, privacy, timeline, activity, notification preferences, linked devices | Communications Hub | Meeting Service host, `/api/v2/communications/*` | `communications/http/v1.openapi.json` (1.3.0) |
| Messaging (personal) | Messaging | Meeting Service relay `/api/v2/messaging/relay/v1/...` + `/api/v2/messaging/*` helpers | `messaging/http/v1.openapi.json` (1.16.0) behind the relay |
| Messaging (organization) | Messaging, in the organization's own technical scope | `/api/v2/organizations/{id}/messaging/relay/v1/...` | same |
| Calling (personal) | Calling | web: same-origin server adapter `/api/calling/*`, `/api/calling-signaling/*`; Hub launch `/api/v2/communications/contacts/{id}/calls`, `/conversations/{id}/calls` | `calling/http/v1.openapi.json` (1.16.0) |
| Enterprise Calling (organization) | Calling, in the organization's scope | `/api/v2/organizations/{id}/calling/v1/...` (relay) | same |
| Developer Portal | Developer Platform (+ API Gateway for customer traffic) | `NEXT_PUBLIC_DEVELOPER_API_BASE_URL`, account session bridged | `platform-developer/http/v1.openapi.json` (1.6.0) |

Rules that never change:

1. **No first-party client ever holds an internal service credential.** Browser and mobile use only the
   account session (and per-device / per-session tokens minted for them). The web Calling adapter's
   platform credential lives on the Next.js server only; production builds refuse to run it without
   the server-side configuration (`CALLING_NOT_CONFIGURED`).
2. **Membership, roles and capabilities are always decided by the server.** Never send a role or an
   organization membership as an assertion; read capabilities and render from them.
3. **Personal and organization data are separate scopes.** The same human has a different technical
   principal per scope (§4). Never mix lists from two scopes in one view without labelling them.

## 2. Contracts, SDKs and generated types

| Artifact | Path | Freshness gate |
|---|---|---|
| Meeting Service served contract | `packages/contracts/openapi/meeting-service.json` | `openapi_contract` test (code ↔ file identity) |
| Public bundles (Swagger UI input) | `packages/contracts/bundled/public/<domain>/v1.openapi.json` | `bundle_openapi.py --check`, Swagger render test |
| Web contract types | `apps/web/src/types/generated/meeting-service.ts` (`npm run contracts:generate`) | frontend CI |
| Mobile contract types | `apps/mobile` (`node scripts/generate-contract-types.mjs`) | mobile CI |
| TypeScript SDK `@cfmeet/platform-sdk` (beta) | `sdks/typescript` | `sdks/generate.py --check`, real-stack parity suite |
| Dart SDK `cfmeet_platform_sdk` (beta) | `sdks/dart` | same |
| Frontend endpoint reference | `docs/frontend/cfmeet-frontend-endpoint-reference.md` | `frontend_reference.py --check` |

Contract versions are frozen for the handover at the versions in §1. A breaking change needs a new
contract major and owner approval; additive changes bump the minor and regenerate every artifact
above in the same commit.

Swagger: every operation has a summary, a description and exactly one tag (796 operations checked).
Internal, operator and provider-callback operations are not in the public bundles; the gateway
catalogue (`infra/api-gateway/route-manifest.json`) classifies every route (`PUBLIC_CUSTOMER`,
`EXCLUDED` = first-party direct, `OPERATOR_ADMIN`, `INTERNAL_SERVICE`, `PROVIDER_CALLBACK`,
`PUBLIC_INFRASTRUCTURE`). Frontend code must only call `PUBLIC_CUSTOMER` and `EXCLUDED` routes, which
is exactly the set in the endpoint reference.

SDK use: the SDKs target the Developer Platform / API Gateway customer surface (machine credentials).
First-party apps use the generated contract types and the Meeting Service directly; adopting SDK model
types in the apps is optional (PLT-013 handoff).

## 3. Authentication and sessions

### 3.1 Account session

| Step | Endpoint | Notes |
|---|---|---|
| Register | `POST /api/v1/auth/register` | email normalized (case-insensitive); 409 on a case variant |
| Verify email | `POST /api/v1/auth/verify-email`, `POST /api/v1/auth/resend-verification` | some features require a verified email (`EMAIL_VERIFICATION_REQUIRED`) |
| Sign in | `POST /api/v1/auth/login` | returns `access_token` + `refresh_token`; opens an account session |
| Google | `GET /api/v1/auth/oauth/google` → callback → app receives `?exchange_code=` → `POST /api/v1/auth/oauth/google/exchange` | redeem once, erase the query string first; tokens never travel in a URL |
| Refresh | `POST /api/v1/auth/refresh` | **rotating**: each refresh token is single use; reuse ends the whole session |
| Sign out | `POST /api/v1/auth/logout` | ends this session only |
| Sessions | `GET /api/v2/me/sessions`, `POST /api/v2/me/sessions/{sessionId}/revoke`, `POST /api/v2/me/sessions/revoke-all` | "sign out everywhere" |
| Password | `POST /api/v1/auth/forgot-password` → `verify-reset-token` → `reset-password`; `POST /api/v1/auth/change-password` | reset / change end every earlier session; responses never reveal whether an account exists |
| Deactivate | `POST /api/v1/users/me/deactivate` | needs the current password or a sign-in within 10 minutes (`403 REAUTHENTICATION_REQUIRED`) |
| Profile | `GET`/`PATCH /api/v2/me/profile` | `409 USERNAME_TAKEN` |

Web storage model (implemented, keep it): access token **in memory only**; refresh token in an
HttpOnly `SameSite=Strict` cookie scoped to `/api/session` (Next.js route handlers rotate it); one
refresh in flight per browser (Web Locks); on `401` refresh once and retry, then end the session
(`/login?reason=session_ended`). Mobile: keep tokens in the platform secure store (Keychain /
Keystore), never in AsyncStorage, never logged.

Revocation is cross-service: revoking a session, changing a password or deactivating an account ends
access at the Meeting Service, the Developer Platform console bridge, Messaging session devices and
the Hub. Clients must treat a `401` from any CF Meet API as "session ended" after one refresh attempt.

### 3.2 Guests

* v2 meetings: a guest joins with `POST /api/v2/conferences/{id}/join` and receives a
  `participantToken` **once**; send it as `X-Participant-Token` on later calls (pre-join, rejoin,
  realtime ticket). Keep it for the conference only.
* Invitations: `POST /api/v2/invitations/{token}/resolve` (public, read-only, does not consume a use);
  pass `invitationToken` on join.
* Legacy v1 guests (old meeting links): their refresh token is single use, lasts at most 24 h from
  the join and stops working when the guest is removed, leaves, or the meeting ends (SEC-004).

### 3.3 Developer Portal

The portal uses the CF Meet account session bridged to the Developer Platform (`userSession`
scheme). Machine credentials (`platformCredential`) are created, shown once and rotated in the portal
but are **never** used by the portal UI itself to call product APIs.

## 4. Personal versus organization scope

* `GET /api/v2/me/workspaces` lists the caller's workspaces: one Personal workspace plus each
  organization. `GET /api/v2/me/workspaces/{workspaceId}/capabilities` returns, per capability,
  `ALLOWED` or a reason (`CAPABILITY_NOT_GRANTED`, `POLICY_RESTRICTED`, organization suspended) and
  the organization's `features.{meetings,messaging,calling}`. Render menus and buttons from this; the
  write paths enforce the same decision.
* The selected workspace is client state (per tab on web). Switching workspaces changes which API
  family you call, not a header:
  * Messaging: personal `/api/v2/messaging/relay/...`; organization
    `/api/v2/organizations/{id}/messaging/relay/...` (+ `/messaging/session-tokens`,
    `/messaging/members/resolve`).
  * Calling: personal through the adapter; organization admin through
    `/api/v2/organizations/{id}/calling/...`.
  * Meetings: create with `organizationId` for an organization meeting; personal meetings omit it.
    Agenda: `GET /api/v2/me/conferences?organizationId=…&from=…&to=…`.
* A user has **one principal per scope**. Organization conversations and calls use the organization
  principal; personal ones use the personal principal. Removing someone from an organization
  suspends only their organization principal: organization reads, sends, calls and realtime stop
  (realtime sessions close within about 30 s); their personal messages and calls are untouched.
* Organization suspension: every organization surface answers `409 ORGANIZATION_SUSPENDED` /
  `403 AUTHORITY_SUSPENDED`; personal features keep working. Show a suspended banner, not an error page.
* Contacts are personal only. Colleagues come from the organization directory
  (`GET /api/v2/messaging/directory`, `GET /api/v2/organizations/{id}/members`) and must be shown in a
  separate "Colleagues" list, never merged into Contacts.

## 5. Roles and capabilities

Organization roles: `OWNER`, `ADMIN`, `MANAGER`, `MEMBER`, `GUEST` (Organizations decides; policy can
restrict). Typical capability holders (default policy):

| Capability | Default holders | Unlocks |
|---|---|---|
| `organization.profile.write`, `organization.members.manage`, `organization.policies.manage`, `organization.audit.read` | OWNER, ADMIN | settings, members, policies, audit tabs |
| `organization.members.invite`, `organization.teams.manage` | OWNER, ADMIN, MANAGER | invite, teams |
| `organization.ownership.transfer`, `organization.lifecycle.manage` | OWNER | transfer, suspend, reactivate, archive |
| `meetings.organization.create` | all but GUEST | create organization meetings |
| `meetings.organization.manage` | OWNER, ADMIN | configure, reassign and archive any meeting of the organization (FULL view) |
| `calling.enterprise.manage` | OWNER, ADMIN | Enterprise Calling admin tab |
| `messaging.organization.moderate` | OWNER, ADMIN | moderation |

Meeting roles inside a meeting space / conference: `OWNER`, `CO_HOST`, `HOST`, `PRESENTER`,
`PARTICIPANT`, `VIEWER` (webinars). The space read returns `view: FULL` (managers: version, members,
settings) or `view: SUMMARY` (everyone else). Send `version` on every space write (optimistic
concurrency; `409 MEETING_VERSION_CONFLICT` → reload and retry).

`ORGANIZATION_ONLY` meetings are only visible to their owner / members, members of the organization
and invited people: an outsider gets `404 MEETING_NOT_FOUND` (same as an unknown code) and a signed-out
user `401 AUTHENTICATION_REQUIRED` on `GET /api/v2/meeting-spaces/{id}` and `/api/v2/resolve/{code}`.
For a signed-out user opening an organization meeting link, show "Sign in with your organization
account", then retry.

## 6. Journeys and screens → endpoints

Exact request / response shapes: endpoint reference + contract. Error columns list the codes a screen
must handle beyond the generic ones in §8.

### 6.1 Account and settings (web + mobile)

| Screen | Endpoints | Specific errors |
|---|---|---|
| Sign up / verify | `POST /api/v1/auth/register`, `verify-email`, `resend-verification` | 409 conflict, 400 validation |
| Sign in / Google | `POST /api/v1/auth/login`, Google start + `oauth/google/exchange` | 401, expired / used exchange code |
| Forgot / reset password | `forgot-password`, `verify-reset-token`, `reset-password` | neutral answers by design |
| Profile | `GET`/`PATCH /api/v2/me/profile`, `GET /api/v1/users/me` | 409 `USERNAME_TAKEN` |
| Security → sessions | `GET /api/v2/me/sessions`, revoke one / all | 404 |
| Security → password | `POST /api/v1/auth/change-password` | 400 wrong current password / Google-only account |
| Notifications | `GET`/`PUT /api/v2/communications/me/notification-preferences`, `GET …/notification-delivery-status` | `VERSION_CONFLICT` |
| Devices | Hub `GET /api/v2/communications/devices`, `POST …/devices/{id}/revoke`, `PUT`/`DELETE …/devices/{id}/push`; Calling endpoints `GET`/`DELETE /v1/calling/endpoints` (adapter); Messaging `POST /api/v2/messaging/devices/{id}/revoke` | revoking a device ends its signaling / realtime / push |
| Deactivate | `POST /api/v1/users/me/deactivate` | 403 `REAUTHENTICATION_REQUIRED` |

### 6.2 Workspaces and organizations

| Screen | Endpoints | Specific errors |
|---|---|---|
| Workspace switcher | `GET /api/v2/me/workspaces`, `GET …/{workspaceId}/capabilities` | 503 when Organizations is unavailable (show retry; never assume allowed) |
| Create organization | `POST /api/v2/organizations` | `FEATURE_DISABLED` before the owner cutover (§10) |
| Organization profile | `GET`/`PATCH /api/v2/organizations/{id}` | 403, 409 |
| Members / invitations | `GET …/members`, `PUT`/`PATCH …/members/{userId}`, `POST …/members/me/leave`, `GET`/`POST …/invitations`, `POST …/invitations/{invitationId}/revoke`; invitee: `POST /api/v2/organization-invitations/accept|decline` | email-bound invitations: accept with the invited account |
| Teams | `GET`/`POST …/teams`, `GET`/`PATCH …/teams/{teamId}`, `POST …/archive`, team members CRUD | 403 `PERMISSION_DENIED` |
| Policies | `GET`/`PATCH …/policies` (compare-and-swap on `revision`) | 409 on a stale revision |
| Lifecycle | `POST …/suspend`, `…/reactivate`, `…/archive`, `…/ownership-transfer` | OWNER only |
| Provisioning / audit | `GET …/provisioning`, `GET …/audit-events` | provisioning states are truthful (`PENDING` until ready) |

### 6.3 Meetings and calendar

| Screen | Endpoints | Notes |
|---|---|---|
| Home: start now | `POST /api/v2/instant-meetings` | LIVE conference at once |
| Schedule | `POST /api/v2/meeting-spaces` (+ `organizationId`), `POST …/{id}/conferences` (`scheduledStartAt`/`EndAt`), recurrence `PUT …/recurrence` (RRULE in the space time zone), exceptions | times are RFC 3339; show in meeting time zone and viewer zone; DST gaps / overlaps handled server-side |
| Agenda / calendar | `GET /api/v2/me/conferences?from&to[&organizationId]` | occurrences across spaces; organization meetings visible to members |
| Meeting details | `GET /api/v2/meeting-spaces/{id}` (id or code), `PATCH` (with `version`), `POST …/archive`, `POST …/owner-transfer`, `POST …/duplicate` | §5 visibility rules |
| Invitations | `GET`/`POST /api/v2/meeting-spaces/{id}/invitations`, revoke | email-bound or open tokens |
| Calendar export | `GET /api/v2/conferences/{id}/calendar.ics`, `GET /api/v2/meeting-spaces/{id}/calendar.ics` | no passcode in the file |
| Join link | `GET /api/v2/resolve/{identifier}` → canonical conference | §5 for organization meetings |
| Pre-join | `GET /api/v2/conferences/{id}/pre-join` | `access.*` (authentication / invitation / passcode required, waiting room expected), `consents.required` + `version`, notices (recording, transcription, AI), media permissions, display-name rules |
| Join / rejoin | `POST /api/v2/conferences/{id}/join` (`displayName`, `invitationToken`, `passcode`, `consents[]`) | `CONSENT_REQUIRED` until every required consent at the current version is sent; `rtcStatus` `NOT_ISSUED_WAITING` while waiting, `ISSUED` with `rtc.credential` + `url` after admission |
| Waiting room (host) | `GET /api/v2/conferences/{id}/waiting-room`, `POST …/waiting-room/admit` (`participantIds` or `all`), deny | |
| In meeting | participants, media controls, chat, Q&A, polls, breakouts, recordings, transcription (tags in the reference) | |
| History / artifacts | conference list per space, artifacts (signed short-lived download grants) | |

### 6.4 Contacts, CF Meet ID and QR (personal)

Covered end to end in `section-17/web-communications.md` (traceability table) and the Hub handoff.
Key endpoints: `GET /api/v2/communications/me`, `GET …/me/qr` (single-use, 10-minute payload; send an
`Idempotency-Key`), `GET …/me/public-id`, `POST …/me/public-id/rotate` (recent authentication),
`POST …/discovery/public-id/resolve` and `…/discovery/invites/resolve` (neutral `NOT_FOUND`),
`POST …/contact-requests`, incoming / outgoing / accept / decline / cancel, `GET`/`PATCH`/`DELETE
…/contacts`, blocks (`PUT`/`DELETE …/profiles/{cfMeetId}/block`), reports, `GET`/`PUT …/me/privacy`,
`GET …/activity`, `GET …/calls/history`, `GET …/bootstrap` + `GET …/sync?after=` (`resyncRequired` →
reload all), `GET …/me/capabilities` (hide areas the backend reports unavailable and show the reason).
The phone number is never shown or searchable; phone verification reports `PROVIDER_BLOCKED` until an
SMS provider is configured (§10).

### 6.5 Messaging

Personal and organization Messaging share one client implementation; only the base path changes (§4).

* List / open / send / edit / delete / react / pin / search / read cursor / attachments / groups: see
  `section-17/web-communications.md` traceability table (all paths relative to the relay base).
* Sends carry a client-generated `clientMessageId` (idempotent; reuse it on retry).
* MANAGED conversations are readable by CF Meet servers: label them so; never show them as end-to-end
  encrypted. PRIVATE_E2EE device operations are disabled in production until independent approval
  (`J-21`); the UI must not offer E2EE as available unless `GET /api/v2/communications/me/capabilities`
  says so.
* Realtime (MANAGED): `POST /api/v2/messaging/session-tokens` (organization:
  `/api/v2/organizations/{id}/messaging/session-tokens`) → `POST /v1/realtime/tickets` through the
  relay → WebSocket `subscribe` **without** a cursor (the server resumes from the stored per-device
  ack cursor) → `ack`, `typing`, `auth.refresh`. Close code `4401` → mint a new session device. When
  realtime is unavailable (`FEATURE_DISABLED`), poll `GET …/events?after=`.
* Colleague pickers (organization): `GET /api/v2/messaging/directory`,
  `POST /api/v2/organizations/{id}/messaging/members/resolve` (active members only),
  `POST /api/v2/messaging/principals/names` for display names.

### 6.6 Calling

* Personal calls (web): see `section-17/web-calling.md` traceability. Device registration
  (`/v1/calling/endpoints`), signaling credential + WebSocket (`wss` only in production, renew 15 s
  before expiry), incoming card accept / decline, media credential per session, hold / resume / end /
  cancel / transfer with `expectedVersion` and `Idempotency-Key`, group calls (≤ 64 targets),
  recording consent and controls, abuse reports, history `GET /v1/calls?principalId=…`.
* Launch from Contacts / chat through the Hub: `POST /api/v2/communications/contacts/{id}/calls`,
  `POST /api/v2/communications/conversations/{id}/calls`.
* Call lifecycle the UI must render: `RINGING` → `ANSWERED` → `CONNECTED` (only after verified media)
  → `HELD` ↔ `CONNECTED` → `ENDED` (or `CANCELLED`, `NO_ANSWER`, `DECLINED`). History entries carry
  per-participant `direction` and `outcome` (`ANSWERED`, `MISSED`, `DECLINED`, `ANSWERED_ELSEWHERE`, …).
* Reconnection: LiveKit's own retry first, then a fresh media credential with bounded backoff inside
  the 60 s participation grace.
* Enterprise (organization admins with `calling.enterprise.manage`): extensions, ring groups, queues,
  IVR, voicemail, forwarding, recordings through `/api/v2/organizations/{id}/calling/v1/...`.
  `409 ORGANIZATION_CALLING_DISABLED` → "not enabled for this organization"; `409
  PROVISIONING_PENDING` → show progress.
* Background ringing on a closed app needs APNs / FCM / Web Push (external gate, §10); an open app
  rings through signaling.

### 6.7 Developer Portal

`/developers/*` in the web app, off unless `NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED=true`. Workspaces
(the developer organization model, separate from business organizations), members and roles,
projects, environments (sandbox / production with production-access requests), service accounts and
credentials (secret shown once; rotate / revoke), API scopes, usage and limits, webhooks with
deliveries and replay, audit, docs. Endpoints: the Developer Portal section of the endpoint reference;
handoff `docs/phases/phase-06/developer-console-api-handoff.md`. Legacy API-key and meeting-webhook
screens are read-only with a migration notice (deprecated, §9).

## 7. Realtime summary

| Channel | Mint | Connect | Resume / recovery |
|---|---|---|---|
| Conference events | `POST /api/v2/conferences/{id}/realtime/ticket` (session or participant token) | WS `/api/v2/conferences/{id}/realtime?ticket=` | `GET /api/v2/conferences/{id}/events?after=` |
| Messaging (MANAGED) | session token → realtime ticket | Messaging realtime WS | server ack cursor; REST `events?after=` |
| Calling signaling | `POST /v1/calling/signaling-credentials` (per registered endpoint) | WS `/v1/calling/signaling` | pending invitations `GET …/calling/invitations` |
| Hub change feed | — (no WebSocket) | poll `GET /api/v2/communications/sync?after=` | `resyncRequired` → `GET …/bootstrap` |

Tickets are one-shot and short-lived; never cache them. Every realtime connection is closed by the
server when its session, device or membership is revoked; reconnect only after re-minting.

## 8. Errors, pagination, idempotency, time

* Envelope (Meeting Service): `{"success": false, "error": {"code", "message", "requestId"}}`. Hub:
  `{"schemaVersion": 1, "error": {"code", "message"}}`. Calling / Messaging / Developer Platform:
  `{"error": {"code", "message", ...}}` with `X-Request-Id`. Always branch on `error.code`, show
  `message` only as fallback text, and include `requestId` in support / bug reports.
* Generic handling: `401` → refresh once, then session ended; `403 PERMISSION_DENIED` → hide / disable
  the action; `404` → not found (also used deliberately to avoid revealing existence); `409
  *_VERSION_CONFLICT` / `VERSION_CONFLICT` → reload and retry; `409 ORGANIZATION_SUSPENDED`; `410`
  expired / used tokens; `429 RATE_LIMITED` (respect `Retry-After`); `503 SERVICE_UNAVAILABLE` /
  `REVOCATION_CHECK_UNAVAILABLE` → retry with backoff, never treat as allowed; `FEATURE_DISABLED` →
  hide the feature.
* Pagination: cursor based almost everywhere (`limit` + `cursor` → `nextCursor`; Meeting Service
  limits 1–200, default 50). Messaging history uses sequences (`beforeSequence` / `afterSequence`);
  event replay uses `after`. A few legacy v1 lists use `page`.
* Idempotency: send a fresh UUID `Idempotency-Key` on every operation that declares it (Hub writes,
  Calling commands, Developer Platform creates) and reuse it only to retry the same request.
* Time: all timestamps are RFC 3339 UTC; meeting schedules also carry the IANA time zone.

## 9. Files and media

* Meeting artifacts (recordings, transcripts) and voicemail / call recordings: request a signed
  download grant, then fetch the short-lived URL (≤ 15 minutes). The object-storage origin must be in
  the web CSP `connect-src` (`CSP_STORAGE_URL` / `CSP_EXTRA_CONNECT_SRC`).
* Messaging attachments (MANAGED): create → upload with the returned grant → finalize → poll while
  the scan state is `VERIFYING` → send with `attachmentIds`; `QUARANTINED` / `REJECTED` → "rejected
  by the security scan". Attachments need configured object storage and a scanner (otherwise
  disabled / pending) — staging configuration item.
* Media (meetings and calls) is LiveKit WebRTC with credentials issued per participant session;
  never reuse a credential across sessions.

## 10. Environments, flags and external gates

Base URLs come from environment variables (`apps/web/.env.example`, `apps/mobile/.env.example`):
`NEXT_PUBLIC_API_URL` / `EXPO_PUBLIC_API_URL` (Meeting Service), `NEXT_PUBLIC_CALLING_API_URL`
(+ server-only `CALLING_*`), `NEXT_PUBLIC_MESSAGING_URL`, `NEXT_PUBLIC_DEVELOPER_API_BASE_URL`,
`NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED`. Production API host: `https://api.cfmeet.com`. The staging
host convention is `https://staging-api.cfmeet.com`.

**Staging readiness:** the consolidated backend on `staging` is fully certified in CI against real
service processes and databases (see the final certification report). A hosted staging deployment of
this exact build is an owner deployment step in the activation runbook; until it is done, run the
full backend locally with the certification stack (`certification/` → `cfmeet-cert-stack up
--env-out <file>`, which also provisions a developer sandbox and a user session) or the compose file
for the Meeting Service.

Backend feature flags that change what clients see (all **off** by default; the owner enables them
per environment):

| Flag | Effect when off |
|---|---|
| `ORGANIZATIONS_AUTHORITY_MODE` (`legacy` → `shadow` → `authoritative`) | first-party organization routes answer `FEATURE_DISABLED`; workspaces come from the legacy directory |
| `ORGANIZATION_EVENTS_ENABLED` | organization offboarding fan-out off |
| `ORGANIZATION_CALLING_SCOPE` / organization Calling binding | Enterprise Calling relay answers `ORGANIZATION_CALLING_DISABLED` |
| Organization Messaging binding | organization Messaging relay refuses (`PROVISIONING_PENDING` / disabled) |
| `MESSAGING_SESSION_DEVICES_ENABLED` | MANAGED realtime via session devices off (polling) |
| `NOTIFICATIONS_EMAIL_ENABLED` | meeting notification emails not sent |
| `EMAIL_VERIFICATION_ENFORCEMENT` (`off`/`communications`/`login`) | unverified accounts keep access |
| `FEATURE_CONFERENCE_REALTIME_ENABLED`, `FEATURE_WEBINAR_MODE_ENABLED`, `FEATURE_HEALTHCARE_PROFILE_ENABLED`, `FEATURE_QUALITY_TELEMETRY_ENABLED` | the corresponding v2 meeting features answer `FEATURE_DISABLED` |
| `NEXT_PUBLIC_DEVELOPER_PLATFORM_ENABLED` (web) | Developer Portal hidden |

Externally gated (do **not** present as working; the backend reports them truthfully):

| Capability | What the API says | Client behaviour |
|---|---|---|
| Push (APNs / FCM / Web Push) | notification delivery status `NOT_CONFIGURED` | in-app / open-tab ringing and notifications only |
| SMS / phone verification | `PROVIDER_BLOCKED` | hide "verify phone"; never collect the number for search |
| PSTN calling, real numbers | Telephony / Numbering flag-disabled | no dial-out to phone numbers |
| PRIVATE_E2EE | device operations disabled in production | MANAGED only; label honestly |
| Physical-device acceptance | not done | test on real devices before release |

## 11. What is backend-complete vs. frontend work remaining

Backend: every endpoint in the reference is implemented, contract-tested and certified (see the final
certification report for the journey evidence). Known backend limitations a client must work around
(documented, not defects): conversations carry no organization field (partition by which relay you
called); `GET /api/v2/messaging/directory` spans all of the caller's organizations (filter with the
organization members list); `principals/names` only names colleagues (DIRECT chats take the name from
the Hub conversation context); no batch reaction read; Hub timeline changes arrive as `timeline.changed` events on the conversation's Messaging stream (other Hub data: poll); contact
requests carry only the CF Meet ID; no "this and following" recurrence split (edit the series or one
occurrence).

Frontend implementation remaining (from the Section 17 records; backend contract already closed):

| Area | Web | Mobile |
|---|---|---|
| Accounts / sessions / profile | done | v2 move done on staging; Google sign-in on mobile needs a native redirect flow; secure-store review |
| Organizations admin, workspaces, teams, policies | done (capability-gated) | organization experience to build (ORG-016) |
| Contacts / CF Meet ID / QR | done | to build |
| Messaging (MANAGED, organization view) | done | managed chats done; organization view to build |
| Calling personal | done (adapter); two-browser staging E2E pending staging credentials | first-party calling route + push pending |
| Enterprise Calling admin | done (behind backend flags) | not planned for mobile |
| Meetings / calendar / guests / pre-join | done (recurrence UI, time zones, pre-join consents) | v2 live room done; pre-join consents and recurrence UI to finish |
| Developer Portal | done (flag) | n/a |
| Physical-device and accessibility acceptance | owner gate | owner gate |
