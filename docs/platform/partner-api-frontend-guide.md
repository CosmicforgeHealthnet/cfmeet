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

## Part 1 — Developer Console screens to add (`NEXT_PUBLIC_DEVELOPER_API_BASE_URL`)

All Part 1 calls use the existing developer console client (`src/lib/developer-platform/client.ts`): the signed-in developer's CF Meet session as `Authorization: Bearer`, through the API gateway. Add these operations to its operation map.

### 1.1 End users and the test token generator

#### `GET /v1/environments/{environmentId}/users`

**List an environment's partner users in the Developer Console**

Roles: OWNER, ADMIN, DEVELOPER (human session; delegated to Platform Access). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `listEnvironmentUsers`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |
  | `limit` | query | `number` | no |  |
  | `cursor` | query | `string` | no |  |

- **Responses:**

  - `200` OK. → `EndUserList`
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/environments/{environmentId}/test-user-tokens`

**Development token generator: create the user if new and mint a user token in a SANDBOX environment (other kinds: 422 POLICY_VIOLATION)**

Roles: OWNER, ADMIN, DEVELOPER (human session). Flag DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `issueTestUserToken`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Request body** (`application/json`, required): `UpsertUserTokenRequest`

- **Responses:**

  - `201` Minted. → `UserWithToken`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `Error`

### 1.2 Allowed websites (CORS for the partner's web app)

#### `GET /v1/environments/{environmentId}/allowed-origins`

**Read the environment's allowed websites (exact origins that may use device logins from a browser)**

Roles: every member. Flag DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED. Machine credentials need `projects.read`.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`) **or** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `getAllowedOrigins`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `AllowedOrigins`
  - Errors `401`, `403`, `404`, `503` → `Error`

#### `PUT /v1/environments/{environmentId}/allowed-origins`

**Replace the environment's allowed websites (at most 20; https only, http://localhost and http://127.0.0.1 in SANDBOX / TEST)**

Roles: OWNER, ADMIN. Flag DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED. Machine credentials need `credentials.manage`.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`) **or** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `putAllowedOrigins`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |

- **Request body** (`application/json`, required): `PutAllowedOriginsRequest`

- **Responses:**

  - `200` OK. → `AllowedOrigins`
  - Errors `400`, `401`, `403`, `404`, `422`, `503` → `Error`

### 1.3 Push credentials (the partner's own APNs / FCM keys)

#### `GET /v1/environments/{environmentId}/push-credentials`

**List the environment's push credentials (metadata only)**

Roles: OWNER, ADMIN (human session). Flag DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `listPushCredentials`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `PushCredentialList`
  - Errors `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/environments/{environmentId}/push-credentials`

**Upload (replace) an APNs or FCM push credential: the secret is sealed to the Notification Service on receipt and never stored or returned; SANDBOX / TEST may only use the APNs sandbox host; …**

PRODUCTION needs the PUSH capability live. Roles: OWNER, ADMIN (human session). Flag DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `createPushCredential`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Request body** (`application/json`, required): `CreatePushCredentialRequest`

- **Responses:**

  - `201` Uploaded. → `PushCredential`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `503` → `Error`

#### `DELETE /v1/environments/{environmentId}/push-credentials/{credentialRef}`

**Delete a push credential (removed from the Notification Service; pushes with it stop)**

Roles: OWNER, ADMIN (human session). Flag DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `deletePushCredential`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |
  | `credentialRef` | path | `Uuid` | yes |  |

- **Responses:**

  - `204` Deleted.
  - Errors `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/environments/{environmentId}/push-credentials/{credentialRef}/verify`

**Verify a push credential with its provider (APNs JWT / FCM OAuth token) and record the outcome**

Roles: OWNER, ADMIN (human session). Flag DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `verifyPushCredential`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `environmentId` | path | `Uuid` | yes |  |
  | `credentialRef` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` Verified (see status). → `PushCredential`
  - Errors `401`, `403`, `404`, `503` → `Error`

### 1.4 CF Meet App registration

#### `GET /v1/projects/{projectId}/app`

**Read the app registered on the project (current manifest, review status, live version)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `getProjectApp`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `App`
  - Errors `401`, `403`, `404`, `503` → `Error`

#### `PUT /v1/projects/{projectId}/app`

**Register or change the project's app (a changed manifest is a new immutable version; SUBMITTED / IN_REVIEW / SUSPENDED apps are locked)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `putProjectApp`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |

- **Request body** (`application/json`, required): `PutAppRequest`

- **Responses:**

  - `200` Changed or unchanged. → `App`
  - `201` Registered. → `App`
  - Errors `400`, `401`, `403`, `404`, `409`, `503` → `Error`

#### `GET /v1/projects/{projectId}/app/grants`

**List the app's installations (from Platform Access; no CF Meet account id)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `listProjectAppGrants`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |
  | `cursor` | query | `string` | no |  |

- **Responses:**

  - `200` OK. → `AppGrantList`
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/projects/{projectId}/app/secret`

**Issue or rotate the app's client secret (returned once; the previous secret keeps Platform Access's 24 h overlap)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `rotateProjectAppSecret`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Responses:**

  - `201` Issued (Cache-Control: no-store). → `AppClientSecret`
  - Errors `401`, `403`, `404`, `409`, `503` → `Error`

#### `POST /v1/projects/{projectId}/app/submit`

**Submit the current version for review (DRAFT / REJECTED, or an APPROVED / PUBLISHED app with pending changes; no loopback redirect; PUBLIC apps name privacy policy and terms)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `submitProjectApp`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `App`
  - Errors `400`, `401`, `403`, `404`, `409`, `503` → `Error`

#### `PUT /v1/projects/{projectId}/app/test-accounts`

**Replace the draft test accounts (CF Meet account ids, at most 25, format checked only)**

Roles: OWNER, ADMIN. Human console session only (machine credentials are refused). Flag DEVELOPER_PLATFORM_APPS_ENABLED.

- **Auth:** Developer session — the signed-in CF Meet user's access token (`Authorization: Bearer <CF Meet session>`)
- **operationId:** `putProjectAppTestAccounts`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `projectId` | path | `Uuid` | yes |  |

- **Request body** (`application/json`, required): `PutAppTestAccountsRequest`

- **Responses:**

  - `200` OK. → `App`
  - Errors `400`, `401`, `403`, `404`, `409`, `503` → `Error`

## Part 2 — CF Meet web screens for end users (`api.cfmeet.com/api/v2`, CF Meet session)

### 2.1 OAuth consent page (`https://cfmeet.com/oauth/authorize`)

#### `GET /api/v2/oauth/authorize/context`

**Consent data for an app's authorization request**

The CF Meet consent page (`https://cfmeet.com/oauth/authorize`) forwards the app's authorization request unchanged. The signed-in account is required; Platform Access validates the client, the exact redirect URI, the scopes and PKCE (S256 only) for that account. A workspace install (`install=organization&organization_id=…`) also reports whether the organization allows this account to install the app.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_oauth_authorize_context`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `client_id` | query | `string \| null` | no | The app's OAuth client id (= app id). |
  | `redirect_uri` | query | `string \| null` | no | Must exactly match a redirect URI registered for the app. |
  | `scope` | query | `string \| null` | no | Space-separated OAuth scopes (`profile:read meetings:write …`). |
  | `state` | query | `string \| null` | no | Opaque value echoed back to the app. |
  | `code_challenge` | query | `string \| null` | no | PKCE S256 challenge (43-128 characters). |
  | `code_challenge_method` | query | `string \| null` | no | Must be `S256`. |
  | `install` | query | `string \| null` | no | `organization` for a workspace install (default: a personal install). |
  | `organization_id` | query | `string \| null` | no | The organization of a workspace install. |
  | `response_type` | query | `string \| null` | no | Ignored (the response type is always `code`). |

- **Responses:**

  - `200` Consent data → `AuthorizeContextResponse`
  - Errors `400`, `401`, `404`, `503` → `ErrorEnvelope`

#### `POST /api/v2/oauth/authorize/approve`

**Approve or deny an app's authorization request**

`APPROVE` creates (or extends) the grant at Platform Access — for a workspace install only if the organization's app policy allows this account — and returns `redirect_uri?code=…&state=…`; `DENY` returns `redirect_uri?error=access_denied&state=…`. The browser navigates to `redirectTo`.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_oauth_authorize_decide`
- **Request body** (`application/json`, required): `AuthorizeDecisionRequest`

- **Responses:**

  - `200` Where to send the browser → `AuthorizeDecisionResponse`
  - Errors `400`, `401`, `403`, `404`, `503` → `ErrorEnvelope`

### 2.2 Settings → Connected apps

#### `GET /api/v2/me/apps`

**The signed-in account's connected apps (personal installs)**

Every app the account authorized for itself (USER grants), with the scopes granted, for the Connected apps settings page.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_list_my_apps`
- **Responses:**

  - `200` Connected apps → `ConnectedAppList`
  - Errors `401`, `404`, `503` → `ErrorEnvelope`

#### `DELETE /api/v2/me/apps/{grantId}`

**Disconnect one of the signed-in account's apps**

Revokes the grant and every token issued for it at Platform Access. A grant that is not the account's own is `APP_NOT_FOUND`.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_revoke_my_app`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `grantId` | path | `string` | yes | Grant id |

- **Responses:**

  - `204` Revoked
  - Errors `401`, `404`, `503` → `ErrorEnvelope`

### 2.3 Workspace settings → Apps (organization admins)

#### `GET /api/v2/organizations/{id}/apps`

**The apps installed for an organization**

Workspace installs (ORGANIZATION grants) of the organization, for its OWNERs and ADMINs only (organization role checked with the Organizations Service).

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_list_organization_apps`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Organization id |

- **Responses:**

  - `200` Installed apps → `ConnectedAppList`
  - Errors `401`, `403`, `404`, `503` → `ErrorEnvelope`

#### `DELETE /api/v2/organizations/{id}/apps/{grantId}`

**Uninstall an app from an organization**

Revokes the grant, every token and the app's bot. Organization OWNERs and ADMINs only; a grant of another organization is `APP_NOT_FOUND`.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`)
- **operationId:** `v2_revoke_organization_app`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Organization id |
  | `grantId` | path | `string` | yes | Grant id |

- **Responses:**

  - `204` Revoked
  - Errors `401`, `403`, `404`, `503` → `ErrorEnvelope`

### 2.4 In-meeting apps (meeting room)

#### `GET /api/v2/conferences/{id}/apps`

**Apps with an in-meeting panel for this conference**

Apps installed for the meeting owner (personal install) or for the meeting's organization that declare a meeting surface. Admitted participants only.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`) **or** Participant token (`X-Participant-Token` header, returned by join)
- **operationId:** `v2_list_conference_apps`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` In-meeting apps → `ConferenceAppList`
  - Errors `404`, `503` → `ErrorEnvelope`

#### `POST /api/v2/conferences/{id}/apps/{appId}/context`

**A 5-minute app-context token for an in-meeting app**

An Ed25519 JWS (`aud` = the app's client id) carrying the conference, a pairwise participant id and the participant's role, plus the user's pairwise `sub` only if that user granted the app. Verify it with `GET /api/v2/app-context-keys`.

- **Auth:** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`) **or** Participant token (`X-Participant-Token` header, returned by join)
- **operationId:** `v2_create_app_context`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |
  | `appId` | path | `string` | yes | App id (OAuth client id) |

- **Responses:**

  - `200` Context token → `AppContextTokenResponse`
  - Errors `404`, `503` → `ErrorEnvelope`

#### `GET /api/v2/app-context-keys`

**Public keys of app-context tokens (JWKS)**

The Ed25519 keys (OKP JWKs, `kid` = RFC 7638 thumbprint) apps use to verify the app-context tokens of their in-meeting panel. Anonymous; cacheable for 5 minutes.

- **Auth:** None (public)
- **operationId:** `v2_app_context_keys`
- **Responses:**

  - `200` JWKS → `AppContextKeySet`
  - Errors `404` → `ErrorEnvelope`

### 2.5 App directory (public, no sign-in)

#### `GET /v1/app-directory`

**List the published public apps**

The app directory: PUBLISHED, PUBLIC, not suspended apps with their published manifest (name, description, logo, developer, scopes, privacy and terms URLs, whether they have an in-meeting surface). Anonymous. Flag DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED.

- **Auth:** None (public)
- **operationId:** `listAppDirectory`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `limit` | query | `number` | no |  |
  | `cursor` | query | `string` | no |  |

- **Responses:**

  - `200` OK. → `DirectoryAppList`
  - Errors `400`, `404`, `429`, `503` → `Error`

#### `GET /v1/app-directory/{appId}`

**Read one published public app**

One directory entry (anything not PUBLISHED, PUBLIC and live is 404). Anonymous. Flag DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED.

- **Auth:** None (public)
- **operationId:** `getAppDirectoryEntry`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `appId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `DirectoryApp`
  - Errors `404`, `429`, `503` → `Error`

## Part 3 — Partner integration reference (partner servers and devices)

These are called by partner applications (their servers and their users' devices), not by the CF Meet web app. They are here so the docs page, the token generator output and any SDK work share one source. Never put a platform credential (`cfmk_…`) in a browser.

### 3.1 Server — users and user tokens

#### `GET /v1/users`

**List the partner users of the credential's environment (optionally by `externalId`)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.read`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `listUsers`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `externalId` | query | `string` | no |  |
  | `limit` | query | `number` | no |  |
  | `cursor` | query | `string` | no |  |

- **Responses:**

  - `200` OK. → `EndUserList`
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/users`

**Create a partner user (idempotent on `externalId`: 201 new, 200 existing; environment caps answer 409 LIMIT_EXCEEDED)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.write`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `createUser`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Request body** (`application/json`, required): `CreateEndUserRequest`

- **Responses:**

  - `200` Existing user. → `EndUser`
  - `201` Created. → `EndUser`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `503` → `Error`

#### `POST /v1/users/tokens`

**Create the user if new and mint a CF Meet user token (returned once; Cache-Control no-store; a replay never returns the token)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Needs `principals.write` as well. Machine credentials need `user_tokens.issue`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `upsertUserToken`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Request body** (`application/json`, required): `UpsertUserTokenRequest`

- **Responses:**

  - `201` Minted. → `UserWithToken`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `Error`

#### `GET /v1/users/{userId}`

**Read a partner user (another environment's user is 404)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.read`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `getUser`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `userId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` OK. → `EndUser`
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `PATCH /v1/users/{userId}`

**Change a partner user's profile or suspend / reactivate them**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.write`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `updateUser`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `userId` | path | `Uuid` | yes |  |

- **Request body** (`application/json`, required): `UpdateEndUserRequest`

- **Responses:**

  - `200` OK. → `EndUser`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `503` → `Error`

#### `DELETE /v1/users/{userId}`

**Delete a partner user (tombstone: profile erased, id kept; their tokens and device credentials stop working)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.write`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `deleteUser`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `userId` | path | `Uuid` | yes |  |

- **Responses:**

  - `204` Deleted.
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/users/{userId}/revoke-sessions`

**Revoke every token and device session of a partner user issued so far**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `principals.write`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `revokeUserSessions`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `userId` | path | `Uuid` | yes |  |

- **Responses:**

  - `200` Revoked. → `RevokeSessionsResult`
  - Errors `400`, `401`, `403`, `404`, `503` → `Error`

#### `POST /v1/users/{userId}/tokens`

**Mint a CF Meet user token for a partner user (grants within the credential's scopes; returned once; at most 120 per user per hour)**

Machine credentials only (server-to-server; the API Gateway forwards the authenticated credential and Platform Access re-verifies it and takes the environment from it). Flag DEVELOPER_PLATFORM_USERS_API_ENABLED. Machine credentials need `user_tokens.issue`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only, never in a browser)
- **operationId:** `issueUserToken`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `userId` | path | `Uuid` | yes |  |
  | `Idempotency-Key` | header | `string` | no | Replays return the original response; reuse with a different request is IDEMPOTENCY_KEY_REUSED. One-time secrets are never replayed. |

- **Request body** (`application/json`, required): `IssueUserTokenRequest`

- **Responses:**

  - `201` Minted. → `UserToken`
  - Errors `400`, `401`, `403`, `404`, `422`, `429`, `503` → `Error`

### 3.2 Server — conversations (partner chat)

The partner server opens and manages MANAGED conversations for its users. Acting for a user: header `CF-Acting-Principal: <user id>`.

#### `POST /v1/conversations`

**create Conversation**

Scope conversations.create; the acting principal becomes the owner. Context class and security profile are checked against the persisted effective policy: the profile defaults to the context's default (PRIVATE_E2EE where eligible), MANAGED only where the application policy allows it, never a silent fallback (E2EE_NOT_AVAILABLE when encryption cannot be established). DIRECT conversations are deduplicated per principal pair and profile; a live context reference is unique. Idempotency-Key replays the stored response (200).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `createConversation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The Platform Access principal the application backend acts for; honoured only inside the credential's principal restriction and when Platform Access resolves it ACTIVE and messaging-capable in the credential's own environment. |
  | `Idempotency-Key` | header | `string` | yes |  |

- **Request body** (`application/json`, required): `CreateConversationRequest`

- **Responses:**

  - `201` Created (201) or an existing / replayed conversation (200). → `Conversation`
  - Errors `400`, `401`, `403`, `409`, `422`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}`

**get Conversation**

One conversation of the credential's environment; for a member principal (messages.read) or the administration (conversations.manage). Others: NOT_FOUND.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `getConversation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal's own conversations (scope messages.read; for PATCH: the owner). Without it: the application's administration view (scope conversations.manage). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `Conversation`
  - Errors `400`, `401`, `403`, `404`, `429`, `503` → `MessagingError`

#### `PATCH /v1/conversations/{conversationId}`

**update Conversation**

Scope conversations.manage. Server-authoritative lifecycle transitions (ACTIVE, LOCKED, CLOSED, ARCHIVED, DELETED; EXPIRED by the server only) and expiry, with optimistic concurrency (expectedVersion). Expiry is evaluated first. A security profile change is refused (SECURITY_PROFILE_IMMUTABLE). S13: announcementOnly (GROUP / CHANNEL) restricts sending, editing and forwarding to OWNER / ADMIN members.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `updateConversation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal's own conversations (scope messages.read; for PATCH: the owner). Without it: the application's administration view (scope conversations.manage). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `UpdateConversationRequest`

- **Responses:**

  - `200` OK. → `Conversation`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/members`

**add Member**

Scope conversations.manage (+ OWNER / ADMIN when a principal acts). The principal must be ACTIVE and messaging-capable in the same environment; re-admission opens a NEW history window (no server-granted history, AD-07). DIRECT membership is fixed. With invite: true the principal is INVITED (201, state INVITED) and becomes a member only when it accepts.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `addMember`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal acts (member checks apply). Without it: the application's administration (scope conversations.manage). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `AddMemberRequest`

- **Responses:**

  - `201` Created. → `Member`
  - `200` Idempotent replay of an earlier identical request. → `Member`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

### 3.3 Device — chat (partner session device)

Exchange the user token for a partner session device once (`POST /v1/messaging/user-sessions`), then call the routes below with `Authorization: CF-Client-Token <clientToken>`. Only these routes accept a partner session device, and only for MANAGED conversations the user is an ACTIVE member of (anything else is 404).

#### `POST /v1/messaging/user-sessions`

**exchange User Session**

ADR 0062 (build contract §4): a partner device exchanges its CF Meet user token (Platform Access verifies it; the msg grant is required: INSUFFICIENT_SCOPE / MSG_GRANT_REQUIRED otherwise) for a MANAGED-only partner session device bound to SHA-256("cfmeet-user-binding:v1|env|principal|installationId") and a client token for it (at most MESSAGING_SESSION_DEVICE_TOKEN_TTL_SECONDS, never past the user token's expiry). With sessionDeviceId the ACTIVE device of the same principal and installation is renewed (NOT_FOUND otherwise). EXTERNAL application environments only (PRINCIPAL_NOT_AUTHORIZED / PARTNER_SESSIONS_EXTERNAL_ONLY); the user must be ACTIVE. The device records the token's minting credential: revoking that credential, suspending or deleting the user, or revoke-sessions ends it and closes its connections. Then POST /v1/realtime/tickets with the client token. 503 FEATURE_DISABLED / PARTNER_SESSION_DEVICES_DISABLED while MESSAGING_PARTNER_SESSION_DEVICES_ENABLED is off; 401 USER_TOKEN_INVALID / USER_TOKEN_EXPIRED / USER_TOKEN_REVOKED.

- **Auth:** User token `cfut_…` (minted by the partner server for one user; used on the device)
- **operationId:** `messaging_exchangeUserSession`
- **Request body** (`application/json`, required): `UserSessionRequest`

- **Responses:**

  - `201` Created. → `UserSession`
  - Errors `400`, `401`, `403`, `404`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations`

**list Conversations**

Keyset-paginated (limit 1-100, cursor). orderBy=createdAt (default): newest first. orderBy=lastActivity: the inbox order, latest application message first (Section 13, MSG-007); a cursor is valid only with the orderBy that produced it. With CF-Acting-Principal: that principal's conversations with ACTIVE membership (messages.read), each carrying its inbox summary (last activity, last visible message preview metadata, unread and mention counts); without: the environment's conversations (conversations.manage), without summaries.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `listConversations`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal's own conversations (scope messages.read; for PATCH: the owner). Without it: the application's administration view (scope conversations.manage). |
  | `lifecycleState` | query | `LifecycleState` | no |  |
  | `contextClass` | query | `ContextClass` | no |  |
  | `limit` | query | `number` | no |  |
  | `cursor` | query | `string` | no |  |
  | `orderBy` | query | `"createdAt" \| "lastActivity"` | no |  |

- **Responses:**

  - `200` OK. → `ConversationPage`
  - Errors `400`, `401`, `403`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}`

**get Conversation**

One conversation of the credential's environment; for a member principal (messages.read) or the administration (conversations.manage). Others: NOT_FOUND.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `getConversation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal's own conversations (scope messages.read; for PATCH: the owner). Without it: the application's administration view (scope conversations.manage). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `Conversation`
  - Errors `400`, `401`, `403`, `404`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/members`

**list Members**

ACTIVE members. With an acting member (messages.read) or the administration (conversations.manage); a non-member is NOT_FOUND. Keyset-paginated in join order (limit 1-500, default 100; cursor = the previous page's nextCursor).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `listMembers`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | no | Optional. With it: the principal acts (member checks apply). Without it: the application's administration (scope conversations.manage). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `limit` | query | `number` | no |  |
  | `cursor` | query | `string` | no |  |

- **Responses:**

  - `200` OK. → `MemberPage`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/messages`

**list Messages**

Scope messages.read, acting member. Canonical order by sequence, within the member's history window; tombstones carry no content.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `listMessages`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `afterSequence` | query | `number` | no |  |
  | `limit` | query | `number` | no |  |
  | `beforeSequence` | query | `number` | no | Newest first, strictly before this sequence (history pagination); exclusive with afterSequence. |
  | `threadRootId` | query | `MessagingUuid` | no | Only this thread: the root message and its replies. |

- **Responses:**

  - `200` OK. → `MessagePage`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/messages`

**send Message**

Scope messages.send, acting member. Commits the message with the next canonical sequence and its durable event in one transaction (no realtime publish before commit). PRIVATE_E2EE: the PUBLIC MLS framing must name this group, an application message and the group's current epoch (EPOCH_MISMATCH otherwise; MLS_REKEY_REQUIRED while removals are pending). Retries with the same clientMessageId and bytes replay (200). PRIVATE_E2EE application content must use MLS PrivateMessage wire format; public application frames are rejected with 422 CONTENT_REJECTED / APPLICATION_MUST_BE_PRIVATE. Public handshake commits remain supported. The server checks public framing and authenticated device authorization, not AEAD authenticity; receiving clients verify ciphertext before accepting content. S13: SEND_RESTRICTED / ANNOUNCEMENT_ONLY for a non-admin member of an announcement-only conversation. S2: PRIVATE_E2EE sends may carry frankingCommitment (verifiable reports).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `sendMessage`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `SendMessageRequest`

- **Responses:**

  - `201` Created. → `Message`
  - `200` Idempotent replay of an earlier identical request. → `Message`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/messages/{messageId}`

**resolve Message**

Permalink resolution (scope messages.read): current membership is checked on every resolution; a permalink is never authority.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `resolveMessage`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `PermalinkResolution`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `PATCH /v1/conversations/{conversationId}/messages/{messageId}`

**edit Managed Message**

MANAGED only (PRIVATE_E2EE edits are new ciphertexts with kind EDIT). Scope messages.send; the sender, within the policy's edit window. The superseded version is kept as a sealed revision (never overwritten in place); expectedEditCount turns a concurrent edit into 409 VERSION_CONFLICT.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `editManagedMessage`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `EditMessageRequest`

- **Responses:**

  - `200` OK. → `Message`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `DELETE /v1/conversations/{conversationId}/messages/{messageId}`

**delete Message**

Scope messages.send. scope=EVERYONE (default, sender only): a server tombstone for every member, purged after the retention grace unless a legal hold protects it. scope=ME: hidden on the acting principal's own devices.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `deleteMessage`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |
  | `scope` | query | `"EVERYONE" \| "ME"` | no |  |

- **Responses:**

  - `200` OK. → `MessageDeletion`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/messages/{messageId}/reactions`

**list Reactions**

Scope messages.read (members, within their history window). Counts per reaction; no list of who reacted.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `listReactions`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `ReactionSummary`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `PUT /v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}`

**add Reaction**

Scope messages.send (members). MANAGED only: in PRIVATE_E2EE conversations reactions are encrypted application messages (E2EE_REACTIONS_ARE_MESSAGES). Idempotent per principal; emits messaging.reaction.added.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `addReaction`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |
  | `reaction` | path | `string` | yes |  |

- **Responses:**

  - `200` OK. → `ReactionChange`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `DELETE /v1/conversations/{conversationId}/messages/{messageId}/reactions/{reaction}`

**remove Reaction**

Scope messages.send (members). Idempotent; emits messaging.reaction.removed.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `removeReaction`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `messageId` | path | `MessagingUuid` | yes |  |
  | `reaction` | path | `string` | yes |  |

- **Responses:**

  - `200` OK. → `ReactionChange`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `PUT /v1/conversations/{conversationId}/read-cursor`

**set Read Cursor**

Scope messages.read. A monotonic read position (distinct from transport delivery and application acknowledgements); shared as read.cursor.updated only when the privacy policy enables read receipts.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `setReadCursor`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `ReadCursorRequest`

- **Responses:**

  - `200` OK. → `ReadCursor`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/unread`

**get Unread Count**

Scope messages.read (members). Application messages of others after the read cursor, inside the history window, not hidden.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `getUnreadCount`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `UnreadCount`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/events`

**replay Conversation Events**

Scope messages.read, acting member. Durable events after a per-conversation cursor (exclusive), oldest first, within the member's history window; the REST synchronization path of realtime clients.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `replayConversationEvents`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `after` | query | `number` | no | The canonical replay cursor: events with eventSequence > after. (The WebSocket subscribe frame names the same cursor afterEventSequence; omit it there to resume from the device's stored ack cursor.) |
  | `limit` | query | `number` | no |  |

- **Responses:**

  - `200` OK. → `EventPage`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/attachments`

**create Attachment Intent**

Scope attachments.write, acting member. Policy-checked (attachments enabled for the environment and context, kind, size, quotas reserved atomically). Returns a signed upload bound to size / digest / type. PRIVATE_E2EE: the client encrypts BEFORE upload; file name, media type and durations are refused (they travel inside MLS). 200: the same pending upload reused.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `createAttachmentIntent`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `AttachmentIntentRequest`

- **Responses:**

  - `201` Created. → `AttachmentUploadIntent`
  - `200` Idempotent replay of an earlier identical request. → `AttachmentUploadIntent`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/attachments/{attachmentId}`

**get Attachment**

Scope attachments.read. The uploader, or a member through a message within their history window.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `getAttachment`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `attachmentId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `Attachment`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/attachments/{attachmentId}/finalize`

**finalize Attachment**

Scope attachments.write, the uploader. Re-verifies the stored object's size and SHA-256; MANAGED: content-type security and the self-hosted scan (fail closed: VERIFYING while the scanner is unavailable, QUARANTINED when infected). Idempotent.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `finalizeAttachment`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `attachmentId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `Attachment`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/attachments/{attachmentId}/download-grants`

**grant Attachment Download**

Scope attachments.read. Authorization re-checked now (membership, history window, message ACTIVE, attachment READY); a removed member gets no fresh grant.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `grantAttachmentDownload`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `attachmentId` | path | `MessagingUuid` | yes |  |

- **Responses:**

  - `200` OK. → `AttachmentDownloadGrant`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `GET /v1/conversations/{conversationId}/search`

**search Conversation**

Scope messages.read, acting member. MANAGED only (PRIVATE_E2EE: LOCAL_SEARCH_ONLY — search on the device). Membership, history window, ACTIVE messages and delete-for-me honoured; rate-limited; query never logged.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `searchConversation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |
  | `q` | query | `string` | no |  |
  | `limit` | query | `number` | no |  |
  | `beforeSequence` | query | `number` | no |  |

- **Responses:**

  - `200` OK. → `SearchResults`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/conversations/{conversationId}/reports`

**create Report**

Scope messages.send, acting member. Policy: moderation and reporting enabled for the environment and context; rate-limited.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device) **or** OAuth access token `cfoa_…` / bot token `cfob_…` (a CF Meet App)
- **operationId:** `createReport`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `CF-Acting-Principal` | header | `MessagingUuid` | yes | The verified acting principal (Platform Access resolves it ACTIVE in the credential's environment). |
  | `conversationId` | path | `MessagingUuid` | yes |  |

- **Request body** (`application/json`, required): `ReportRequest`

- **Responses:**

  - `201` Created. → `ReportReceipt`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `MessagingError`

#### `POST /v1/realtime/tickets`

**issue Realtime Ticket**

Device-direct. Verifies the client token (signature, algorithm, key id, issuer, audience, expiry) and re-validates its session, device and principal (suspension included) before issuing a single-use ticket valid for seconds.

- **Auth:** Client token (`Authorization: CF-Client-Token <clientToken>` from the user-session exchange; device)
- **operationId:** `issueRealtimeTicket`
- **Responses:**

  - `201` Created. → `RealtimeTicket`
  - Errors `401`, `403`, `429`, `503` → `MessagingError`

#### `PUT /v1/messaging/user-push-registrations/{installationId}`

**register User Push**

ADR 0063 (§6 N-P2): registers (or replaces) the push handle of a partner device installation for the user token's principal; the handle is synchronised to the Notification Service with the subject {kind: platform-principal, tenantId, applicationId, environmentId, principalId, installationId} and never stored, echoed or logged by Messaging. Partner MANAGED messages then wake the installation with metadata only (§6 N-P4). msg grant required; EXTERNAL environments only. 503 FEATURE_DISABLED / PARTNER_PUSH_DISABLED while MESSAGING_PARTNER_PUSH_ENABLED is off; 400 VALIDATION_ERROR / PUSH_REGISTRATION_REFUSED when the Notification Service refuses the handle.

- **Auth:** User token `cfut_…` (minted by the partner server for one user; used on the device)
- **operationId:** `registerUserPush`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `installationId` | path | `string` | yes |  |

- **Request body** (`application/json`, required): `UserPushRegistrationRequest`

- **Responses:**

  - `200` OK. → `UserPushRegistration`
  - Errors `400`, `401`, `403`, `429`, `503` → `MessagingError`

#### `DELETE /v1/messaging/user-push-registrations/{installationId}`

**unregister User Push**

ADR 0063 (§6 N-P2): removes the installation's partner push registration for one platform (every platform without `platform`) at the Notification Service. Idempotent. msg grant required. 503 FEATURE_DISABLED / PARTNER_PUSH_DISABLED while MESSAGING_PARTNER_PUSH_ENABLED is off.

- **Auth:** User token `cfut_…` (minted by the partner server for one user; used on the device)
- **operationId:** `unregisterUserPush`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `installationId` | path | `string` | yes |  |
  | `platform` | query | `"APNS" \| "APNS_VOIP" \| "FCM"` | no |  |

- **Responses:**

  - `204` Removed.
  - Errors `400`, `401`, `403`, `503` → `MessagingError`

### 3.4 Server — calls

#### `POST /v1/calls`

**Create and ring an application call**

Requires calls.write and an Idempotency-Key. Creates the Call (REQUESTED), records the APP_DIRECT_V1 authorization, resolves the target principals inside the credential's scope, selects the APPLICATION route, prepares the Call's media resource through the configured media provider, authorizes the initiator's session and rings every eligible endpoint of every target (one offer each, server-owned expiry). Answers 201 with the call (normally RINGING). A replay with the same key returns the recorded response; the call is never created twice. 1.25.0 (direct forwarding): a one-to-one application call (one targetPrincipalIds entry, no privacyRelaySessionId) to a callee with an ACTIVE forwarding rule (/v1/enterprise/forwarding-rules) or an ACTIVE mailbox of its own is routed like an extension call from the callee: ALWAYS forwards before ringing; with no eligible endpoint the UNAVAILABLE rule (else the mailbox) takes it instead of FAILED; BUSY (every offer declined) / NO_ANSWER (offers expired) follow the matching rule, else the callee's mailbox, on the same Call. Loops (including back to the caller) are blocked, the scope's effective enterprise policy applies (forwardingAllowed, voicemailAllowed), ENTERPRISE_CALLING_ENABLED off disables it, and in a scope governed by the Communications personal policy only the voicemail fallback applies. The response of such a call carries no enterprise block; without a rule or mailbox nothing changes. 1.31.0 (CF1, "on another call"): a target that …

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `createCall`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `idempotency-key` | header | `string` | yes | Required. Client-chosen key (1-255 printable ASCII characters, no spaces), sent once. The same key with the same request returns the recorded response (no second call or transfer); with a different request it is 409 IDEMPOTENCY_CONFLICT. |

- **Request body** (`application/json`, required): `CreateCallRequest`

- **Responses:**

  - `201` The call was created (or the recorded response of an identical earlier request). → `CreateCallResult`
  - Errors `400`, `401`, `403`, `404`, `409`, `422`, `429`, `503` → `CallingErrorEnvelope`

#### `GET /v1/calls`

**List the caller's calls, or one principal's call history (newest first, keyset pagination)**

Requires the calls.read scope. Ordered by createdAt descending, then callId descending. Only the bounded filters below are accepted; unknown or repeated query parameters are rejected with 400. The cursor is opaque, carries position only and never scope. Part F §62: with principalId the response is that principal's canonical call history (CallHistoryList): only Calls it participates in, each with its participant-relative direction and outcome derived from Call evidence (never from push or realtime delivery), safe enterprise labels, the voicemail link for mailbox owners and recording availability; direction / outcome / missed need principalId. enterpriseDestinationType + enterpriseDestinationId narrow either listing.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `listCalls`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `limit` | query | `number` | no | Page size (default 25, at most 100). |
  | `cursor` | query | `string` | no | Opaque nextCursor of the previous page. |
  | `state` | query | `CallState` | no |  |
  | `createdAfter` | query | `string` | no | Inclusive lower bound of createdAt (RFC 3339). |
  | `createdBefore` | query | `string` | no | Exclusive upper bound of createdAt (RFC 3339). |
  | `initiatorPrincipalId` | query | `CallingUuid` | no |  |
  | `principalId` | query | `CallingUuid` | no | Part F §62: this principal's history (a principal of the credential's scope; any other id yields an empty page). |
  | `direction` | query | `HistoryDirection` | no | History only (needs a principal): INBOUND (the principal was a recipient) or OUTBOUND (the principal initiated the Call). |
  | `outcome` | query | `ParticipantOutcome` | no | History only (needs a principal): the participant outcome derived from Call evidence. |
  | `missed` | query | `boolean` | no | History only (needs a principal): true lists only Calls the principal missed (outcome MISSED); false excludes them. |
  | `enterpriseDestinationType` | query | `HistoryDestinationType` | no | With enterpriseDestinationId: only Calls routed through this enterprise destination (an extension — as dialed or reached —, a ring group, a queue, an IVR flow or a voicemail mailbox of the same scope). Never a public contact. |
  | `enterpriseDestinationId` | query | `CallingUuid` | no | With enterpriseDestinationType: the destination's id. |

- **Responses:**

  - `200` One page of calls (CallList), or with principalId one page of history (CallHistoryList).

    ```ts
    CallList | CallHistoryList
    ```
  - Errors `400`, `401`, `403`, `429`, `503` → `CallingErrorEnvelope`

#### `GET /v1/calls/{call_id}`

**Get one call with its participants, sessions and durations**

Requires the calls.read scope. A call that does not exist, belongs to another tenant, application or environment, or whose id is malformed is answered with the same 404.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `getCall`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `call_id` | path | `string` | yes | CF Meet call id. |

- **Responses:**

  - `200` The call. → `Call`
  - Errors `401`, `403`, `404`, `429`, `503` → `CallingErrorEnvelope`

#### `GET /v1/calls/{call_id}/actions`

**Actions currently available on a call**

Requires calls.read. Server-derived from the Call's state, its live offers, the credential's scopes and the media provider's capabilities.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `getCallActions`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `call_id` | path | `string` | yes | CF Meet call id. |

- **Responses:**

  - `200` The available actions. → `AvailableActions`
  - Errors `401`, `403`, `404`, `429`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calls/{call_id}/cancel`

**Cancel a call before it is active**

Requires calls.write. Pre-active termination (REQUESTED ... ANSWERED): cancels live offers and outstanding attempts, closes route, leg and sessions and tears down the media resource. 1.31.0: the answer comes as soon as the cancellation is committed; the media teardown runs after it (a failed teardown is retried by reconciliation). Repeating it on a CANCELLED call is a no-op.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `cancelCall`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `call_id` | path | `string` | yes | CF Meet call id. |
  | `idempotency-key` | header | `string` | no | Client-chosen key (1-255 printable ASCII characters, no spaces), sent once. The same key with the same request returns the recorded response; with a different request it is 409 IDEMPOTENCY_CONFLICT; concurrent duplicates take effect once. |

- **Request body** (`application/json`, required): `CommandRequest`

- **Responses:**

  - `200` The command's result (or the recorded response of an identical earlier command). → `ControlResult`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calls/{call_id}/end`

**End an active call**

Requires calls.write. Moves the Call to ENDING, removes every media connection and terminates the media resource; the Call becomes ENDED only on cleanup evidence (otherwise it stays ENDING and the reconciler retries). 1.31.0: the answer comes as soon as ENDING is committed (normally state ENDING); the cleanup runs after it and moves the Call to ENDED (call.ended is signalled then). Usage and quality are finalized. 1.21.0 (host): actingParticipantId names the participant acting for everyone; in a group Call (more than two participants, ever) only the host may do this (another participant: 403 CALL_NOT_AUTHORIZED - it leaves by removing its session). Without actingParticipantId the application backend acts on its own authority, as before.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only)
- **operationId:** `endCall`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `call_id` | path | `string` | yes | CF Meet call id. |
  | `idempotency-key` | header | `string` | no | Client-chosen key (1-255 printable ASCII characters, no spaces), sent once. The same key with the same request returns the recorded response; with a different request it is 409 IDEMPOTENCY_CONFLICT; concurrent duplicates take effect once. |

- **Request body** (`application/json`, required): `CommandRequest`

- **Responses:**

  - `200` The command's result (or the recorded response of an identical earlier command). → `ControlResult`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `CallingErrorEnvelope`

### 3.5 Device — calls

#### `POST /v1/calling/user-sessions`

**Exchange a CF Meet user token for an endpoint and a signaling credential**

Device route (CF Meet Platform, ADR 0062), served only while CALLING_USER_TOKEN_EXCHANGE_ENABLED=true (otherwise 404). Authenticated by a CF Meet user token (userToken), verified live with Platform Access; every refusal (invalid, expired or revoked token, inactive user, minting credential no longer valid, suspended scope, a tenant that is not EXTERNAL) is the uniform 401. The token must hold the effective grant call:receive and its minting credential calls.write (403 otherwise); the request counts toward the minting credential's rate limit. Creates (201) or reuses (200) the ACTIVE endpoint keyed inst:<installationId> for the token's user (audio and video); when that installation's endpoint belongs to another user of the same environment or has another kind, it is revoked (its live offers end) and registered anew. The signaling credential records the token's minting credential and service account, so revoking that credential, suspending the user or the scope ends it; it never outlives the user token. Calls stay server-placed (POST /v1/calls by the partner's backend).

- **Auth:** User token `cfut_…` (minted by the partner server for one user; used on the device)
- **operationId:** `exchangeUserSession`
- **Request body** (`application/json`, required): `CallingUserSessionRequest`

- **Responses:**

  - `200` The installation's existing endpoint and a new signaling credential. → `CallingUserSession`
  - `201` The installation's endpoint was registered; a new signaling credential. → `CallingUserSession`
  - Errors `400`, `401`, `403`, `404`, `409`, `429`, `503` → `CallingErrorEnvelope`

#### `GET /v1/calling/invitations`

**Recover the endpoint's offers**

Signaling credential. The endpoint's live and recently finished offers with the authoritative Call state; only unexpired RINGING offers of answerable calls are actionable. Stale client state is never trusted.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `recoverInvitations`
- **Responses:**

  - `200` The endpoint's offers. → `RecoveredInvitationList`
  - Errors `401`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calling/invitations/{invitation_id}/acknowledge`

**Acknowledge receipt of one's own ring offer**

Signaling credential: only an offer addressed to the credential's own endpoint (any other offer is 404). The device confirms it received the offer and is alerting its user: the offer moves PENDING → RINGING, and the first acknowledged offer of a DIALING application Call moves the Call to RINGING, so a caller never sees "Ringing" for a callee whose devices are signed out or offline (their offers stay PENDING and expire). Idempotent; no body. Accepting or declining a PENDING offer acknowledges it implicitly. Also available on the signaling WebSocket as {"type":"invitation.acknowledge","invitationId":…}.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `acknowledgeInvitation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `invitation_id` | path | `string` | yes | Ring offer (invitation) id. |

- **Responses:**

  - `200` The offer is RINGING. → `AcknowledgeResult`
  - Errors `400`, `401`, `404`, `409`, `410`, `429`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calling/invitations/{invitation_id}/accept`

**Accept one's own ring offer**

Signaling credential: only an offer addressed to the credential's own endpoint (any other offer is 404). Same arbitration and semantics as POST /v1/calls/{call_id}/accept.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `acceptInvitation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `invitation_id` | path | `string` | yes | Ring offer (invitation) id. |
  | `idempotency-key` | header | `string` | no | Client-chosen key (1-255 printable ASCII characters, no spaces), sent once. The same key with the same request returns the recorded response; with a different request it is 409 IDEMPOTENCY_CONFLICT; concurrent duplicates take effect once. |

- **Request body** (`application/json`, required): `CommandRequest`

- **Responses:**

  - `200` The command's result (or the recorded response of an identical earlier command). → `AcceptResult`
  - Errors `400`, `401`, `404`, `409`, `410`, `429`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calling/invitations/{invitation_id}/decline`

**Decline one's own ring offer**

Signaling credential: only an offer addressed to the credential's own endpoint (any other offer is 404). Same arbitration and semantics as POST /v1/calls/{call_id}/decline.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `declineInvitation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `invitation_id` | path | `string` | yes | Ring offer (invitation) id. |
  | `idempotency-key` | header | `string` | no | Client-chosen key (1-255 printable ASCII characters, no spaces), sent once. The same key with the same request returns the recorded response; with a different request it is 409 IDEMPOTENCY_CONFLICT; concurrent duplicates take effect once. |

- **Request body** (`application/json`, required): `CommandRequest`

- **Responses:**

  - `200` The command's result (or the recorded response of an identical earlier command). → `DeclineResult`
  - Errors `400`, `401`, `404`, `409`, `410`, `429`, `503` → `CallingErrorEnvelope`

#### `POST /v1/calling/sessions/{session_id}/media-credentials`

**Issue a media credential for one's own session**

Signaling credential: only a session running on the credential's own endpoint (any other session is 404). Same rules as POST /v1/calls/{call_id}/sessions/{session_id}/media-credentials.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `issueOwnMediaCredential`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `session_id` | path | `string` | yes | Call session id. |

- **Request body** (`application/json`): `EmptyRequest`

- **Responses:**

  - `200` The media credential. → `MediaCredential`
  - Errors `400`, `401`, `404`, `409`, `422`, `429`, `503` → `CallingErrorEnvelope`

#### `GET /v1/calling/signaling`

**Open the endpoint's realtime signaling channel (WebSocket)**

Authenticated before the upgrade with the endpoint's signaling credential. Delivery only: the server sends signaling.ready, then the endpoint's realtime signals (cfmeet.calling.realtime-signal, https://schemas.cfmeet.com/calling/realtime/v1.schema.json), answers {"type":"ping"} with signaling.pong closes with code 4001 when the credential expires and with code 4401 when its authority is revoked (the endpoint revoked, its principal or the minting credential chain no longer valid, or communications authorization withdrawn; re-checked every 5 s and before every delivery). Signals are sent only after the authoritative transaction committed; a lost signal never loses state (recover with GET /v1/calling/invitations and the call API). Commands use HTTP.

- **Auth:** Signaling credential `cfsig_…` (from the calling user-session exchange; device)
- **operationId:** `openSignalingChannel`
- **Responses:**

  - `101` Switching Protocols: the WebSocket channel is open (subprotocol cfmeet.signaling.v1 when it was offered).
  - Errors `400`, `401`, `503` → `CallingErrorEnvelope`

### 3.6 Server — meetings (machine family through the gateway)

The same `/v1/meetings/*` routes also accept a CF Meet App's OAuth access token with `meetings:write` (delegated apps), acting as the granting user on meetings that user owns.

#### `GET /v1/meetings/conferences/{id}`

**Read a conference. Callers without authority get the SUMMARY view**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/conferences/{id}`; the CF Meet apps call that path directly. Works with or without authentication. The space owner, members and admitted participants get the FULL view (schedule, lock, recording/transcription state, version); anyone else gets the SUMMARY view with the id, meeting space id, status and — except for HEALTHCARE / HIGH_SECURITY / healthcare-appointment meetings — the scheduled start and end (pre-join countdown).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_get_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Conference → `ConferenceResponse`
  - Errors `404` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/conferences/{id}/attendance`

**Conference attendance (Part J)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/conferences/{id}/attendance`; the CF Meet apps call that path directly. A thin read over the same aggregation as the `ATTENDANCE_REPORT` export; nothing is stored and no artefact is created.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_conference_attendance`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Attendance → `AttendanceReport`
  - Errors `401`, `403`, `404` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/cancel`

**Cancel an unstarted conference (single-occurrence exception for recurring series)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/cancel`; the CF Meet apps call that path directly. Moves a CREATED, PREPARING or WAITING conference to CANCELLED and releases any media room already prepared. Requires `conference.start`. For an occurrence of a recurring series only that occurrence is cancelled. Cancelling an already cancelled conference is a no-op (`changed: false`); live or finished conferences return CONFERENCE_STATE_CONFLICT.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_cancel_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Applied (or idempotent no-op) → `ConferenceActionResponse`
  - Errors `403`, `409` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/end`

**LIVE → ENDING. Admission and new join credentials stop immediately; ENDED follows provider termination (Part E)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/end`; the CF Meet apps call that path directly. Ends a LIVE conference for everyone: active recordings are stopped and the media room is terminated with the RTC provider. Requires `conference.end`; a conference that never went live returns CONFERENCE_NOT_LIVE (cancel it instead). Repeating the call while ENDING or ENDED is a no-op (`changed: false`); while ENDING it retries provider termination. `providerEnforcement` is ENFORCED once the conference is ENDED.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_end_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Applied (or idempotent no-op) → `ConferenceActionResponse`
  - Errors `403`, `409` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/conferences/{id}/participants`

**Admitted participants (roster)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/conferences/{id}/participants`; the CF Meet apps call that path directly. Returns the participants currently ADMITTED to the conference, ordered by admission time. Available to admitted participants and to the space owner and members; other callers (including waiting, denied, removed or banned participants) get PERMISSION_DENIED. A VIEWER (a webinar or town-hall attendee) sees only the people running the session (presenters and above) and themselves, never the other attendees.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_list_participants`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Roster

    ```ts
    ParticipantResponse[]
    ```
  - Errors `403` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/participants/{participantId}/remove`

**Remove a participant (business authority; media disconnect enforced by the RTC provider)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/participants/{participantId}/remove`; the CF Meet apps call that path directly. Sets the participant to REMOVED and disconnects their active media sessions; an optional `reason` is recorded in the audit log. Requires `participant.remove`, and the target must be less privileged than the caller (never yourself). Idempotent for participants already removed or banned; `providerEnforcement` tells whether the RTC provider has confirmed the disconnect.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_remove`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |
  | `participantId` | path | `string` | yes | Participant id |

- **Request body** (`application/json`): `RemoveParticipantRequest`

- **Responses:**

  - `200` Removed → `ModerationResponse`
  - Errors `404` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/start`

**Start a conference. Concurrent starts yield exactly one LIVE authority**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/start`; the CF Meet apps call that path directly. Moves a CREATED, PREPARING or WAITING conference to LIVE (a CREATED one is prepared first). Requires `conference.start`, an active meeting space, and the current time within the start window (from the space's early-start allowance before the scheduled start until its grace period after the scheduled end). Starting a LIVE conference is a no-op (`changed: false`), and only one conference per meeting space can be LIVE at a time.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_start_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Applied (or idempotent no-op) → `ConferenceActionResponse`
  - Errors `403`, `409` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/conferences/{id}/waiting-room`

**Waiting queue (requires `participant.admit`)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/conferences/{id}/waiting-room`; the CF Meet apps call that path directly. Returns the participants currently WAITING for admission, oldest request first. The caller's conference role (from an admitted participant or a space membership) must hold `participant.admit`. Returns FEATURE_DISABLED when the waiting room is not enabled.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_get_waiting_room`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Responses:**

  - `200` Waiting participants → `WaitingRoomResponse`
  - Errors `403` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/waiting-room/admit`

**Admit one, selected or all waiting participants (transactional, idempotent)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/waiting-room/admit`; the CF Meet apps call that path directly. Moves WAITING participants to ADMITTED: pass `participantIds` (up to 500) or `all: true` for everyone currently waiting. Requires `participant.admit` and a LIVE conference, and respects the meeting's capacity. Any id outside this conference fails the whole request; already admitted ids are reported as `unchanged`, and banned or otherwise ineligible ones as `skipped`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_admit`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Request body** (`application/json`, required): `AdmitRequest`

- **Responses:**

  - `200` Result → `BulkAdmissionResponse`
  - Errors `403`, `404`, `409` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/conferences/{id}/waiting-room/deny`

**Deny one, selected or all waiting participants**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/conferences/{id}/waiting-room/deny`; the CF Meet apps call that path directly. Moves WAITING participants to DENIED: pass `participantIds` (up to 500) or `all: true`. Optional `reasonCode` (max 64 chars) and `message` (max 500 chars, shown to the denied participant), plus an `internalNote` that is never shown to them. Requires `participant.deny`; any id outside this conference fails the whole request, already denied ids are `unchanged` and others `skipped`.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_deny`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Request body** (`application/json`, required): `DenyRequest`

- **Responses:**

  - `200` Result → `BulkAdmissionResponse`
  - Errors `404` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/instant-meetings`

**Instant meeting: creates a meeting space and a LIVE conference atomically**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/instant-meetings`; the CF Meet apps call that path directly. Creates a meeting space owned by the caller and a conference that is LIVE immediately, in one step, and returns both. The body is optional: the title defaults to "Instant meeting" and the access policy to PUBLIC; timezone, waiting-room policy and passcode may be set. Requires a signed-in user with v2 meetings enabled for the account. The conference's media room is prepared right away on a best-effort basis.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_create_instant_meeting`
- **Request body** (`application/json`): `InstantMeetingRequest`

- **Responses:**

  - `201` Created → `InstantMeetingResponse`
  - Errors `401`, `429` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/meeting-spaces`

**List meeting spaces the caller owns or is a member of**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/meeting-spaces`; the CF Meet apps call that path directly. Returns every non-deleted meeting space the signed-in caller owns or is an active member of, newest first; archived spaces are included. Each item carries the caller's role and its canonical conference (the LIVE one, else the next unstarted one whose start window is still open, else the latest). Owners, hosts and co-hosts get the full view, other members the summary view. Without `limit` and `cursor` the complete list is returned (legacy behaviour); with either, one page of `limit` (1-200, default 50) items and `nextCursor` while more exist.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_list_meeting_spaces`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `cursor` | query | `string \| null` | no | Opaque cursor from nextCursor |
  | `limit` | query | `number \| null` | no | Page size, 1-200 (default 50 when paginating) |

- **Responses:**

  - `200` Spaces → `MeetingSpaceListResponse`
  - Errors `400`, `401` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/meeting-spaces`

**Create a meeting space. The owner is always the authenticated caller**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/meeting-spaces`; the CF Meet apps call that path directly. Creates an ACTIVE meeting space with a newly generated meeting code; any owner sent by the client is ignored. Requires a signed-in user with v2 meetings enabled for the account (FEATURE_DISABLED otherwise). An `organizationId` is accepted only if the caller is a verified member of that organization, and an optional passcode is validated and stored only as a hash. No conference is created; schedule one separately or use `POST /api/v2/instant-meetings`. Send an `Idempotency-Key` header (1-128 visible ASCII characters, kept 24 hours per caller) to make retries safe: a retry with the same key and body returns the same space (header `Idempotent-Replayed: true`); the same key with a different body is IDEMPOTENCY_CONFLICT.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_create_meeting_space`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `Idempotency-Key` | header | `string \| null` | no | Optional retry key (1-128 visible ASCII characters) |

- **Request body** (`application/json`, required): `CreateMeetingSpaceRequest`

- **Responses:**

  - `201` Created (or replayed for the same Idempotency-Key) → `MeetingSpaceResponse`
  - Errors `400`, `401`, `403`, `409`, `429` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/meeting-spaces/{id}`

**Read a meeting space by v2 id, v2 meeting code, legacy meeting id or legacy code**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/meeting-spaces/{id}`; the CF Meet apps call that path directly. Callers without management rights receive the SUMMARY view; resolution never implies authorization. An ORGANIZATION_ONLY space is disclosed only to its owner / members, verified members of its organization and callers with calendar access (invitation, registration, participation): a signed-out caller gets AUTHENTICATION_REQUIRED, anyone else MEETING_NOT_FOUND.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_get_meeting_space`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Space UUID, meeting code, legacy meeting UUID or legacy code |

- **Responses:**

  - `200` Space → `MeetingSpaceResponse`
  - Errors `401`, `404` → `MeetingsErrorEnvelope`

#### `PATCH /v1/meetings/meeting-spaces/{id}`

**Update a meeting space (optimistic concurrency via `version`)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `PATCH /api/v2/meeting-spaces/{id}`; the CF Meet apps call that path directly. Partially updates the space's settings and policies; omitted fields are left unchanged. Requires the owner or a host, v2 meetings enabled for the account, and the space's current `version` (a stale version returns MEETING_VERSION_CONFLICT). Archived spaces cannot be modified. A passcode can be set or cleared with `clearPasscode`; changing the timezone re-anchors an active recurrence to the new timezone at the same wall-clock time. Ownership cannot be changed here; use the owner-transfer endpoint. Organization OWNER / ADMIN holding `meetings.organization.manage` (Organizations authoritative mode) may also do this for their organization's meeting spaces (ORG-011).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_update_meeting_space`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |

- **Request body** (`application/json`, required): `UpdateMeetingSpaceRequest`

- **Responses:**

  - `200` Updated → `MeetingSpaceResponse`
  - Errors `403`, `409` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/meeting-spaces/{id}/archive`

**Soft-archive a meeting space (owner only). History is preserved**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/meeting-spaces/{id}/archive`; the CF Meet apps call that path directly. Archives the space: it can no longer be modified, its recurring series is ended and unstarted future occurrences are cancelled, while past conferences, participants and audit history are kept. Only the owner (or a platform administrator) may archive. The optional body `version` is checked against the current version (MEETING_VERSION_CONFLICT on mismatch). Archiving an already-archived space returns it unchanged. Organization OWNER / ADMIN holding `meetings.organization.manage` (Organizations authoritative mode) may also do this for their organization's meeting spaces (ORG-011).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_archive_meeting_space`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |

- **Request body** (`application/json`): `VersionedRequest`

- **Responses:**

  - `200` Archived → `MeetingSpaceResponse`
  - Errors `403` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/meeting-spaces/{id}/conferences`

**List occurrences of a meeting space (members only)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/meeting-spaces/{id}/conferences`; the CF Meet apps call that path directly. Returns the conferences of the meeting space (full view), ordered by scheduled start. Optional `from` (inclusive) and `to` (exclusive) RFC 3339 timestamps filter on the scheduled start time. Requires a bearer user token of the space owner or an active member.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_list_conferences`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |
  | `from` | query | `string \| null` | no | RFC 3339 |
  | `to` | query | `string \| null` | no | RFC 3339 |

- **Responses:**

  - `200` Occurrences → `ConferenceListResponse`
  - Errors `403` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/meeting-spaces/{id}/conferences`

**Create a one-time scheduled conference occurrence**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/meeting-spaces/{id}/conferences`; the CF Meet apps call that path directly. Adds a single (non-recurring) conference to an active meeting space in state CREATED, with optional `scheduledStartAt` / `scheduledEndAt` (the end requires a start, must be after it and at most 24 hours later; a start more than 5 minutes in the past is rejected). Requires a bearer user token whose space role may schedule conferences. Send an `Idempotency-Key` header (1-128 visible ASCII characters, kept 24 hours per caller and space) to make retries safe: the same key and body return the same conference (header `Idempotent-Replayed: true`); the same key with a different body is IDEMPOTENCY_CONFLICT.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_create_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |
  | `Idempotency-Key` | header | `string \| null` | no | Optional retry key (1-128 visible ASCII characters) |

- **Request body** (`application/json`, required): `CreateConferenceRequest`

- **Responses:**

  - `201` Created (or replayed for the same Idempotency-Key) → `ConferenceResponse`
  - Errors `400`, `403`, `409`, `429` → `MeetingsErrorEnvelope`

#### `GET /v1/meetings/meeting-spaces/{id}/invitations`

**List invitations of a space (no tokens or hashes)**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `GET /api/v2/meeting-spaces/{id}/invitations`; the CF Meet apps call that path directly. Returns all invitations of the meeting space, newest first, including revoked and expired ones, with their status and usage count. Requires a bearer user token with invite authority in the space (a role allowed to admit participants).

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_list_invitations`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |

- **Responses:**

  - `200` Invitations

    ```ts
    InvitationResponse[]
    ```
  - Errors `403` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/meeting-spaces/{id}/invitations`

**Create an invitation. The raw token is returned once and never stored**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/meeting-spaces/{id}/invitations`; the CF Meet apps call that path directly. Issues a bearer invitation for an active meeting space, optionally limited to one conference (`conferenceId`), bound to a verified account email (`inviteeEmail`) or capped by `useLimit` (1–10000). It expires after `expiresInHours` (default 168, capped by server policy) and grants `role` (default PARTICIPANT), which must be below the inviter's own role. Requires a bearer user token with invite authority in the space (a role allowed to admit participants); store the token from the response, it cannot be retrieved again. With `sendEmail: true` (requires `inviteeEmail`) the link is also emailed to the invitee when notification email is enabled on the deployment; `emailDelivery` reports the outcome and at most a configured number of invitation emails per hour can be requested (`429 RATE_LIMITED`). Machine callers cannot request emails.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_create_invitation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |

- **Request body** (`application/json`): `CreateInvitationRequest`

- **Responses:**

  - `201` Created → `CreateInvitationResponse`
  - Errors `400`, `403`, `429` → `MeetingsErrorEnvelope`

#### `POST /v1/meetings/meeting-spaces/{id}/invitations/{invitationId}/revoke`

**Revoke an invitation**

Partner server route (platform credential or CF Meet Apps OAuth access token) of `POST /api/v2/meeting-spaces/{id}/invitations/{invitationId}/revoke`; the CF Meet apps call that path directly. Marks the invitation REVOKED so it can no longer be resolved or used to join. Requires a bearer user token with invite authority in the space (a role allowed to admit participants). Revoking an already revoked invitation is a no-op that returns it unchanged.

- **Auth:** Platform credential `cfmk_…` (partner SERVER only) **or** OAuth access token `cfoa_…` (a CF Meet App acting for a user)
- **operationId:** `platform_v2_revoke_invitation`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Meeting space id |
  | `invitationId` | path | `string` | yes | Invitation id |

- **Responses:**

  - `200` Revoked → `InvitationResponse`
  - Errors `403` → `MeetingsErrorEnvelope`

### 3.7 Device — join a partner meeting

The conference must be started first (`POST /v1/meetings/conferences/{id}/start`); joins are admitted only while it is LIVE (`409 CONFERENCE_NOT_LIVE` otherwise). The user token must carry an `mtg` grant for the conference. Partner meeting chat is NOT read through `/api/v2/conferences/{id}/messages`: the joined user finds the MANAGED meeting conversation (`context.namespace = "meeting"`, `context.reference = "conference:<id>"`) with their partner session device (3.3).

#### `POST /api/v2/conferences/{id}/join`

**Join (or reconnect to) a conference**

Identity, role and admission are resolved server-side. A WAITING participant never receives an RTC credential. Partner users (CF Meet Platform, `userToken`) present their CF Meet user token as `Authorization: Bearer cfut_…` (accepted only here, while the deployment enables it). They join only conferences of meetings owned by the token's application environment (any other conference is CONFERENCE_NOT_FOUND) whose id the token's effective meeting grants name (MEETING_ACCESS_DENIED otherwise). The role comes from the grant, limited to what the meeting's owner may grant; the display name comes from the user profile (a `displayName` in the body is ignored); invitation tokens and passcodes do not apply. A re-join keeps the same participant and returns a fresh `participantToken`, which the device uses for every other participant operation. A refused, expired or revoked user token is AUTHENTICATION_REQUIRED.

- **Auth:** None (public) **or** CF Meet session — the signed-in user's access token (`Authorization: Bearer …`) **or** Participant token (`X-Participant-Token` header, returned by join) **or** User token `cfut_…` (minted by the partner server for one user; used on the device)
- **operationId:** `v2_join_conference`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `id` | path | `string` | yes | Conference id |

- **Request body** (`application/json`): `JoinConferenceRequest`

- **Responses:**

  - `200` Joined or waiting → `JoinConferenceResponse`
  - Errors `401`, `403`, `404`, `409`, `410`, `503` → `MeetingsErrorEnvelope`

### 3.8 CF Meet Apps — OAuth endpoints (app servers)

#### `GET /v1/oauth/metadata`

**Read the OAuth authorization server metadata**

RFC 8414-style metadata: the authorization endpoint (the CF Meet consent page), token, revocation and userinfo endpoints, scopes, PKCE S256, the code response type and both grant types. Anonymous. Flag DEVELOPER_PLATFORM_OAUTH_ENABLED.

- **Auth:** None (public)
- **operationId:** `oauthMetadata`
- **Responses:**

  - `200` OK. → `OAuthMetadata`
  - Errors `404` → `Error`
  - Errors `429` → `OAuthError`

#### `POST /v1/oauth/revoke`

**Revoke an access or refresh token (RFC 7009)**

RFC 7009 revocation: 200 whatever the token's state, 401 invalid_client when client authentication fails. Anonymous at the API Gateway; same client authentication as the token endpoint. Flag DEVELOPER_PLATFORM_OAUTH_ENABLED.

- **Auth:** None (public)
- **operationId:** `oauthRevoke`
- **Request body** (`application/x-www-form-urlencoded`, required): `OAuthRevokeRequest`

- **Responses:**

  - `200` Revoked or unknown (empty body).
  - Errors `400`, `401`, `429`, `503` → `OAuthError`

#### `POST /v1/oauth/token`

**Exchange an authorization code (PKCE S256) or a refresh token for tokens**

RFC 6749 token endpoint. Anonymous at the API Gateway: the client authenticates with its client secret (client_secret_basic or client_secret_post) and Platform Access verifies it; refresh tokens rotate and a reused code or superseded refresh token revokes the whole installation. Responses are snake_case with Cache-Control: no-store. Flag DEVELOPER_PLATFORM_OAUTH_ENABLED.

- **Auth:** None (public)
- **operationId:** `oauthToken`
- **Request body** (`application/x-www-form-urlencoded`, required): `OAuthTokenRequest`

- **Responses:**

  - `200` OK. → `OAuthToken`
  - Errors `400`, `401`, `429`, `503` → `OAuthError`

#### `GET /v1/oauth/userinfo`

**Read the signed-in user's profile with a user access token**

Bearer cfoa_ user access token with profile:read (email only with profile:email). Anonymous at the API Gateway: Platform Access introspects the token, the Meeting Service supplies the profile; sub is pairwise per app. Flag DEVELOPER_PLATFORM_OAUTH_ENABLED.

- **Auth:** None (public)
- **operationId:** `oauthUserinfo`
- **Parameters:**

  | Name | In | Type | Required | Description |
  |---|---|---|---|---|
  | `Authorization` | header | `string` | yes |  |

- **Responses:**

  - `200` OK. → `OAuthUserinfo`
  - Errors `401`, `403`, `429`, `503` → `OAuthError`

### 3.9 Index — other Calling operations (same auth rules; full schemas in the contract)

| Method | Path | Auth | Summary |
|---|---|---|---|
| `GET` | `/v1/calling/endpoints` | Platform credential `cfmk_…` (partner SERVER only) | List a principal's endpoints |
| `POST` | `/v1/calling/endpoints` | Platform credential `cfmk_…` (partner SERVER only) | Register an application endpoint of a principal |
| `DELETE` | `/v1/calling/endpoints/{endpoint_id}` | Platform credential `cfmk_…` (partner SERVER only) | Revoke an endpoint |
| `GET` | `/v1/calling/history` | Signaling credential `cfsig_…` (device) | The endpoint's principal's call history |
| `GET` | `/v1/calling/limits` | Platform credential `cfmk_…` (partner SERVER only) | Get Calling limit status (service-specific view) |
| `POST` | `/v1/calling/signaling-credentials` | Platform credential `cfmk_…` (partner SERVER only) | Mint a signaling credential for one endpoint |
| `GET` | `/v1/calling/usage` | Platform credential `cfmk_…` (partner SERVER only) | List Calling usage facts (service-specific view) |
| `POST` | `/v1/calls/{call_id}/abuse-reports` | Platform credential `cfmk_…` (partner SERVER only) | Report an unwanted or abusive call |
| `POST` | `/v1/calls/{call_id}/accept` | Platform credential `cfmk_…` (partner SERVER only) | Accept a ring offer on behalf of one endpoint |
| `POST` | `/v1/calls/{call_id}/acknowledge` | Platform credential `cfmk_…` (partner SERVER only) | Acknowledge receipt of a ring offer on behalf of one endpoint |
| `POST` | `/v1/calls/{call_id}/consultations` | Platform credential `cfmk_…` (partner SERVER only) | Consult a target before transferring (attended transfer) |
| `POST` | `/v1/calls/{call_id}/consultations/{consultation_id}/cancel` | Platform credential `cfmk_…` (partner SERVER only) | Cancel an attended transfer |
| `POST` | `/v1/calls/{call_id}/consultations/{consultation_id}/complete` | Platform credential `cfmk_…` (partner SERVER only) | Complete an attended transfer |
| `POST` | `/v1/calls/{call_id}/decline` | Platform credential `cfmk_…` (partner SERVER only) | Decline a ring offer on behalf of one endpoint |
| `DELETE` | `/v1/calls/{call_id}/history` | Platform credential `cfmk_…` (partner SERVER only) | Hide a finished call from a principal's history |
| `POST` | `/v1/calls/{call_id}/hold` | Platform credential `cfmk_…` (partner SERVER only) | Hold a connected call |
| `POST` | `/v1/calls/{call_id}/host` | Platform credential `cfmk_…` (partner SERVER only) | Hand the host role to another participant |
| `POST` | `/v1/calls/{call_id}/ivr/input` | Platform credential `cfmk_…` (partner SERVER only) | Send one key to the IVR (RTC caller) |
| `POST` | `/v1/calls/{call_id}/participants` | Platform credential `cfmk_…` (partner SERVER only) | Invite participants into an ongoing call |
| `POST` | `/v1/calls/{call_id}/participants/{participant_id}/mute` | Platform credential `cfmk_…` (partner SERVER only) | Mute or unmute a participant (host only) |
| `POST` | `/v1/calls/{call_id}/participants/{participant_id}/rejoin` | Platform credential `cfmk_…` (partner SERVER only) | Rejoin an ongoing call |
| `POST` | `/v1/calls/{call_id}/participants/{participant_id}/remove` | Platform credential `cfmk_…` (partner SERVER only) | Remove a participant (host only) |
| `GET` | `/v1/calls/{call_id}/quality` | Platform credential `cfmk_…` (partner SERVER only) | Per-participant call quality |
| `GET` | `/v1/calls/{call_id}/recording-authorizations` | Platform credential `cfmk_…` (partner SERVER only) | List a Call's recording decisions |
| `POST` | `/v1/calls/{call_id}/recording-consent` | Platform credential `cfmk_…` (partner SERVER only) | Record a participant's recording consent |
| `GET` | `/v1/calls/{call_id}/recordings` | Platform credential `cfmk_…` (partner SERVER only) | List a Call's recordings |
| `POST` | `/v1/calls/{call_id}/recordings` | Platform credential `cfmk_…` (partner SERVER only) | Start recording a Call |
| `POST` | `/v1/calls/{call_id}/recordings/{recording_id}/stop` | Platform credential `cfmk_…` (partner SERVER only) | Stop a recording |
| `POST` | `/v1/calls/{call_id}/resume` | Platform credential `cfmk_…` (partner SERVER only) | Resume a held call |
| `POST` | `/v1/calls/{call_id}/screen-share` | Platform credential `cfmk_…` (partner SERVER only) | Start presenting a screen |
| `POST` | `/v1/calls/{call_id}/screen-share/stop` | Platform credential `cfmk_…` (partner SERVER only) | Stop presenting a screen |
| `POST` | `/v1/calls/{call_id}/sessions/{session_id}/media-credentials` | Platform credential `cfmk_…` (partner SERVER only) | Issue a media credential for a session |
| `POST` | `/v1/calls/{call_id}/sessions/{session_id}/quality-samples` | Platform credential `cfmk_…` (partner SERVER only) | Report a session's call-quality sample |
| `POST` | `/v1/calls/{call_id}/sessions/{session_id}/remove` | Platform credential `cfmk_…` (partner SERVER only) | Remove one session from a call |
| `POST` | `/v1/calls/{call_id}/transfer` | Platform credential `cfmk_…` (partner SERVER only) | Transfer a participant's place to another principal |
| `POST` | `/v1/calls/{call_id}/upgrade` | Platform credential `cfmk_…` (partner SERVER only) | Upgrade a voice call to video |

## Appendix A — TypeScript types (generated from the contracts)

Every request and response type referenced above, once. Field comments are the contract descriptions.

```ts
/** The offer was accepted: this endpoint's device won its participant's answer (single winner), the participant's other offers became ANSWERED_ELSEWHERE, and sessionId is the winning session that may now request media credentials. callAnswered is true when this acceptance moved the Call to ANSWERED. */
export interface AcceptResult {
  call: CallSummary;
  invitationId: CallingUuid;
  participantId: CallingUuid;
  sessionId: CallingUuid;
  outcome: "ACCEPTED";
  callAnswered: boolean;
}

/** Access-policy configuration (no secrets). */
export interface AccessConfig {
  /** CUSTOM: registered server-side access adapter. */
  adapterRef?: string | null;
  /** DOMAIN_RESTRICTED / EXTERNAL_ONLY: verified email domains treated as internal. */
  allowedDomains?: string[];
  /** HEALTHCARE_APPOINTMENT: optional appointment reference forwarded to the appointment authority (which alone decides). */
  appointmentRef?: string | null;
  /** Fail closed when an authenticated session's revocation cannot be verified (always on for HEALTHCARE and HIGH_SECURITY security profiles). */
  requireVerifiedRevocation?: boolean;
  /** CUSTOM waiting-room evaluator. */
  waitingRoomAdapterRef?: string | null;
}

export type AccessPolicy = "PUBLIC" | "PUBLIC_PASSCODE" | "AUTHENTICATED" | "INVITE_ONLY" | "ORGANIZATION_ONLY" | "DOMAIN_RESTRICTED" | "HEALTHCARE_APPOINTMENT" | "CUSTOM";

/** Result of POST /v1/calling/invitations/{invitation_id}/acknowledge: the endpoint's device confirmed it received the ring offer and is alerting its user. The offer is RINGING (ringingAt is the server time of the first acknowledgement); the first acknowledged offer of a DIALING application Call moves the Call to RINGING. Repeating the acknowledgement changes nothing. */
export interface AcknowledgeResult {
  invitationId: CallingUuid;
  callId: CallingUuid;
  status: "RINGING";
  /** (date-time) */
  ringingAt: string;
  /** (date-time) */
  expiresAt: string;
  callState: CallState;
  /** (minimum: 1) */
  callVersion: number;
}

export interface AddMemberRequest {
  principalId?: MessagingUuid;
  /** BOT_SERVICE only for BOT_OR_SERVICE principals (BOT_SERVICE_REQUIRES_BOT_PRINCIPAL); bots are never ADMIN / MODERATOR (BOT_CANNOT_ADMINISTER). */
  role?: "MEMBER" | "ADMIN" | "MODERATOR" | "READ_ONLY" | "BOT_SERVICE";
  /** §35: invite instead of adding. The principal becomes INVITED (no history, no events, no sends) until it accepts (POST .../invitation/accept); invitations expire after 7 days and count toward the member limit. */
  invite?: boolean;
  /** Personal CF Meet ID resolved privately through Communications under the authenticated actor and scope. Exactly one recipient identifier is allowed. Rotation invalidates the old public ID; existing canonical conversations remain unchanged. (pattern: "^CF-[A-HJ-NP-Z2-9]{6}$") */
  cfMeetId?: string;
  // exactly one of these field sets: [principalId] | [cfMeetId]
}

export type AdmissionState = "WAITING" | "ADMITTED" | "DENIED" | "REMOVED" | "BANNED";

export interface AdmitRequest {
  /** Admit every participant currently WAITING. */
  all?: boolean;
  participantIds?: string[];
}

/** ADR 0062: an environment's allowed websites (exact origins). */
export interface AllowedOrigins {
  environmentId: Uuid;
  /** (maxItems: 20) */
  origins: string[];
}

export interface ApnsPushCredentialUpload {
  provider: "APNS";
  /** (pattern: "^[A-Z0-9]{10}$") */
  teamId: string;
  /** (pattern: "^[A-Z0-9]{10}$") */
  keyId: string;
  /** (minLength: 3; maxLength: 155; pattern: "^[A-Za-z0-9.-]+$") */
  bundleId: string;
  voip: boolean;
  /** SANDBOX and TEST environments may only use SANDBOX. */
  apnsHost: "SANDBOX" | "PRODUCTION";
  /** The APNs token signing key (.p8, PEM). Write-only. (maxLength: 8192; pattern: "^-----BEGIN PRIVATE KEY-----") */
  p8: string;
}

/** ADR 0064 / 0065: the app registered on a project (clientId = projectId = the Platform Access application id). The live version is what Platform Access serves: the published version, else the approved one, else the current draft. */
export interface App {
  appId: Uuid;
  clientId: Uuid;
  projectId: Uuid;
  organizationId: Uuid;
  status: AppStatus;
  /** (minimum: 1) */
  currentVersion: number;
  submittedVersion: number | null;
  reviewedVersion: number | null;
  publishedVersion: number | null;
  /** (minimum: 1) */
  liveVersion: number;
  pendingChanges: boolean;
  manifest: AppManifest;
  /** (maxItems: 25) */
  testAccountIds: Uuid[];
  secretIssuedAt: string | null;
  review: {
    reasonCode: string | null;
    /** (date-time) */
    decidedAt: string;
  } | null;
  listedInDirectory: boolean;
  publishedAt: string | null;
  /** (date-time) */
  createdAt: string;
  /** (date-time) */
  updatedAt: string;
}

/** ADR 0064: a new client secret, returned ONCE (an idempotent replay returns clientSecretRedacted instead). */
export interface AppClientSecret {
  clientId: Uuid;
  /** (pattern: "^cfcs_(sbx|test|live)_[0-9a-f]{24}_[0-9a-f]{64}$") */
  clientSecret?: string;
  clientSecretRedacted?: true;
  secretId: string | null;
  /** (date-time) */
  issuedAt: string;
}

export interface AppContextJwk {
  alg: string;
  crv: string;
  kid: string;
  kty: string;
  use: string;
  x: string;
}

/** JWKS of the app-context signing keys. */
export interface AppContextKeySet {
  keys: AppContextJwk[];
}

export interface AppContextTokenResponse {
  /** Ed25519 JWS (verify with `GET /api/v2/app-context-keys`). */
  contextToken: string;
  /** Seconds (300). (int64) */
  expiresIn: number;
}

/** ADR 0064: an installation of the app (never a CF Meet account id). */
export interface AppGrant {
  id: Uuid;
  target: "USER" | "ORGANIZATION" | null;
  organizationId: Uuid | null;
  scopes: string[];
  status: string | null;
  createdAt: string | null;
  revokedAt: string | null;
}

export interface AppGrantList {
  items: AppGrant[];
  nextCursor: string | null;
}

/** ADR 0064: an app manifest (normalized; every change is a new immutable version). */
export interface AppManifest {
  /** (minLength: 1; maxLength: 120) */
  name: string;
  description: string | null;
  logoUrl: string | null;
  privacyUrl: string | null;
  termsUrl: string | null;
  /** (maxItems: 10) */
  redirectUris: string[];
  userScopes: ("profile:read" | "profile:email" | "meetings:read" | "meetings:write" | "chat:read" | "chat:write" | "events:meetings")[];
  botScopes: ("chat:read:bot" | "chat:write:bot")[];
  /** (maxItems: 64) */
  eventTypes: ("app.grant_revoked" | "app.installed" | "app.uninstalled" | "artifact.deleted" | "artifact.ready" | "bot.mentioned" | "bot.message_received" | "conversation.created" | "conversation.member_added" | "conversation.member_removed" | "conversation.updated" | "meeting.created" | "meeting.ended" | "meeting.started" | "message.created" | "message.deleted" | "message.expired" | "message.moderated" | "participant.joined" | "participant.left")[];
  meetingSurface: {
    /** (maxLength: 2048; pattern: "^https://") */
    url: string;
    /** (maxItems: 10) */
    origins: string[];
  } | null;
  distribution: "PRIVATE" | "PUBLIC";
}

/** ADR 0065 review status. */
export type AppStatus = "DRAFT" | "SUBMITTED" | "IN_REVIEW" | "APPROVED" | "REJECTED" | "PUBLISHED" | "SUSPENDED";

/** An attachment's authorized metadata and lifecycle. Bytes live in protected object storage under a server-generated key. */
export interface Attachment {
  attachmentId: MessagingUuid;
  conversationId: MessagingUuid;
  kind: "IMAGE" | "DOCUMENT" | "FILE" | "VOICE_NOTE" | "VIDEO";
  state: "INTENT_CREATED" | "VERIFYING" | "READY" | "QUARANTINED" | "REJECTED" | "EXPIRED" | "DELETED";
  stateReason: string | null;
  securityProfile: "PRIVATE_E2EE" | "MANAGED";
  uploaderPrincipalId: MessagingUuid;
  /** Exact byte size of the stored object (ciphertext for PRIVATE_E2EE). (minimum: 1) */
  size: number;
  /** (pattern: "^[0-9a-f]{64}$") */
  sha256: string;
  /** PRIVATE_E2EE: always application/octet-stream (the real type is protected inside MLS). (maxLength: 127) */
  mediaType: string;
  fileName: string | null;
  media: {
    /** (minimum: 1) */
    durationMs?: number;
    durationVerified?: false;
    /** (maxLength: 64) */
    codec?: string;
    /** (maxItems: 256) */
    waveform?: number[];
  } | null;
  cryptoFormat: "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1" | null;
  /** PRIVATE_E2EE attachments are ciphertext: never scanned and never reported as scanned (NOT_APPLICABLE_ENCRYPTED). */
  scan: {
    status: "NOT_REQUIRED" | "PENDING" | "CLEAN" | "INFECTED" | "FAILED" | "NOT_APPLICABLE_ENCRYPTED";
    engine: string | null;
    finding: string | null;
  };
  messageId: MessagingUuid | null;
  /** (date-time) */
  createdAt: string;
  finalizedAt: string | null;
  /** (date-time) */
  intentExpiresAt: string;
  /** Server-made derivatives (metadata reads only; empty for PRIVATE_E2EE). (maxItems: 8) */
  derivatives?: AttachmentDerivative[];
}

/** A server-made derivative of a MANAGED attachment: a re-encoded PNG thumbnail of an image without the original's metadata (THUMBNAIL), or the measured container, duration and dimensions of an image, voice note or video (INSPECTION, no object). Never produced for PRIVATE_E2EE attachments, whose thumbnails the sending client encrypts itself. */
export interface AttachmentDerivative {
  kind: "THUMBNAIL" | "INSPECTION";
  /** QUOTA_DEFERRED: the environment's storage budget has no room for the thumbnail. No object is stored or charged; the original attachment is unaffected; the derivative is retried later or when the attachment policy changes (errorCode STORAGE_BUDGET). */
  status: "PENDING" | "READY" | "FAILED" | "QUOTA_DEFERRED";
  mediaType: string | null;
  /** (minimum: 1) */
  width: number | null;
  /** (minimum: 1) */
  height: number | null;
  /** (minimum: 1) */
  sourceWidth: number | null;
  /** (minimum: 1) */
  sourceHeight: number | null;
  /** The stored object's actual bytes (THUMBNAIL), charged to the environment's storage budget alongside the original. (minimum: 0) */
  size: number | null;
  /** (pattern: "^[0-9a-f]{64}$") */
  sha256: string | null;
  /** (pattern: "^[A-Z][A-Z0-9_]{1,31}$") */
  errorCode: string | null;
  /** (date-time) */
  completedAt: string | null;
  /** INSPECTION only: the measured facts (null for THUMBNAIL and until READY). */
  metadata: MediaInspection | null;
}

/** A short-lived signed read of one object forcing Content-Disposition: attachment and the stored content type. Issued only after authorization (current membership, history window, message ACTIVE, attachment READY). */
export interface AttachmentDownloadGrant {
  attachmentId: MessagingUuid;
  method: "GET";
  /** (maxLength: 4096) */
  url: string;
  /** (date-time) */
  expiresAt: string;
  /** (minimum: 1) */
  size: number;
  /** (pattern: "^[0-9a-f]{64}$") */
  sha256: string;
  cryptoFormat: "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1" | null;
  /** A grant issued before a revocation stays usable at most this long; GET .../content re-checks authorization on every request. (minimum: 1) */
  residualAccessSeconds: number;
}

export interface AttachmentIntentRequest {
  kind: "IMAGE" | "DOCUMENT" | "FILE" | "VOICE_NOTE" | "VIDEO";
  /** (minimum: 1) */
  size: number;
  /** (pattern: "^[0-9a-f]{64}$") */
  sha256: string;
  /** MANAGED: required, in the kind's allow-list. PRIVATE_E2EE: omit (or application/octet-stream). (maxLength: 127) */
  mediaType?: string;
  /** MANAGED only (sanitized). Refused for PRIVATE_E2EE. (maxLength: 1024) */
  fileName?: string;
  /** (minimum: 1) */
  durationMs?: number;
  /** (maxLength: 64) */
  codec?: string;
  /** (maxItems: 256) */
  waveform?: number[];
  /** PRIVATE_E2EE: the uploading device (the acting principal's). */
  deviceId?: MessagingUuid;
  cryptoFormat?: "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1";
}

export interface AttachmentUploadIntent {
  attachmentId: MessagingUuid;
  conversationId: MessagingUuid;
  kind: "IMAGE" | "DOCUMENT" | "FILE" | "VOICE_NOTE" | "VIDEO";
  state: "INTENT_CREATED" | "VERIFYING" | "READY" | "QUARANTINED" | "REJECTED" | "EXPIRED" | "DELETED";
  stateReason: string | null;
  securityProfile: "PRIVATE_E2EE" | "MANAGED";
  uploaderPrincipalId: MessagingUuid;
  /** Exact byte size of the stored object (ciphertext for PRIVATE_E2EE). (minimum: 1) */
  size: number;
  /** (pattern: "^[0-9a-f]{64}$") */
  sha256: string;
  /** PRIVATE_E2EE: always application/octet-stream (the real type is protected inside MLS). (maxLength: 127) */
  mediaType: string;
  fileName: string | null;
  media: {
    /** (minimum: 1) */
    durationMs?: number;
    durationVerified?: false;
    /** (maxLength: 64) */
    codec?: string;
    /** (maxItems: 256) */
    waveform?: number[];
  } | null;
  cryptoFormat: "CFMEET_ATTACHMENT_XCHACHA20POLY1305_STREAM_V1" | null;
  /** PRIVATE_E2EE attachments are ciphertext: never scanned and never reported as scanned (NOT_APPLICABLE_ENCRYPTED). */
  scan: {
    status: "NOT_REQUIRED" | "PENDING" | "CLEAN" | "INFECTED" | "FAILED" | "NOT_APPLICABLE_ENCRYPTED";
    engine: string | null;
    finding: string | null;
  };
  messageId: MessagingUuid | null;
  /** (date-time) */
  createdAt: string;
  finalizedAt: string | null;
  /** (date-time) */
  intentExpiresAt: string;
  /** A signed single-object upload bound to the exact size (content-length), SHA-256 (x-amz-checksum-sha256) and content type: the store rejects any other bytes. Send exactly these headers. */
  upload: {
    method: "PUT";
    /** (maxLength: 4096) */
    url: string;
    headers: {
      name: string;
      value: string;
    }[];
    /** (date-time) */
    expiresAt: string;
  };
}

export interface AttendanceEntry {
  admissionState: AdmissionState;
  /** (date-time) */
  admittedAt?: string | null;
  /** (int64) */
  attendanceSeconds: number;
  displayName: string;
  identityType: IdentityType;
  /** (uuid) */
  participantId: string;
  role: Role;
}

/** Signed, short-lived, single-object read access (never persisted). Conference attendance (Part J attendance contract). Attendance time is merged from participant-session intervals, so multi-device presence is never double counted. */
export interface AttendanceReport {
  /** (uuid) */
  conferenceId: string;
  /** (date-time) */
  generatedAt: string;
  participants: AttendanceEntry[];
  /** (int64; minimum: 0) */
  sessionCount: number;
}

/** Consent data for the consent page. */
export interface AuthorizeContextResponse {
  app: OauthAppSummary;
  /** (uuid) */
  organizationId?: string | null;
  organizationInstall?: OrganizationInstallDecision | null;
  /** The redirect URI Platform Access validated (exactly the requested one). */
  redirectUri: string;
  scopes: OauthScopeView[];
  state?: string | null;
  /** `USER` or `ORGANIZATION`. */
  target: string;
}

/** `POST /api/v2/oauth/authorize/approve`. */
export interface AuthorizeDecisionRequest {
  clientId: string;
  codeChallenge: string;
  codeChallengeMethod: string;
  /** `APPROVE` or `DENY`. */
  decision: string;
  /** `organization` for a workspace install; absent or `user` for a personal install. */
  install?: string | null;
  /** (uuid) */
  organizationId?: string | null;
  redirectUri: string;
  scope: string;
  state?: string | null;
}

export interface AuthorizeDecisionResponse {
  /** Where the browser goes next: `redirect_uri?code=…&state=…` on approval, `redirect_uri?error=access_denied&state=…` on denial. */
  redirectTo: string;
}

export interface AvailableAction {
  action: "ACCEPT" | "DECLINE" | "CANCEL" | "END" | "HOLD" | "RESUME" | "TRANSFER" | "ISSUE_MEDIA_CREDENTIAL" | "UPGRADE" | "START_SCREEN_SHARE" | "STOP_SCREEN_SHARE" | "ADD_PARTICIPANTS";
  available: boolean;
  /** Why the action is unavailable: the credential lacks calls.write, the Call's state does not allow it, or the configured media provider cannot enforce it. */
  reason?: "INSUFFICIENT_SCOPE" | "STATE" | "CAPABILITY_UNAVAILABLE";
}

/** Server-derived actions currently valid for a Call and the authenticated credential. The client never computes them from local state. */
export interface AvailableActions {
  callId: CallingUuid;
  callState: CallState;
  /** (minimum: 1) */
  callVersion: number;
  actions: AvailableAction[];
}

/** Breakout recording metadata only — recording itself is Part G. */
export type BreakoutRecordingPolicy = "INHERIT_MAIN_POLICY" | "DISABLED" | "SEPARATE_ALLOWED";

/** C15: the controlled built-in hold-music catalogue. The media plane / application client plays its own copy of each; never a URL, path or file name. */
export type BuiltinHoldMusic = "DEFAULT" | "CALM" | "BRIGHT";

/** The controlled built-in prompt catalog. The media plane plays its own copy (Telephony maps it to edge-local audio; an application client to its own asset). Never a URL, path, file name or synthesized text. Each prompt has a nominal length the media plane's audio must fit (WELCOME, MENU, NO_INPUT, GOODBYE 2 s; PLEASE_HOLD, TRANSFERRING, CLOSED 3 s; INVALID_OPTION 4 s; VOICEMAIL_GREETING 6 s; BEEP 1 s): a menu's input timer starts after the prompts it queued, and a HANGUP node's prompt plays for … */
export type BuiltinPrompt = "WELCOME" | "MENU" | "INVALID_OPTION" | "NO_INPUT" | "PLEASE_HOLD" | "TRANSFERRING" | "GOODBYE" | "CLOSED" | "VOICEMAIL_GREETING" | "BEEP";

export interface BulkAdmissionResponse {
  /** Transitioned by this request. */
  changed: string[];
  /** In a state that does not allow this transition. */
  skipped: SkippedParticipant[];
  /** Already in the requested state (idempotent retry). */
  unchanged: string[];
}

/** One call with participants, session summaries and durations. Durations of open connections are measured up to durationsAsOf (the call's end for terminal calls). connectedDurationMs runs from the first CONNECTED to the end (null if never connected). invitations are the Call's ring offers (one per target endpoint and ring round); transfers its application transfers. */
export type Call = CallCore;

/** Fields of every Call representation (HTTP API, call-control results). Provider-independent: no provider or carrier identifiers, execution bindings, media identities, telephone numbers or protected contact data. */
export interface CallCore {
  callId: CallingUuid;
  kind: CallKind;
  state: CallState;
  /** Optimistic-concurrency version: 1 at creation, +1 per state transition. (minimum: 1) */
  version: number;
  initiatorPrincipalId: CallingUuid;
  terminalReason: CallTerminalReason | null;
  failureCategory: CallFailureCategory | null;
  /** (date-time) */
  requestedAt: string;
  /** (date-time) */
  authorizedAt: string | null;
  /** (date-time) */
  ringingAt: string | null;
  /** (date-time) */
  answeredAt: string | null;
  /** (date-time) */
  connectedAt: string | null;
  /** (date-time) */
  endingAt: string | null;
  /** When the call reached a terminal state. (date-time) */
  endedAt: string | null;
  /** (date-time) */
  createdAt: string;
  /** (date-time) */
  updatedAt: string;
}

/** Normalized failure category. Provider error codes never become categories. */
export type CallFailureCategory = "POLICY" | "TARGET_UNAVAILABLE" | "ROUTING" | "TIMEOUT" | "MEDIA" | "NETWORK" | "EXECUTION" | "INTERNAL" | "UNKNOWN";

/** A Call as it appears in one principal's history. */
export type CallHistoryEntry = CallCore;

/** One page of a principal's call history, newest Call first (createdAt DESC, callId DESC). */
export interface CallHistoryList {
  principalId: CallingUuid;
  /** (maxItems: 100) */
  data: CallHistoryEntry[];
  /** Opaque cursor of the next page; null on the last page. (maxLength: 64) */
  nextCursor: string | null;
}

/** A call as listed and as returned by call-control commands. */
export type CallingCallSummary = CallCore;

/** Stable public error codes of the Calling service. */
export type CallingErrorCode = "AUTHENTICATION_FAILED" | "INSUFFICIENT_SCOPE" | "NOT_FOUND" | "METHOD_NOT_ALLOWED" | "VALIDATION_ERROR" | "RATE_LIMITED" | "SERVICE_UNAVAILABLE" | "INTERNAL_ERROR" | "STALE_CALL_VERSION" | "CALL_STATE_CONFLICT" | "CALL_EVIDENCE_REQUIRED" | "CALL_NOT_AUTHORIZED" | "CONCURRENT_CALL_LIMIT_EXCEEDED" | "CALL_RESOURCE_STATE_CONFLICT" | "CONFLICT" | "IDEMPOTENCY_CONFLICT" | "INVITATION_EXPIRED" | "INVITATION_NOT_ACTIONABLE" | "ANSWERED_ELSEWHERE" | "CAPABILITY_UNAVAILABLE" | "MEDIA_UNAVAILABLE" | "PARTICIPANT_LIMIT_EXCEEDED" | "TARGET_UNAVAILABLE" | "PRIVACY_AUTHORIZATION_UNAVAILABLE" | "PUBLIC_ROUTE_DENIED" | "PUBLIC_ROUTE_UNAVAILABLE" | "TELECOM_SPEND_LIMIT_EXCEEDED" | "VERSION_CONFLICT" | "ENTERPRISE_POLICY_DENIED" | "ARTIFACT_STORAGE_UNAVAILABLE" | "RECORDING_NOT_AUTHORIZED" | "PRINCIPAL_NOT_ELIGIBLE" | "COMMUNICATIONS_POLICY_UNAVAILABLE" | "SIGNALING_CREDENTIAL_EXPIRED";

/** Error response body. Messages are fixed, safe strings; authentication failures are uniform. */
export interface CallingErrorEnvelope {
  error: {
    code: CallingErrorCode;
    message: string;
    requestId: CallingRequestId | null;
  };
}

/** Server-generated identifier of one request / one service hop. */
export type CallingRequestId = string;

export interface CallingUserSession {
  /** The token's user (principal). (uuid) */
  principalId: string;
  /** The installation's endpoint. (uuid) */
  endpointId: string;
  /** Short-lived signaling credential of the endpoint (secret; signalingCredential security scheme): GET /v1/calling/signaling, invitations, accept / decline, media credentials. (maxLength: 2048; pattern: "^cfsig_[A-Za-z0-9_.-]+$") */
  signalingCredential: string;
  /** Expiry of the signaling credential: the earlier of the configured signaling lifetime and the user token's expiry. Exchange the (refreshed) user token again before it. (date-time) */
  signalingCredentialExpiresAt: string;
}

export interface CallingUserSessionRequest {
  /** Stable id of this app installation, generated by the SDK. The installation's endpoint key is inst:<installationId> (inst:h:<sha-256 hex> when that would exceed 128 characters). (pattern: "^[A-Za-z0-9._-]{8,128}$") */
  installationId: string;
  /** The device's endpoint kind. */
  kind: "WEB" | "MOBILE";
}

/** Lower-case hyphenated UUID. */
export type CallingUuid = string;

/** Why a ring offer exists: the Call itself, or an application transfer to a new target. */
export type CallInvitationPurpose = "CALL" | "TRANSFER";

/** State of a ring offer to one endpoint. PENDING and RINGING are live; every other state is terminal and final (an offer is never revived). The server owns expiry. */
export type CallInvitationStatus = "PENDING" | "RINGING" | "ACCEPTED" | "DECLINED" | "ANSWERED_ELSEWHERE" | "CANCELLED" | "EXPIRED";

/** Why a ring offer ended. */
export type CallInvitationTerminalReason = "ACCEPTED" | "DECLINED" | "ANSWERED_ELSEWHERE" | "EXPIRED" | "CALL_CANCELLED" | "CALL_TERMINATED" | "ENDPOINT_REVOKED" | "PARTICIPANT_REMOVED" | "TRANSFER_ENDED";

/** Neutral media kind of a Call. No consumer business meaning. */
export type CallKind = "VOICE" | "VIDEO";

export interface CallList {
  /** (maxItems: 100) */
  data: CallingCallSummary[];
  /** Opaque cursor of the next page; null on the last page. (maxLength: 64) */
  nextCursor: string | null;
}

/** Media capabilities the server authorizes for one session's media connection. Derived from server state (call kind, participant permissions, hold); never chosen by the client. There is no administrative, recording, room-creation or room-listing capability. */
export interface CallMediaCapabilities {
  canPublishAudio: boolean;
  canPublishVideo: boolean;
  canPublishData: boolean;
  canSubscribe: boolean;
  /** The grant includes the screen sources (screen video and screen audio). True only for the Call's current presenter (POST /v1/calls/{call_id}/screen-share) while the Call is not held. */
  canShareScreen: boolean;
}

export type CallParticipantRole = "INITIATOR" | "RECIPIENT";

/** Canonical business state of a Call. Terminal: ENDED, DECLINED, BUSY, NO_ANSWER, FAILED, CANCELLED. */
export type CallState = "REQUESTED" | "AUTHORIZED" | "RESOLVING" | "ROUTING" | "DIALING" | "RINGING" | "ANSWERED" | "CONNECTED" | "HELD" | "TRANSFERRING" | "ENDING" | "ENDED" | "DECLINED" | "BUSY" | "NO_ANSWER" | "FAILED" | "CANCELLED";

/** A call as listed and as returned by call-control commands. */
export type CallSummary = CallCore;

/** How a Call reached its terminal state. ENDED records its completion evidence; every other terminal state has the reason of the same name. */
export type CallTerminalReason = "CLEANUP_CONFIRMED" | "PARTICIPATION_ENDED" | "RECONCILIATION_TIMEOUT" | "DECLINED" | "BUSY" | "NO_ANSWER" | "FAILED" | "CANCELLED";

export type ChatSendMode = "EVERYONE" | "HOSTS_ONLY";

export type Ciphersuite = "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519";

/** Sparse override. Unknown fields are rejected. */
export interface CollaborationPolicyPatch {
  aiNotice?: boolean | null;
  allowedReactions?: string[] | null;
  audioLocked?: boolean | null;
  breakoutChatEnabled?: boolean | null;
  breakoutRecordingPolicy?: BreakoutRecordingPolicy | null;
  chatEnabled?: boolean | null;
  chatSendMode?: ChatSendMode | null;
  consentVersion?: string | null;
  handRaiseEnabled?: boolean | null;
  mainChatVisibleInBreakouts?: boolean | null;
  /** (int32; minimum: 0) */
  maxBreakouts?: number | null;
  /** (int32; minimum: 0) */
  maxSpotlights?: number | null;
  pollsEnabled?: boolean | null;
  privateChatMode?: PrivateChatMode | null;
  qnaAnonymousAllowed?: boolean | null;
  qnaEnabled?: boolean | null;
  reactionsEnabled?: boolean | null;
  requiredConsents?: ConsentKind[] | null;
  screenSharePolicy?: ScreenSharePolicy | null;
  videoLocked?: boolean | null;
}

/** Body of cancel, end, hold, resume and session removal: the expected Call version and, for end / hold / resume only, the acting participant (no state, reason or permission can be sent). */
export interface CommandRequest {
  /** The Call version the command was decided on (optimistic concurrency). A different current version is refused with 409 STALE_CALL_VERSION. (minimum: 1) */
  expectedVersion: number;
  /** 1.8.0: end / hold / resume only (cancel and session removal refuse it, 400). The participant acting for everyone. In a group Call (more than two participants, ever) only its host may end or hold / resume it for everyone (another participant: 403 CALL_NOT_AUTHORIZED; it leaves by removing its … */
  actingParticipantId?: CallingUuid;
}

export interface ConferenceActionResponse {
  /** False when the request was an idempotent no-op (e.g. already LIVE). */
  changed: boolean;
  conference: ConferenceResponse;
  providerEnforcement: ProviderEnforcement;
}

/** An app with a meeting surface installed for the meeting's owner or organization. */
export interface ConferenceApp {
  /** (uuid) */
  appId: string;
  developerName?: string | null;
  /** `OWNER` (the meeting owner's personal install) or `ORGANIZATION`. */
  installedFor: string;
  logoUrl?: string | null;
  name?: string | null;
  surface: MeetingSurfaceView;
}

export interface ConferenceAppList {
  items: ConferenceApp[];
}

export type ConferenceKind = "INSTANT" | "SCHEDULED" | "RECURRING" | "LEGACY";

export interface ConferenceListResponse {
  items: ConferenceResponse[];
}

export interface ConferenceResponse {
  /** (date-time) */
  endedAt?: string | null;
  /** (date-time) */
  endingRequestedAt?: string | null;
  /** (uuid) */
  id: string;
  isException?: boolean | null;
  isLegacy?: boolean | null;
  isLocked?: boolean | null;
  kind?: ConferenceKind | null;
  /** (date-time) */
  lockedAt?: string | null;
  /** (uuid) */
  meetingSpaceId: string;
  occurrenceKey?: string | null;
  /** (date-time) */
  originalStartAt?: string | null;
  /** Part G: recording indicator (`OFF` / `REQUESTED` / `RECORDING` / `STOPPING`) — the authoritative recovery source after a reconnect. */
  recordingState?: string | null;
  /** (date-time) */
  scheduledEndAt?: string | null;
  /** (date-time) */
  scheduledStartAt?: string | null;
  /** (date-time) */
  startedAt?: string | null;
  status: ConferenceStatus;
  terminationReason?: TerminationReason | null;
  /** Part G: live transcription indicator. */
  transcriptionState?: string | null;
  /** (int32) */
  version?: number | null;
  view: ResourceView;
}

export type ConferenceStatus = "CREATED" | "PREPARING" | "WAITING" | "LIVE" | "ENDING" | "ENDED" | "CANCELLED" | "FAILED";

/** Minimal conference info included in space responses. */
export interface ConferenceSummary {
  /** (date-time) */
  endedAt?: string | null;
  /** (uuid) */
  id: string;
  /** (date-time) */
  scheduledEndAt?: string | null;
  /** (date-time) */
  scheduledStartAt?: string | null;
  /** (date-time) */
  startedAt?: string | null;
  status: ConferenceStatus;
}

/** One installed app (a grant). */
export interface ConnectedApp {
  /** (uuid) */
  appId: string;
  createdAt?: string | null;
  developerName?: string | null;
  /** (uuid) */
  grantId: string;
  /** Whether the app has an in-meeting panel. */
  hasMeetingSurface: boolean;
  logoUrl?: string | null;
  name?: string | null;
  /** (uuid) */
  organizationId?: string | null;
  scopes: string[];
  status: string;
  /** `USER` or `ORGANIZATION`. */
  target: string;
}

export interface ConnectedAppList {
  items: ConnectedApp[];
}

export interface ConsentGrant {
  type: ConsentKind;
  version: string;
}

/** Explicit pre-join consent kinds (`meeting_consents.consent_type`). */
export type ConsentKind = "RECORDING" | "TRANSCRIPTION" | "AI_PROCESSING" | "DATA_SHARING" | "HEALTHCARE_TERMS" | "PRIVACY_NOTICE";

/** Lower-case hyphenated UUID. */
export type ContactReference = string;

/** Generic context classes; a product's order, trip, consultation or case is only an opaque ContextRef. */
export type ContextClass = "DIRECT" | "GROUP" | "MEETING" | "TRANSACTION" | "CONSULTATION" | "SUPPORT" | "ENTERPRISE" | "DEVELOPER_DEFINED";

/** An opaque reference in the application's own namespace. Never authority, never a membership grant. */
export interface ContextRef {
  /** (pattern: "^[a-z][a-z0-9-]{0,62}$") */
  namespace: string;
  /** (pattern: "^[A-Za-z0-9._:/-]{1,256}$") */
  reference: string;
  /** (minimum: 1; maximum: 2147483647) */
  version?: number;
}

/** Result of cancel, end, hold, resume and upgrade: the Call after the command. END answers with ENDING until provider cleanup is confirmed, or ENDED once it is. UPGRADE answers with the Call's new kind (VIDEO); its state and version are unchanged. */
export interface ControlResult {
  call: CallSummary;
  operation: "CANCEL" | "END" | "HOLD" | "RESUME" | "UPGRADE";
}

/** The canonical Conversation aggregate. PRIVATE_E2EE: MLS 1.0 with the server-assigned 32-byte MLS group id; serverCanReadContent is false. MANAGED: no MLS binding; serverCanReadContent is true. */
export interface Conversation {
  conversationId: MessagingUuid;
  contextClass: ContextClass;
  context: ContextRef | null;
  securityProfile: SecurityProfile;
  encryption: {
    protocol: Protocol;
    ciphersuite: Ciphersuite | null;
    mlsGroupId: string | null;
    /** (minimum: 1) */
    securityPolicyVersion: number;
    serverCanReadContent: boolean;
  };
  lifecycleState: LifecycleState;
  lifecycleReason: string | null;
  policy: {
    snapshotId: MessagingUuid;
    /** (pattern: "^[0-9a-f]{64}$") */
    digest: string;
    /** (minimum: 0) */
    applicationPolicyVersion: number;
  };
  /** (minimum: 1) */
  version: number;
  createdByPrincipalId: MessagingUuid;
  ownerPrincipalId: MessagingUuid;
  /** The first 100 ACTIVE members (owner, administrators, then members); page through GET /v1/conversations/{id}/members for the rest (membersTruncated). (maxItems: 100) */
  members: {
    principalId: MessagingUuid;
    role: MemberRole;
  }[];
  /** (date-time) */
  createdAt: string;
  /** (date-time) */
  updatedAt: string;
  /** (date-time) */
  stateChangedAt: string;
  expiresAt: string | null;
  /** (minimum: 0) */
  memberCount: number;
  membersTruncated: boolean;
  expiryGraceSeconds: number | null;
  retentionSeconds: number | null;
  predecessorConversationId: MessagingUuid | null;
  successorConversationId: MessagingUuid | null;
  /** Appendix K: false — no read receipts in this conversation; null — the environment policy decides. */
  readReceipts: boolean | null;
  /** Section 13: display title of a MANAGED conversation (always null for PRIVATE_E2EE, whose name belongs inside the MLS group). */
  title?: string | null;
  /** Section 13: opaque avatar reference of a MANAGED conversation (for example an attachment id the client resolves); never a URL the server fetches. */
  avatarRef?: string | null;
  inbox?: ConversationInbox;
  /** S13: only OWNER / ADMIN members may send (send, edit and forward); others read and react as policy allows. */
  announcementOnly?: boolean;
}

/** A durable conversation event (derived from committed state, replayable by its per-conversation eventSequence). Every durable event names its tenant, application and environment (§117). */
export interface ConversationEvent {
  /** (pattern: "^evt_[0-9a-f]{32}$") */
  eventId: string;
  /** (pattern: "^[a-z][a-z0-9.-]{2,79}$") */
  type: string;
  /** (minimum: 1) */
  schemaVersion: number;
  conversationId: MessagingUuid;
  /** (minimum: 1) */
  eventSequence: number;
  /** (date-time) */
  occurredAt: string;
  /** camelCase payload; message events carry the message's CURRENT representation (a deleted message replays as a tombstone). */
  data: Record<string, unknown>;
  tenantId: MessagingUuid;
  applicationId: MessagingUuid;
  environmentId: MessagingUuid;
}

/** Section 13 (MSG-007): the inbox summary of a conversation for the acting member, present in GET /v1/conversations answers that have an acting principal. Left conversations are not listed (only ACTIVE memberships are); a tombstoned last message carries its lifecycle and no content. */
export interface ConversationInbox {
  /** The latest application message (or the creation time); the key of orderBy=lastActivity. (date-time) */
  lastActivityAt: string;
  lastMessage: ConversationLastMessage | null;
  /** Active application messages from others after the member's read cursor, inside its history window, not hidden for it. (minimum: 0) */
  unreadCount: number;
  /** Unread messages that mention the member (the sender's declared mention ids: MANAGED and — B3, 1.34.0 — PRIVATE_E2EE, whose mention text stays inside the ciphertext). (minimum: 0) */
  mentionCount: number;
  /** F1 (1.34.0): the member archived this chat for itself (PUT /v1/conversations/{conversationId}/inbox-settings). It stays archived when new messages arrive. Never visible to other members. */
  archived?: boolean;
  /** F1 (1.34.0): the member pinned this chat for itself (at most three; clients list pinned chats first, most recently pinned first). */
  pinned?: boolean;
  /** F1 (1.34.0): when the member pinned this chat (the pin order); null when not pinned. */
  pinnedAt?: string | null;
}

/** Preview metadata of the last application message visible to the member (its history window and delete-for-me markers honoured). */
export interface ConversationLastMessage {
  messageId: MessagingUuid;
  /** (minimum: 1) */
  sequence: number;
  senderPrincipalId: MessagingUuid;
  /** (date-time) */
  createdAt: string;
  lifecycle: MessageLifecycle;
  securityProfile: SecurityProfile;
  /** (minimum: 0) */
  attachmentCount: number;
  /** MANAGED and ACTIVE only: the first 140 characters of the text. Always null for PRIVATE_E2EE (the server holds no plaintext and never returns ciphertext here) and for tombstones. */
  previewText: string | null;
}

export interface ConversationPage {
  /** (maxItems: 100) */
  conversations: Conversation[];
  nextCursor: string | null;
}

/** Body of POST /v1/calls. The initiator calls from one of its registered ACTIVE endpoints; every target is a principal of the same application environment (resolved inside the credential's scope; an unknown or foreign principal is 404). One target is a one-to-one call, several targets a group call (at most the configured participant limit, 16 by default, including the initiator). The server decides authorization, routing, ringing and media; nothing of that can be sent. Part E: instead of … */
export interface CreateCallRequest {
  kind: CallKind;
  initiatorPrincipalId: CallingUuid;
  /** The initiator's registered ACTIVE endpoint. Required for application and APP_TO_PSTN calls; absent for a callback relay. */
  initiatorEndpointId?: CallingUuid;
  /** The requester asks to be called back on its own protected contact and bridged to the callee (Part E §47). A request, not a route: Privacy Relay and the Routing Engine decide whether CALLBACK_RELAY is allowed (403 PUBLIC_ROUTE_DENIED otherwise). */
  callbackRequested?: true;
  /** The requester's own verified protected contact (callback relay only). */
  initiatorContactReference?: ContactReference;
  /** (maxItems: 64) */
  targetPrincipalIds?: CallingUuid[];
  /** A protected contact (opaque ContactReference). Requires privacyRelaySessionId; exclusive with targetPrincipalIds. */
  targetContactReference?: ContactReference;
  /** Optional Privacy Relay Session (cfmeet.privacy-relay.session). The call becomes a PROTECTED call: Calling asks Privacy Relay for current authority (PRIVACY_RELAY_V1) before anything rings, records the decision and fails closed (503 PRIVACY_AUTHORIZATION_UNAVAILABLE) when no answer can be obtained; … */
  privacyRelaySessionId?: CallingUuid;
  /** Presentation metadata only. (minLength: 1; maxLength: 100) */
  initiatorDisplayName?: string;
  /** Part F: an enterprise extension number (an internal short code, never a telephone number) resolved in the credential's scope and routed by enterprise configuration over ENTERPRISE_INTERNAL (no public leg, no spend). Requires enterprise.calls.use and initiatorEndpointId; excludes every other target. … */
  targetExtension?: ExtensionNumber;
  /** Part IV L: a Digital Communications Number (CF Meet Number Resource id — never a telephone number) to present as the caller identity of a targetContactReference call instead of the session's relay identity. Numbering must hold it ACTIVE for the credential's scope with usable outbound voice, and the … */
  presentedNumberResourceId?: CallingUuid;
  // exactly one of these field sets: [initiatorEndpointId, targetPrincipalIds] | [initiatorEndpointId, targetContactReference, privacyRelaySessionId] | [callbackRequested, initiatorContactReference, targetContactReference, privacyRelaySessionId] | [initiatorEndpointId, targetExtension]
}

/** Response of POST /v1/calls (201; replays return the same body with Idempotent-Replayed: true). The call is normally RINGING with one live offer per eligible target endpoint; it can still be ROUTING while the media resource is being prepared (the reconciler resumes it), or already FAILED (ROUTING when no target has an eligible endpoint, MEDIA when the resource could not be prepared in time). Part F: an enterprise Call (targetExtension) additionally carries its enterprise context; while it waits … */
export interface CreateCallResult {
  call: CallSummary;
  initiatorParticipantId: CallingUuid;
  initiatorSessionId: CallingUuid | null;
  participants: CreatedParticipant[];
  /** (minimum: 0) */
  liveInvitationCount: number;
  /** Present for an enterprise Call only: its safe enterprise context. */
  enterprise?: EnterpriseCallContext;
}

export interface CreateConferenceRequest {
  /** (date-time) */
  scheduledEndAt?: string | null;
  /** (date-time) */
  scheduledStartAt?: string | null;
}

/** A tenant / application / environment is never part of a request: it comes from the authenticated credential. */
export interface CreateConversationRequest {
  contextClass: ContextClass;
  securityProfile?: SecurityProfile;
  counterpartPrincipalId?: MessagingUuid;
  context?: ContextRef;
  /** (date-time) */
  expiresAt?: string;
  /** (minimum: 0) */
  expectedPolicyVersion?: number;
  /** Regulated contexts: creating a MANAGED conversation requires acknowledging that the server can read its content (refused otherwise with MANAGED_DISCLOSURE_REQUIRED). */
  managedDisclosureAcknowledged?: boolean;
  /** §38: requires an expiring conversation. Past expiresAt the conversation refuses new messages (CONVERSATION_NOT_ACTIVE / EXPIRED) but stays readable and extendable for this many seconds before it becomes EXPIRED. (minimum: 0; maximum: 2592000) */
  expiryGraceSeconds?: number;
  /** §90 / disappearing messages: a conversation-specific retention, never looser than the context's (or environment's) retention policy (RETENTION_LOOSER_THAN_POLICY). The retention worker expires older messages (tombstone, then purge unless a legal hold protects them); clients delete locally on the … (minimum: 60; maximum: 3153600000) */
  retentionSeconds?: number;
  /** Appendix G: create a deliberate SUCCESSOR of this conversation (the only way to move to another security profile). The caller must be an ACTIVE OWNER / ADMIN of it; same context class (DIRECT: the same two principals); never a context-bound conversation (SUCCESSOR_CONTEXT_BOUND); one successor each … */
  predecessorConversationId?: MessagingUuid;
  /** Required (true) when the successor's security profile differs from its predecessor's; refused otherwise with SECURITY_PROFILE_NOT_ALLOWED / PROFILE_CHANGE_DISCLOSURE_REQUIRED. Clients show the change (e.g. end-to-end encrypted to server-readable) before acknowledging. */
  profileChangeDisclosureAcknowledged?: boolean;
  /** Appendix K: false turns read receipts off for this conversation (never on where the environment policy has them off). */
  readReceipts?: boolean;
  /** Personal CF Meet ID resolved privately through Communications under the authenticated actor and scope. Exactly one recipient identifier is allowed. Rotation invalidates the old public ID; existing canonical conversations remain unchanged. (pattern: "^CF-[A-HJ-NP-Z2-9]{6}$") */
  counterpartCfMeetId?: string;
  /** Section 13: display title (MANAGED only; PRIVATE_E2EE refuses it with CONTENT_REJECTED / DISPLAY_METADATA_MANAGED_ONLY). (minLength: 1; maxLength: 120) */
  title?: string;
  /** Section 13: opaque avatar reference (MANAGED only). (pattern: "^[A-Za-z0-9][A-Za-z0-9._:/-]{0,255}$") */
  avatarRef?: string;
}

export interface CreatedParticipant {
  participantId: CallingUuid;
  principalId: CallingUuid;
  role: CallParticipantRole;
}

export interface CreateEndUserRequest {
  /** The partner's opaque user id (printable ASCII, no spaces); never a phone number, tel: / sip: URI. (minLength: 1; maxLength: 128; pattern: "^[!-~]{1,128}$") */
  externalId: string;
  /** Display name (no control characters). (minLength: 1; maxLength: 64) */
  displayName?: string;
  /** https:// image URL. (maxLength: 512; pattern: "^https://") */
  imageUrl?: string;
}

export interface CreateInvitationRequest {
  /** Restrict the invitation to one occurrence. (uuid) */
  conferenceId?: string | null;
  /** Default 168 (7 days); capped by server policy. (int64) */
  expiresInHours?: number | null;
  inviteeEmail?: string | null;
  role?: Role | null;
  /** Blueprint Section 11: also email the invitation link to `inviteeEmail` (required with this flag). The result is reported in `emailDelivery`. Default false. */
  sendEmail?: boolean | null;
  /** (int32) */
  useLimit?: number | null;
}

export interface CreateInvitationResponse {
  emailDelivery?: EmailDelivery | null;
  invitation: InvitationResponse;
  /** Raw bearer token. Returned exactly once; only its hash is stored. */
  token: string;
}

export interface CreateMeetingSpaceRequest {
  accessConfig?: AccessConfig | null;
  accessPolicy?: AccessPolicy | null;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  defaultRole?: Role | null;
  description?: string | null;
  /** (int32) */
  interactiveCapacity?: number | null;
  meetingType?: MeetingType | null;
  /** Must be an organization the caller verifiably belongs to. (uuid) */
  organizationId?: string | null;
  /** Plaintext passcode; only a bcrypt hash is stored and it is never returned. */
  passcode?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: MeetingsSecurityProfile | null;
  /** IANA timezone, e.g. `Africa/Lagos`. Defaults to `UTC`. */
  timezone?: string | null;
  title: string;
  transcriptionPolicy?: MediaCapturePolicy | null;
  /** (int32) */
  viewerCapacity?: number | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}

/** Uploads (replaces) the environment's credential of that provider (APNS with voip false / true, or FCM). */
export type CreatePushCredentialRequest = ApnsPushCredentialUpload | FcmPushCredentialUpload;

/** The offer was declined. Only this endpoint's offer ends; the Call ends as DECLINED only when every live offer of every pending target has been declined. */
export interface DeclineResult {
  call: CallSummary;
  invitationId: CallingUuid;
  participantId: CallingUuid;
  outcome: "DECLINED";
}

export interface DenyRequest {
  all?: boolean;
  /** Administrative note — stored, never shown to the participant. */
  internalNote?: string | null;
  /** Shown to the denied participant. */
  message?: string | null;
  participantIds?: string[];
  reasonCode?: string | null;
}

/** Kinds of enterprise destination. SCHEDULE is a business-hours schedule (its OPEN / CLOSED route). PUBLIC_CONTACT is a registered public contact (an opaque Privacy Relay ContactReference; forwarding and closed-hours routes only), never a telephone number. */
export type DestinationType = "ENTERPRISE_MEMBER" | "PRINCIPAL" | "EXTENSION" | "RING_GROUP" | "QUEUE" | "IVR" | "VOICEMAIL" | "SCHEDULE" | "PUBLIC_CONTACT";

/** ADR 0065: a PUBLISHED, PUBLIC app of the directory (its published manifest). */
export interface DirectoryApp {
  appId: Uuid;
  name: string;
  description: string | null;
  logoUrl: string | null;
  developerName: string;
  userScopes: ("profile:read" | "profile:email" | "meetings:read" | "meetings:write" | "chat:read" | "chat:write" | "events:meetings")[];
  botScopes: ("chat:read:bot" | "chat:write:bot")[];
  privacyUrl: string | null;
  termsUrl: string | null;
  /** Whether the app has an in-meeting surface. */
  meetingSurface: boolean;
  /** (date-time) */
  publishedAt: string;
}

export interface DirectoryAppList {
  items: DirectoryApp[];
  nextCursor: string | null;
}

export interface EditMessageRequest {
  /** MANAGED plain text (UTF-8, 1-16384 characters, no control characters). No markup subset is defined: the server never interprets HTML, Markdown or scripts, stores the text exactly as given and returns it as a JSON string; clients render it as text (escaping it), never as markup. (minLength: 1; maxLength: 16384) */
  text: string;
  /** Optimistic version check: the editCount the editor last saw. If another edit committed first the request is refused with 409 VERSION_CONFLICT (reason EDITED_CONCURRENTLY). Omitted: last writer wins. (minimum: 0) */
  expectedEditCount?: number;
}

/** What happened to a requested invitation email. */
export type EmailDelivery = "DISABLED" | "ACCEPTED" | "FAILED";

/** A command without parameters (media credentials): an empty object or no body. The server derives every capability; any field is refused. */
export type EmptyRequest = Record<string, unknown>;

/** ADR 0062: a partner user (a Platform Access principal of the credential's environment). DEACTIVATED: deleted (profile erased, id kept). */
export interface EndUser {
  /** The Platform Access principal id (kind EXTERNAL_APPLICATION_PRINCIPAL). */
  id: Uuid;
  /** (maxLength: 128) */
  externalId: string;
  displayName: string | null;
  imageUrl: string | null;
  status: EndUserStatus;
  createdAt: string | null;
  updatedAt: string | null;
}

/** A page of partner users; pass `nextCursor` as `cursor` for the next page (null: last page). */
export interface EndUserList {
  /** (maxItems: 100) */
  items: EndUser[];
  nextCursor: string | null;
}

export type EndUserStatus = "ACTIVE" | "SUSPENDED" | "DEACTIVATED";

/** The safe enterprise context of a Call (create result and call history): the dialled extension's number and label as they were when the Call was placed (a snapshot that survives renumbering and offboarding), the policy version relied on, the routing phase, the current destination kind and the enterprise outcome. No member, device, provider or contact data. */
export interface EnterpriseCallContext {
  extensionId: CallingUuid;
  extensionNumber: ExtensionNumber;
  /** Bounded display name (no control characters, never a telephone number). (minLength: 1; maxLength: 100) */
  extensionLabel: string;
  /** (minimum: 0) */
  policyVersion: number;
  /** Routing phase. */
  phase: "ROUTING" | "RINGING" | "RING_GROUP" | "QUEUED" | "IVR" | "VOICEMAIL" | "FORWARDED" | "ANSWERED" | "ENDED";
  destinationType: DestinationType | null;
  outcome: "ANSWERED" | "VOICEMAIL" | "NO_ANSWER" | "DECLINED" | "ABANDONED" | "FAILED" | "FORWARDED_PUBLIC" | "BUSY" | "COMPLETED_IN_IVR" | null;
  queue?: EnterpriseQueueState;
  ivr?: EnterpriseIvrState;
}

/** The IVR state of a Call (caller view): what the caller hears (recent built-in prompts) and may press (allowed keys) while a menu waits. Never the keys pressed; selectedBranch is a node id. ACTIVE without awaitingInput and with an inputDeadline: a HANGUP prompt is playing and the Call ends at the deadline. */
export interface EnterpriseIvrState {
  ivrFlowId: CallingUuid;
  /** IVR session state. */
  state: "ACTIVE" | "ROUTED" | "COMPLETED" | "ABANDONED" | "FAILED";
  awaitingInput: boolean;
  /** (maxItems: 8) */
  prompts: BuiltinPrompt[];
  /** (maxItems: 12) */
  options: string[];
  inputTimeoutSeconds: number | null;
  /** (date-time) */
  inputDeadline: string | null;
  /** (minimum: 0) */
  retries: number;
  selectedBranch: string | null;
}

/** The queue state of a queued enterprise Call (caller view): the queue, the entry state and the authoritative position (null once the entry finished). */
export interface EnterpriseQueueState {
  queueId: CallingUuid;
  state: QueueEntryState;
  /** (minimum: 1) */
  position: number | null;
  /** (date-time) */
  enteredAt: string;
  /** C15: the queue's built-in hold music the caller's application plays while it waits (null = none). */
  holdMusic?: HoldMusicRef | null;
}

/** Error envelope (Cache-Control: no-store). Messages never contain an identifier, credential, token or database text. */
export interface Error {
  error: {
    code: ErrorCode;
    /** (maxLength: 200) */
    message: string;
    requestId: RequestId | null;
  };
}

export interface ErrorBody {
  code: MeetingServiceErrorCode;
  /** Present on `VALIDATION_ERROR` when the request body failed field validation: one entry per problem, naming the public JSON field (`message` still summarises them as `field: message` lines for older clients). */
  details?: ValidationIssue[] | null;
  /** Safe, human-readable message (never SQL, provider bodies, secrets or paths). */
  message: string;
  /** Server-generated correlation ID (also in the `x-request-id` response header). */
  requestId?: string | null;
  /** Present on `RATE_LIMITED` only. (int64; minimum: 0) */
  retryAfterSeconds?: number | null;
}

export type ErrorCode = "VALIDATION_ERROR" | "NOT_FOUND" | "METHOD_NOT_ALLOWED" | "PAYLOAD_TOO_LARGE" | "INTERNAL_AUTHENTICATION_FAILED" | "INTERNAL_PERMISSION_DENIED" | "DELEGATED_CONTEXT_REQUIRED" | "DELEGATED_CONTEXT_INVALID" | "AUTHENTICATION_REQUIRED" | "PERMISSION_DENIED" | "INSUFFICIENT_SCOPE" | "ORGANIZATION_RESTRICTED" | "ORGANIZATION_SUSPENDED" | "ORGANIZATION_CLOSED" | "ORGANIZATION_NOT_ACTIVE" | "VERSION_CONFLICT" | "INVALID_STATE" | "LAST_OWNER" | "ALREADY_MEMBER" | "INVITATION_PENDING" | "INVITATION_INVALID" | "INVITATION_EMAIL_MISMATCH" | "IDEMPOTENCY_KEY_REUSED" | "CONFLICTING_REPLAY" | "POLICY_VIOLATION" | "OPERATOR_REQUIRED" | "APPROVAL_EXPIRED" | "SELF_APPROVAL_FORBIDDEN" | "PRODUCTION_ACCESS_REQUIRED" | "LIMIT_EXCEEDED" | "RATE_LIMITED" | "CAPABILITY_UNAVAILABLE" | "SERVICE_UNAVAILABLE" | "INTERNAL_ERROR";

/** OpenAPI description of the public error envelope. */
export interface ErrorEnvelope {
  error: ErrorBody;
  success: boolean;
}

export interface EventPage {
  events: ConversationEvent[];
  nextAfter: number | null;
  hasMore: boolean;
  /** The requested position is older than the retained event window: resynchronize messages by sequence. */
  syncRequired: boolean;
}

/** An internal short code — never a public telephone number: 2-6 digits, or a lower-case word (letters, digits, '-') starting with a letter. Matched case-insensitively. */
export type ExtensionNumber = string;

export interface FcmPushCredentialUpload {
  provider: "FCM";
  /** (pattern: "^[a-z][a-z0-9-]{4,28}[a-z0-9]$") */
  projectId: string;
  /** The Firebase service-account key JSON (type service_account; its project_id, when present, must equal projectId). Write-only. (maxLength: 16384) */
  serviceAccountJson: string;
}

/** Enterprise destinations a listing may be narrowed to. */
export type HistoryDestinationType = "EXTENSION" | "RING_GROUP" | "QUEUE" | "IVR" | "VOICEMAIL";

/** Direction of a Call relative to the principal whose history it is. */
export type HistoryDirection = "INBOUND" | "OUTBOUND";

/** C15: a queue's hold music: a built-in choice (customer audio artifacts are not accepted by this deployment). */
export interface HoldMusicRef {
  builtin: BuiltinHoldMusic;
}

export type IdentityType = "REGISTERED_USER" | "INVITED_EXTERNAL" | "GUEST" | "PHONE" | "SERVICE" | "BOT";

export interface InstantMeetingRequest {
  accessPolicy?: AccessPolicy | null;
  passcode?: string | null;
  timezone?: string | null;
  title?: string | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}

export interface InstantMeetingResponse {
  conference: ConferenceResponse;
  meetingSpace: MeetingSpaceResponse;
}

export interface InvitationResponse {
  /** (uuid) */
  conferenceId?: string | null;
  /** (date-time) */
  createdAt: string;
  /** (date-time) */
  expiresAt: string;
  /** (uuid) */
  id: string;
  inviteeEmail?: string | null;
  /** (uuid) */
  meetingSpaceId: string;
  requestedRole: Role;
  status: InvitationStatus;
  /** (int32) */
  usageCount: number;
  /** (int32) */
  useLimit?: number | null;
}

export type InvitationStatus = "ACTIVE" | "REVOKED";

export interface IssueUserTokenRequest {
  grants: UserTokenGrants;
  /** Token lifetime in seconds (default 3600). (minimum: 60; maximum: 86400) */
  ttlSeconds?: number;
}

export interface JoinConferenceRequest {
  /** Part F: explicit consents granted before admission (see the pre-join contract). */
  consents?: ConsentGrant[] | null;
  /** Presentation name only — never identity. Required for guests. */
  displayName?: string | null;
  /** Opaque context forwarded to a server-side access authority (e.g. a HealthNet appointment token). Never treated as proof by the meeting service itself. */
  externalContext?: string | null;
  invitationToken?: string | null;
  passcode?: string | null;
  /** Reconnect: supersede this earlier session of the same participant. (uuid) */
  previousSessionId?: string | null;
}

export interface JoinConferenceResponse {
  conference: JoinConferenceSummary;
  mediaContext: MediaContextView;
  participant: ParticipantResponse;
  participantSession?: ParticipantSessionResponse | null;
  /** Server-issued participant credential for non-registered actors (send as `X-Participant-Token`). Returned only when newly issued. */
  participantToken?: string | null;
  permissions: JoinPermissions;
  policy: JoinPolicy;
  realtime: JoinRealtimeInfo;
  rtc?: RtcCredentialResponse | null;
  rtcStatus: RtcStatus;
}

export interface JoinConferenceSummary {
  /** (uuid) */
  id: string;
  isLocked: boolean;
  /** (uuid) */
  meetingSpaceId: string;
  status: ConferenceStatus;
}

export interface JoinPermissions {
  audioPublish: boolean;
  dataPublish: boolean;
  screenSharePublish: boolean;
  subscribe: boolean;
  videoPublish: boolean;
}

export interface JoinPolicy {
  recordingActive: boolean;
  transcriptionActive: boolean;
}

/** Realtime subscription and recovery resources for a joined (or waiting) participant. Paths are relative to the API origin; authorization is re-evaluated on every call. */
export interface JoinRealtimeInfo {
  /** Canonical conference state — the primary recovery resource. */
  conferencePath: string;
  /** Whether the conference WebSocket stream is available (`FEATURE_CONFERENCE_REALTIME_ENABLED`). */
  enabled: boolean;
  /** REST replay of missed events (`?after=<sequence>`; present when `enabled`). */
  eventsPath?: string | null;
  /** The caller's own participant state (admission decision fallback when polling). */
  participantPath: string;
  /** WebSocket stream (`?ticket=…&cursor=…`; present when `enabled`). */
  streamPath?: string | null;
  /** `POST` → one-shot WebSocket ticket (present when `enabled`). */
  ticketPath?: string | null;
}

export type LifecycleState = "ACTIVE" | "LOCKED" | "CLOSED" | "ARCHIVED" | "EXPIRED" | "DELETED";

export type MediaCapturePolicy = "DISABLED" | "HOST_OPTIONAL" | "AUTO_START" | "ORGANIZATION_CONTROLLED";

export interface MediaContextView {
  /** (uuid) */
  breakoutId?: string | null;
  /** MAIN, BREAKOUT or BACKSTAGE. */
  kind: string;
  visit: boolean;
}

/** A short-lived join credential for one authorized session's connection to the Call's media resource: the connection URL, the opaque token, its expiry and the capabilities the connection receives once admitted. Room names, provider identifiers, provider keys and administrative grants are never part of it. A losing device, a removed session, an expired offer or a terminal Call never receives one. */
export interface MediaCredential {
  sessionId: CallingUuid;
  /** Calling's media connection gateway URL (wss:// in production), never the media server itself. (uri; maxLength: 512; pattern: "^wss?://") */
  url: string;
  /** Opaque join credential bound to this session's media identity, its current media generation and the Call's media resource, sealed by Calling's media edge: the media server does not accept it directly, and the gateway admits it only while that generation of the session holds media access. It carries … (minLength: 1; maxLength: 4096) */
  token: string;
  /** (date-time) */
  expiresAt: string;
  capabilities: CallMediaCapabilities;
}

/** What the server measured in a READY MANAGED attachment's bytes (never inferred for PRIVATE_E2EE ciphertext). */
export interface MediaInspection {
  container: "MP4" | "MATROSKA" | "WEBM" | "OGG_OPUS" | "OGG_VORBIS" | "WAV" | "MP3" | "AAC_ADTS" | "AMR" | "PNG" | "JPEG";
  /** Measured from the bytes (null: the container records none). (minimum: 0) */
  durationMs: number | null;
  /** (minimum: 1) */
  width: number | null;
  /** (minimum: 1) */
  height: number | null;
  hasVideo: boolean;
  hasAudio: boolean;
  /** What the uploader declared (Attachment.media.durationMs). */
  declaredDurationMs: number | null;
  /** Declared vs measured within max(1 s, 5 %); null when either is absent. */
  durationMatchesDeclared: boolean | null;
}

export interface MeetingsErrorBody {
  code: MeetingsErrorCode;
  /** Present on `VALIDATION_ERROR` when the request body failed field validation: one entry per problem, naming the public JSON field (`message` still summarises them as `field: message` lines for older clients). */
  details?: MeetingsValidationIssue[] | null;
  /** Safe, human-readable message (never SQL, provider bodies, secrets or paths). */
  message: string;
  /** Server-generated correlation ID (also in the `x-request-id` response header). */
  requestId?: string | null;
  /** Present on `RATE_LIMITED` only. (int64; minimum: 0) */
  retryAfterSeconds?: number | null;
}

/** Stable, semantic public error codes for the Phase 2 meeting domain (spec v1.1 §5). */
export type MeetingsErrorCode = "MEETING_NOT_FOUND" | "SESSION_NOT_FOUND" | "MEETING_ACCESS_DENIED" | "MEETING_PASSCODE_REQUIRED" | "MEETING_PASSCODE_INVALID" | "MEETING_VERSION_CONFLICT" | "MEETING_CAPACITY_REACHED" | "INVITATION_INVALID" | "INVITATION_EXPIRED" | "CONFERENCE_NOT_FOUND" | "CONFERENCE_NOT_LIVE" | "CONFERENCE_LOCKED" | "CONFERENCE_STATE_CONFLICT" | "WAITING_ROOM_REQUIRED" | "PARTICIPANT_NOT_FOUND" | "PARTICIPANT_BANNED" | "PARTICIPANT_STATE_CONFLICT" | "PERMISSION_DENIED" | "AUTHENTICATION_REQUIRED" | "VALIDATION_ERROR" | "RATE_LIMITED" | "FEATURE_DISABLED" | "RTC_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_ALLOWED" | "REVOCATION_CHECK_UNAVAILABLE" | "MEDIA_PERMISSION_DENIED" | "MEDIA_LOCKED" | "SCREEN_SHARE_NOT_ALLOWED" | "SCREEN_SHARE_NOT_ACTIVE" | "CHAT_DISABLED" | "PRIVATE_CHAT_DISABLED" | "CHAT_PARTICIPANT_RESTRICTED" | "REACTIONS_DISABLED" | "HAND_RAISE_DISABLED" | "BREAKOUT_NOT_FOUND" | "BREAKOUT_CLOSED" | "BREAKOUT_ASSIGNMENT_CONFLICT" | "POLL_NOT_FOUND" | "POLL_NOT_OPEN" | "POLL_ALREADY_RESPONDED" | "POLL_RESPONSE_INVALID" | "POLL_RESULTS_NOT_VISIBLE" | "QUESTION_NOT_FOUND" | "QNA_DISABLED" | "POLLS_DISABLED" | "MESSAGE_NOT_FOUND" | "CONSENT_REQUIRED" | "RECORDING_DISABLED" | "RECORDING_ALREADY_ACTIVE" | "RECORDING_NOT_ACTIVE" | "RECORDING_STATE_CONFLICT" | "RECORDING_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_FOUND" | "CONSENT_DECLINED" | "CONSENT_POLICY_CONFLICT" | "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_READY" | "ARTIFACT_DELETED" | "ARTIFACT_ACCESS_DENIED" | "ARTIFACT_STORAGE_UNAVAILABLE" | "ARTIFACT_STORAGE_NOT_CONFIGURED" | "ARTIFACT_INTEGRITY_FAILED" | "TRANSCRIPTION_DISABLED" | "TRANSCRIPTION_NOT_ALLOWED" | "TRANSCRIPTION_PROVIDER_UNAVAILABLE" | "TRANSCRIPTION_NOT_FOUND" | "INTELLIGENCE_DISABLED" | "INTELLIGENCE_NOT_ALLOWED" | "INTELLIGENCE_PROVIDER_UNAVAILABLE" | "REGISTRATION_DISABLED" | "REGISTRATION_CLOSED" | "REGISTRATION_FULL" | "REGISTRATION_NOT_FOUND" | "ADMISSION_TOKEN_INVALID" | "BACKSTAGE_ACCESS_DENIED" | "PROFILE_POLICY_DENIED" | "STAGE_CAPACITY_REACHED" | "QUALITY_SAMPLE_INVALID" | "QUALITY_RATE_LIMITED" | "OPERATIONS_ACCESS_DENIED" | "REQUEST_TOO_LARGE" | "ENDPOINT_NOT_FOUND" | "METHOD_NOT_ALLOWED" | "LEGACY_WRITE_DISABLED" | "CHAT_UNAVAILABLE" | "CHAT_MIGRATION_REFUSED" | "SERVICE_UNAVAILABLE" | "ORGANIZATION_NOT_FOUND" | "MEMBERSHIP_REQUIRED" | "ORGANIZATION_SUSPENDED" | "ORGANIZATION_ARCHIVED" | "LAST_OWNER" | "VERSION_CONFLICT" | "SLUG_TAKEN" | "INVITE_EXPIRED" | "IDEMPOTENCY_CONFLICT" | "PROVISIONING_PENDING" | "INVITATION_REQUIRED" | "ALREADY_MEMBER" | "INVITEE_NOT_FOUND" | "ORGANIZATION_POLICY_RESTRICTED" | "TEAM_NOT_FOUND" | "TEAM_ARCHIVED" | "TEAM_NAME_TAKEN" | "ORGANIZATION_CALLING_DISABLED" | "USERNAME_TAKEN" | "REAUTHENTICATION_REQUIRED" | "EMAIL_VERIFICATION_REQUIRED" | "DEVICE_PROOF_INVALID" | "WEB_DEVICE_LIMIT_REACHED" | "DEVICE_REPLACEMENT_PENDING" | "DEVICE_NOT_TRUSTED" | "DEVICE_DECLARATION_REFUSED" | "DEVICE_DECLARATION_REQUIRED" | "DEVICE_NOT_FOUND" | "MFA_REQUIRED" | "USER_NOT_FOUND" | "AVATAR_NOT_FOUND" | "DEVICE_KEY_IN_USE" | "DEVICE_LINK_INVALID" | "APP_NOT_FOUND" | "OAUTH_REQUEST_INVALID" | "CALENDAR_FEED_NOT_FOUND" | "EXPORT_LINK_INVALID" | "EXPORT_LINK_EXPIRED" | "DATABASE_ERROR" | "INTERNAL_ERROR";

/** OpenAPI description of the public error envelope. */
export interface MeetingsErrorEnvelope {
  error: MeetingsErrorBody;
  success: boolean;
}

/** Stable, semantic public error codes for the Phase 2 meeting domain (spec v1.1 §5). */
export type MeetingServiceErrorCode = "MEETING_NOT_FOUND" | "SESSION_NOT_FOUND" | "MEETING_ACCESS_DENIED" | "MEETING_PASSCODE_REQUIRED" | "MEETING_PASSCODE_INVALID" | "MEETING_VERSION_CONFLICT" | "MEETING_CAPACITY_REACHED" | "INVITATION_INVALID" | "INVITATION_EXPIRED" | "CONFERENCE_NOT_FOUND" | "CONFERENCE_NOT_LIVE" | "CONFERENCE_LOCKED" | "CONFERENCE_STATE_CONFLICT" | "WAITING_ROOM_REQUIRED" | "PARTICIPANT_NOT_FOUND" | "PARTICIPANT_BANNED" | "PARTICIPANT_STATE_CONFLICT" | "PERMISSION_DENIED" | "AUTHENTICATION_REQUIRED" | "VALIDATION_ERROR" | "RATE_LIMITED" | "FEATURE_DISABLED" | "RTC_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_ALLOWED" | "REVOCATION_CHECK_UNAVAILABLE" | "MEDIA_PERMISSION_DENIED" | "MEDIA_LOCKED" | "SCREEN_SHARE_NOT_ALLOWED" | "SCREEN_SHARE_NOT_ACTIVE" | "CHAT_DISABLED" | "PRIVATE_CHAT_DISABLED" | "CHAT_PARTICIPANT_RESTRICTED" | "REACTIONS_DISABLED" | "HAND_RAISE_DISABLED" | "BREAKOUT_NOT_FOUND" | "BREAKOUT_CLOSED" | "BREAKOUT_ASSIGNMENT_CONFLICT" | "POLL_NOT_FOUND" | "POLL_NOT_OPEN" | "POLL_ALREADY_RESPONDED" | "POLL_RESPONSE_INVALID" | "POLL_RESULTS_NOT_VISIBLE" | "QUESTION_NOT_FOUND" | "QNA_DISABLED" | "POLLS_DISABLED" | "MESSAGE_NOT_FOUND" | "CONSENT_REQUIRED" | "RECORDING_DISABLED" | "RECORDING_ALREADY_ACTIVE" | "RECORDING_NOT_ACTIVE" | "RECORDING_STATE_CONFLICT" | "RECORDING_PROVIDER_UNAVAILABLE" | "RECORDING_NOT_FOUND" | "CONSENT_DECLINED" | "CONSENT_POLICY_CONFLICT" | "ARTIFACT_NOT_FOUND" | "ARTIFACT_NOT_READY" | "ARTIFACT_DELETED" | "ARTIFACT_ACCESS_DENIED" | "ARTIFACT_STORAGE_UNAVAILABLE" | "ARTIFACT_STORAGE_NOT_CONFIGURED" | "ARTIFACT_INTEGRITY_FAILED" | "TRANSCRIPTION_DISABLED" | "TRANSCRIPTION_NOT_ALLOWED" | "TRANSCRIPTION_PROVIDER_UNAVAILABLE" | "TRANSCRIPTION_NOT_FOUND" | "INTELLIGENCE_DISABLED" | "INTELLIGENCE_NOT_ALLOWED" | "INTELLIGENCE_PROVIDER_UNAVAILABLE" | "REGISTRATION_DISABLED" | "REGISTRATION_CLOSED" | "REGISTRATION_FULL" | "REGISTRATION_NOT_FOUND" | "ADMISSION_TOKEN_INVALID" | "BACKSTAGE_ACCESS_DENIED" | "PROFILE_POLICY_DENIED" | "STAGE_CAPACITY_REACHED" | "QUALITY_SAMPLE_INVALID" | "QUALITY_RATE_LIMITED" | "OPERATIONS_ACCESS_DENIED" | "REQUEST_TOO_LARGE" | "ENDPOINT_NOT_FOUND" | "METHOD_NOT_ALLOWED" | "LEGACY_WRITE_DISABLED" | "CHAT_UNAVAILABLE" | "CHAT_MIGRATION_REFUSED" | "SERVICE_UNAVAILABLE" | "ORGANIZATION_NOT_FOUND" | "MEMBERSHIP_REQUIRED" | "ORGANIZATION_SUSPENDED" | "ORGANIZATION_ARCHIVED" | "LAST_OWNER" | "VERSION_CONFLICT" | "SLUG_TAKEN" | "INVITE_EXPIRED" | "IDEMPOTENCY_CONFLICT" | "PROVISIONING_PENDING" | "INVITATION_REQUIRED" | "ALREADY_MEMBER" | "INVITEE_NOT_FOUND" | "ORGANIZATION_POLICY_RESTRICTED" | "TEAM_NOT_FOUND" | "TEAM_ARCHIVED" | "TEAM_NAME_TAKEN" | "ORGANIZATION_CALLING_DISABLED" | "USERNAME_TAKEN" | "REAUTHENTICATION_REQUIRED" | "EMAIL_VERIFICATION_REQUIRED" | "DEVICE_PROOF_INVALID" | "WEB_DEVICE_LIMIT_REACHED" | "DEVICE_REPLACEMENT_PENDING" | "DEVICE_NOT_TRUSTED" | "DEVICE_DECLARATION_REFUSED" | "DEVICE_DECLARATION_REQUIRED" | "DEVICE_NOT_FOUND" | "MFA_REQUIRED" | "USER_NOT_FOUND" | "AVATAR_NOT_FOUND" | "DEVICE_KEY_IN_USE" | "DEVICE_LINK_INVALID" | "APP_NOT_FOUND" | "OAUTH_REQUEST_INVALID" | "CALENDAR_FEED_NOT_FOUND" | "EXPORT_LINK_INVALID" | "EXPORT_LINK_EXPIRED" | "DATABASE_ERROR" | "INTERNAL_ERROR";

export interface MeetingSpaceListResponse {
  items: MeetingSpaceResponse[];
  /** Present only on paginated requests (`limit` or `cursor` sent) when another page exists: pass it back as `cursor`. */
  nextCursor?: string | null;
}

export interface MeetingSpaceResponse {
  accessConfig?: AccessConfig | null;
  accessPolicy: AccessPolicy;
  /** (date-time) */
  archivedAt?: string | null;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  /** (date-time) */
  createdAt?: string | null;
  defaultRole?: Role | null;
  description?: string | null;
  /** (uuid) */
  id: string;
  /** (int32) */
  interactiveCapacity?: number | null;
  isLegacy: boolean;
  latestConference?: ConferenceSummary | null;
  meetingCode: string;
  meetingType: MeetingType;
  /** (uuid) */
  organizationId?: string | null;
  /** (uuid) */
  ownerUserId?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  requiresPasscode: boolean;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: MeetingsSecurityProfile | null;
  status: SpaceStatus;
  timezone?: string | null;
  title?: string | null;
  transcriptionPolicy?: MediaCapturePolicy | null;
  /** (date-time) */
  updatedAt?: string | null;
  /** (int32) */
  version?: number | null;
  view: ResourceView;
  /** (int32) */
  viewerCapacity?: number | null;
  viewerRole?: Role | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}

export type MeetingsSecurityProfile = "STANDARD" | "BUSINESS" | "ENTERPRISE" | "HEALTHCARE" | "HIGH_SECURITY";

export interface MeetingSurfaceView {
  /** Origins the panel's messaging bridge accepts. */
  origins: string[];
  /** The iframe URL (`https://`). */
  url: string;
}

/** One request-validation problem. */
export interface MeetingsValidationIssue {
  /** Stable, machine-readable rule code, e.g. `length`, `email`, `regex`, `range`. */
  code: string;
  /** Public JSON path of the offending field, as sent by the client (honours the request's serde naming), e.g. `email`, `displayName`, `items[0].email`. `null` when the problem concerns the request as a whole. */
  field: string | null;
  /** Safe, human-readable message for this field. */
  message: string;
}

export type MeetingType = "STANDARD" | "HEALTHCARE" | "WEBINAR" | "TOWN_HALL" | "TRAINING";

export interface Member {
  principalId: MessagingUuid;
  role: MemberRole;
  state: "ACTIVE" | "INVITED" | "REMOVED" | "LEFT";
  /** (date-time) */
  joinedAt: string;
  sendRestrictedUntil: string | null;
  /** (minimum: 1) */
  version: number;
  /** How the membership came to be: an administrator or the application backend (API), an accepted invitation, the meeting integration, an invite link (INVITE_LINK) or an organization team snapshot (ORGANIZATION_TEAM, G7). */
  authoritySource: "API" | "INVITATION" | "MEETING" | "INVITE_LINK" | "ORGANIZATION_TEAM";
  invitedByPrincipalId: MessagingUuid | null;
  invitationExpiresAt: string | null;
}

export interface MemberPage {
  members: Member[];
  /** Opaque keyset cursor of the next page; null on the last page. */
  nextCursor: string | null;
}

/** §30: OWNER (the owner), ADMIN, MODERATOR (moderates MEMBER / READ_ONLY / BOT_SERVICE members: remove, ban, unban; no role or ownership administration), MEMBER, READ_ONLY (reads its history window, sends no messages: READ_ONLY_MEMBER), BOT_SERVICE (BOT_OR_SERVICE principals only). */
export type MemberRole = "OWNER" | "ADMIN" | "MODERATOR" | "MEMBER" | "READ_ONLY" | "BOT_SERVICE";

/** A message. Content (ciphertext or MANAGED text) only while ACTIVE. e2ee for PRIVATE_E2EE rows, managed for MANAGED rows; the acceptance digest binds the server sequence and id to the ciphertext (cfmeet-messaging-acceptance:v2). */
export interface Message {
  messageId: MessagingUuid;
  conversationId: MessagingUuid;
  /** (minimum: 1) */
  sequence: number;
  kind: MessageKind;
  securityProfile: SecurityProfile;
  senderPrincipalId: MessagingUuid;
  senderDeviceId?: MessagingUuid | null;
  clientMessageId: MessagingUuid;
  targetMessageId?: MessagingUuid | null;
  lifecycle: MessageLifecycle;
  lifecycleReason?: string | null;
  /** (minimum: 0) */
  editCount?: number;
  editedAt?: string | null;
  /** (date-time) */
  createdAt: string;
  attachmentIds: MessagingUuid[];
  /** The protected envelope (Appendix H.1): version, protocol, cipher suite and the scope the client bound into the AAD; the server stores and relays the ciphertext without reading it. */
  e2ee?: {
    /** (minimum: 0) */
    epoch: number;
    ciphertext: string | null;
    /** (pattern: "^[0-9a-f]{64}$") */
    ciphertextSha256: string;
    acceptanceDigest: string | null;
    envelopeVersion: 1;
    protocol: "MLS_1_0";
    cipherSuite: "MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519";
    tenantId: MessagingUuid;
    applicationId: MessagingUuid;
    environmentId: MessagingUuid;
    /** S2: the sender's franking commitment (HMAC-SHA256, see SendMessageRequest.frankingCommitment). */
    frankingCommitment?: string | null;
  };
  managed?: {
    text: string | null;
    presentation?: "STANDARD" | "ANNOUNCEMENT";
  };
  /** (minimum: 1) */
  mlsEpoch?: number;
  /** NATIVE: created through Messaging. LEGACY_MANAGED: imported from a legacy chat store (MANAGED only, never presented as end-to-end encrypted). */
  securityProvenance?: "NATIVE" | "LEGACY_MANAGED";
  originalCreatedAt?: string | null;
  /** IN_APP, or SMS: arrived through the SMS Bridge (MANAGED only; never end-to-end encrypted). */
  channel?: "IN_APP" | "SMS";
  /** The message this one replies to (same conversation, within the sender's history window). */
  replyToMessageId?: MessagingUuid | null;
  /** The root of the reply thread (the first message replied to). */
  threadRootId?: MessagingUuid | null;
  /** S11: a server-side forward (the source conversation and message are never disclosed). */
  forwarded?: boolean;
  /** S11: the original sender, only when the policy shows it (messages.forwardShowsOriginalSender). */
  forwardedFromPrincipalId?: MessagingUuid | null;
  /** ADR 0064: the CF Meet App (OAuth client id = application id) whose OAuth access or bot credential sent this message; absent on every other message. */
  viaApplicationId?: MessagingUuid;
}

export interface MessageDeletion {
  messageId: MessagingUuid;
  deletion: "DELETED" | "HIDDEN_FOR_YOU" | "MODERATED" | "EXPIRED" | "PURGED";
  /** (date-time) */
  purgeAfter?: string;
}

export type MessageKind = "APPLICATION" | "MLS_COMMIT" | "EDIT";

/** ACTIVE carries content; every other state is a tombstone without content. HIDDEN_FOR_YOU: deleted on the reader's own devices only. */
export type MessageLifecycle = "ACTIVE" | "DELETED" | "MODERATED" | "EXPIRED" | "PURGED" | "HIDDEN_FOR_YOU";

export interface MessagePage {
  messages: Message[];
  nextAfterSequence: number | null;
  nextBeforeSequence?: number | null;
}

/** Error envelope. Messages never contain key material, ciphertext, content, a credential or database text. */
export interface MessagingError {
  error: {
    code: MessagingErrorCode;
    /** (maxLength: 300) */
    message: string;
    requestId: MessagingRequestId | null;
    /** (pattern: "^[A-Z][A-Z0-9_]{1,63}$") */
    reason?: string;
  };
}

export type MessagingErrorCode = "VALIDATION_ERROR" | "AUTHENTICATION_FAILED" | "CLIENT_TOKENS_NOT_ENABLED" | "AUTHORITY_SUSPENDED" | "INSUFFICIENT_SCOPE" | "ACTING_PRINCIPAL_REQUIRED" | "PRINCIPAL_NOT_AUTHORIZED" | "NOT_FOUND" | "METHOD_NOT_ALLOWED" | "PAYLOAD_TOO_LARGE" | "RATE_LIMITED" | "INTERNAL_AUTHENTICATION_FAILED" | "INTERNAL_PERMISSION_DENIED" | "DELEGATED_CONTEXT_REQUIRED" | "DELEGATED_CONTEXT_INVALID" | "AUTHORIZATION_REVOKED" | "FEATURE_DISABLED" | "CHALLENGE_INVALID" | "KEY_PACKAGE_REJECTED" | "DEVICE_REVOKED" | "DEVICE_LIMIT_REACHED" | "KEY_PACKAGE_LIMIT_REACHED" | "IDEMPOTENCY_KEY_REQUIRED" | "IDEMPOTENCY_KEY_REUSED" | "CONTEXT_NOT_ALLOWED" | "SECURITY_PROFILE_NOT_ALLOWED" | "SECURITY_PROFILE_IMMUTABLE" | "E2EE_NOT_AVAILABLE" | "VERSION_CONFLICT" | "INVALID_TRANSITION" | "CONVERSATION_NOT_ACTIVE" | "POLICY_VERSION_CONFLICT" | "CONFLICT" | "SERVICE_UNAVAILABLE" | "INTERNAL_ERROR" | "EPOCH_MISMATCH" | "MLS_REKEY_REQUIRED" | "SEND_RESTRICTED" | "DELIVERY_REFUSED" | "CONTENT_REJECTED" | "MESSAGE_NOT_MODIFIABLE" | "QUOTA_EXCEEDED" | "ATTACHMENT_NOT_READY" | "INTEGRITY_CHECK_FAILED" | "LOCAL_SEARCH_ONLY" | "E2EE_EXPORT_NOT_AVAILABLE" | "LEGAL_HOLD_ACTIVE" | "STORAGE_UNAVAILABLE";

/** Server-generated identifier of one request / one service hop. */
export type MessagingRequestId = string;

/** Lower-case hyphenated UUID. */
export type MessagingUuid = string;

export interface ModerationResponse {
  /** (uuid) */
  banId?: string | null;
  participant: ParticipantResponse;
  providerEnforcement: ProviderEnforcement;
}

export interface OauthAppSummary {
  /** (uuid) */
  clientId: string;
  developerName?: string | null;
  /** `PRIVATE` or `PUBLIC`. */
  distribution?: string | null;
  name: string;
  /** `DRAFT`, `APPROVED`, `PUBLISHED` (or `SUSPENDED`, never authorizable). */
  status?: string | null;
}

/** RFC 6749 §5.2 / RFC 6750 §3.1 error (fixed descriptions). */
export interface OAuthError {
  error: "invalid_request" | "invalid_client" | "invalid_grant" | "unauthorized_client" | "unsupported_grant_type" | "invalid_scope" | "invalid_token" | "insufficient_scope" | "temporarily_unavailable" | "slow_down";
  error_description: string;
}

/** RFC 8414-style authorization server metadata. */
export interface OAuthMetadata {
  /** (maxLength: 2048; pattern: "^https://") */
  issuer: string;
  /** (maxLength: 2048; pattern: "^https://") */
  authorization_endpoint: string;
  /** (maxLength: 2048; pattern: "^https://") */
  token_endpoint: string;
  /** (maxLength: 2048; pattern: "^https://") */
  revocation_endpoint: string;
  /** (maxLength: 2048; pattern: "^https://") */
  userinfo_endpoint: string;
  scopes_supported: ("profile:read" | "profile:email" | "meetings:read" | "meetings:write" | "chat:read" | "chat:write" | "events:meetings" | "chat:read:bot" | "chat:write:bot")[];
  response_types_supported: ["code"];
  grant_types_supported: ["authorization_code", "refresh_token"];
  code_challenge_methods_supported: ["S256"];
  token_endpoint_auth_methods_supported: ["client_secret_basic", "client_secret_post"];
  revocation_endpoint_auth_methods_supported: ["client_secret_basic", "client_secret_post"];
}

/** RFC 7009 revocation request (application/x-www-form-urlencoded; same client authentication as the token endpoint). */
export interface OAuthRevokeRequest {
  token: string;
  token_type_hint?: string;
  client_id?: string;
  client_secret?: string;
}

export interface OauthScopeView {
  description?: string | null;
  name: string;
  /** `user` or `bot`. */
  who?: string | null;
}

/** RFC 6749 token response (snake_case). bot_token only on the first authorization-code exchange of an ORGANIZATION install with bot scopes. */
export interface OAuthToken {
  /** (pattern: "^cfoa_") */
  access_token: string;
  token_type: "Bearer";
  /** (minimum: 1) */
  expires_in: number;
  /** (pattern: "^cfor_") */
  refresh_token?: string;
  scope?: string;
  installation?: {
    id: string;
    target: "USER" | "ORGANIZATION";
    organization_id?: string;
  };
  /** (pattern: "^cfob_") */
  bot_token?: string;
}

/** RFC 6749 token request (application/x-www-form-urlencoded). Client authentication: HTTP Basic (client_secret_basic) or client_id + client_secret in the body (client_secret_post), never both. PKCE (code_verifier, S256) is mandatory for authorization_code. */
export interface OAuthTokenRequest {
  grant_type: "authorization_code" | "refresh_token";
  code?: string;
  redirect_uri?: string;
  /** (minLength: 43; maxLength: 128) */
  code_verifier?: string;
  refresh_token?: string;
  client_id?: string;
  client_secret?: string;
}

/** The signed-in user's profile for an app: sub is pairwise per app; email only with profile:email. */
export interface OAuthUserinfo {
  /** (minLength: 1) */
  sub: string;
  name: string | null;
  picture: string | null;
  email?: string;
}

/** The organization's install decision for a workspace install (Organizations policy). */
export interface OrganizationInstallDecision {
  allowed: boolean;
  /** `NOT_A_MEMBER`, `ROLE_REQUIRED`, `NOT_ALLOWLISTED` or `ORGANIZATION_NOT_READY`. */
  reason?: string | null;
}

/** The outcome of a Call for ONE logical participant, derived from Call evidence only (participant, ring offers on every device, session connection intervals, the Call's state) — never from a push, a realtime signal or a client acknowledgement, and never stored as a flag. OUTBOUND: ANSWERED, NOT_ANSWERED (ended unanswered), IN_PROGRESS. INBOUND, first match wins: ANSWERED (an offer accepted on any of the participant's devices, or a session connected), DECLINED, ANSWERED_ELSEWHERE (the offers were … */
export type ParticipantOutcome = "ANSWERED" | "NOT_ANSWERED" | "MISSED" | "DECLINED" | "ANSWERED_ELSEWHERE" | "RINGING" | "NOT_REACHED" | "IN_PROGRESS";

export interface ParticipantResponse {
  admissionState: AdmissionState;
  /** (date-time) */
  admittedAt?: string | null;
  /** (uuid) */
  conferenceId: string;
  /** User-visible denial message (internal notes are never exposed). */
  denialMessage?: string | null;
  denialReasonCode?: string | null;
  displayName: string;
  /** (uuid) */
  id: string;
  identityType: IdentityType;
  /** (date-time) */
  requestedAt: string;
  role: Role;
  /** (int32) */
  version: number;
}

export interface ParticipantSessionResponse {
  /** (date-time) */
  expiresAt?: string | null;
  /** (uuid) */
  id: string;
}

/** A permalink is never authority: resolution checks current membership; a non-member and an unknown id are the same NOT_FOUND. */
export interface PermalinkResolution {
  resolution: MessageLifecycle;
  message: Message;
}

export type PrivateChatMode = "EVERYONE" | "HOSTS_ONLY" | "DISABLED";

export type Protocol = "MLS_1_0" | "CFMEET_MANAGED_V1";

/** Provider enforcement state for operations whose media effect is Part E. */
export type ProviderEnforcement = "PENDING_PROVIDER_CONTROL" | "ENFORCED" | "NOT_REQUIRED";

/** ADR 0063: push credential METADATA. The secret (APNs .p8 key / FCM service-account JSON) is sealed to the Notification Service on receipt and never stored, logged or returned. */
export interface PushCredential {
  credentialRef: Uuid;
  environmentId: Uuid;
  provider: "APNS" | "FCM";
  /** APNs VoIP (PushKit) key. */
  voip: boolean;
  teamId: string | null;
  keyId: string | null;
  bundleId: string | null;
  apnsHost: "SANDBOX" | "PRODUCTION" | null;
  projectId: string | null;
  /** SHA-256 of the uploaded secret (hex). (pattern: "^[0-9a-f]{64}$") */
  fingerprint: string;
  status: PushCredentialStatus;
  lastVerifiedAt: string | null;
  lastError: string | null;
  /** (date-time) */
  createdAt: string;
  /** (date-time) */
  updatedAt: string;
}

export interface PushCredentialList {
  /** (maxItems: 3) */
  items: PushCredential[];
  nextCursor: null;
}

export type PushCredentialStatus = "UNVERIFIED" | "VERIFIED" | "FAILED";

/** The complete list (replaces it): exact `scheme://host[:port]`, lowercase host, no path, no wildcard, no explicit default port; https only, except http://localhost and http://127.0.0.1 in SANDBOX / TEST. */
export interface PutAllowedOriginsRequest {
  /** (maxItems: 20) */
  origins: string[];
}

/** PUT /v1/projects/{projectId}/app: the manifest (http://localhost redirect URIs only while DRAFT; at least one user or bot scope; event types from the install-routable families). */
export interface PutAppRequest {
  /** (minLength: 1; maxLength: 120) */
  name: string;
  description?: string | null;
  logoUrl?: string | null;
  privacyUrl?: string | null;
  termsUrl?: string | null;
  /** (maxItems: 10) */
  redirectUris: string[];
  userScopes?: ("profile:read" | "profile:email" | "meetings:read" | "meetings:write" | "chat:read" | "chat:write" | "events:meetings")[];
  botScopes?: ("chat:read:bot" | "chat:write:bot")[];
  /** (maxItems: 64) */
  eventTypes?: ("app.grant_revoked" | "app.installed" | "app.uninstalled" | "artifact.deleted" | "artifact.ready" | "bot.mentioned" | "bot.message_received" | "conversation.created" | "conversation.member_added" | "conversation.member_removed" | "conversation.updated" | "meeting.created" | "meeting.ended" | "meeting.started" | "message.created" | "message.deleted" | "message.expired" | "message.moderated" | "participant.joined" | "participant.left")[];
  meetingSurface?: {
    /** (maxLength: 2048; pattern: "^https://") */
    url: string;
    /** (maxItems: 10) */
    origins?: string[];
  } | null;
  distribution?: "PRIVATE" | "PUBLIC";
}

/** ADR 0064: the draft test accounts (CF Meet account ids, format checked only, at most 25). */
export interface PutAppTestAccountsRequest {
  /** (maxItems: 25) */
  accountIds: Uuid[];
}

/** WAITING / OFFERING are active (OFFERING: one agent is being offered the Call); ANSWERED, ABANDONED (the caller hung up), TIMED_OUT (maximum wait, no overflow), OVERFLOWED (sent to the overflow destination) and CANCELLED (the Call ended otherwise) are final. */
export type QueueEntryState = "WAITING" | "OFFERING" | "ANSWERED" | "ABANDONED" | "TIMED_OUT" | "OVERFLOWED" | "CANCELLED";

export interface ReactionChange {
  messageId: MessagingUuid;
  /** An emoji sequence or a :short_code:. (minLength: 1; maxLength: 64) */
  reaction: string;
  reacted: boolean;
  changed: boolean;
}

export interface ReactionSummary {
  messageId: MessagingUuid;
  reactions: {
    /** An emoji sequence or a :short_code:. (minLength: 1; maxLength: 64) */
    reaction: string;
    /** (minimum: 1) */
    count: number;
    reactedByMe: boolean;
  }[];
}

export interface ReadCursor {
  conversationId: MessagingUuid;
  /** (minimum: 0) */
  readSequence: number;
}

export interface ReadCursorRequest {
  /** (minimum: 0) */
  sequence: number;
}

/** Single-use, seconds-long WebSocket ticket, presented as a Sec-WebSocket-Protocol entry (never in the URL). */
export interface RealtimeTicket {
  /** (pattern: "^[0-9a-f]{64}$") */
  ticket: string;
  /** (date-time) */
  expiresAt: string;
  path: "/v1/realtime";
  subprotocol: "cfmeet.messaging.v1";
  ticketSubprotocolPrefix: "cfmeet.ticket.";
}

/** An offer of the endpoint as recovered after a reconnect, with the authoritative Call state. status is PENDING until this device acknowledges receipt (POST /v1/calling/invitations/{invitation_id}/acknowledge), then RINGING. actionable is true only for a live (PENDING or RINGING), unexpired offer of a Call that can still be answered; a live offer of an ending or terminal Call is reported CANCELLED. */
export interface RecoveredInvitation {
  invitationId: CallingUuid;
  callId: CallingUuid;
  callKind: CallKind;
  callState: CallState;
  /** (minimum: 1) */
  callVersion: number;
  purpose: CallInvitationPurpose;
  status: CallInvitationStatus;
  actionable: boolean;
  /** (date-time) */
  issuedAt: string;
  /** (date-time) */
  expiresAt: string;
  terminalReason: CallInvitationTerminalReason | null;
  sessionId: CallingUuid | null;
  /** The Call initiator's display name (shown on the incoming call), or null when the Call was created without one. 1.6.0. */
  callerDisplayName: string | null;
}

/** Response of GET /v1/calling/invitations (signaling credential): the endpoint's live and recently finished offers. */
export interface RecoveredInvitationList {
  endpointId: CallingUuid;
  data: RecoveredInvitation[];
}

export interface RemoveParticipantRequest {
  reason?: string | null;
}

/** What the reporter learns: that the report was received. The reported principal is never told who reported. */
export interface ReportReceipt {
  reportId: MessagingUuid;
  status: "OPEN";
  category: "SPAM" | "HARASSMENT" | "HATE" | "VIOLENCE" | "SEXUAL_CONTENT" | "CHILD_SAFETY" | "SELF_HARM" | "ILLEGAL_CONTENT" | "IMPERSONATION" | "OTHER";
  evidenceKind: "NONE" | "MANAGED_REFERENCE" | "REPORTER_DISCLOSURE";
  /** (date-time) */
  createdAt: string;
}

export interface ReportRequest {
  category: "SPAM" | "HARASSMENT" | "HATE" | "VIOLENCE" | "SEXUAL_CONTENT" | "CHILD_SAFETY" | "SELF_HARM" | "ILLEGAL_CONTENT" | "IMPERSONATION" | "OTHER";
  /** (maxItems: 20) */
  messageIds?: MessagingUuid[];
  reportedPrincipalId?: MessagingUuid;
  /** (maxLength: 2000) */
  note?: string;
  /** PRIVATE_E2EE only and optional: what the reporter's own device decrypted, disclosed voluntarily (sealed at rest). Unverifiable unless the message was franked and the disclosure carries its frankingKey (S2). (maxItems: 20) */
  disclosure?: {
    messageId: MessagingUuid;
    /** (maxLength: 4000) */
    text: string;
    /** S2: the message's franking key the reporter's device decrypted (unpadded base64url, 32 bytes); with it the disclosure is verified against the sender's commitment. (pattern: "^[A-Za-z0-9_-]{43}$") */
    frankingKey?: string;
  }[];
}

/** Server-generated identifier of one request / one service hop. */
export type RequestId = string;

export type ResourceView = "FULL" | "SUMMARY";

export interface RevokeSessionsResult {
  /** Tokens issued before this instant (and every device credential derived from them) no longer work. (date-time) */
  tokensNotBefore: string;
}

export type Role = "OWNER" | "HOST" | "CO_HOST" | "MODERATOR" | "PRESENTER" | "PARTICIPANT" | "VIEWER";

/** Controlled per-space / organization overrides of POLICY cells only. */
export interface RolePolicy {
  /** role → permission key → allowed. Only POLICY cells may be overridden. */
  overrides?: Record<string, Record<string, boolean>>;
}

export interface RtcCredentialResponse {
  /** Short-lived provider credential. */
  credential: string;
  /** (date-time) */
  expiresAt: string;
  provider: string;
  url: string;
}

export type RtcStatus = "ISSUED" | "NOT_ISSUED_WAITING" | "PROVIDER_CONTROL_DISABLED";

/** Start-window / late-start / auto-end policy (minutes). */
export interface SchedulePolicy {
  autoEnd?: boolean;
  /** (int32; minimum: 0) */
  autoEndGraceMinutes?: number;
  /** (int32; minimum: 0) */
  earlyStartMinutes?: number;
  /** (int32; minimum: 0) */
  lateStartGraceMinutes?: number;
}

/** Who may publish a screen share (on top of the `screenshare.publish` role grant). */
export type ScreenSharePolicy = "HOST_ONLY" | "PRESENTERS_ONLY" | "ALL_INTERACTIVE";

/** Server search covers MANAGED conversations only; PRIVATE_E2EE conversations are searched on devices (LOCAL_SEARCH_ONLY). */
export interface SearchResults {
  results: Message[];
  searchedProfile: "MANAGED";
  nextBeforeSequence?: number | null;
  /** (minimum: 0) */
  localSearchOnlyConversations?: number;
}

/** PRIVATE_E2EE: client-side MLS 1.0 encryption, the server holds no decryption key. MANAGED: server-processable, never presented as E2EE. */
export type SecurityProfile = "PRIVATE_E2EE" | "MANAGED";

/** Which fields apply is decided by the conversation's PERSISTED security profile: PRIVATE_E2EE needs deviceId, epoch and ciphertext and refuses text (CONTENT_REJECTED / PLAINTEXT_IN_E2EE); MANAGED needs text and refuses ciphertext. */
export interface SendMessageRequest {
  clientMessageId: MessagingUuid;
  deviceId?: MessagingUuid;
  /** (minimum: 0; maximum: 9007199254740992) */
  epoch?: number;
  /** PRIVATE_E2EE: the MLS PrivateMessage (MLSMessage bytes, base64url). Never plaintext. (pattern: "^[A-Za-z0-9_-]*$") */
  ciphertext?: string;
  /** MANAGED plain text (UTF-8, 1-16384 characters, no control characters). No markup subset is defined: the server never interprets HTML, Markdown or scripts, stores the text exactly as given and returns it as a JSON string; clients render it as text (escaping it), never as markup. (minLength: 1; maxLength: 16384) */
  text?: string;
  kind?: "APPLICATION" | "EDIT";
  targetMessageId?: MessagingUuid;
  /** (maxItems: 100) */
  attachmentIds?: MessagingUuid[];
  /** false: no push notification jobs (mirrored / migrated writes). Default true. */
  notify?: boolean;
  /** MANAGED only (refused for PRIVATE_E2EE, whose presentation travels inside the ciphertext). */
  managedPresentation?: "STANDARD" | "ANNOUNCEMENT";
  replyToMessageId?: MessagingUuid;
  /** Section 13: current ACTIVE members this message mentions (ids only; the sender is ignored; anyone else is VALIDATION_ERROR). Used only for the members' mention count and the "mentions only" notification preference. B3 (1.34.0): PRIVATE_E2EE accepts it too — only these ids reach the server, the … (maxItems: 50) */
  mentionedPrincipalIds?: MessagingUuid[];
  /** S2, PRIVATE_E2EE only (optional): hex(HMAC-SHA256(frankingKey, 'cfmeet-messaging-franking:v1' || 0x00 || conversationId || clientMessageId || senderPrincipalId (16 bytes each) || plaintext UTF-8)). The client draws a fresh 32-byte frankingKey per message and carries it INSIDE the MLS plaintext; … (pattern: "^[0-9a-f]{64}$") */
  frankingCommitment?: string;
}

export interface SkippedParticipant {
  admissionState: AdmissionState;
  /** (uuid) */
  participantId: string;
}

export type SpaceStatus = "ACTIVE" | "ARCHIVED" | "DELETED";

export type TerminationReason = "HOST_ENDED" | "SCHEDULED_END" | "ADMIN" | "FAILURE" | "EMPTY_TIMEOUT" | "CANCELLED" | "LEGACY";

export interface UnreadCount {
  conversationId: MessagingUuid;
  /** (minimum: 0) */
  unreadCount: number;
  /** (minimum: 0) */
  lastSequence: number;
  /** (minimum: 0) */
  readSequence: number;
}

/** Optimistic concurrency on the aggregate version. EXPIRED is set by the server only. A security profile can never be changed (SECURITY_PROFILE_IMMUTABLE). */
export interface UpdateConversationRequest {
  /** (minimum: 1) */
  expectedVersion: number;
  lifecycleState?: "ACTIVE" | "LOCKED" | "CLOSED" | "ARCHIVED" | "DELETED";
  /** (pattern: "^[A-Z][A-Z0-9_]{1,47}$") */
  reason?: string;
  /** (date-time) */
  expiresAt?: string;
  /** §90 / disappearing messages: a conversation-specific retention, never looser than the context's (or environment's) retention policy (RETENTION_LOOSER_THAN_POLICY). The retention worker expires older messages (tombstone, then purge unless a legal hold protects them); clients delete locally on the … */
  retentionSeconds?: number | null;
  /** Section 13: a new display title; null clears it (MANAGED only, owner). */
  title?: string | null;
  /** Section 13: a new avatar reference; null clears it (MANAGED only, owner). */
  avatarRef?: string | null;
  /** S13 (owner): announcement-only on / off; GROUP and CHANNEL only. */
  announcementOnly?: boolean;
}

/** Profile changes (`null` clears) and suspension / reactivation; deletion is DELETE. */
export interface UpdateEndUserRequest {
  displayName?: string | null;
  imageUrl?: string | null;
  status?: "ACTIVE" | "SUSPENDED";
}

export interface UpdateMeetingSpaceRequest {
  accessConfig?: AccessConfig | null;
  accessPolicy?: AccessPolicy | null;
  clearPasscode?: boolean;
  collaborationPolicy?: CollaborationPolicyPatch | null;
  defaultRole?: Role | null;
  description?: string | null;
  /** (int32) */
  interactiveCapacity?: number | null;
  meetingType?: MeetingType | null;
  passcode?: string | null;
  recordingPolicy?: MediaCapturePolicy | null;
  retentionPolicyRef?: string | null;
  rolePolicy?: RolePolicy | null;
  schedulePolicy?: SchedulePolicy | null;
  securityProfile?: MeetingsSecurityProfile | null;
  timezone?: string | null;
  title?: string | null;
  transcriptionPolicy?: MediaCapturePolicy | null;
  /** Optimistic concurrency version the client last read. (int32) */
  version: number;
  /** (int32) */
  viewerCapacity?: number | null;
  waitingRoomPolicy?: WaitingRoomPolicy | null;
}

/** Creates the user if new (idempotent on externalId), then mints a token. */
export interface UpsertUserTokenRequest {
  /** The partner's opaque user id (printable ASCII, no spaces); never a phone number, tel: / sip: URI. (minLength: 1; maxLength: 128; pattern: "^[!-~]{1,128}$") */
  externalId: string;
  /** Display name (no control characters). (minLength: 1; maxLength: 64) */
  displayName?: string;
  /** https:// image URL. (maxLength: 512; pattern: "^https://") */
  imageUrl?: string;
  grants: UserTokenGrants;
  /** Token lifetime in seconds (default 3600). (minimum: 60; maximum: 86400) */
  ttlSeconds?: number;
}

/** ADR 0063: a partner push registration as the Notification Service accepted it (never the handle). */
export interface UserPushRegistration {
  /** Stable per app installation, generated by the SDK. (minLength: 8; maxLength: 128; pattern: "^[A-Za-z0-9._-]{8,128}$") */
  installationId: string;
  platform: "APNS" | "APNS_VOIP" | "FCM";
  status: "ACTIVE";
}

/** ADR 0063: a partner device's push handle (an APNs / APNs VoIP device token or an FCM registration token). Synchronised to the Notification Service; Messaging never stores, echoes or logs it. */
export interface UserPushRegistrationRequest {
  platform: "APNS" | "APNS_VOIP" | "FCM";
  /** (minLength: 1; maxLength: 4096) */
  handle: string;
}

/** ADR 0062: a MANAGED-only partner session device and a short-lived client token bound to it (scopes: the user token's effective msg grants; read -> messages.read / attachments.read, send -> messages.send / attachments.write). Present it as `Authorization: CF-Client-Token <clientToken>` to POST /v1/realtime/tickets and to the partner REST allowlist; renew through this exchange. */
export interface UserSession {
  principalId: MessagingUuid;
  sessionDeviceId: MessagingUuid;
  /** (date-time) */
  sessionDeviceExpiresAt: string;
  /** (maxLength: 2048) */
  clientToken: string;
  /** (date-time) */
  clientTokenExpiresAt: string;
}

/** ADR 0062: the body of POST /v1/messaging/user-sessions (Authorization: Bearer cfut_...). */
export interface UserSessionRequest {
  /** Stable per app installation, generated by the SDK. (minLength: 8; maxLength: 128; pattern: "^[A-Za-z0-9._-]{8,128}$") */
  installationId: string;
  /** Renew this ACTIVE partner session device of the same principal and installation (NOT_FOUND otherwise). */
  sessionDeviceId?: MessagingUuid;
}

export interface UserToken {
  /** The CF Meet user token, shown ONCE (`Authorization: Bearer cfut_…` on the device). (maxLength: 2048; pattern: "^cfut_(sbx|test|live)_") */
  token?: string;
  /** An Idempotency-Key replay: the token is never returned again. */
  tokenRedacted?: true;
  tokenId: Uuid;
  /** (date-time) */
  expiresAt: string;
}

/** ADR 0062 user token grants; effective grants are re-intersected with the minting credential's CURRENT scopes on every verification. */
export interface UserTokenGrants {
  /** Chat: needs `messages.read` / `messages.send` on the minting credential. (maxItems: 2) */
  msg?: ("read" | "send")[];
  /** Receive calls: needs `calls.write`. (maxItems: 1) */
  call?: "receive"[];
  /** Meetings the user may join, with a role: needs `meetings.write`. The token's compact `{c, r}` form is accepted too. (maxItems: 10) */
  mtg?: ({
    conferenceId: Uuid;
    role: "HOST" | "CO_HOST" | "PRESENTER" | "PARTICIPANT" | "VIEWER";
  } | {
    c: Uuid;
    r: "HOST" | "CO_HOST" | "PRESENTER" | "PARTICIPANT" | "VIEWER";
  })[];
}

export interface UserWithToken {
  user: EndUser;
  /** The CF Meet user token, shown ONCE (`Authorization: Bearer cfut_…` on the device). (maxLength: 2048; pattern: "^cfut_(sbx|test|live)_") */
  token?: string;
  tokenRedacted?: true;
  tokenId: Uuid;
  /** (date-time) */
  expiresAt: string;
}

/** Lower-case hyphenated UUID. */
export type Uuid = string;

/** One request-validation problem. */
export interface ValidationIssue {
  /** Stable, machine-readable rule code, e.g. `length`, `email`, `regex`, `range`. */
  code: string;
  /** Public JSON path of the offending field, as sent by the client (honours the request's serde naming), e.g. `email`, `displayName`, `items[0].email`. `null` when the problem concerns the request as a whole. */
  field: string | null;
  /** Safe, human-readable message for this field. */
  message: string;
}

export interface VersionedRequest {
  /** (int32) */
  version?: number | null;
}

export type WaitingRoomPolicy = "OFF" | "EVERYONE" | "GUESTS_ONLY" | "UNAUTHENTICATED_ONLY" | "NON_MEMBERS_ONLY" | "EXTERNAL_ONLY" | "CUSTOM";

export interface WaitingRoomResponse {
  /** (uuid) */
  conferenceId: string;
  participants: ParticipantResponse[];
}
```
