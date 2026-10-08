# Platform boundaries

This document defines where each CF Meet capability belongs. Anything marked *planned* is a target boundary only: no placeholder project or deployable unit should be created until real implementation work begins.

## Services (`services/`)

```text
services/
├── meeting-service/     ✅ current Meeting & Conference Platform backend
├── calling/             ◐ active: trust foundation, internal service auth, Calling core (Part C §15–§19), application calling (§20–§25) and the Privacy Relay ingress / protected calls (Part D §26–§30) implemented; telephony execution planned
├── privacy-relay/       ◐ active: Communication Contexts, reassignment / revocation, Privacy Relay Sessions, authorization and active-call enforcement (Part D §26–§30); Contact Vault (§31–§33) planned
├── messaging/           ◻ planned: unified messaging
├── telephony/           ◻ planned: SIP/PSTN telephony
├── numbering/           ◻ planned: Digital Communications Numbers
├── webhook-delivery/    ◻ future extraction if needed
└── workers/             ◻ future extraction if needed
```

## Meeting service

`services/meeting-service` is the current Rust/Axum backend imported from the original RTC project and is the primary backend implementation home for **Phase 2 — Meeting & Conference Platform**.

During Phase 2 it remains **one deployable modular monolith**. Its current internal domains include:

| Domain | Current location |
|---|---|
| Meeting authentication/account flows used by the product | meeting-service |
| Meetings and scheduling | meeting-service |
| Participants and participant state | meeting-service |
| Waiting room/admission | meeting-service |
| In-meeting chat | meeting-service |
| LiveKit integration | meeting-service |
| API keys / external meeting API | meeting-service |
| Webhook registration | meeting-service |
| Admin/user functionality inherited from RTC backend | meeting-service |
| Email and meeting lifecycle workers | meeting-service |

Phase 2 may add internal modules for Meeting Spaces, Conference Sessions, memberships/invitations, roles/permissions, scheduling/recurrence, recording orchestration, breakouts, polls, Q&A, webinar controls, attendance, realtime event delivery and related meeting features. These remain modules of `meeting-service` unless an explicit later ADR approves extraction.

The name **meeting-service** is deliberate. This service is not the universal backend for future calling, Privacy Relay, messaging, telephony or numbering. Those domains receive their own services when their implementation phases begin.

## Calling service

`services/calling` is an independent Rust/Axum service (own database, own migration
history, no source or table dependency on `meeting-service`). It currently implements the
platform trust foundation of Part B §7–§10: tenants, applications, SANDBOX / TEST /
PRODUCTION environments, service accounts, HMAC-verified API credentials with
server-authoritative scopes, rotation and revocation, application / credential limit
policy, and communication principals resolved separately from API client identity. It
also authenticates internal service calls on `/internal/v1/*` and signs its own outbound
calls with `packages/service-auth` (Part B §11). The Calling core (Part C §15–§19) is
implemented: the canonical, provider-independent Call aggregate and its 17-state machine,
participants / sessions / connection intervals, routes / legs / attempts,
authorization decisions, quality and usage records, immutable call events with a
transactional outbox, and the call API. Application calling (Part C §20–§25) is
implemented too: endpoints and signaling credentials, realtime signaling, multi-device
ringing with single-winner arbitration, app voice / video / group calls through
Calling's own provider-neutral media boundary (LiveKit adapter; no Meeting Service
dependency), and call control / transfer. Telephony execution is later work. See
[ADR 0003](../decisions/0003-calling-platform-trust-model.md),
[ADR 0004](../decisions/0004-internal-service-authentication.md),
[ADR 0006](../decisions/0006-canonical-call-aggregate-and-state-machine.md),
[ADR 0007](../decisions/0007-calling-media-provider-boundary.md),
[ADR 0008](../decisions/0008-calling-endpoints-ringing-and-answer-arbitration.md),
[ADR 0009](../decisions/0009-calling-call-control-idempotency-and-transfer.md) and
[`services/calling/README.md`](../../services/calling/README.md).

## When to create or extract another service

Create a separate `services/<name>/` only when a concrete domain/operational reason exists:

1. **Distinct business/domain responsibility** — e.g. public telephony, number lifecycle or unified messaging.
2. **Scaling** — the load profile differs materially from meetings.
3. **Security/isolation** — credentials or regulated identity mappings need a smaller blast radius.
4. **Runtime/protocol** — the capability requires a different runtime or network stack, such as SIP/PSTN integration.
5. **Failure isolation** — its outage must not take meetings down.
6. **Deployment cadence** — independent releases provide real operational value.

Record significant extractions in `docs/decisions/` and define cross-service contracts in `packages/contracts`.

## Application surfaces (`apps/`)

```text
apps/
├── web/                 ✅ current CF Meet web app
├── admin/               ◻ planned
├── developer-portal/    ◻ planned
└── mobile/              ◻ planned
```

Applications communicate with services through published HTTP/WebSocket/event contracts. They do not import service source code.

## Shared packages (`packages/`)

```text
packages/
├── contracts/           ✅ contract authority: manifest, JSON Schema 2020-12 / OpenAPI contracts, offline validator
└── service-auth/        ✅ internal service authentication library (Ed25519 service tokens, trust bundles)
```

`packages/contracts` is the authority for APIs, event schemas, webhook schemas, permission identifiers and future SDK generation inputs that cross component boundaries ([ADR 0005](../decisions/0005-contract-authority-and-runtime-boundaries.md)). `packages/service-auth` is linked by every Rust service that calls or is called by another service ([ADR 0004](../decisions/0004-internal-service-authentication.md)). Packages never depend on services.

## Dependency rules

| From → To | Allowed? |
|---|---|
| apps/* → services/* source code | ❌ Never; use APIs/realtime protocols |
| services/* → apps/* source/build output | ❌ Never |
| service → service source import / Cargo path dependency | ❌ Never; use authenticated internal APIs or durable events |
| service → another service's database, tables or database URL | ❌ Never ([service data ownership](service-data-ownership.md)) |
| service → `packages/*` | ✅ |
| `packages/*` → services/* | ❌ Never |
| apps/services → generated shared contracts | ✅ |
| contracts → application/service implementation | ❌ Never |

Synchronous service-to-service calls use `X-CF-Service-Authorization` service tokens
against `/internal/v1/` endpoints; asynchronous integration uses the
`cfmeet.platform.event-envelope` contract. The Meeting Service is not an identity
provider for other services. Enforced by `make service-boundaries`
(`.github/workflows/service-boundaries.yml`).

## Infrastructure (`infra/`)

| Path | Responsibility |
|---|---|
| `infra/livekit/` | LiveKit/media-plane configuration |
| `infra/docker/` | local/container orchestration guidance |
| `infra/deployment/` | deployment guidance; web root `apps/web`, meeting-service context `services/meeting-service/` |

## Phase 2 ownership map

| Phase 2 concern | Primary location |
|---|---|
| Backend/control plane | `services/meeting-service/` |
| Meeting UI | `apps/web/` |
| Shared API/event/permission contracts | `packages/contracts/` |
| LiveKit/media configuration | `infra/livekit/` |
| Specification and acceptance evidence | `docs/phases/phase-02/` |
