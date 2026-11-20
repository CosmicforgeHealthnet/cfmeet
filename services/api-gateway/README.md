# services/api-gateway — CF Meet Platform API Gateway

The single front door of the **CF Meet Platform** (`https://api.cfmeet.com/v1`): partner backends
use CF Meet meetings, messaging and calling from their own products with a scoped API key
([ADR 0055](../../docs/decisions/0055-developer-platform-and-api-gateway-foundation.md),
[ADR 0061](../../docs/decisions/0061-cf-meet-platform-partner-embedding.md)). CF Meet's own apps do
not use it (they stay on their direct `/api/v2` ingress), and partner end-user devices use
short-lived credentials on the direct realtime / media / signaling ingress.

The route table is generated (`infra/api-gateway/route-manifest.json`, from the contracts and the
allow-list `infra/api-gateway/public-routes.json` by `scripts/contracts/public_api.py`). In
production the edge sends only the activated `/v1` families here (`coordinated_release.py`).

**Partner meetings family (`/v1/meetings`).** The Meeting Service's machine operations live on
`/api/v2/...`, which the edge keeps on the CF Meet apps' direct ingress. The generator therefore
also emits, for every meetings operation that accepts a platform credential, an alias route
`meetings.platform.<operationId>`: public `/v1/meetings/<rest>`, upstream `/api/v2/<rest>`,
`PLATFORM_CREDENTIAL` only, same scope (`aliasOf` names the `/api/v2` route, which is unchanged).
`src/routing.rs` accepts exactly this rewrite shape (Meeting Service upstream, REST,
platform credential only, identical segments and parameters, no forbidden segment); the
edge prefix is added by `activate-platform-meetings`.

Request pipeline (`src/http/ingress.rs`):

1. new server-generated request / correlation ids (returned as `X-Request-Id`,
   `X-Correlation-Id`; signed into every upstream hop);
2. **default deny** — a path outside the validated route table is `NOT_FOUND` before any
   credential is examined; the table refuses internal, provider-callback, operator,
   Platform Access, Privacy Relay and Telephony targets by construction (`src/routing.rs`);
3. a client `X-CF-Service-Authorization` is refused; every other client header claiming
   authority (`X-CF-*`, `X-Tenant-*`, `X-Forwarded-*`, …) is dropped — upstream requests
   are built from scratch;
4. `Authorization: Bearer` verified by **Platform Access** (`platform-credential.authenticate`);
   tenant / application / environment / service account come only from its authority chain;
5. route scope check (Platform Access scopes), protocol check (WebSocket forwarding is off
   unless `API_GATEWAY_WEBSOCKETS_ENABLED`: `PROTOCOL_NOT_ENABLED`), bounded body (forwarded verbatim with its `Content-Type`);
6. forward with a fresh **short-lived (60 s) signed service token** carrying the
   `DelegatedPlatformContext` for exactly one upstream audience, under the route deadline.
   The domain service keeps final resource authorization; its status and JSON are relayed.

The gateway owns **no database** and no communications data. It enforces platform
request quotas on platform-credential traffic, with limits from the Developer Console (`services/developer-platform`).
With `API_GATEWAY_REDIS_URL` set, the counters are shared across instances in Redis;
otherwise each instance counts on its own. Setting
`API_GATEWAY_TOKEN_RATE_LIMIT_PER_MINUTE` adds a per-token ceiling on end-user and
device traffic (meeting sessions, participant tokens, developer sessions, client tokens,
signaling credentials, realtime tickets). A request over the ceiling gets
`429 RATE_LIMITED` with `x-cf-quota-level: GATEWAY_TOKEN`. The ceiling is off by default.
Anonymous requests carry no token and are rate-limited by the services and the nginx
edge. Flags default off:
`API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_WEBSOCKETS_ENABLED`,
`API_GATEWAY_USER_TOKENS_ENABLED`, `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`,
`API_GATEWAY_OAUTH_ENABLED`.

Partner devices and websites ([ADR 0062](../../docs/decisions/0062-cf-meet-platform-build-users-tokens-devices.md),
[build contract §9](../../docs/platform/build-internal-contracts.md)):

* **CF Meet user tokens** (`API_GATEWAY_USER_TOKENS_ENABLED`, `src/user_tokens.rs`). On routes whose
  manifest auth includes `USER_TOKEN` (contract security scheme `userToken`), a device sends
  `Authorization: Bearer cfut_<sbx|test|live>_<JWS>`. The gateway checks the format, the
  `EdDSA` / `cfmeet-user+jwt` header, the Ed25519 signature, `iss` / `aud` / `nbf` / `exp` (60 s
  skew) and the tag-to-environment-kind match, with the ACTIVE keys of Platform Access
  (`GET /internal/v1/user-tokens/keys`, permission `platform-user-token.verify`; refreshed every
  5 minutes, on an unknown `kid` at most every 30 s, unusable after 15 minutes without a refresh).
  A failing token is `401 AUTHENTICATION_FAILED` and is not forwarded; without usable keys the
  answer is `503` (fail closed). A passing token is forwarded with `Authorization` unchanged and
  no gateway service token: the owning service verifies it live (revocation, grants).
* **Partner websites** (`API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`, `src/origins.rs`). The gateway
  indexes the allowed origins of ACTIVE environments from Platform Access's change feed
  (`GET /internal/v1/environment-origins?after=&limit=500`, permission
  `platform-environment-origins.read`, polled every 30 s). The static
  `API_GATEWAY_CORS_ALLOWED_ORIGINS` list is checked first and is unchanged. A partner origin
  gets a preflight answer only on paths whose route accepts a device mode (`USER_TOKEN`,
  `CLIENT_TOKEN`, `REALTIME_TICKET`, `SIGNALING_CREDENTIAL`, `PARTICIPANT_TOKEN`), never on
  `PLATFORM_CREDENTIAL`-only routes, and an `Access-Control-Allow-Origin` on the actual response
  only when the origin belongs to the environment of the request's device token (`eid` of the
  verified user token; the unverified `eid` / `env` claim of a client token / signaling
  credential, which the owning service then verifies). WebSocket upgrades forward the browser's
  `Origin` to the owning service.

CF Meet Apps ([ADR 0064](../../docs/decisions/0064-cf-meet-apps-oauth-installs-delegated-access.md),
[Integrate contract §6](../../docs/platform/integrate-internal-contracts.md); `API_GATEWAY_OAUTH_ENABLED`):

* **`OAUTH_ACCESS_TOKEN`** (contract scheme `oauthAccessToken`). `Bearer cfoa_…` (access) and
  `Bearer cfob_…` (bot) are authenticated through Platform Access exactly like platform
  credentials (same refusals, scope pre-check, quotas). Platform Access must report the matching
  `credentialKind` (`OAUTH_ACCESS` / `OAUTH_BOT`, with `appId` and `grantId`) and a principal
  restriction of exactly one principal, otherwise `401`. The upstream gets the signed
  `DelegatedPlatformContext` with `initiating_credential_id` = the token's credential id and
  `initiating_principal` = the restricted principal (`NATIVE_CF_USER` / `BOT_OR_SERVICE`); the
  token is never forwarded. `cfor_` refresh tokens are always `401`. An OAuth token on a route
  that does not list `OAUTH_ACCESS_TOKEN` is `403 CREDENTIAL_NOT_ACCEPTED` and is not forwarded.
  The `/v1/meetings/*` aliases accept it when the machine operation does.
* **Quotas and usage.** An OAuth token's authority chain is the install target (the user's or
  organization's FIRST_PARTY environment and the installation service account), so the platform
  request quota is that environment's (the app's developer environment is not charged), and
  `API_OPERATION` usage is recorded for it with the token's `credentialId` (and the installation
  service account) as caller dimensions.
* **`forwardAuthorization`** (manifest; contract marker `x-cfmeet-forward-authorization: true`).
  Only on ANONYMOUS-only Developer Platform REST routes (the OAuth token / revoke / userinfo
  endpoints): `Authorization` — an OAuth bearer token or `Basic` client credentials — is
  transported unchanged and the Developer Console verifies it with Platform Access. The gateway
  grants such a request no authority (no delegated context, no service token: the internal
  protocol refuses a service token next to a customer `Authorization`); platform credentials,
  refresh tokens and any other bearer value are refused `401`. Anonymous bodies of any declared
  media type (JSON, `application/x-www-form-urlencoded`) are forwarded verbatim, bounded by
  `API_GATEWAY_MAX_BODY_BYTES`.

Documentation (`src/docs.rs`): `GET /openapi.json` and `GET /openapi/{domain}.json` serve the
generated public bundles REDUCED to the operations this deployment routes (P11): families in
`API_GATEWAY_DISABLED_DOMAINS` (production: numbering, SMS, privacy relay) are served with empty
`paths` and `info.x-cfmeet-family-enabled: false`, WebSocket channels appear only with
WebSockets on, and nothing is listed while public routing is off. The static contracts under
`packages/contracts` are unchanged; `/discovery` reports the same families as enabled / not.

```bash
make api-gateway               # services/api-gateway/.env
make api-gateway-test          # unit, routing, ingress, startup (no database)
make api-gateway-integration   # real Platform Access + Developer Platform (TEST_DATABASE_URL)
make api-gateway-lint
```
