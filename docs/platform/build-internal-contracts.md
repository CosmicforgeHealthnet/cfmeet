# CF Meet Platform "Build": shared contract for the parallel implementation (ADR 0062 / 0063)

Every service team (agent) builds against THIS document. Do not change a shape here on your own: if
something is impossible, implement the closest safe shape and report the difference.

Conventions:
* JSON is camelCase.
* Errors use each service's existing envelope (`{"error":{"code","message",...}}`).
* Ids are UUIDs.
* Every feature is behind a default-off flag. With the flag off, behaviour is exactly as today.

## 1. The CF Meet user token

* **Wire format:** `cfut_<sbx|test|live>_<compact JWS>`. The tag follows the environment kind (SANDBOX → `sbx`, TEST → `test`, PRODUCTION → `live`). Maximum 2048 bytes.
* **JWS header:** `{"alg":"EdDSA","typ":"cfmeet-user+jwt","kid":"<16 lowercase hex>"}`. Ed25519.
* **Claims:**

```json
{"iss":"cfmeet.platform-access","aud":"cfmeet.platform.client",
 "sub":"<principalId>","tid":"<tenantId>","aid":"<applicationId>","eid":"<environmentId>",
 "ekd":"SANDBOX|TEST|PRODUCTION","sva":"<minting serviceAccountId>","crd":"<minting credentialId>",
 "jti":"<uuid>","iat":0,"nbf":0,"exp":0,
 "grt":{"msg":["read","send"],"call":["receive"],"mtg":[{"c":"<conferenceId>","r":"PARTICIPANT"}]}}
```

* **Grants.** Every part of `grt` is optional.
  * `msg` ⊆ {`read`, `send`}.
  * `call` ⊆ {`receive`}.
  * `mtg` has at most 10 entries; `r` ∈ {`HOST`, `CO_HOST`, `PRESENTER`, `PARTICIPANT`, `VIEWER`}.
  * Scopes the minting credential must hold for each grant:

    | Grant | Required scope |
    |---|---|
    | `msg:read` | `messages.read` |
    | `msg:send` | `messages.send` |
    | `call:receive` | `calls.write` |
    | `mtg` | `meetings.write` |

* **Lifetime:** 3600 s by default, 60–86400 s allowed.
* **How a device presents it:** HTTP header `Authorization: Bearer cfut_…`. Services recognise it by the `cfut_` prefix.
* **Validity.** The token is valid only while ALL of these hold:
  * the signature is correct, and `iss`/`aud`/`nbf`/`exp` check out (60 s clock skew);
  * the principal is ACTIVE;
  * `iat >= principal.tokens_not_before`;
  * the minting credential is ACTIVE and not expired;
  * the environment, application and tenant are ACTIVE, and the tenant is EXTERNAL;
  * the signing key `kid` is not retired.
* **Effective grants** = the claimed grants ∩ what the credential's CURRENT scopes allow.

## 2. Platform Access internal APIs (owner: Platform Access)

### 2.1 Partner Users API, called by the Developer Console only (`platform-end-users.delegate`)

Every request carries `CF-Forwarded-Credential-Id: <uuid>` (the API key the gateway authenticated,
taken from the signed delegated context). Platform Access re-verifies that credential live. The
environment ALWAYS comes from that credential. The credential's tenant must be EXTERNAL; otherwise
the call fails with `403 POLICY_VIOLATION`.

| Method + path | Scope the credential needs | Result |
|---|---|---|
| `POST /internal/v1/end-users` `{externalId, displayName?, imageUrl?}` | `principals.write` | `201 {user}` new · `200 {user}` existing (idempotent on externalId) |
| `GET /internal/v1/end-users?externalId=&cursor=&limit=` | `principals.read` | `{items:[user], nextCursor}` |
| `GET /internal/v1/end-users/{userId}` | `principals.read` | `{user}` |
| `PATCH /internal/v1/end-users/{userId}` `{displayName?, imageUrl?, status?: ACTIVE\|SUSPENDED}` | `principals.write` | `{user}` |
| `DELETE /internal/v1/end-users/{userId}` | `principals.write` | `204` (tombstone: profile erased, ids kept) |
| `POST /internal/v1/end-users/{userId}/revoke-sessions` | `principals.write` | `{tokensNotBefore}` |
| `POST /internal/v1/end-users/{userId}/tokens` `{grants, ttlSeconds?}` | `user_tokens.issue` (+ the grant scopes) | `201 {token, tokenId, expiresAt}` |
| `POST /internal/v1/end-users/tokens` `{externalId, displayName?, imageUrl?, grants, ttlSeconds?}` | `principals.write` + `user_tokens.issue` | `201 {user, token, tokenId, expiresAt}` (upsert) |

**User object:**

```json
{"id":"<principalId>","externalId":"…","displayName":"…|null","imageUrl":"https://…|null",
 "status":"ACTIVE|SUSPENDED|DEACTIVATED","createdAt":"…","updatedAt":"…"}
```

**Validation:**
* `externalId`: 1–128 printable ASCII, no spaces. It must not look like a phone number or SIP address (reuse `validate_external_reference`).
* `displayName`: up to 64 characters, no control characters.
* `imageUrl`: `https://` only, up to 512 characters.

**Errors:**

| Code | When |
|---|---|
| `404 NOT_FOUND` | the user belongs to another environment, or doesn't exist |
| `409 LIMIT_EXCEEDED` | an environment cap is hit |
| `422 VALIDATION_ERROR` | invalid input |
| `403 SCOPE_REQUIRED` | the credential lacks the required scope |
| `429 RATE_LIMITED` | more than 120 token mints per user per hour |

**Caps** (count of ACTIVE users per environment):

| Environment kind | Variable | Default |
|---|---|---|
| SANDBOX | `PLATFORM_ACCESS_MAX_END_USERS_SANDBOX` | 100 |
| TEST | `PLATFORM_ACCESS_MAX_END_USERS_TEST` | 1000 |
| PRODUCTION | `PLATFORM_ACCESS_MAX_END_USERS_PRODUCTION` | 100000 |

**Console read (human members)**, under the existing `platform-identity.delegate` with the existing actor headers:
* `GET /internal/v1/delegation/environments/{environmentId}/end-users?cursor=&limit=`.

### 2.2 User token verification (`platform-user-token.verify`; Messaging, Calling, Meeting Service, API Gateway)

* `POST /internal/v1/user-tokens/verify` `{token}`:
  * **200:**

    ```json
    {"tokenId","tenantId","applicationId","environmentId","environmentKind","credentialId","serviceAccountId",
     "user":{"id","externalId","displayName","imageUrl","status"},
     "grants":{"msg":[…],"call":[…],"mtg":[{"conferenceId","role"}]},"issuedAt","expiresAt"}
    ```

    `grants` here are the EFFECTIVE grants.
  * **401** `{error:{code: USER_TOKEN_INVALID | USER_TOKEN_EXPIRED | USER_TOKEN_REVOKED}}`.
* `GET /internal/v1/user-tokens/keys` returns `{keys:[{kid, alg:"EdDSA", crv:"Ed25519", x:"<base64url>", status:"ACTIVE|RETIRED"}]}`.

Consumers may cache a successful verify for at most 30 s, keyed by `tokenId`.

### 2.3 Allowed websites (origins)

* **Read** (`platform-environment-origins.read`; API Gateway, Messaging, Calling, Meeting Service):
  * `GET /internal/v1/environment-origins?after=<seq>&limit=<≤500>` returns `{items:[{environmentId, origins:[…], environmentStatus, seq}], nextAfter}`. This is a change feed: one item per change, the latest state.
  * `GET /internal/v1/environments/{environmentId}/origins` returns `{environmentId, origins:[…]}`.
* **Manage** (Developer Console, under the existing `platform-identity.delegate` with actor headers):
  * `GET` and `PUT /internal/v1/delegation/environments/{environmentId}/allowed-origins`, body `{origins:[…]}`.
* **Rules:**
  * at most 20 origins per environment;
  * each is an exact `scheme://host[:port]` with no path, lowercase host;
  * `https` only, except `http://localhost[:port]` and `http://127.0.0.1[:port]`, which only SANDBOX and TEST environments may use;
  * no wildcards.

### 2.4 Partner meeting chat principals (`platform-principal.ensure-meeting-platform`; Meeting Service only)

* `POST /internal/v1/principals/ensure-meeting-platform` with body `{environmentId, kind, externalReference}`.
* Allowed combinations:
  * `kind = BOT_OR_SERVICE` with `externalReference = "cfmeet:meeting-system"` (only this exact reference);
  * `kind = TEMPORARY_EXTERNAL_PARTICIPANT` with `externalReference = "meeting-participant:<uuid>"`.
* The environment must be ACTIVE and EXTERNAL.
* Returns `{principalId, tenantId, applicationId, environmentId, kind}`.

## 3. Developer Console public API (owner: Developer Console; served through the gateway; `platformCredential`)

| Route | Delegates to | Scope |
|---|---|---|
| `POST /v1/users` | §2.1 create | `principals.write` |
| `GET /v1/users`, `GET /v1/users/{userId}` | §2.1 | `principals.read` |
| `PATCH /v1/users/{userId}`, `DELETE /v1/users/{userId}`, `POST /v1/users/{userId}/revoke-sessions` | §2.1 | `principals.write` |
| `POST /v1/users/{userId}/tokens`, `POST /v1/users/tokens` | §2.1 | `user_tokens.issue` |

**Console routes (human session):**
* `GET /v1/environments/{environmentId}/users`;
* `GET`/`PUT /v1/environments/{environmentId}/allowed-origins`;
* `POST /v1/environments/{environmentId}/test-user-tokens` (SANDBOX only; same body as `/v1/users/tokens`; a development token generator);
* `GET`/`POST /v1/environments/{environmentId}/push-credentials`, `DELETE /v1/environments/{environmentId}/push-credentials/{credentialRef}`, `POST /v1/environments/{environmentId}/push-credentials/{credentialRef}/verify`.

**Flags:** `DEVELOPER_PLATFORM_USERS_API_ENABLED`, `DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED`, `DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED`.

**Push credential upload body:**
* `{provider:"APNS", teamId, keyId, bundleId, voip:boolean, apnsHost:"SANDBOX|PRODUCTION", p8:"-----BEGIN PRIVATE KEY-----…"}`, or
* `{provider:"FCM", projectId, serviceAccountJson:"{…}"}`.

**Handling the secret:**
* Seal it to the Notification Service public key immediately, with `packages/notification-client` `seal`.
* Never store, log or return it.
* Keep only metadata: `{credentialRef, provider, bundleId|projectId, keyId, voip, apnsHost, fingerprint (sha256 of the secret, hex), status, lastVerifiedAt, createdAt}`.
* SANDBOX and TEST environments may use only `apnsHost: SANDBOX`.

## 4. Device exchange routes (device-facing; `Authorization: Bearer cfut_…`; contract security scheme `userToken`)

| Owner | Route | Body | Response |
|---|---|---|---|
| Messaging | `POST /v1/messaging/user-sessions` | `{installationId, sessionDeviceId?}` | `{principalId, sessionDeviceId, sessionDeviceExpiresAt, clientToken, clientTokenExpiresAt}` (then the existing `POST /v1/realtime/tickets` with the client token) |
| Messaging | `PUT /v1/messaging/user-push-registrations/{installationId}` | `{platform:"APNS\|APNS_VOIP\|FCM", handle}` | `{installationId, platform, status}` |
| Messaging | `DELETE /v1/messaging/user-push-registrations/{installationId}?platform=` | — | `204` |
| Calling | `POST /v1/calling/user-sessions` | `{installationId, kind:"WEB\|MOBILE"}` | `{principalId, endpointId, signalingCredential, signalingCredentialExpiresAt}` |
| Meeting Service | `POST /api/v2/conferences/{conferenceId}/join` (existing route; now also accepts a user token) | existing | existing join response |

**Rules:**
* `installationId` is 8–128 characters of `[A-Za-z0-9._-]`. It is stable per app installation, generated by the SDK.
* The device must hold the required grant for each exchange:

  | Exchange | Required grant |
  |---|---|
  | chat | `msg` (`read` for realtime, `send` to send) |
  | calls | `call:receive` |
  | meeting | an `mtg` entry for that conference |

* Derived device credentials record the user token's `crd` (minting credential) and `sva`.
* Derived credentials end when:
  * the principal is suspended or deleted;
  * `revoke-sessions` is called;
  * the minting credential is revoked.

**Feature flags:**

| Flag | Owner |
|---|---|
| `MESSAGING_PARTNER_SESSION_DEVICES_ENABLED` | Messaging |
| `MESSAGING_PARTNER_PUSH_ENABLED` | Messaging |
| `CALLING_USER_TOKEN_EXCHANGE_ENABLED` | Calling |
| `CALLING_PARTNER_PUSH_ENABLED` | Calling |
| `MEETING_PLATFORM_USER_JOIN_ENABLED` | Meeting Service |
| `MEETING_PLATFORM_CHAT_ENABLED` | Meeting Service |

## 5. Partner chat policy (owner: Messaging)

* **Baseline `CFMEET_PLATFORM_PARTNER`** for EXTERNAL tenants, applied while `MESSAGING_PARTNER_SESSION_DEVICES_ENABLED` is on:
  * every context class allows `[MANAGED, PRIVATE_E2EE]`, with MANAGED as the default;
  * attachments are on.
* An explicit application policy row still wins.
* **What partner session devices can do:**
  * They are MANAGED-only. A PRIVATE_E2EE target answers `404`, or `E2EE_DEVICE_REQUIRED` on realtime.
  * They get a closed REST allowlist `PARTNER_SESSION_DEVICE_ROUTES`:
    * list and get conversations and members;
    * list, get, send, edit and delete messages; reactions; read cursor; unread; events;
    * attachments (create, finalize, get, download grants);
    * search within a conversation;
    * report.
  * They never reach MLS, KeyPackages, welcomes, backups, history transfer, key transparency or device registration.
* **Usage:** export a `MESSAGE_SENT` fact for EXTERNAL environments: unit OPERATION, producer `cfmeet.messaging`, dimensions `contextClass`, `securityProfile`.

## 6. Notification Service partner push (owner: Notification Service; ADR 0063)

* **N-P1, provider vault** (`notifications-push-provider.manage`, Developer Console only).
  * `PUT /internal/v1/push/providers/{tenantId}/{applicationId}/{environmentId}/{provider}`, where `{provider}` is `APNS`, `APNS_VOIP` or `FCM`:
    * body: `{sealed:"<seal envelope as produced by packages/notification-client seal>", metadata:{teamId?, keyId?, bundleId?, apnsHost?, projectId?}, fingerprint}`;
    * returns `{provider, status:"UNVERIFIED|VERIFIED|FAILED", fingerprint, updatedAt}`.
  * Also `GET …/{environmentId}` (list), `DELETE …/{provider}`, and `POST …/{provider}/verify`. Verify builds an APNs JWT or fetches an FCM OAuth token and returns `{status, lastError?}`.
  * Stored sealed under the service's data keys. A credential can only ever send to endpoints of the same (tenant, application, environment).
* **N-P2, registrations.** The existing registration sync (`notifications-push-registration.sync`, from Messaging) also accepts the subject `{kind:"platform-principal", tenantId, applicationId, environmentId, principalId, installationId}` with platform `APNS`, `APNS_VOIP` or `FCM`, plus a handle. Removal works the same way.
* **N-P3, call fast lane.** The existing call push dispatch (`notifications-call-push.dispatch`, from Calling) also accepts the recipient `{kind:"platform-principal", tenantId, applicationId, environmentId, principalId}`.
  * iOS: `APNS_VOIP` (topic `<bundleId>.voip`, `apns-push-type: voip`).
  * Android: FCM high-priority data message.
  * Same delivery rules as today: synchronous, no retry.
* **N-P4, recipient kind.** The notification event contract gets the recipient kind `platform-principal` (`platform-principal:<environmentId>:<principalId>`) for metadata-only partner events (`messaging.message.created`).
* **N-P5, observability.** Per-scope sent/failed counters; invalid handles are disabled.
* **Flag:** `NOTIFICATIONS_PARTNER_PUSH_ENABLED`, default off.

## 7. Calls (owner: Calling)

* Calls stay server-placed (`POST /v1/calls`, unchanged).
* The `user-sessions` exchange creates or reuses the endpoint keyed by `inst:<installationId>` for the principal, then mints a `cfsig_` signaling credential with `crd`/`sva` taken from the user token.
* With `CALLING_PARTNER_PUSH_ENABLED`, a call to an EXTERNAL principal also sends the §6 N-P3 platform-principal recipient.

## 8. Meetings (owner: Meeting Service)

* New actor `PlatformUser`, resolved from `Authorization: Bearer cfut_…` through §2.2.
* **Join rules:**
  * the space's `platform_environment_id` must equal the token's `environmentId`; otherwise `404`;
  * the conference must be in the effective `mtg` grants; otherwise `403`;
  * the role is clamped to what the space allows;
  * the display name comes from the user profile.
* Re-joining keeps the same participant: new table `meeting_platform_participants(conference_id, principal_id, participant_id, …)`.
* **Partner meeting chat** (`MEETING_PLATFORM_CHAT_ENABLED`):
  * the owner is the environment's system principal (§2.4);
  * the conversation is MANAGED;
  * partner users read it with their Messaging partner session device.

## 9. API Gateway (owner: API Gateway)

* New auth mode `USER_TOKEN`. In contracts it is the security scheme `userToken`, mapped `"userToken": "USER_TOKEN"` in `scripts/contracts/public_api.py`.
* For `USER_TOKEN` routes, the gateway:
  * verifies the signature and expiry statelessly, with the keys from §2.2, refreshed every 5 minutes or less;
  * forwards `Authorization` unchanged.

  The owning service always does the live verify.
* **CORS for partner websites** (`API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`), with origins taken from the §2.3 feed:
  * **Preflight:** allowed only for routes that accept a device auth mode (`USER_TOKEN`, `CLIENT_TOKEN`, `REALTIME_TICKET`, `SIGNALING_CREDENTIAL`, `PARTICIPANT_TOKEN`), and only when the origin is registered for some ACTIVE environment.
  * **Actual request:** `Access-Control-Allow-Origin` is set only when the origin belongs to the token's environment (`eid`).
  * **Never** on `PLATFORM_CREDENTIAL`-only routes.
* Forward `Origin` upstream on WebSocket upgrades.

## 10. Shared files: only the integrator edits these

Agents report the entries they need here; they do NOT edit these files:
* `infra/api-gateway/public-routes.json` and every generated output: `route-manifest.json`, `packages/contracts/bundled/**`, `sdks/**/generated`, `sdk-discovery.json`, the frontend reference, the first-party ingress record;
* `packages/contracts/manifest.json` (contract versions);
* `infra/calling/production/**` (activations, acceptance);
* `.github/workflows/**`;
* `packages/service-auth` (already done: six new permissions);
* `docs/decisions/**` and this file.
