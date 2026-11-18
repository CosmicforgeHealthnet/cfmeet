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
   unless `API_GATEWAY_WEBSOCKETS_ENABLED`: `PROTOCOL_NOT_ENABLED`), bounded JSON body;
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
`API_GATEWAY_USER_TOKENS_ENABLED`, `API_GATEWAY_ENVIRONMENT_ORIGINS_ENABLED`.

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
