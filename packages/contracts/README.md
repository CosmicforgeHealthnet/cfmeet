# packages/contracts

**Status: boundary established. No generated artefacts yet.**

This package is the permanent home for shared, externally visible contracts across the CF Meet platform. Anything multiple services/apps or external integrators must agree on belongs here rather than being hand-copied between codebases.

## Responsibilities

| Contract type | Examples | Status |
|---|---|---|
| HTTP API contracts | Meeting Service OpenAPI | Source exists in meeting-service; export not yet automated |
| Event schemas | meeting/participant/session lifecycle events | Planned |
| Webhook definitions | payloads delivered to customer webhooks | Planned |
| Permission identifiers | role/permission keys shared by service and UI | Planned |
| Shared enums | meeting/session/participant states | Planned |
| External protocol definitions | published signalling/control formats | Planned |
| SDK generation sources | future client SDK inputs | Planned |

## Current authority

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

Future calling, Privacy Relay, messaging, telephony and numbering services will add their own externally shared contracts here without turning `meeting-service` into a universal platform API.
