# packages/contracts

**Status: contract authority (ADR 0005).** Every shared contract is registered in
[`manifest.json`](manifest.json) and validated offline by [`validator/`](validator):
`make contracts-validate` (CI: `.github/workflows/contracts.yml`).

This package is the permanent home for shared, externally visible contracts across the CF Meet platform. Anything multiple services/apps or external integrators must agree on belongs here rather than being hand-copied between codebases.

## Registered contracts

| Id | Version | Kind | Path | Authority |
|---|---|---|---|---|
| `cfmeet.meeting-service.http-api` | 1.1.0 | OpenAPI 3.0 | [`openapi/meeting-service.json`](openapi/meeting-service.json) | generated from `services/meeting-service` (`export_openapi`, diffed by `backend.yml`) |
| `cfmeet.calling.http-api` | 1.6.0 (beta) | OpenAPI 3.1 | [`calling/http/v1.openapi.json`](calling/http/v1.openapi.json) | contract-first; exactly the implemented Calling routes: health, call API, call creation / control (incl. protected calls), endpoints, signaling credentials, endpoint signaling routes, the media connection gateway, media provider callback, Privacy Relay active-call enforcement (internal), protected public-network contact targets and the callback relay (Part E) and the Telephony event receiver (internal); security schemes and the error envelope. The Privacy Relay ingress routes Calling serves are specified by `cfmeet.privacy-relay.http-api` |
| `cfmeet.calling.call-common` | 1.1.0 (beta) | JSON Schema 2020-12 | [`calling/common/v1.schema.json`](calling/common/v1.schema.json) | contract-first; canonical call vocabulary (17 call states, failure categories, terminal reasons, event types, ring-offer / media-access / transfer / endpoint vocabularies, call representation, media capabilities, safe event data), defined once |
| `cfmeet.calling.call-lifecycle-event` | 1.0.0 (beta) | JSON Schema 2020-12 | [`calling/events/call-lifecycle/v1.schema.json`](calling/events/call-lifecycle/v1.schema.json) | contract-first; payload of every Calling outbox event (inside the event envelope) |
| `cfmeet.calling.call-control` | 1.3.0 (beta) | JSON Schema 2020-12 | [`calling/control/v1.schema.json`](calling/control/v1.schema.json) | contract-first; request / response bodies of the application-calling API (creation incl. APP_TO_PSTN and CALLBACK_RELAY, answers, control, transfer, media credentials, endpoints, signaling credentials, recovery) |
| `cfmeet.calling.realtime-signal` | 1.0.0 (beta) | JSON Schema 2020-12 | [`calling/realtime/v1.schema.json`](calling/realtime/v1.schema.json) | contract-first; frames of the endpoint signaling channel (call signals, control frames) |
| `cfmeet.calling.push-invitation` | 1.0.0 (experimental) | JSON Schema 2020-12 | [`calling/push/invitation/v1.schema.json`](calling/push/invitation/v1.schema.json) | contract-first; transport-neutral push-ready ring offer |
| `cfmeet.platform.common` | 1.3.0 | JSON Schema 2020-12 | [`platform/common/v1.schema.json`](platform/common/v1.schema.json) | contract-first |
| `cfmeet.platform.service-token-header` | 1.0.0 | JSON Schema 2020-12 | [`platform/service-auth/token-header/v1.schema.json`](platform/service-auth/token-header/v1.schema.json) | contract-first |
| `cfmeet.platform.service-token-claims` | 1.0.0 | JSON Schema 2020-12 | [`platform/service-auth/token-claims/v1.schema.json`](platform/service-auth/token-claims/v1.schema.json) | contract-first |
| `cfmeet.platform.delegated-platform-context` | 1.0.0 | JSON Schema 2020-12 | [`platform/service-auth/delegated-platform-context/v1.schema.json`](platform/service-auth/delegated-platform-context/v1.schema.json) | contract-first |
| `cfmeet.platform.service-trust-bundle` | 1.0.0 | JSON Schema 2020-12 | [`platform/service-auth/trust-bundle/v1.schema.json`](platform/service-auth/trust-bundle/v1.schema.json) | contract-first |
| `cfmeet.platform.internal-http` | 1.0.0 | OpenAPI 3.1 (components) | [`platform/internal-http/v1.openapi.json`](platform/internal-http/v1.openapi.json) | contract-first |
| `cfmeet.platform.event-envelope` | 1.0.2 | JSON Schema 2020-12 | [`platform/events/envelope/v1.schema.json`](platform/events/envelope/v1.schema.json) | contract-first; payloads registered by producers (Calling: `cfmeet.calling.call-lifecycle-event`; Privacy Relay: `cfmeet.privacy-relay.event`) |
| `cfmeet.privacy-relay.http-api` | 1.0.0 (beta) | OpenAPI 3.1 | [`privacy-relay/http/v1.openapi.json`](privacy-relay/http/v1.openapi.json) | contract-first; the public `/v1/privacy-relay/...` API (served by Calling's platform ingress, Part D §26–§30) |
| `cfmeet.privacy-relay.internal-api` | 1.0.0 (beta) | OpenAPI 3.1 | [`privacy-relay/internal/v1.openapi.json`](privacy-relay/internal/v1.openapi.json) | contract-first; Privacy Relay's service-authenticated `/internal/v1/...` API and `/health` |
| `cfmeet.privacy-relay.common` | 1.0.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/common/v1.schema.json`](privacy-relay/common/v1.schema.json) | contract-first; statuses, channels, strategies, subject references (principal id or opaque ContactReference id — never a contact value), versions, enforcement state |
| `cfmeet.privacy-relay.context` | 1.0.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/context/v1.schema.json`](privacy-relay/context/v1.schema.json) | contract-first; Communication Context requests / representations / evidence |
| `cfmeet.privacy-relay.session` | 1.0.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/session/v1.schema.json`](privacy-relay/session/v1.schema.json) | contract-first; Privacy Relay Session requests / representations |
| `cfmeet.privacy-relay.authorization` | 1.1.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/authorization/v1.schema.json`](privacy-relay/authorization/v1.schema.json) | contract-first; `communication.authorize` request / decision (1.1.0: `contactFacts` — country / verification of protected contacts on an authorized public-network decision) |
| `cfmeet.privacy-relay.event` | 1.0.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/events/v1.schema.json`](privacy-relay/events/v1.schema.json) | contract-first; payload of every Privacy Relay outbox event (inside the event envelope) |
| `cfmeet.privacy-relay.enforcement-command` | 1.0.0 (beta) | JSON Schema 2020-12 | [`privacy-relay/internal/enforcement/v1.schema.json`](privacy-relay/internal/enforcement/v1.schema.json) | contract-first; Privacy Relay → Calling active-call enforcement command / result (received by Calling) |
| `cfmeet.telephony.common` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/common/v1.schema.json`](telephony/common/v1.schema.json) | contract-first; provider-neutral public-network vocabulary (leg statuses incl. UNKNOWN, route classes, end reasons, failure categories, event types, bridge kinds, route health) |
| `cfmeet.telephony.inbound-offer` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/inbound/v1.schema.json`](telephony/inbound/v1.schema.json) | contract-first; phone-to-app offer (Telephony → Calling) and Calling's answer — references only |
| `cfmeet.telephony.event` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/events/v1.schema.json`](telephony/events/v1.schema.json) | contract-first; normalized Telephony events delivered to Calling (leg status, bridge, usage) and Calling's disposition — CF Meet ids and normalized vocabulary only |
| `cfmeet.telephony.command` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/commands/v1.schema.json`](telephony/commands/v1.schema.json) | contract-first; Calling → Telephony commands (ORIGINATE, TERMINATE / HOLD / RESUME, BRIDGE), leg / bridge views and the error envelope — versioned, idempotent, no caller identity / number / provider |
| `cfmeet.telephony.routes` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/routes/v1.schema.json`](telephony/routes/v1.schema.json) | contract-first; normalized route availability (opaque route refs, health, KNOWN / UNKNOWN evidence) |
| `cfmeet.telephony.usage` | 1.0.0 (beta) | JSON Schema 2020-12 | [`telephony/usage/v1.schema.json`](telephony/usage/v1.schema.json) | contract-first; Telephony's restricted per-leg usage record (cost known only when reported) |
| `cfmeet.telephony.internal-http-api` | 1.0.0 (beta) | OpenAPI 3.1 | [`telephony/internal/v1.openapi.json`](telephony/internal/v1.openapi.json) | contract-first; exactly the implemented Telephony routes: health, provider callback ingress, the service-authenticated /internal/v1 API for Calling |

Schemas have `examples/` (must validate) and `counter-examples/` (must be rejected).
Implementations are checked against these contracts in both directions by
`packages/service-auth/tests/contract_conformance.rs` and
`services/calling/tests/http_contract.rs` (which also validates real call responses and
every Calling outbox envelope / lifecycle payload).

## Rules

1. Each contract has exactly one authority: generated from code, or contract-first (the file is the truth).
2. Ids are `cfmeet.<owner>.<name>`; the owner is `platform` or an existing `services/<owner>`.
3. Contract-first files are `v<major>.schema.json` / `v<major>.openapi.json`. A breaking change is a new major file registered next to the old one; compatible changes bump the manifest version.
4. `$schema` is JSON Schema 2020-12; `$id` is `https://schemas.cfmeet.com/<path>`; cross-contract `$ref`s use those ids and resolve offline.
5. No unregistered JSON outside `examples/` / `counter-examples/`.
6. Contracts describe what exists. Do not register Contact Vault, relay-number, Telephony or other contracts before their domain is implemented (the call domain and its lifecycle event exist since Part C §15–§19; Privacy Relay contexts, sessions, authorization and enforcement since Part D §26–§30).
7. Generated code is not manually edited.

The validator (`validator/`, Rust, no Docker, no network) enforces 2–5 plus meta-schema
validity, compilation, `$ref` resolution, examples / counter-examples, OpenAPI 3.0 / 3.1
meta-validation against vendored schemas ([`validator/meta/`](validator/meta)) and
`info.version` agreement. Its tests mutate copies of a contract tree and expect each rule
to fire.

## Responsibilities

| Contract type | Examples | Status |
|---|---|---|
| HTTP API contracts | Meeting Service OpenAPI; Calling OpenAPI | Meeting Service: generated by `cargo run --bin export_openapi` in `services/meeting-service`, CI fails if the checked-in file differs from the code. Calling: contract-first, conformance-tested |
| Internal platform contracts | service token, delegated context, trust bundle, internal HTTP | Registered (ADR 0004) |
| Event schemas | durable event envelope; call lifecycle payloads; meeting/participant/session lifecycle payloads | Envelope and call lifecycle payload registered; meeting payloads planned |
| Webhook definitions | payloads delivered to customer webhooks | Planned |
| Permission identifiers | internal service permissions (`cfmeet.platform.common`); role/permission keys shared by service and UI | Internal permissions registered; UI keys planned |
| Shared enums | call states and call vocabulary (`cfmeet.calling.call-common`); meeting/session/participant states | Call vocabulary registered; meeting enums planned |
| External protocol definitions | published signalling/control formats | Calling realtime signals and push-ready offers registered |
| SDK generation sources | future client SDK inputs | Planned |

## Current Meeting Service authority

The current Meeting Service HTTP API contract is defined in Rust using `utoipa` annotations and collected in:

`services/meeting-service/src/swagger/mod.rs`

A running service publishes:

- `GET /api-docs/openapi.json`
- `GET /swagger-ui/`

The TypeScript types in `apps/web/src/types/` are currently hand-written mirrors. That is existing technical debt, not the target architecture.

## Intended generation strategy

```text
services/meeting-service (Rust DTOs + utoipa)   ← authority for meeting HTTP API
        │ export
        ▼
packages/contracts/openapi/meeting-service.json
        │ codegen
        ├──► generated TS contracts → apps/web
        └──► future public SDK generators

packages/contracts/events/*.schema.json
        │ codegen
        ├──► Rust consumers
        └──► TypeScript consumers
```

Rules:

1. Each contract has one authority.
2. Generated code is not manually edited.
3. Cross-service event/webhook schemas live here when they become shared platform contracts.
4. Once consumers are wired to this package, `packages/contracts/**` must trigger the relevant CI workflows.
5. Phase 2 meeting-specific contracts should be introduced here only when they cross the Meeting Service/Web boundary or become externally published contracts; internal Rust implementation types remain inside `services/meeting-service`.

Calling's HTTP, call-vocabulary and call-lifecycle-event contracts, Privacy Relay's context / session / authorization / event / enforcement contracts and the platform service-auth / event-envelope contracts are registered above. Future Contact Vault, messaging, telephony and numbering work will add their own externally shared contracts here, when implemented, without turning `meeting-service` into a universal platform API.
