# CF Meet Calling service

`services/calling/` is the Rust/Axum service for the CF Meet **Calling** domain. It is a
peer of `services/meeting-service/` and is independently buildable and deployable: it
shares no source, models, migrations or database with the Meeting Service.

## What is implemented

This service currently contains the **platform trust foundation** that the Calling
domain is built on (CF Meet Part III, Part B §7–§14). There is no Calling business API
yet: no calls, call participants, sessions, legs, routes, signaling, LiveKit calling,
Privacy Relay, PSTN/SIP, telephony, voicemail or webhooks.

| Area | Status |
|---|---|
| §7 Platform consumer model: tenant → application → environment → service account → API credential; principals | Implemented |
| §8 First-party and external consumers on one model (explicit trust classification + scopes, no product-specific code) | Implemented |
| §9 API credential security: keyed verifiers, server-authoritative scopes, rotation with overlap, immediate revocation, limit-policy foundation, request-rate enforcement | Implemented |
| §10 Separation of API client identity (`PlatformAuthContext`) from communication principal identity (`ResolvedPrincipal`) | Implemented |
| Concurrent-communication and telecom-spend enforcement | Policy resolved here; enforcement belongs to the future call / telephony operations |
| §11 Internal service authentication: Ed25519 service tokens (`packages/service-auth`), receiver-authoritative permissions, `/internal/v1/*` middleware, delegated context, authenticated internal HTTP client | Implemented (no internal business routes yet) |
| §12 Contract authority: `packages/contracts/calling/http/v1.openapi.json` describes exactly the implemented routes; conformance tests | Implemented |
| §13 Data ownership: own database only; no other service's database URL, tables or source | Enforced (`architecture_boundaries`, `make service-boundaries`) |
| §14 Runtime direction: synchronous calls through the authenticated internal client; events only through the platform event envelope contract | Foundation only (no events produced yet) |

HTTP surface: `GET /health` only (contract:
[`packages/contracts/calling/http/v1.openapi.json`](../../packages/contracts/calling/http/v1.openapi.json)).
The customer authentication middleware and `PlatformAuthContext` extractor, and the
internal service authentication middleware and `InternalCaller` extractor, are in place
for future routes and are exercised by test-only routes. Every path under
`/internal/v1/` — including unknown ones — requires a valid service token and answers
`401 INTERNAL_AUTHENTICATION_FAILED` otherwise; customer API credentials are never
accepted there, and service tokens are never accepted on customer routes.

## Internal service authentication

```text
customer request ─► PlatformAuthContext ─► delegated_context() ─► DelegatedPlatformContext
                                                                     │ signed into
peer service ◄─ X-CF-Service-Authorization: Bearer <EdDSA JWS> ◄──────┘ the service token
                X-CF-Request-Id (new per hop) / X-CF-Correlation-Id (stable per operation)
```

* Calling signs as `cfmeet.calling` with its own key (`CF_SERVICE_AUTH_KEY_ID`,
  `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`); tokens are bound to exactly one audience and one
  runtime environment, live 120 s by default (≤ 300 s) and carry no permissions.
* Inbound, Calling verifies against its trust bundle (`CF_SERVICE_TRUST_BUNDLE_FILE`):
  pinned algorithm and token type, known issuer and `kid`, exact audience, runtime
  environment, lifetime, and header ids equal to the signed ids. Permissions come only
  from Calling's own bundle (`InternalCaller::require_permission`). Calling grants
  `operational-callback.deliver` at most, and it is never granted
  `protected-contact.resolve-for-execution` by any receiver.
* Outbound helpers: `internal_auth::delegation::{delegated_context,
  outbound_for_customer, outbound_next_hop}` and `ServiceAuthRuntime::client()` (HTTPS in
  staging / production, no redirects, no proxies, `/internal/v1/` paths only).

ADR: [0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md).

## Model

```text
Tenant (FIRST_PARTY | EXTERNAL; ACTIVE | SUSPENDED)
└── Application (ACTIVE | SUSPENDED) ── allowed scopes ── limit policy
    └── Environment (SANDBOX | TEST | PRODUCTION; ACTIVE | SUSPENDED; live_telecom_allowed)
        ├── Service account (ACTIVE | SUSPENDED | REVOKED)
        │   └── API credential (ACTIVE | REVOKED; validity window; rotation links) ── granted scopes ── limit policy
        └── Principal (7 categories, opaque reference; ACTIVE | SUSPENDED | DEACTIVATED)
```

* Every child row carries its whole ownership chain and references its parent with a
  composite foreign key on `(…ownership…, id)`, so a row can never point at a parent of
  another tenant / application / environment. Ownership columns are immutable (trigger).
* An application has exactly one environment of each kind. SANDBOX and TEST can never
  have `live_telecom_allowed = true` (validated and `CHECK`-enforced); PRODUCTION starts
  with it disabled.
* FIRST_PARTY is a trust classification only. CosmicForge products and external
  customers are ordinary applications; privileges exist only as explicit scopes.

## API credentials

Format: `cfmk_<sbx|test|live>_<public id: 24 hex>_<secret: 64 hex>`.

* The secret is 32 bytes from the OS CSPRNG. The environment tag is convenience only; it
  grants nothing and is bound into the verifier, so an edited tag never verifies.
* The database stores `HMAC-SHA-256(API_CREDENTIAL_PEPPER, context ‖ tag ‖ public id ‖
  secret)` plus algorithm and pepper version — never the secret. Verification is
  constant time. The raw credential is returned once, at creation or rotation.
* Authentication (`Authorization: Bearer <credential>`) loads the credential's full
  authority chain in one query and requires: verifier match, credential ACTIVE, not
  revoked, inside its validity window and rotation overlap, and service account,
  environment, application and tenant all ACTIVE. No caches: suspension and revocation
  apply to the next request. Every failure returns the same `AUTHENTICATION_FAILED` 401.
* Scopes come from a server-side registry, must be allowed for the application
  (foreign-key enforced) and are loaded from the database at authentication; requests
  cannot add scopes. `PlatformAuthContext::require_scope` is the check for handlers.
* Rotation issues an independent successor for the same service account (scopes and
  credential limits preserved) and limits the predecessor to `min(original expiry,
  now + overlap)`. Revocation is immediate and idempotent.

## Principals

`PlatformAuthContext` says which application / service account is calling.
`PrincipalResolver::resolve(ctx, reference)` resolves *who takes part* — only inside the
context's tenant, application and environment; a principal elsewhere is reported exactly
like a nonexistent one. Authentication never creates or implies a principal; a service
account is a principal only if explicitly registered as a linked SERVICE_ACCOUNT
principal.

Categories: `NATIVE_CF_USER`, `EXTERNAL_APPLICATION_PRINCIPAL`, `ENTERPRISE_MEMBER`,
`SERVICE_ACCOUNT`, `TELEPHONE_ENDPOINT`, `TEMPORARY_EXTERNAL_PARTICIPANT`,
`BOT_OR_SERVICE`. References are opaque; no plaintext contact data is stored —
E.164-style and `tel:`/`sip:` values are refused, and TELEPHONE_ENDPOINT references must
be opaque UUID handles (for the future Contact Vault).

## Limits

Application- and credential-level limits (requests per minute, concurrent
communications, daily / monthly telecom spend in minor units of the tenant's billing
currency) are set by operators only. The effective policy takes the stricter value where
both levels configure a limit, otherwise the configured one, otherwise the platform
default. Telecom spend defaults to zero and is always zero where live telecom is not
allowed. The request-rate limit is enforced now (shared PostgreSQL one-minute windows,
per credential and per application); the other limits are resolved for the future
operations that consume them.

## Configuration

See [`.env.example`](.env.example). Required: `DATABASE_URL`, `API_CREDENTIAL_PEPPER`
(≥ 32 random bytes, e.g. `openssl rand -base64 48`; placeholder values are rejected and
there is no fallback), `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`,
`CF_SERVICE_AUTH_PRIVATE_KEY_FILE` and `CF_SERVICE_TRUST_BUNDLE_FILE`. The service exits
at startup if its identity, key, trust bundle or runtime environment is missing or
inconsistent. The Calling service owns its own database.

## Running locally (no Docker required)

```bash
cp .env.example .env              # set DATABASE_URL and API_CREDENTIAL_PEPPER
# A local signing key (outside the repository) and a trust bundle with no peers yet:
mkdir -p ~/.cfmeet && cargo run -q --manifest-path ../../packages/service-auth/Cargo.toml \
  --bin cf-service-keygen -- generate --kid calling-local-1 --out ~/.cfmeet/calling-local-1.pem
echo '{"bundle_version":1,"receiver":"cfmeet.calling","runtime_environment":"local","trusted_issuers":[]}' \
  > ~/.cfmeet/calling-trust-bundle.json
# then set CF_SERVICE_AUTH_PRIVATE_KEY_FILE / CF_SERVICE_TRUST_BUNDLE_FILE in .env
cargo run -p calling-migration -- up
cargo run --bin calling
curl http://127.0.0.1:8090/health
```

From the repository root: `make calling-migrate`, `make calling`.

## Administration

`platform_admin` uses the same domain services as future administration APIs. There are
no public credential-management endpoints.

```bash
A="cargo run -q --bin platform_admin --"
$A tenant create --slug acme --name "Acme" --classification external
$A application create --tenant acme --slug dispatch --name "Dispatch"
$A environment create --tenant acme --application dispatch --environment production
$A service-account create --tenant acme --application dispatch --environment production --name backend
$A scope grant --tenant acme --application dispatch --scope principals.read
$A credential create --tenant acme --application dispatch --environment production \
   --service-account backend --scope principals.read      # prints the secret ONCE
$A credential rotate --credential <public id> --overlap-minutes 60
$A credential revoke --credential <public id>
$A principal register --tenant acme --application dispatch --environment production \
   --kind external-application-principal --reference user-123
$A limits set-credential --credential <public id> --requests-per-minute 120
$A limits effective --credential <public id>
```

`$A --help` lists every command (suspend / reactivate for tenants, applications,
environments, service accounts and principals; `environment set-live-telecom`; scope
registry; credential show / list). Output is JSON on stdout; audit lines go to stderr.

## Tests

```bash
TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test --workspace --no-fail-fast
```

Each test clones its own database from a migrated template (no Docker needed). Without
`TEST_DATABASE_URL` the database suites fail, unless `CFMEET_SKIP_DB_TESTS=1` is set
(explicit SKIPPED lines). Suites: `migrations`, `tenant_isolation`,
`first_party_external`, `credential_storage`, `authentication`, `environment_isolation`,
`scopes`, `rotation`, `secret_leakage`, `principals`, `limits`, `admin_cli`,
`architecture_boundaries`, `internal_auth` (service-token authentication on
`/internal/v1/*`), `outbound` (delegated context and the internal HTTP client over
loopback HTTP) and `http_contract` (conformance with `packages/contracts`).

CI: [`.github/workflows/calling.yml`](../../.github/workflows/calling.yml).

## Container image

```bash
# from the repository root: the image also needs packages/service-auth
docker build -f services/calling/Dockerfile -t cfmeet-calling .
```

The image runs as a non-root user and contains `calling`, `calling-migration` and
`platform_admin`. All configuration is supplied at runtime.

## Decisions

* [ADR 0003 — Calling platform consumer, credential and principal trust model](../../docs/decisions/0003-calling-platform-trust-model.md)
* [ADR 0004 — Internal service authentication](../../docs/decisions/0004-internal-service-authentication.md)
* [ADR 0005 — Contract authority and runtime boundaries](../../docs/decisions/0005-contract-authority-and-runtime-boundaries.md)
* [Service data ownership](../../docs/architecture/service-data-ownership.md)
