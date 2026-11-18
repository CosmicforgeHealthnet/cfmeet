# CF Meet Platform Access

The shared identity authority of CF Meet (Part IV Workstream B,
[ADR 0032](../../docs/decisions/0032-platform-access-shared-identity-authority.md)).
It owns, in its own PostgreSQL database:

* the hierarchy **Tenant → Application → Environment → Service Account → API Credential**,
  first-party / external classification, suspension and revocation;
* **API credentials**: issuance, rotation with overlap, revocation, and verification —
  `cfmk_<sbx|test|live>_<24 hex>_<64 hex>`, HMAC-SHA-256 verifiers under the pepper (the
  Part III construction, unchanged; the raw secret is returned once and never stored);
* the **scope registry** — Calling scopes, `numbers.search | read | manage | assign |
  kyc.read | kyc.manage | portability`, `sms.send | read`, the Part V Messaging scopes `messages.send | read`,
  `conversations.create | manage`, `attachments.write | read`, `moderation.manage`,
  `messaging.export` (STANDARD) and `numbers.admin` / `messaging.admin`
  (PRIVILEGED_OPERATOR: an explicit audited operator grant to a FIRST_PARTY application
  only) — with optional per-environment-kind restriction;
* **principals** (opaque references only) and their tombstones;
* the **failed-authentication throttle**, the commit-ordered **identity change feed**
  consumers project from, Part III **import receipts** and an append-only **operator
  audit**.

It exposes no customer API. Consumers call the service-authenticated `/internal/v1` API
(`packages/service-auth`; contract `cfmeet.platform-access.internal-api`, messages
`cfmeet.platform-access.identity`):

| Route | Permission | Consumer |
|---|---|---|
| `POST /internal/v1/credentials/authenticate` | `platform-credential.authenticate` | Calling, Numbering, SMS Gateway, Messaging |
| `GET /internal/v1/credentials/{id}` (delegated) | `platform-identity.read` | Calling, Numbering |
| `POST /internal/v1/authorization/evaluate` (delegated) | `platform-identity.read` | Calling, Numbering |
| `GET /internal/v1/identity/context` (delegated) | `platform-identity.read` | Calling, Numbering |
| `POST /internal/v1/identity/changes` | `platform-identity.read` | Calling (projection), Messaging (cache invalidation) |
| `GET /internal/v1/identity/imports/{digest}` | `platform-identity.read` | Calling (hand-off) |
| `GET /internal/v1/scopes` | `platform-identity.read` | any consumer |
| `POST /internal/v1/principals/resolve` (delegated) | `platform-principal.resolve` | Calling, Numbering, SMS Gateway, Messaging |
| `POST /internal/v1/principals/telephone-endpoints` (delegated) | `platform-principal.register` | Calling |
| `POST /internal/v1/principals/{id}/tombstone` | `platform-principal.register` | Calling |
| `/internal/v1/admin/**` (+ `X-CF-Operator`) | `platform-identity.administer` | operator console |
| `PUT` / `GET /internal/v1/organization-scopes/{organizationRef}/{capability}`, `POST …/status` | `platform-identity.provision-first-party` | Organizations only ([ADR 0058](../../docs/decisions/0058-first-party-organization-scopes-and-provisioning.md)): idempotent create-or-return / read-back / suspend-resume of one FIRST_PARTY tenant + first-party application + PRODUCTION environment per first-party organization; no credentials or scope grants |

| `POST`/`GET /internal/v1/end-users`, `GET`/`PATCH`/`DELETE /internal/v1/end-users/{userId}`, `POST …/revoke-sessions`, `POST /internal/v1/end-users/{userId}/tokens`, `POST /internal/v1/end-users/tokens` (+ `CF-Forwarded-Credential-Id`) | `platform-end-users.delegate` | Developer Console only ([ADR 0062](../../docs/decisions/0062-cf-meet-platform-build-users-tokens-devices.md)): partner users (EXTERNAL_APPLICATION_PRINCIPAL) of the forwarded credential's own EXTERNAL environment, re-verified live; user-token minting (`user_tokens.issue`) |
| `GET /internal/v1/delegation/environments/{id}/end-users`, `GET`/`PUT …/allowed-origins` | `platform-identity.delegate` | Developer Console (console reads / allowed-websites management) |
| `POST /internal/v1/user-tokens/verify`, `GET /internal/v1/user-tokens/keys` | `platform-user-token.verify` | Messaging, Calling, Meeting Service, API Gateway |
| `GET /internal/v1/environment-origins`, `GET /internal/v1/environments/{id}/origins` | `platform-environment-origins.read` | API Gateway, Messaging, Calling, Meeting Service |
| `POST /internal/v1/principals/ensure-meeting-platform` | `platform-principal.ensure-meeting-platform` | Meeting Service only (partner meeting chat) |

"Delegated" operations require the signed delegated platform context; Platform Access
re-validates its tenant / application / environment / service account against its own
rows and refuses anything inconsistent (`DELEGATED_CONTEXT_INVALID`). A tenant id in a
request never becomes authority.

## CF Meet Platform "Build" (ADR 0062)

Partner users, CF Meet user tokens, allowed websites and partner meeting principals
(contract: `docs/platform/build-internal-contracts.md` §1–§2). Every feature is **off by
default**; with a flag off its routes answer `404 NOT_FOUND` exactly like an unknown route.

| Variable | Default | Meaning |
|---|---|---|
| `PLATFORM_ACCESS_END_USERS_ENABLED` | `false` | Partner Users API and the console read |
| `PLATFORM_ACCESS_USER_TOKENS_ENABLED` | `false` | user-token minting (with the Users API) and verification / keys; needs the signing key |
| `PLATFORM_ACCESS_ENVIRONMENT_ORIGINS_ENABLED` | `false` | allowed websites (feed, reads, console management) |
| `PLATFORM_ACCESS_MEETING_PLATFORM_ENABLED` | `false` | `POST /internal/v1/principals/ensure-meeting-platform` |
| `PLATFORM_ACCESS_MAX_END_USERS_SANDBOX` / `_TEST` / `_PRODUCTION` | 100 / 1000 / 100000 | ACTIVE partner users per environment (`409 LIMIT_EXCEEDED`) |
| `PLATFORM_ACCESS_USER_TOKEN_SIGNING_KEY_FILE` | — | PKCS#8 PEM Ed25519 key file (0600) signing `cfut_<sbx\|test\|live>_<JWS>` tokens; inline keys refused |
| `PLATFORM_ACCESS_USER_TOKEN_PREVIOUS_PUBLIC_KEYS` | — | rotation: base64url raw public keys still accepted (`ACTIVE`) |
| `PLATFORM_ACCESS_USER_TOKEN_RETIRED_PUBLIC_KEYS` | — | retired public keys: their tokens are `USER_TOKEN_REVOKED` (`RETIRED`) |

* **Partner users** are EXTERNAL_APPLICATION_PRINCIPAL principals; profiles (display
  name, image URL) and the session epoch (`tokens_not_before`, advanced by
  `revoke-sessions`) live in `platform_end_user_profiles`. DELETE is the principal
  tombstone plus profile erasure. Every mutation is audited (`devplat.s-<service account>`).
* **User tokens** are stateless Ed25519 JWS (`typ: cfmeet-user+jwt`, ≤ 2048 bytes, 3600 s
  default, 60–86400 s). Verification is live: user ACTIVE, `iat ≥ tokens_not_before`,
  minting credential still authenticating, tenant EXTERNAL and chain ACTIVE, key not
  retired; effective grants = claimed grants ∩ the credential's current scopes. At most 120
  mints per user per hour (`429 RATE_LIMITED`).
* **Allowed websites**: at most 20 exact origins per environment, `https` only except
  `http://localhost[:port]` / `http://127.0.0.1[:port]` in SANDBOX / TEST; every change (and
  every effective status change of an environment with origins) appends to the feed.

## Running locally (no Docker required)

```bash
cp .env.example .env     # DATABASE_URL, API_CREDENTIAL_PEPPER, CF_SERVICE_* (see comments)
make platform-access-migrate
make platform-access     # listens on 127.0.0.1:8094
curl http://127.0.0.1:8094/ready
```

## Administration

`platform-access-admin` (JSON on stdout; every mutation needs `--operator` /
`CF_OPERATOR_ID` and writes one audit row):

```bash
export CF_OPERATOR_ID=ops.alice
A="cargo run -q --bin platform-access-admin --"
$A tenant create --slug acme --name "Acme" --classification external
$A application create --tenant acme --slug dispatch --name "Dispatch"
$A environment create --tenant acme --application dispatch --environment production
$A service-account create --tenant acme --application dispatch --environment production --name backend
$A scope grant --tenant acme --application dispatch --scope calls.read [--environment-kind production]
$A scope grant-privileged --tenant cosmicforge --application numbering-console --scope numbers.admin
$A credential create --tenant acme --application dispatch --environment production \
   --service-account backend --scope calls.read           # prints the secret ONCE
$A credential rotate --credential <public id> --overlap-minutes 60
$A credential revoke --credential <public id>
$A principal register --tenant acme --application dispatch --environment production \
   --kind external-application-principal --reference user-123
$A identity import --bundle part-iii.json                 # hand-off from Part III Calling
$A identity export --out recovery.json                    # 0600; verifiers, never secrets
```

## Credential expiry (P15)

Every platform API credential expires; none is non-expiring by default or otherwise:

* `platform_api_credentials.expires_at` is `NOT NULL` with `CHECK (expires_at > not_before)`
  since the first migration, and authentication refuses an expired credential.
* A credential created or rotated without `expiresAt` gets `not_before +
  CREDENTIAL_DEFAULT_LIFETIME_DAYS` (default **365**); an explicit `expiresAt` may not exceed
  `now + CREDENTIAL_MAX_LIFETIME_DAYS` (default **730**); the default may not exceed the
  maximum (startup refuses). The Developer Platform passes no expiry unless the organization's
  credential policy caps it, so console-created credentials get the same default.
* Credential views report `expiringSoon` near the end, and the Developer Platform emits
  `credential.expiring_soon` / `credential.expired` notices.
* Existing credentials therefore all carry an expiry already; there is nothing to migrate.

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres make platform-access-test
```

Suites: `authentication` (Part III continuity, every rejection reason, rotation,
expiry, revocation, safety-only authority, throttling, pepper rotation),
`credential_storage`, `scopes` (registry, first-party / external, privileged grants,
environment-kind restriction), `isolation` (cross-tenant / application / environment,
forged delegated context, internal authorization), `messaging_scopes` (Part V B), `change_feed`, `handoff`
(Part III import), `admin_cli`, `migrations`, `contracts` (OpenAPI = routes, responses
conform), `platform_build` (ADR 0062: partner users, user tokens, allowed websites, partner
meeting principals, flags off). Calling's suite runs against this service's real binaries.
