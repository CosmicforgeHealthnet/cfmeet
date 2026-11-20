# CF Meet Apps ("Integrate"): shared contract for the parallel implementation (ADR 0064 / 0065)

Every service team (agent) builds against THIS document. If a shape is impossible, implement the
closest safe shape and report the difference. The Build conventions apply (see
`build-internal-contracts.md`):
* JSON is camelCase, except the OAuth wire format, which is snake_case per RFC 6749.
* Each service keeps its own error envelope.
* Everything is behind a default-off flag, and migrations are additive.

## 0. Model

* **App.** A Developer Console project (Platform Access application of the developer's EXTERNAL
  tenant) that has an **OAuth client** profile. `client_id` is the application id. The client
  secret uses the format `cfcs_<sbx|test|live>_<24hex>_<64hex>`, verifier-only, issued and rotated
  by Platform Access, shown once.
* **Grant (installation).** One of two kinds:
  * **USER**: an app × a CF Meet account. Its target is the account's personal FIRST_PARTY
    communications environment; its principal is `NATIVE_CF_USER` with reference
    `cfmeet-user:<accountId>`.
  * **ORGANIZATION**: an app × a CF Meet business organization. Its target is the organization's
    FIRST_PARTY triple from `platform_organization_scopes`; its principal is a new
    `BOT_OR_SERVICE` bot with reference `app:<appId>`.

  In both cases there is one installation service account per (app, target environment). Codes
  and tokens belong to the grant.
* **Tokens.** All are Platform Access credentials, verified live, with verifier-only storage:

  | Prefix | Kind | Life | Notes |
  |---|---|---|---|
  | `cfoa_<tag>_…` | access | 1 h | |
  | `cfor_<tag>_…` | refresh | 90 d | rotating, single use; presenting a superseded one revokes the whole grant |
  | `cfob_<tag>_…` | bot | until uninstall, rotatable | ORGANIZATION grants only |

  Every access and bot credential carries a **principal restriction** to exactly the user
  principal (or the bot principal). That restriction is enforced by the existing restriction
  machinery.
* **Acting app.** Product services learn that an app is acting from the credential itself.
  Platform Access authentication returns `credentialKind` (`OAUTH_ACCESS` | `OAUTH_BOT`), plus
  `appId` and `grantId`. **The `DelegatedPlatformContext` wire format does not change.** The
  gateway sets `initiating_credential_id` = the token id and `initiating_principal` = the
  restricted principal.
* **OAuth scopes** (closed catalogue in Platform Access `identity/oauth_scopes.rs`), mapped to
  internal scopes:

  | OAuth scope | Who | Internal scopes | Constraint |
  |---|---|---|---|
  | `profile:read` | user | — | userinfo: pairwise `sub`, display name, avatar |
  | `profile:email` | user | — | adds the email |
  | `meetings:read` | user | `meetings.read` | only meetings the user owns, or is host or co-host of |
  | `meetings:write` | user | `meetings.write` | only meetings the user owns |
  | `chat:read`, `chat:write` | user | `messages.read`, `messages.send` | MANAGED conversations the user is a member of |
  | `chat:read:bot`, `chat:write:bot` | bot | `messages.read`, `messages.send` | MANAGED conversations the bot is a member of |
  | `events:meetings` | install | — | meeting event delivery for the install target |

  `FIRST_PARTY_ONLY` and `PRIVILEGED_OPERATOR` internal scopes are never reachable. PRIVATE_E2EE
  content is never reachable.

## 1. Permissions (already in `packages/service-auth`, added by the integrator)

| Permission | Caller → receiver | Purpose |
|---|---|---|
| `platform-oauth.manage` | developer-platform → platform-access | OAuth client profile, secret issue and rotation, client status, list grants of an app |
| `platform-oauth.token` | developer-platform → platform-access | token, revoke, introspect |
| `platform-oauth.authorize` | meeting-service → platform-access | validate an authorization request, create the code and grant, list and revoke a user's or organization's grants |
| `organization-apps.evaluate` | meeting-service → organizations | install policy check for an organization |
| `meeting-oauth-userinfo.read` | developer-platform → meeting-service | profile for `/v1/oauth/userinfo` |

## 2. Platform Access (owner: Platform Access) — internal routes

### OAuth client profile (`platform-oauth.manage`)

* `PUT /internal/v1/oauth/clients/{applicationId}`
  * Body: `{name, redirectUris:[…], userScopes:[…], botScopes:[…], distribution:"PRIVATE|PUBLIC", status:"DRAFT|APPROVED|PUBLISHED|SUSPENDED", testAccountIds?:[uuid]}`.
  * Returns `{clientId, …same, secretIssuedAt?}`.
* `POST /internal/v1/oauth/clients/{applicationId}/secret` returns `201 {clientSecret, secretId, issuedAt}`. The secret is shown once. Rotating gives the old secret a 24 h overlap.
* `GET /internal/v1/oauth/clients/{applicationId}/grants?cursor` returns `{items:[grant]}`.

**Redirect URI rules:** exact `https://` URIs; `http://localhost[:port]/…` only while the app is
DRAFT; no fragments and no wildcards; at most 10.

**Distribution rules:**
* DRAFT: only `testAccountIds` (at most 25 CF Meet accounts) may authorize.
* PRIVATE: only the developer workspace's own members' accounts, plus organizations whose owner is
  one of those members.
* PUBLISHED (PUBLIC): anyone.
* SUSPENDED: nobody, and every grant is revoked.

### Authorization (`platform-oauth.authorize`, Meeting Service only)

* `POST /internal/v1/oauth/authorization-requests/validate`
  * Body: `{clientId, redirectUri, scope, codeChallenge, codeChallengeMethod, target:"USER|ORGANIZATION", organizationId?, accountId}`.
  * Returns `200 {app:{clientId, name, distribution, status, developerName}, scopes:[{name, description, who:"user|bot"}], target}`.
  * Fails with `400 {error:{code: INVALID_CLIENT | INVALID_REDIRECT_URI | INVALID_SCOPE | PKCE_REQUIRED | APP_NOT_AVAILABLE}}`.
  * `codeChallengeMethod` must be `S256`.
* `POST /internal/v1/oauth/authorizations`
  * Body: same fields plus `{decision:"APPROVE"}`.
  * Ensures the user principal, or the organization bot principal and installation service account, then creates or updates the grant (scopes accumulate for the same app and target).
  * Returns `201 {code, expiresIn:60, grantId}`. The code is single use, hash-only, and bound to client, redirect URI, PKCE challenge and grant.
* `GET /internal/v1/oauth/grants?accountId=…` or `?organizationId=…` returns `{items:[grant]}`.
* `DELETE /internal/v1/oauth/grants/{grantId}` revokes every token, deactivates the bot, and records a change-feed `APP_GRANT` entry with REVOKED.

### Token endpoint backend (`platform-oauth.token`, Developer Console only)

* `POST /internal/v1/oauth/token` with body `{clientId, clientSecret, grantType:"authorization_code|refresh_token", code?, redirectUri?, codeVerifier?, refreshToken?}`.
  * Returns `200 {accessToken, tokenType:"Bearer", expiresIn:3600, refreshToken, scope, grant:{id, target, organizationId?}, botToken?}`. `botToken` is present only on the first authorization-code exchange of an ORGANIZATION grant with bot scopes.
  * Errors follow RFC 6749 codes in `error.code`: `invalid_grant`, `invalid_client`, `unsupported_grant_type`, `invalid_request`.
  * Reusing a code revokes the grant. Reusing a superseded refresh token revokes the grant.
* `POST /internal/v1/oauth/revoke` with body `{clientId, clientSecret, token}` always returns 200.
* `POST /internal/v1/oauth/introspect` with body `{token}` returns `{active, grantId, appId, accountId?, organizationId?, principalId, scopes, sub}`. `sub` is pairwise: `HMAC(app, accountId)`, base64url.

### Authentication

The existing `/internal/v1/credentials/authenticate` also accepts `cfoa_` and `cfob_` tokens,
returning:
* the authority chain (the target FIRST_PARTY triple and installation service account);
* scopes = the internal scopes mapped from the grant;
* the principal restriction;
* `credentialKind`, `appId` and `grantId`.

`cfor_` tokens are never accepted there.

### Other

* **Change feed:** entity `APP_GRANT` (id, appId, target, organizationId, accountId, status, scopes).
* **Flag:** `PLATFORM_ACCESS_OAUTH_ENABLED`.

## 3. Meeting Service (owner: Meeting Service) — account authority and delegated actor

**First-party direct ingress, account session required:**
* `GET /api/v2/oauth/authorize/context?client_id&redirect_uri&scope&state&code_challenge&code_challenge_method[&install=organization&organization_id]` relays to §2 validate (with the signed-in `accountId`). It returns the consent data for the frontend.
* `POST /api/v2/oauth/authorize/approve`
  * Body: `{clientId, redirectUri, scope, state, codeChallenge, codeChallengeMethod, install?, organizationId?, decision:"APPROVE|DENY"}`.
  * Returns `{redirectTo}`, which is either `redirect_uri?code=…&state=…` or `?error=access_denied&state=…`.
  * Organization installs first call Organizations §4 (`organization-apps.evaluate`) for the acting account.
* `GET /api/v2/me/apps` and `DELETE /api/v2/me/apps/{grantId}` cover the user's own grants.
* `GET /api/v2/organizations/{organizationId}/apps` and `DELETE …/apps/{grantId}` are for organization admins, checked through the existing organization role relay.

**Delegated actor.** When a gateway-forwarded request's verified credential has `credentialKind` `OAUTH_ACCESS`, the actor is `DelegatedUser{accountId, appId, grantId, scopes}`:
* `accountId` comes from the restricted principal reference `cfmeet-user:<uuid>`;
* the account must be ACTIVE, and its revocation epoch is checked.

It may only use the meeting machine operations on meetings the user owns, under §0 scope
constraints. It is reachable through the gateway `/v1/meetings/*` family once the gateway accepts
`OAUTH_ACCESS_TOKEN` there. It can never join, get media, read recordings or do admin.
Audit: `actor = user, via = app`.

**Userinfo.** `GET /internal/v1/oauth/userinfo?accountId=&scopes=` (`meeting-oauth-userinfo.read`)
returns `{displayName, avatarUrl, email?}`.

**In-meeting apps (I7):**
* `GET /api/v2/conferences/{id}/apps` lists apps with a meeting surface installed for the meeting owner (USER grant) or its organization.
* `POST /api/v2/conferences/{id}/apps/{appId}/context` returns `{contextToken, expiresIn:300}`, a JWS signed with a Meeting Service Ed25519 app-context key. Claims: `aud`=clientId, conference, pairwise participant id, role, and the user `sub` only if that user granted the app.
* `GET /api/v2/app-context-keys` is public and returns the JWKS.

**Flags:** `MEETING_OAUTH_AUTHORIZE_ENABLED`, `MEETING_DELEGATED_APPS_ENABLED`, `MEETING_IN_MEETING_APPS_ENABLED`.

## 4. Organizations (owner: Organizations)

* **Policy.** New section `apps:{installTier:"ADMINS|MEMBERS", allowList:[appId]|null, userInstallsAllowed:true}`. The default is ADMINS with a null allow list.
* `POST /internal/v1/organizations/{organizationId}/app-install-check` (`organization-apps.evaluate`)
  * Body: `{accountId, appId}`.
  * Returns `{allowed:boolean, reason?:"NOT_A_MEMBER|ROLE_REQUIRED|NOT_ALLOWLISTED|ORGANIZATION_NOT_READY"}`.
* **Flag:** `ORGANIZATIONS_APP_INSTALLS_ENABLED`.

## 5. Developer Console (owner: Developer Console)

**Console (human session, project OWNER and ADMIN):**
* `GET`/`PUT /v1/projects/{projectId}/app`: the app manifest (name, description, logo URL, privacy and terms URLs, redirect URIs, user scopes, bot scopes, event types, meeting surface `{url, origins:[…]}`, distribution). Versions are kept immutable in `devplat_app_versions`.
* `POST /v1/projects/{projectId}/app/secret`: rotate the client secret (shown once).
* `POST /v1/projects/{projectId}/app/submit`, then review (§ADR 0065).
* `GET /v1/projects/{projectId}/app/grants`.
* `PUT /v1/projects/{projectId}/app/test-accounts`.

**OAuth public endpoints** (through the gateway, ANONYMOUS, RFC wire format, `application/x-www-form-urlencoded`):
* `POST /v1/oauth/token`: client_secret_basic or post; `grant_type=authorization_code|refresh_token`. Responds in snake_case: `{access_token, token_type, expires_in, refresh_token, scope, installation:{id, target, organization_id?}, bot_token?}`.
* `POST /v1/oauth/revoke`.
* `GET /v1/oauth/userinfo`, Bearer `cfoa_`: introspect, then Meeting Service userinfo. Returns `{sub, name, picture, email?}`.
* `GET /v1/oauth/metadata`: RFC 8414-style metadata. `authorization_endpoint` is `https://cfmeet.com/oauth/authorize`, a frontend page that calls §3.

**Directory:** `GET /v1/app-directory`, `GET /v1/app-directory/{appId}` (ANONYMOUS, PUBLISHED and PUBLIC apps only).

**Review:** state machine DRAFT → SUBMITTED → IN_REVIEW → APPROVED | REJECTED → PUBLISHED, plus SUSPENDED.
* Reviewers come from `DEVELOPER_PLATFORM_APP_REVIEWERS`; nobody reviews their own app.
* Operator CLI subcommands, and operator routes under `/internal/v1/operator/apps/*`.
* Every transition syncs the Platform Access client status.

**Install events.** The webhooks ingest gets a second pass that routes events of the install target to installed apps subscribed to them, with `installationId` added. New catalog events: `app.installed`, `app.uninstalled`, `app.grant_revoked`, `bot.message_received`, `bot.mentioned`.

**Flags:** `DEVELOPER_PLATFORM_APPS_ENABLED`, `DEVELOPER_PLATFORM_OAUTH_ENABLED`, `DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED`, `DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED`.

## 6. API Gateway (owner: API Gateway)

* **New auth mode `OAUTH_ACCESS_TOKEN`.** Contract scheme `oauthAccessToken`, mapped `"oauthAccessToken": "OAUTH_ACCESS_TOKEN"`.
  * Accepts `Bearer cfoa_…` and `Bearer cfob_…`.
  * Authenticated through Platform Access exactly like platform credentials. The forwarded `DelegatedPlatformContext` carries the restricted principal as `initiating_principal`.
* **Routes:**
  * The `/v1/meetings/*` alias rewrite now accepts `OAUTH_ACCESS_TOKEN` in addition to `PLATFORM_CREDENTIAL`.
  * The `/v1/oauth/*` and `/v1/app-directory*` routes are ANONYMOUS.
  * A route with no OAuth scheme refuses OAuth tokens with `403`.
* **Flag:** `API_GATEWAY_OAUTH_ENABLED`.

## 7. Messaging (owner: Messaging; built after the Build wave)

* **OAuth credentials** (`credentialKind` `OAUTH_ACCESS` or `OAUTH_BOT`):
  * the acting principal is forced to the restricted principal;
  * MANAGED conversations only, otherwise `404`;
  * messages record `viaApplicationId`.
* **Bots** (`BOT_OR_SERVICE` with reference `app:<appId>`):
  * may be members only of MANAGED organization conversations;
  * a mention of, or a direct message to, the bot publishes `bot.mentioned` or `bot.message_received` through the existing developer-event publication path, scoped to the organization triple, with `subjectPrincipalId` = the bot.
* **Flag:** `MESSAGING_APPS_ENABLED`.

## 8. Shared files

The integrator owns the same set as Build §10: permissions, public routes, manifest, generated
files, production tooling, workflows, ADRs, and this file.
