# ADR 0004: Internal service authentication

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part III, Part B §11. Implemented in
  [`packages/service-auth/`](../../packages/service-auth) and integrated into
  [`services/calling/`](../../services/calling)

## Context

Calling, Privacy Relay and Telephony will call each other synchronously: Calling asks
Privacy Relay to authorize a communication, Telephony asks Privacy Relay to resolve a
protected contact for one authorized call leg, Telephony delivers operational callbacks
to Calling. Each of those requests must prove **which service** sent it, **for which
service** it was minted, **in which deployment** it is valid, and — for customer-initiated
work — **on behalf of which tenant / application / environment** it runs.

Things that must not be the answer:

* customer API credentials (`cfmk_*`), their table (`platform_api_credentials`) or their
  pepper (`API_CREDENTIAL_PEPPER`) — a customer secret would become a service identity;
* network location ("it came from the private network", "it came from the mesh");
* a shared symmetric secret — one leak forges every service;
* permissions declared by the caller inside its own token.

## Decision

### 1. One reusable library: `packages/service-auth` (`cf_service_auth`)

Every Rust service links the same crate. It has no dependency on any service, holds no
service-specific business logic, and is verified against the canonical contracts in
`packages/contracts` in both directions (`tests/contract_conformance.rs`).

### 2. Token format: EdDSA (Ed25519) compact JWS

* Signed with Ed25519 through the audited `jsonwebtoken` (RustCrypto backend) and
  `ed25519-dalek` crates. No hand-written cryptography.
* Protected header is exactly `{"alg":"EdDSA","typ":"cf-service+jwt","kid":…}` (contract
  `cfmeet.platform.service-token-header`). The algorithm is pinned by the verifier;
  `alg: none`, HMAC / RSA / ECDSA algorithms (algorithm confusion), `jwk` / `jku` /
  `x5u` / `x5c` / `crit` and any other header parameter are rejected.
* Claims are a closed set (contract `cfmeet.platform.service-token-claims`, unknown claims
  rejected): `ver` (1), `iss`, `aud` (exactly one service — no list, no wildcard),
  `runtime_environment`, `iat`, `nbf`, `exp`, `jti`, `request_id`, `correlation_id` and an
  optional signed `delegated_context`.
* Lifetime: 120 s by default, configurable 10–300 s, **hard maximum 300 s** enforced by the
  verifier on `exp - iat`; clock skew tolerance 30 s; tokens longer than 4 KiB are refused
  before parsing.
* `kid` is required and must belong to the token's issuer. Each issuer may publish up to
  four keys so rotation has an overlap window: publish the new public key → switch the
  signer → remove the old key after the maximum lifetime has passed.

### 3. Transport: a separate header

```text
X-CF-Service-Authorization: Bearer <service token>
X-CF-Request-Id:            req_<32 hex>    new for every hop
X-CF-Correlation-Id:        corr_<32 hex>   stable for the whole operation
```

`Authorization` stays reserved for customer credentials. A request carrying both is
rejected (`customer_credential_present`), customer routes reject service tokens, and
internal routes never fall back to customer authentication. The id headers are mandatory,
must appear exactly once, and must **equal** the signed `request_id` / `correlation_id`
claims — a proxy or attacker cannot re-label a signed request.

### 4. Runtime environment is separate from application environment

`runtime_environment` (`local | test | staging | production`) identifies the
*deployment*. It is unrelated to a customer application environment
(`sandbox | test | production`, carried in the delegated context). Tokens, trust bundles
and services are bound to one runtime environment; a staging token is never valid in
production. Plain HTTP is only allowed for `local` / `test` targets.

### 5. Receiver-authoritative permissions (trust bundle)

Tokens carry **no** permissions or scopes. Each receiver loads a trust bundle (contract
`cfmeet.platform.service-trust-bundle`) for exactly its own identity and runtime
environment:

```json
{ "bundle_version": 1, "receiver": "cfmeet.privacy-relay", "runtime_environment": "staging",
  "trusted_issuers": [ { "issuer": "cfmeet.calling", "audience": "cfmeet.privacy-relay",
      "runtime_environment": "staging", "keys": [ { "kty": "OKP", "crv": "Ed25519", "kid": "…", "x": "…" } ],
      "permissions": [ "communication.authorize", "communication-context.read" ] } ] }
```

The bundle is validated at startup and the service refuses to start on any defect: wrong
receiver or environment, wildcard identities, self-trust, duplicate issuers or key ids,
non-Ed25519 / weak / malformed keys, empty, unknown, wildcard or duplicate permissions, a
permission received by another service, or a permission that may never be granted to that
issuer.

### 6. Least-privilege permission registry

Permissions are a closed enum in code and in the contract (`InternalPermission`). Each has
exactly one receiver and a fixed list of issuers it may ever be granted to:

| Permission | Receiver | Grantable to |
|---|---|---|
| `communication.authorize` | privacy-relay | calling |
| `communication-context.read` | privacy-relay | calling, telephony |
| `protected-contact.resolve-for-execution` (sensitive) | privacy-relay | **telephony only** |
| `call-leg.execute` | telephony | calling |
| `call-leg-status.read` | telephony | calling |
| `operational-callback.deliver` | calling | privacy-relay, telephony |

Calling can never be granted plaintext contact resolution: a bundle that tries is refused.

### 7. Distinct context types

| Type | Produced by | Meaning |
|---|---|---|
| `PlatformAuthContext` (Calling) | customer credential verification | which customer application environment / service account called |
| `ResolvedPrincipal` (Calling) | principal resolution inside that scope | which communication participant |
| `InternalServiceContext` (service-auth) | service token verification | which **service** called, for which hop, with the receiver's grants |
| `DelegatedPlatformContext` (service-auth) | `delegated_context()` from a trusted `PlatformAuthContext` | safe ids of the customer scope the internal call acts for |

There are no `From` / `TryFrom` conversions between them (enforced by Calling's
architecture tests). `DelegatedPlatformContext` carries ids only — tenant, application,
application environment and kind, initiating service account, optionally an initiating
principal of the same scope — never credentials, verifiers or contact data. It is signed
inside the token, so a downstream service can trust it without re-running customer
authentication.

### 8. API shape

* Outbound: `OutboundServiceRequest::{new_operation, continuing, next_hop_of}` →
  `ServiceTokenSigner::sign` (refuses its own audience; accepts no permissions) →
  `SignedServiceRequest::apply_to` (marks the header sensitive).
* Inbound: `ServiceTokenVerifier::verify_headers` → `InternalServiceContext` →
  `require_permission(...)`. Failures map to one stable reason code each
  (`VerificationFailure::as_str`) for logs, and to a single uniform
  `INTERNAL_AUTHENTICATION_FAILED` response (contract `cfmeet.platform.internal-http`).
* Client (`client` feature): `InternalServiceClient` — targets selected by audience from
  configuration only, paths confined to `/internal/v1/`, HTTPS outside local / test, no
  redirects, no proxies, bounded timeouts, safe error mapping.
* Tokens, keys and contexts have redacted `Debug`; tokens are never logged.

### 9. Calling integration

* Configuration (`.env.example`): `CF_SERVICE_ID` (optional, must be `cfmeet.calling`),
  `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`
  (PKCS#8 PEM file; inline keys refused), `CF_SERVICE_TRUST_BUNDLE_FILE`,
  `CF_SERVICE_TOKEN_TTL_SECONDS`, `CF_INTERNAL_HTTP_TIMEOUT_MS`, optional peer base URLs.
  There is no default or development fallback key; the service exits (status 2) if any of
  them is missing or inconsistent.
* Every path under `/internal/v1/` — including unknown ones — runs behind
  `require_internal_service`; handlers use the `InternalCaller` extractor and
  `require_permission`. No permanent test / whoami endpoints exist.
* Keys are generated per service and per runtime environment with
  `cf-service-keygen generate --kid … --out …` (file mode 0600, never overwritten); only
  the public JWK is distributed.

## Consequences

* A leaked customer credential cannot impersonate a service and a leaked service key
  cannot act in another runtime environment or towards an audience its holder's peers do
  not trust it for.
* Adding a service requires: a registry entry for each permission it receives or is
  granted, a key per runtime environment, and trust-bundle entries on each receiver.
* Trust bundles are configuration, delivered with the deployment; rotating a key is a
  bundle update on every receiver.

## Deferred

* **Replay detection.** `jti` is unique per token and exposed as `token_id`, but there is
  no replay cache yet; the 300 s ceiling bounds replay. Receivers of state-changing,
  non-idempotent internal operations should add a `jti` cache when those operations exist.
* **Key distribution automation** (secret manager / bundle publishing) belongs to
  deployment work; no production keys or bundles are created by this change.
* **mTLS** may be layered underneath later; it does not replace token authentication.
