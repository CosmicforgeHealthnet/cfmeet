# ADR 0005: Contract authority, data ownership and runtime dependency direction

- **Status:** Accepted
- **Date:** 2026-10-07
- **Scope:** CF Meet Part III, Part B §12–§14

## Context

CF Meet now has two deployable services (`meeting-service`, `calling`) and will add Privacy
Relay, Telephony and others. Without explicit rules, services drift into the usual
failure modes: copied DTOs that disagree, one service reading another's tables "just for
a lookup", source imports across service crates, and synchronous call chains nobody can
reason about.

## Decision

### 1. `packages/contracts` is the contract authority (§12)

* Every shared contract is registered in
  [`packages/contracts/manifest.json`](../../packages/contracts/manifest.json) (itself
  validated by `manifest.schema.json`) with: id `cfmeet.<owner>.<name>`, semantic
  version, kind (`json-schema` | `openapi`), path, owner (`platform` or an existing
  `services/<owner>`), visibility, lifecycle and authority.
* **Authority** is either `generated` (the Meeting Service OpenAPI, exported from code by
  `export_openapi` and diffed in `backend.yml` — unchanged by this ADR) or
  `contract-first` (the file is the source of truth and implementations conform to it).
* Formats: JSON Schema **2020-12** for data; OpenAPI **3.1** for new HTTP contracts
  (the generated Meeting Service document stays 3.0). Contract-first files are named
  `v<major>.schema.json` / `v<major>.openapi.json`; a breaking change is a new major file,
  never an edit in place.
* Every schema ships examples that must validate and counter-examples that must be
  rejected (for example: wildcard audiences, self-declared permissions, `alg: none`,
  telephone numbers in a delegated context or event payload).
* Contracts registered by this ADR — only things that exist:

  | Contract | What |
  |---|---|
  | `cfmeet.platform.common` | service ids, runtime environments, request / correlation ids, permission enum, shared scalar types |
  | `cfmeet.platform.service-token-header` / `-claims` | the internal service token (ADR 0004) |
  | `cfmeet.platform.delegated-platform-context` | safe customer-scope ids signed into service tokens |
  | `cfmeet.platform.service-trust-bundle` | receiver-authoritative trust configuration |
  | `cfmeet.platform.internal-http` | internal auth headers, security scheme, internal error envelope |
  | `cfmeet.platform.event-envelope` | durable event envelope (no payload contracts yet) |
  | `cfmeet.calling.http-api` | the Calling HTTP surface as implemented: `GET /health` and the error envelope |

  **No** Privacy Relay, Telephony, call, call-leg or event-payload contract is invented
  before the domain is implemented.
* Enforcement:
  * `make contracts-validate` runs the offline Rust validator
    (`packages/contracts/validator`): manifest schema, ids / owners / paths / file
    names, no unregistered JSON, `$schema` and `$id` rules, meta-schema validation,
    compilation with an offline registry, `$ref` resolution, examples /
    counter-examples, OpenAPI meta-validation (vendored 3.0 / 3.1 schemas) and version
    agreement. Its own tests are mutation tests.
  * `packages/service-auth/tests/contract_conformance.rs` and
    `services/calling/tests/http_contract.rs` check implementations against the
    contracts in both directions — including that Calling's registered routes equal the
    contract's paths and that real responses validate.
  * CI: `.github/workflows/contracts.yml`; `calling.yml` also runs on contract changes.

### 2. Each service owns its data (§13)

Ownership is documented in
[`docs/architecture/service-data-ownership.md`](../architecture/service-data-ownership.md)
and machine-enforced by `scripts/service-boundaries/check_service_boundaries.py`
(`make service-boundaries`, `.github/workflows/service-boundaries.yml`): no cross-service
Cargo path dependency, no source include, no other service's database URL, no SQL against
another service's tables (ownership is derived from each service's migrations), no
helpers like `load_user_from_meeting_db`, and shared packages never depend on services.
The guard is itself mutation-tested against synthetic trees and single-violation copies
of this repository.

### 3. Runtime dependency direction (§14)

| Need | Mechanism |
|---|---|
| Synchronous question / command to another service | Authenticated internal HTTP (`/internal/v1/…`, ADR 0004, `InternalServiceClient`) against a contract owned by the **receiver** |
| Something happened that others may react to | Durable event in the `cfmeet.platform.event-envelope`, written transactionally (outbox) by the **producer** |
| Data owned by another service | Ask its API or keep a projection built from its events — never read its database |
| Shared code | A package in `packages/` with no dependency on any service |

Rules:

* The **Meeting Service is not an identity provider** for Calling or any new service.
  Customer identity for platform consumers comes from Calling's own credential model
  (ADR 0003); service identity comes from service tokens (ADR 0004).
* Dependencies are acyclic per operation: a synchronous chain must not call back into a
  service already on the chain; callbacks go through events or explicit
  `operational-callback.deliver` endpoints.
* Every hop gets a new request id; the correlation id is propagated unchanged across
  synchronous hops and into the `correlation_id` of any event the operation produces.
* Synchronous calls have bounded timeouts and do not follow redirects; failure of a
  downstream dependency maps to a safe error, never to a leaked upstream body.
* No outbox table, event producer or consumer is created until a real domain event
  exists; the envelope contract defines the shape they will use.

## Consequences

* Contract changes are reviewable diffs in one place, and CI catches drift between a
  contract and its implementation.
* A service can be extracted, scaled or redeployed without breaking another service's
  database queries — there are none.
* Some lookups that would be one SQL join become an API call or an event-fed projection;
  this cost is accepted.

## Deferred

* Code generation (Rust / TypeScript types) from contracts.
* Event bus / broker selection, outbox implementation and payload contracts — with the
  first real cross-service event.
* Publishing contracts at `https://schemas.cfmeet.com/` (ids are already stable URIs; the
  validator resolves them offline).
