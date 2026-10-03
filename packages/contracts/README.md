# packages/contracts

**Status: boundary established. No generated artefacts yet.**

This package is the permanent home for the **shared, externally visible contracts**
of the CF Meet platform. Anything that more than one application or service
has to agree on, or that external integrators depend on, belongs here
instead of being hand-copied between codebases.

## Responsibilities

| Contract type | Examples | Status |
|---------------|----------|--------|
| HTTP API contracts | OpenAPI document for `services/core-api` | Source exists in core-api (see below); not exported here yet |
| Event schemas | meeting/participant/chat lifecycle events | Planned |
| Webhook event definitions | payloads delivered to customer webhook endpoints | Planned (today: `WebhookEventType` in core-api) |
| Permission identifiers | role/permission keys shared by API and UI | Planned |
| Shared enums | meeting status, participant role, media kinds | Planned |
| External protocol definitions | published signalling/control formats | Planned |
| SDK generation sources | inputs for future client SDKs (CPaaS / developer APIs) | Planned |

## Single source of truth

There must be **one** authority per contract. Do **not** hand-write a second copy of
a Rust DTO in TypeScript (or the other way round) and try to keep them in sync.

**Current authority (today):** the HTTP API contract is defined in Rust by
[`utoipa`](https://docs.rs/utoipa) annotations on the core-api handlers and DTOs, and
collected in `services/core-api/src/swagger/mod.rs` (`#[derive(OpenApi)]`). The
running service publishes it at:

- `GET /api-docs/openapi.json`: OpenAPI 3.0 document
- `GET /swagger-ui/`: interactive explorer

The web app's TypeScript types in `apps/web/src/types/` are currently
**hand-written** mirrors of those DTOs. That is existing technical debt, not the
target model.

## Intended generation strategy

```
services/core-api (Rust DTOs + utoipa annotations)      ← authority for HTTP API
        │  export (CI / script)
        ▼
packages/contracts/openapi/core-api.json                ← committed, reviewed artefact
        │  codegen
        ├──► packages/contracts/ts/    → consumed by apps/web (and future apps)
        └──► SDK generators            → future developer SDKs

packages/contracts/events/*.schema.json                 ← authority for events/webhooks
        │  codegen
        ├──► Rust types consumed by core-api / future services
        └──► TS types consumed by apps
```

Rules:

1. **HTTP API:** Rust stays the authority. The OpenAPI document gets exported into
   this package (for example via a `dump-openapi` binary or by fetching
   `/api-docs/openapi.json` in CI), and TypeScript types are generated from it.
2. **Events and webhooks:** they are not owned by a single service, so the schema
   lives here (JSON Schema) and Rust/TS types are generated from it.
3. **Generated output is never edited by hand.** CI checks that committed
   artefacts match a fresh export.
4. Once anything consumes this package, add `packages/contracts/**` to the
   path filters of the consuming workflows (see the comments in
   `.github/workflows/backend.yml` and `frontend.yml`).

Choosing the exporter and codegen tools, and replacing the hand-written TS types,
is implementation work for a later phase. It is intentionally **not** done in the
layout restructure.
