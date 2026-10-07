# ADR 0003: Calling platform consumer, credential and principal trust model

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part III, Part B §7–§10, implemented in `services/calling/`

## Context

Calling, and later Privacy Relay and telephony, will be consumed by CosmicForge's own
products and by external customers through developer APIs. Before any call exists, the
platform needs an authoritative answer to: which customer, which application, which
environment, which machine identity, which credential, which permissions — and, separately,
which communication participant.

The Meeting Service has an `api_keys` table that stores keys in plaintext and binds them to
meeting-service users. That design is not reused.

## Decision

### 1. A new, independent `services/calling/` service

The foundation lives in `services/calling/` (Rust, Axum, SeaORM, PostgreSQL), a peer of
`services/meeting-service/`. It has its own migration history (recorded in
`calling_seaql_migrations`), its own database and its own entities. It never imports
Meeting Service code, never reads Meeting Service tables and does not use Meeting Service
users or `ActorContext` for identity. Architecture tests and a CI guard enforce this.

### 2. Consumer hierarchy

`Tenant → Application → Environment (SANDBOX | TEST | PRODUCTION) → Service account → API
credential`, plus allowed scopes per application and principals per environment.

* Every child row stores its full ownership chain and references its parent through a
  composite foreign key on `(…ownership…, id)`. Ownership, slugs, environment kind and
  tenant classification are immutable (trigger-enforced). Mismatched or re-parented rows
  are impossible at the database level, not only in service code.
* One environment of each kind per application. `live_telecom_allowed` is
  server-authoritative, defaults to `false`, and can be `true` only for PRODUCTION
  (`CHECK` + service validation).
* Statuses are `CHECK`-constrained; service-account and credential revocation are terminal.

### 3. First-party and external consumers share one model

FIRST_PARTY / EXTERNAL is a persisted tenant trust classification. There are no
product-specific tables, types, APIs or name checks. Privileges for first-party products
exist only as explicitly granted scopes whose registry entry is marked
`FirstPartyOnly`.

### 4. Credentials

* Format `cfmk_<sbx|test|live>_<public id>_<secret>`: 12-byte random public lookup id,
  32-byte CSPRNG secret, both hex. The environment tag is a convenience; the persisted
  environment binding is authoritative.
* Verifier: `HMAC-SHA-256(pepper, "cfmeet.calling.api-credential.v1" ‖ tag ‖ public id ‖
  secret)`, stored with `verifier_algorithm` and `verifier_key_version`. The pepper comes
  only from deployment secret management (`API_CREDENTIAL_PEPPER`); short, low-variety or
  placeholder values are rejected; there is no fallback. A previous pepper version can be
  configured during pepper rotation. Comparison is constant time.

  *Rationale.* The secret already has 256 bits of entropy, so a slow password hash adds
  latency without security benefit; a keyed MAC additionally makes a leaked database
  useless for offline guessing without the pepper. Binding the tag means an edited prefix
  never verifies.
* The raw credential is returned exactly once (issuance / rotation). Secret containers
  redact `Debug`, are not `Serialize`/`Display`, and are zeroized; database error
  summaries contain only SQLSTATE and constraint names.
* Authentication verifies the whole chain on every request (no cache): verifier,
  credential state (ACTIVE, not revoked, `not_before ≤ now < expires_at`, inside any
  rotation overlap), and service account, environment, application and tenant ACTIVE.
  Clients receive one normalized `AUTHENTICATION_FAILED`; reasons are logged for operators
  with the public id only.
* Scopes: strings validated against a server-side registry; credential scopes reference
  the application's allowed scopes by foreign key (withdrawing an application scope
  cascades immediately). The authenticated context carries only persisted scopes.
* Rotation: transactional, row-locked; creates an independent successor for the same
  service account (scopes re-validated, credential limits copied) and sets the
  predecessor's `supersession_valid_until = min(expires_at, now + overlap)` (never
  extended, `CHECK`-enforced). One successor per credential (unique constraint).
  Changing authority means issuing a credential to another service account and revoking
  the old one.
* Revocation: immediate and idempotent.

### 5. Limits

Application- and credential-level limit rows (requests per minute, concurrent
communications, daily / monthly telecom spend in minor units of the tenant's immutable
billing currency). Effective policy: stricter configured value wins; otherwise the
configured one; otherwise the platform default. Telecom spend defaults to zero and is zero
wherever live telecom is not allowed. Request rate is enforced now with shared PostgreSQL
fixed one-minute windows (per credential against the effective limit, per application
against the application-wide limit). Concurrent-communication and spend enforcement are
deferred to the operations that consume them.

### 6. Client identity is not participant identity

`PlatformAuthContext` (tenant, classification, application, environment, environment
kind, live-telecom flag, service account, credential, scopes, request id) is built only
from persisted state by the authentication middleware; it cannot be constructed outside
the crate or deserialized.

`ResolvedPrincipal` is a different type, produced only by
`PrincipalResolver::resolve(ctx, reference)`, which filters on the context's tenant,
application and environment and takes no identifiers from the caller. A principal outside
that scope is indistinguishable from a nonexistent one. Authentication never creates a
principal; a service account is a principal only through explicit registration as a linked
`SERVICE_ACCOUNT` principal.

Principal categories: NATIVE_CF_USER, EXTERNAL_APPLICATION_PRINCIPAL, ENTERPRISE_MEMBER,
SERVICE_ACCOUNT, TELEPHONE_ENDPOINT, TEMPORARY_EXTERNAL_PARTICIPANT, BOT_OR_SERVICE.
References are opaque and scoped-unique. No plaintext contact data: there is no phone
column, E.164-style and `tel:`/`sip:` references are refused, and TELEPHONE_ENDPOINT
references must be opaque UUID handles (the future Contact Vault issues them).

### 7. Administration

No public credential-management endpoints exist until internal service authentication
(§11) and the developer-platform administration model exist. Operators use the
`platform_admin` CLI, which calls the same domain services.

## Consequences

* Future Calling APIs mount behind `require_platform_credential`, call
  `ctx.require_scope(...)`, and resolve participants through `PrincipalResolver`.
* Future telecom work must check `ctx.live_telecom_allowed()` and the resolved spend
  limits; non-production environments structurally cannot be enabled.
* Losing the pepper invalidates every credential; it must be backed up in secret
  management. Pepper rotation uses the previous-pepper slot and credential rotation.
* Request-rate enforcement adds one upsert per authenticated request; counters live in an
  UNLOGGED table pruned by the service.
* Not decided here: internal service authentication (§11), shared contract authority
  (§12), cross-service data ownership (§13), the Contact Vault, and the call model (Part C).
  §11 is decided in [ADR 0004](0004-internal-service-authentication.md) and §12–§14 in
  [ADR 0005](0005-contract-authority-and-runtime-boundaries.md).
