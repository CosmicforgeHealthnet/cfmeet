# CF Meet Platform & Apps — frontend API guide

**Audience:** the CF Meet web frontend developer. **Scope:** everything a partner touches (CF Meet Platform: users, user tokens, chat, calls, push, meetings; CF Meet Apps: registration, OAuth, delegated access, installs, events, directory, in-meeting apps).

**Source of truth.** The endpoint reference (Parts 1–3) and the TypeScript types (Appendix A) are generated from the published contracts, not retyped:

- `packages/contracts/bundled/public/aggregate/v1.openapi.json` (the public API gateway, `https://api.cfmeet.com`)
- `packages/contracts/openapi/meeting-service.json` (the Meeting Service `/api/v2` routes the CF Meet web app calls directly)

Both contracts are checked against the code in CI. If this guide and a contract ever disagree, the contract wins. Regenerate it with `python3 scripts/contracts/partner_guide/generate.py`; don't hand-edit the generated parts.

Companion documents: `docs/frontend/cf-meet-platform-handover.md` (the backend team's page-by-page handover; its UI decisions are folded in below) and `docs/frontend/cfmeet-frontend-endpoint-reference.md` (one-line index of every client endpoint in the product).

---

## 0. Status — what is live in production

All 13 Platform and Apps activations are live in production. The final two-tenant acceptance passed (131/131 checks) and production certification passed on 2026-11-21:

| # | Feature | Live | What it unlocks for the frontend |
|---|---|---|---|
| 1 | Certification | ✅ | — |
| 2 | Partner chat | ✅ | Partner users chat through Messaging (Parts 3.2 / 3.3) |
| 3 | Partner calls | ✅ | Partner server rings partner users (Parts 3.4 / 3.5) |
| 4 | Calling through the gateway | ✅ | `/v1/calls`, `/v1/calling/*` on `api.cfmeet.com` |
| 5 | Partner push | ✅ | **Push credentials** screen (Part 1.3); device push registration (Part 3.3) |
| 6 | Partner meetings | ✅ | `/v1/meetings/*` machine family (Part 3.6); user-token join (Part 3.7); partner meeting chat |
| 7 | App registration | ✅ | **CF Meet App** screens (Part 1.4) |
| 8 | OAuth | ✅ | **Consent page** (Part 2.1), **Connected apps** (Part 2.2), `/v1/oauth/*` (Part 3.8) |
| 9 | Delegated apps | ✅ | Apps act for a user on `/v1/meetings/*` and Messaging |
| 10 | Workspace installs | ✅ | **Workspace → Apps** (Part 2.3); `install=organization` on the consent page |
| 11 | App events | ✅ | `eventTypes` in the app manifest; delivered through the environment's webhooks |
| 12 | App directory | ✅ | **App directory** (Part 2.5) |
| 13 | In-meeting apps | ✅ | **In-meeting apps** panel (Part 2.4) |

**What the backend does not yet prove or allow** (design around these; don't treat them as bugs):

- **No app reviewers are named yet.** Apps work end to end as `DRAFT` for their test accounts (the developer's own account plus up to 25 listed accounts), and `Submit for review` works. Approval is done by named reviewers through the operator CLI (ADR 0065). There is no review page in the web app, and the owner has not named any reviewers. Until they do, nothing reaches `APPROVED`/`PUBLISHED`, so production has no public installs and an empty app directory. The directory page must handle an empty list.
- **Real push ringing** needs a partner's own APNs/FCM keys and a real device. Uploading and verifying keys is live; actual delivery to a phone has not been demonstrated.
- **An allowed workspace install** has not been exercised in production acceptance (it needs a business-organization fixture). Refusals and the policy plumbing are live.
- **Production alerting** isn't delivering yet (an operations item, not an API one).
- **Known backend follow-ups that limit screens:** a workspace's app policy (who may install, the allow-list) can't be edited from the web app yet, so Workspace → Apps is list-and-remove only, with the default "admins install" policy. App directory entries have no install / homepage URL yet: an install starts on the app's own site.

---

## What to build

The web app already has a developer console (`src/app/developers/*`) using `src/lib/developer-platform/client.ts` for workspaces, projects, scopes, environments, service accounts, credentials, webhooks, usage and production access. These are the new screens:

| Where | Screen | Endpoints |
|---|---|---|
| Console → Environment | **End users** (list, search by externalId) | `GET /v1/environments/{environmentId}/users` |
| Console → Environment (SANDBOX only) | **Test token generator** (create a user and mint a `cfut_…` token, shown once) | `POST /v1/environments/{environmentId}/test-user-tokens` |
| Console → Environment | **Allowed websites** (exact origins, max 20) | `GET`/`PUT /v1/environments/{environmentId}/allowed-origins` |
| Console → Environment | **Push credentials** (APNs / FCM upload, verify, delete; max 3) | `/v1/environments/{environmentId}/push-credentials…` |
| Console → Project | **CF Meet App**: register or edit the manifest, client secret (shown once), test accounts, submit for review, installs (grants) | `/v1/projects/{projectId}/app…` |
| CF Meet web (signed-in user) | **OAuth consent page** at `/oauth/authorize` | `GET /api/v2/oauth/authorize/context`, `POST /api/v2/oauth/authorize/approve` |
| CF Meet web → Settings | **Connected apps** (list, remove) | `GET /api/v2/me/apps`, `DELETE /api/v2/me/apps/{grantId}` |
| CF Meet web → Workspace settings (OWNER/ADMIN) | **Workspace apps** (list, uninstall) | `GET /api/v2/organizations/{id}/apps`, `DELETE …/{grantId}` |
| CF Meet meeting room | **In-meeting apps** panel (list, open the app's iframe with a context token) | `GET /api/v2/conferences/{id}/apps`, `POST …/apps/{appId}/context` |
| Public | **App directory** (browse, app detail) | `GET /v1/app-directory`, `GET /v1/app-directory/{appId}` |
| Console → Docs (optional) | Partner integration docs | Part 3 (server and device APIs) |

---

## Basics

### B1. Hosts

| Calls | Base URL | Web app setting |
|---|---|---|
| Developer console (Part 1), app directory, all partner APIs (Part 3) | `https://api.cfmeet.com` (API gateway) | `NEXT_PUBLIC_DEVELOPER_API_BASE_URL` |
| CF Meet end-user screens (Part 2) | `https://api.cfmeet.com/api/v2/...` (Meeting Service) | origin of `NEXT_PUBLIC_API_URL` |

Staging uses `staging-api.cfmeet.com` the same way.

### B2. Credentials: who holds what

| Credential | Looks like | Who holds it | Sent as | Used for |
|---|---|---|---|---|
| CF Meet session | JWT access token | the signed-in CF Meet user (web app) | `Authorization: Bearer …` | Part 1 (as a developer) and Part 2 |
| Platform credential | `cfmk_<sbx\|test\|live>_<id>_<secret>` | the **partner's server** only | `Authorization: Bearer cfmk_…` | Part 3 server APIs. **Never display it after creation, never send it from a browser.** |
| User token | `cfut_<sbx\|test\|live>_<JWS>` (≤ 2048 bytes) | a partner user's device | `Authorization: Bearer cfut_…` | the device exchanges for chat/call sessions; meeting join |
| Client token | opaque | a partner device after `POST /v1/messaging/user-sessions` | `Authorization: CF-Client-Token <token>` | partner chat device routes (Part 3.3) |
| Signaling credential | `cfsig_<JWS>` | a partner device after `POST /v1/calling/user-sessions` | `Authorization: Bearer cfsig_…` | ring offers, signaling WebSocket, media credentials (Part 3.5) |
| Participant token | opaque | a meeting participant after join | `X-Participant-Token: …` | in-meeting calls after join, in-meeting apps |
| App client secret | `cfcs_…` | the **app developer's server** | HTTP Basic (`clientId:clientSecret`) or form fields | `POST /v1/oauth/token` (shown once in the console) |
| OAuth access / refresh token | `cfoa_…` / `cfor_…` | the app's server | `Authorization: Bearer cfoa_…` | delegated calls for one user |

The environment kind decides the prefix: `SANDBOX → sbx`, `TEST → test`, `PRODUCTION → live`. The test token generator works only in `SANDBOX` (other kinds return `422 POLICY_VIOLATION`).

### B3. Scopes a project needs per feature

A partner grants scopes to the project (existing console calls `GET /v1/scopes`, `POST /v1/projects/{projectId}/scopes`), then issues a credential with them:

| Feature | Platform credential scopes |
|---|---|
| Users | `principals.read`, `principals.write` |
| User tokens | `user_tokens.issue` |
| Chat | `conversations.create`, `conversations.manage`, `messages.read`, `messages.send` |
| Calls | `calls.read`, `calls.write` |
| Meetings | `meetings.read`, `meetings.write` |

The grants inside a user token (`UserTokenGrants`: `msg`, `call`, `mtg`) only work while the minting credential still holds the matching scope. They are re-checked on every use.

### B4. Conventions

- **Errors.** Gateway and Developer Platform routes return `{ "error": { "code", "message", "requestId" } }` (type `Error`). Meeting Service `/api/v2` routes return `{ "success": false, "error": { "code", "message", "requestId", "details"?, "retryAfterSeconds"? } }` (type `ErrorEnvelope` / `ErrorBody`). OAuth endpoints return RFC 6749 errors (`OAuthError`: `error`, `error_description`). Branch on `error.code`, never on `message`. Show `requestId` in support-facing error UI.
- **Idempotency.** Endpoints listing an `Idempotency-Key` header accept a client-generated UUID. Send one on every create and retry with the same key. A replay answers with `Idempotent-Replayed: true`.
- **Pagination.** List endpoints take `cursor` / `limit` and return `nextCursor` (`null` on the last page).
- **Secrets shown once.** `test-user-tokens` (`token`), `app/secret` (`clientSecret`) and credential creation return the secret once with `Cache-Control: no-store`. Show it with a copy button and a "you won't see this again" warning, and never store it client-side.
- **Rate limits.** A `429` carries `Retry-After` (and `retryAfterSeconds` on `/api/v2`). Wait at least that long before retrying.
- **Roles.** Console write operations need workspace role OWNER/ADMIN (some allow DEVELOPER; see each endpoint's description). Hide or disable controls the user's role can't use. The server enforces this regardless.

---

## Flows

### F1. Partner onboarding (console)

1. Workspace, project and environment (`SANDBOX` first), using existing screens.
2. Grant the project the scopes for the features it uses (Part 2.3).
3. Create a service account and a platform credential with those scopes (existing screens). The partner copies `cfmk_…` to its server.
4. **Allowed websites:** add the partner web app's exact origins (`https://app.partner.com`; `http://localhost:3000` allowed in SANDBOX/TEST; wildcards are refused). Without this, browsers can't call the device routes.
5. **Push credentials** (optional): upload APNs (`.p8` key; VoIP keys are separate entries) and/or FCM (service account) credentials, then **Verify**. The key file is sent once and sealed to the Notification Service. The console only ever gets metadata back (`provider`, `voip`, `teamId`, `keyId`, `bundleId`, `apnsHost`, `projectId`, `fingerprint`, `status`, `lastVerifiedAt`, `lastError`). Never echo or cache the uploaded file.
6. **Test token generator:** enter an `externalId` (the partner's own user id, never a phone number), a display name, grants and an optional `ttlSeconds` (60–86400, default 3600). Show the returned `token` once with `expiresAt` and a copy button; afterwards only `tokenRedacted`. The developer pastes it into their app to try chat, calls and meetings without writing server code.
7. **End users:** a list of the users the partner created (from its server or the generator).

### F2. Partner chat (what a partner app does)

1. Server: `POST /v1/users` (idempotent on `externalId`), then `POST /v1/users/{userId}/tokens` with `grants.msg = ["read","send"]`.
2. Server: `POST /v1/conversations` (`securityProfile: "MANAGED"`, `managedDisclosureAcknowledged: true`, header `CF-Acting-Principal: <userId>`).
3. Device: `POST /v1/messaging/user-sessions` with the user token gives `{ principalId, clientToken, … }`.
4. Device: `GET/POST /v1/conversations/{id}/messages` with `Authorization: CF-Client-Token <clientToken>`; `POST /v1/realtime/tickets` for live updates.

Partner devices see MANAGED conversations only. End-to-end-encrypted (MLS) routes refuse them.

### F3. Partner calls

1. Device: `POST /v1/calling/user-sessions` (user token with `grants.call = ["receive"]`) gives an endpoint and a `cfsig_…` signaling credential.
2. Server: `POST /v1/calls` places the call (`targetPrincipalIds`). The callee's device sees it in `GET /v1/calling/invitations` (or on the signaling WebSocket) and accepts with `POST /v1/calling/invitations/{id}/accept` (`expectedVersion` required).
3. Call control (cancel, end, and so on) needs the call's current `version` (`expectedVersion`), read from `GET /v1/calls/{call_id}`.

### F4. Partner meetings

1. Server: `POST /v1/meetings/meeting-spaces`, then `POST /v1/meetings/meeting-spaces/{id}/conferences`, then **`POST /v1/meetings/conferences/{id}/start`**. Joins are admitted only while the conference is `LIVE`.
2. Server: mint the user's token with `grants.mtg = [{ conferenceId, role }]`.
3. Device: `POST /api/v2/conferences/{id}/join` with the user token. The response carries the participant (role from the grant, name from the user profile), `participantToken` and media credentials. Re-joining keeps the same participant. A user without a grant for that conference gets `403`; another environment's user gets `404`.
4. **Meeting chat for partner users** is a MANAGED Messaging conversation linked to the conference. The joined user finds it with their partner session device: `GET /v1/conversations?contextClass=MEETING`, item with `context.namespace == "meeting"` and `context.reference == "conference:<id>"`. Users who have not joined are not members. (The first-party route `/api/v2/conferences/{id}/messages` is **not** the partner chat surface.)
5. Server: `POST /v1/meetings/conferences/{id}/end` ends it, which also closes the meeting chat.

### F5. CF Meet Apps — registration (Part 1.4)

1. `PUT /v1/projects/{projectId}/app` with the manifest (`PutAppRequest`: name, redirect URIs, user/bot scopes, event types, optional `meetingSurface`, `distribution`). The response is the `App` with `status: "DRAFT"`. `clientId` = `projectId`.
2. `POST …/app/secret` returns `clientSecret` (`cfcs_…`) once. Rotating replaces it.
3. `PUT …/app/test-accounts` with CF Meet user ids allowed to authorize the DRAFT app (the developer is always one).
4. `POST …/app/submit` moves it to review (`SUBMITTED`; see the review note in section 0). PUBLIC apps need a privacy policy and terms URL, and loopback redirect URIs are refused at submit.
5. `GET …/app/grants` lists installs (who authorized the app).

Status chips: `DRAFT`, `SUBMITTED`, `IN_REVIEW`, `APPROVED`, `REJECTED` (show `review.reasonCode`), `PUBLISHED`, `SUSPENDED`. `SUBMITTED`, `IN_REVIEW` and `SUSPENDED` apps are locked, so disable editing. Every manifest change becomes a new immutable version. Show `pendingChanges` when an approved or published app has an unsubmitted edit. These screens need a human console session (machine credentials are refused).

### F6. CF Meet Apps — OAuth consent page (Part 2.1)

The app sends the user to:

```
https://cfmeet.com/oauth/authorize?client_id=…&redirect_uri=…&scope=profile:read%20meetings:write&state=…&code_challenge=…&code_challenge_method=S256
                                  [&install=organization&organization_id=…]   (workspace install)
```

The page:

1. Requires sign-in (keep the full query string through the login redirect).
2. Calls `GET /api/v2/oauth/authorize/context` with the query parameters **unchanged** (snake_case). On `200` (`AuthorizeContextResponse`), render the app (`app`: name, logo, developer, distribution, status), the requested scopes (`scopes[]` with descriptions) and the `target` (`USER` / `ORGANIZATION`). For a workspace install, `organizationInstall.allowed` is false with a `reason` of `NOT_A_MEMBER`, `ROLE_REQUIRED`, `NOT_ALLOWLISTED` or `ORGANIZATION_NOT_READY`; in that case disable **Allow** and show the reason.
3. On `4xx`, **don't redirect back to the app**: the client or redirect URI may be invalid. Show an error page with `error.code`. Example: B's account on a DRAFT app it isn't a test account for gets `400 APP_NOT_AVAILABLE`. PKCE `plain` gets `400 OAUTH_REQUEST_INVALID`.
4. **Allow / Deny** calls `POST /api/v2/oauth/authorize/approve` (`AuthorizeDecisionRequest`: `clientId`, `redirectUri`, `scope`, `state`, `codeChallenge`, `codeChallengeMethod`, `decision: "APPROVE" | "DENY"`, plus `install`/`organizationId` for workspace installs), then **navigate the browser to `redirectTo`**. That is `redirect_uri?code=…&state=…` on approve, or `?error=access_denied&state=…` on deny.

The app then exchanges the code at `POST /v1/oauth/token` (server-side, client secret plus `code_verifier`). Access tokens are `cfoa_…`; refresh tokens `cfor_…` rotate, and reusing a superseded refresh token revokes the whole grant.

### F7. Connected apps / Workspace apps (Parts 2.2, 2.3)

- `GET /api/v2/me/apps` lists `ConnectedApp` items (`name`, `logoUrl`, `developerName`, `scopes`, `target` `USER`/`ORGANIZATION`, `hasMeetingSurface`). **Remove** calls `DELETE /api/v2/me/apps/{grantId}`, which revokes immediately (the app's tokens stop working).
- Workspace admins: `GET /api/v2/organizations/{id}/apps` and `DELETE …/{grantId}` (OWNER/ADMIN only; others get `403`).

### F8. In-meeting apps (Part 2.4)

1. In the meeting room, `GET /api/v2/conferences/{id}/apps` lists the apps installed for the meeting owner (personal) or the meeting's organization that declare a meeting surface (`surface.url`, allowed `origins`). Admitted participants only.
2. Opening an app: `POST /api/v2/conferences/{id}/apps/{appId}/context` returns `{ contextToken, expiresIn: 300 }`. Fetch a new one before it expires while the panel stays open.
   - Load `surface.url` in a **sandboxed iframe**: `sandbox="allow-scripts allow-forms allow-same-origin allow-popups"`, with no top navigation.
   - Hand the token over with `postMessage`, addressed to the exact origin of `surface.url`. Accept messages back only from `surface.origins`.
   - The meeting page CSP must allow `frame-src` for the surface origins. They are per app, so build the CSP per request or relax it for this panel.
3. The app verifies the token against the public keys at `GET /api/v2/app-context-keys` (Ed25519 JWKS). It carries the conference, a pairwise participant id and the role, plus the user's pairwise `sub` only if that user granted the app.

---
