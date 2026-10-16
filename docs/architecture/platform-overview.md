# CF Meet platform overview

**CF Meet, CosmicForge Tech Limited**

CF Meet is growing from a video-meeting application into CosmicForge's unified communications platform. This document distinguishes what exists today from the planned platform boundaries.

> **Components with working code:** `apps/web` (Next.js web client),
> `services/meeting-service` (Rust/Axum Meeting & Conference Platform service),
> `services/calling` (Rust/Axum Calling service — platform trust foundation, the canonical Call aggregate, application RTC calling, routing, public-network calls through Telephony, enterprise calling, webhooks, usage and limits),
> `services/privacy-relay` (Communication Contexts, Privacy Relay Sessions, Contact Vault, temporary relay numbers) and
> `services/telephony` (public-network execution and the CF-owned SIP / media edge in `infra/telephony`).
> Part III status and its external blockers (real lawful carrier certification pending):
> [`part-l-final-closure-register.md`](part-l-final-closure-register.md).
> Every other service/app boundary below is planned until implementation begins.

## Four platform layers

```text
APPLICATION SURFACES
  apps/web ✅
  apps/mobile ◻  apps/admin ◻  apps/developer-portal ◻

PLATFORM SERVICES
  services/meeting-service ✅
  services/calling ✅
  services/privacy-relay ✅
  services/messaging ◻
  services/telephony ✅  (real lawful carrier interconnect pending)
  services/numbering ◻
  developer infrastructure ◻

SHARED CONTRACTS
  packages/contracts ✅  packages/service-auth ✅  packages/webhooks ✅  packages/object-storage ✅

INFRASTRUCTURE
  PostgreSQL ✅  Redis ✅  LiveKit ✅  SIP / media edge (Asterisk) ✅
  lawful carrier interconnect ◻ (external)  per-service metrics / incidents ✅
```

✅ implemented today   ◐ boundary established   ◻ planned

## Application surfaces

| Surface | Path | Status |
|---|---|---|
| CF Meet web app | `apps/web` | **Current.** Auth, dashboard, scheduling, pre-join, LiveKit room, waiting room, participants, chat, API keys and webhooks UI |
| Mobile apps | `apps/mobile` | Planned |
| Admin console | `apps/admin` | Planned |
| Developer portal | `apps/developer-portal` | Planned |

## Platform services

| Service | Path | Status / responsibility |
|---|---|---|
| Meeting & Conference Platform | `services/meeting-service` | **Current.** Meeting-domain control plane and Phase 2 backend. One deployable modular monolith during Phase 2. |
| Realtime app-to-app calling | `services/calling` | **Active.** Platform consumer / credential / principal trust foundation (Part B §7–§10, [ADR 0003](../decisions/0003-calling-platform-trust-model.md)) and Calling core (Part C §15–§19, [ADR 0006](../decisions/0006-canonical-call-aggregate-and-state-machine.md)): canonical Call model, state machine, events, call API; application calling (Part C §20–§25, ADRs [0007](../decisions/0007-calling-media-provider-boundary.md), [0008](../decisions/0008-calling-endpoints-ringing-and-answer-arbitration.md), [0009](../decisions/0009-calling-call-control-idempotency-and-transfer.md)): endpoints, signaling, multi-device ringing, single-winner arbitration, app voice / video / group calls over Calling's own media provider boundary, call control and transfer; Part D §26–§30: the public Privacy Relay ingress, PRIVACY_RELAY_V1 protected calls and transfers (fail closed) and the active-call enforcement endpoint; Part E: the Routing Engine and Calling's side of public-network calls executed by Telephony; Parts F–J: enterprise calling, platform API / webhooks / usage / limits, security, reliability and persistence closure ([`part-f-certification.md`](part-f-certification.md) … [`part-k-production-acceptance.md`](part-k-production-acceptance.md)). |
| Privacy Relay communications | `services/privacy-relay` | **Active.** Part D §26–§30 ([ADR 0010](../decisions/0010-communication-context-and-privacy-relay-session-architecture.md)): generic Communication Contexts (participants, channels, directions, validity, policy; reassignment and revocation without replacement), Privacy Relay Sessions that can only narrow their context, recorded communication authorization for Calling's protected calls, gap-free audit + outbox, durable active-call enforcement. Own database; internal API only (customers reach it through Calling), except the Contact Vault routes, which take a Calling-minted Contact Vault access token. §31–§36 ([ADR 0011](../decisions/0011-contact-vault-cryptography-and-resolution.md), [ADR 0012](../decisions/0012-temporary-relay-number-assignment-and-cooldown.md)): encrypted protected contacts with scoped keyed equality, trusted verification, Contact Resolution Grants and Telephony-only resolution, a temporary relay-number pool with cooldown and inbound mapping, privacy-safe call evidence (composed by Calling). |
| Unified messaging | `services/messaging` | Planned |
| SIP/PSTN telephony | `services/telephony` | **Active.** Part E §39–§51 ([ADR 0014](../decisions/0014-telephony-service-core.md)–[ADR 0017](../decisions/0017-provider-failover.md)): provider-independent leg execution, durable idempotent commands with a transmit fence and UNKNOWN outcome, provider abstraction (SIP-trunk and deterministic adapters), authenticated provider events, reconciliation, failover; the CF-owned SIP / media edge (Asterisk + ARI, `infra/telephony/`). Real lawful carrier certification is pending an external interconnect ([ADR 0016](../decisions/0016-first-telecom-interconnect.md)). |
| Digital Communications Numbers | `services/numbering` | Planned |
| Webhook delivery / workers | future service if extracted | Planned only when operationally justified |
| Communications intelligence | future | Transcription, translation and assistance |

The meeting service currently contains some cross-cutting functionality inherited from the original RTC backend—such as auth, users, admin, API keys, webhook registration and email workers. Those capabilities stay inside the meeting service for Phase 2 unless a later ADR establishes a real extraction boundary.

The service is named by its **primary domain responsibility**, not `core-api`, so developers do not mistake it for the permanent backend of calling, messaging, telephony, numbering or Privacy Relay.

## Phase 2 architecture

Phase 2 — Meeting & Conference Platform — uses:

```text
apps/web
   │ HTTP/realtime control APIs
   ▼
services/meeting-service
   ├── PostgreSQL
   ├── Redis
   └── LiveKit control/token integration
          │
          ▼
     LiveKit media plane
```

Shared contracts belong in `packages/contracts`; media infrastructure/configuration belongs in `infra/livekit`; Phase 2 specifications and closure evidence belong in `docs/phases/phase-02/`.

## Shared contracts

`packages/contracts` is the home for contracts shared across services/apps or exposed externally: API contracts, event schemas, webhook schemas, permission identifiers, shared enums and SDK generation inputs.

Today, the meeting HTTP API authority is the utoipa/OpenAPI definition generated by `services/meeting-service`. The web application still contains hand-written TypeScript mirrors; replacing those with generated contracts is tracked as platform technical debt.

## Infrastructure

| Component | Status | Where |
|---|---|---|
| PostgreSQL / SeaORM | Current | meeting-service database/migrations |
| Redis | Current | meeting-service runtime dependencies |
| LiveKit | Current | meeting media plane; config in `infra/livekit/` |
| Local orchestration | Current | root `docker-compose.yml`, `infra/docker/` |
| Web deployment | Current | Vercel, root `apps/web` |
| Meeting-service deployment | Current | Docker context `services/meeting-service/` |
| Calling / Privacy Relay / Telephony deployment | Current (staging) | one image each, own database and roles; backup / restore per service (`infra/deployment/`) |
| SIP / media edge | Current (staging) | Asterisk + ARI configuration and certification in `infra/telephony/` |
| Lawful carrier interconnect | Pending (external) | provisioned as a Telephony `SIP_TRUNK` provider once a carrier is onboarded (ADR 0016) |
| Observability | Current, per service | bounded metrics listeners, durable incidents, correlation ids (ADR 0031); a cross-platform observability stack is future work |

## Current request flow

```text
Browser (apps/web)
      │ HTTPS /api/v1
      ▼
services/meeting-service ──► PostgreSQL
      │              └─────► Redis
      │ signs/enforces LiveKit access
      ▼
LiveKit media plane
```

The web app and meeting service are independently buildable/deployable and communicate over published APIs rather than source imports.
