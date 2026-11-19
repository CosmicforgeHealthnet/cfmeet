# CF Meet Platform (partner embedding and CF Meet Apps): backend handover to the frontend team

**Status:** the backend for the CF Meet Platform is built: partner users, user tokens, partner
chat, calls, push, meetings, OAuth apps, installs, the app directory and in-meeting apps. Every
feature is behind a default-off flag and is switched on in production one step at a time by a
journaled activation. Nothing here changes how the existing web or mobile apps behave today.
The backend team made no change to web or mobile source, UI or frontend tests. The pages below
belong to the frontend team.

Decisions: ADR 0061–0065 (`docs/decisions/`). Plan: `docs/platform/cf-meet-platform-integration-plan.md`.

## 1. Regenerate the generated types first

`apps/web/src/types/generated/meeting-service.ts` (and the mobile copy, if any) is stale. Regenerate
it from `packages/contracts/openapi/meeting-service.json`. The new operations are under the tag
**Apps (v2)**. The Developer Console contract is
`packages/contracts/platform-developer/http/v1.openapi.json` (1.8.0).

## 2. Developer Console pages (console, signed-in developer)

All routes are on the Developer Console API. They use the developer's existing console session and
are listed in the public route allow-list. Field shapes are in the Developer Console contract.

| Page | Endpoints | Notes |
|---|---|---|
| **Users** (per environment) | `GET /v1/environments/{environmentId}/users` | Read-only list of the partner's end users (display name, external id, status). Partners create users from their own server with `POST /v1/users`; the console doesn't need a create form. |
| **Test token generator** (SANDBOX only) | `POST /v1/environments/{environmentId}/test-user-tokens` | Body: `externalId`, `displayName`, `imageUrl`, `grants`, `ttlSeconds`. Creates the user if new and mints a `cfut_` token so a developer can try the SDK samples. Show `token` once, with a copy button and `expiresAt`; afterwards show only `tokenRedacted`. Other environment kinds answer `422 POLICY_VIOLATION`. |
| **Allowed websites** | `GET` / `PUT /v1/environments/{environmentId}/allowed-origins` | Exact `https://` origins, at most 20. `http://localhost` and `http://127.0.0.1` are accepted only in SANDBOX and TEST. `PUT` replaces the whole list. |
| **Push keys** | `GET` / `POST /v1/environments/{environmentId}/push-credentials`, `DELETE …/{credentialRef}`, `POST …/{credentialRef}/verify` | Upload APNs (incl. VoIP) and FCM keys. The key file is sent once and sealed to the Notification Service; the console only ever gets metadata back (`provider`, `voip`, `teamId`, `keyId`, `bundleId`, `apnsHost`, `projectId`, `fingerprint`, `status`, `lastVerifiedAt`, `lastError`). Never echo or cache the uploaded file. Message and call (VoIP) keys are separate entries. |
| **App** (one per project) | `GET` / `PUT /v1/projects/{projectId}/app`, `POST …/app/secret`, `POST …/app/submit`, `GET …/app/grants`, `PUT …/app/test-accounts` | App registration: `name`, `description`, `logoUrl`, `privacyUrl`, `termsUrl`, exact `redirectUris`, `userScopes`, `botScopes`, `eventTypes`, `distribution` (PRIVATE/PUBLIC) and an optional `meetingSurface` (`url` + `origins`). A changed manifest becomes a new immutable version. SUBMITTED, IN_REVIEW and SUSPENDED apps are locked. Human console session only. `POST …/secret` returns the client secret **once** (`cfcs_…`); show it once with a warning. `submit` sends the app to review. `grants` lists installs. `test-accounts` names the CF Meet accounts allowed to install a DRAFT app. |

**Review has no frontend page.** Named reviewers approve or reject apps through the operator CLI
(ADR 0065). The console only shows the app's status: DRAFT, SUBMITTED, IN_REVIEW, APPROVED,
REJECTED, PUBLISHED or SUSPENDED, and the rejection reason when present.

## 3. CF Meet web app pages (signed-in CF Meet user, `/api/v2`, Account bearer session)

### 3a. OAuth consent page: `https://cfmeet.com/oauth/authorize`

Apps send users to this URL with the standard OAuth query (`client_id`, `redirect_uri`, `scope`,
`state`, `code_challenge`, `code_challenge_method=S256`, optional `install=organization` and
`organization_id`).

1. If the user isn't signed in, sign them in first, then come back with the query unchanged.
2. Forward the query **unchanged** (snake_case) to `GET /api/v2/oauth/authorize/context`. The response
   has the app (name, developer, distribution, status), the scopes with descriptions, the target
   (`USER` or `ORGANIZATION`) and, for a workspace install, `organizationInstall.allowed` and `reason`
   (`NOT_A_MEMBER`, `ROLE_REQUIRED`, `NOT_ALLOWLISTED`, `ORGANIZATION_NOT_READY`).
3. On an error response (unknown app, bad redirect URI, missing PKCE), show an error page and
   **do not redirect**: the redirect URI is untrusted until the backend has validated it.
4. Show Approve and Deny. Disable Approve when `organizationInstall.allowed` is false and show
   the reason.
5. Send `POST /api/v2/oauth/authorize/approve` with the same values in camelCase plus
   `decision: "APPROVE" | "DENY"`. Navigate the browser to the returned `redirectTo`.

### 3b. Connected apps (account settings)

- `GET /api/v2/me/apps` lists the user's installs (name, developer, logo, scopes, target, created
  date, `hasMeetingSurface`).
- `DELETE /api/v2/me/apps/{grantId}` removes an install and revokes its tokens at once.

### 3c. Workspace admin: Apps (organization settings, admins only)

- `GET /api/v2/organizations/{id}/apps` lists the workspace's installs.
- `DELETE /api/v2/organizations/{id}/apps/{grantId}` removes one.
- Which apps a workspace allows (who may install, the allow-list) is stored in the Organizations
  policy `apps` section. **It isn't editable from the web app yet:**
  `/api/v2/organizations/{id}/policies` doesn't carry the `apps` section. Exposing it is a backend
  follow-up. Until then the page is list-and-remove only, and the default (admins only install)
  applies.

### 3d. App directory (public page, no sign-in)

- `GET /v1/app-directory` (via `api.cfmeet.com`, anonymous) lists PUBLISHED, public apps.
- `GET /v1/app-directory/{appId}` returns one.
- An entry has `name`, `description`, `logoUrl`, `developerName`, `userScopes`, `botScopes`,
  `privacyUrl`, `termsUrl`, `meetingSurface` and `publishedAt`.
- There is no install URL in the entry yet. An install starts on the app's own site, which sends
  the user to the consent page in 3a with its own PKCE values. For now the page shows the
  scopes and the privacy and terms links. A homepage / install URL field is a backend follow-up.

### 3e. In-meeting app panel (meeting room)

1. `GET /api/v2/conferences/{id}/apps` lists apps with a meeting surface installed for the meeting
   owner or the workspace: name, logo, `surface.url`, `surface.origins`, `installedFor`.
2. When the user opens one, call `POST /api/v2/conferences/{id}/apps/{appId}/context`. It returns a
   `contextToken` (Ed25519 JWS, 5 minutes) and `expiresIn`. Fetch a new one before it expires
   while the panel stays open.
3. Load `surface.url` in a **sandboxed iframe**: `sandbox="allow-scripts allow-forms allow-same-origin
   allow-popups"`, with no top navigation. Hand the token to the iframe with `postMessage`,
   addressed to the exact origin of `surface.url`. Accept messages back only from
   `surface.origins`.
4. The app verifies the token against `GET /api/v2/app-context-keys` (JWKS). The token carries the
   conference, a pairwise participant id and the role. It carries the user's principal only if
   the user installed the app.
5. The meeting page CSP must allow `frame-src` for the surface origins. A static allow-list won't
   work because the origins are per app, so the page's CSP has to be built per request or
   relaxed for this panel.

## 4. Mobile

- The CF Meet mobile app needs only 3b (Connected apps) and, later, 3e.
- Partner apps use the SDKs (`sdks/typescript`, `sdks/dart`, `sdks/react-native`). The native
  CallKit/PushKit and Android ConnectionService glue for partner ringing is still an open owner
  decision (plan, decision 10).

## 5. What stays off until production activation

- The pages above can be built and tested against local or staging stacks now.
- In production each one answers `404` / `FEATURE_DISABLED` until its activation runs (order: users,
  user tokens, chat, calls, push, meetings, OAuth apps, OAuth, delegated apps, installs, app events,
  directory, in-meeting apps).
- Approving apps for the directory also needs the owner to name app reviewers.
