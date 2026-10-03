# Platform boundaries

This document defines where each CF Meet capability belongs. Anything marked *planned* is a target boundary only: no placeholder project or deployable unit should be created until real implementation work begins.

## Services (`services/`)

```text
services/
├── meeting-service/     ✅ current Meeting & Conference Platform backend
├── calling/             ◻ planned: realtime app-to-app calling
├── privacy-relay/       ◻ planned: masked/private transaction communications
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
└── contracts/           ◐ shared contract boundary established
```

`packages/contracts` is the source/artefact home for APIs, event schemas, webhook schemas, permission identifiers and future SDK generation inputs that cross component boundaries.

## Dependency rules

| From → To | Allowed? |
|---|---|
| apps/* → services/* source code | ❌ Never; use APIs/realtime protocols |
| services/* → apps/* source/build output | ❌ Never |
| service → service source import | ❌ Avoid; use APIs/events/contracts |
| apps/services → generated shared contracts | ✅ |
| contracts → application/service implementation | ❌ Never |

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
