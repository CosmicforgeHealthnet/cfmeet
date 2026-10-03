# CF Meet platform overview

**CF Meet, CosmicForge Tech Limited**

CF Meet is growing from a video-meeting application into CosmicForge's
**unified communications platform**. This document describes the target shape of
the platform and, just as importantly, what exists **today**.

> **Only two components currently contain working application code:**
> `apps/web` (Next.js web client) and `services/core-api` (Rust/Axum API).
> Every other component below is an **architectural target**, not an implemented service.

## The four layers

```
┌──────────────────────────────────────────────────────────────────────────┐
│ APPLICATION SURFACES                                                     │
│   apps/web  ✅      apps/mobile ◻    apps/admin ◻    apps/developer-portal ◻ │
├──────────────────────────────────────────────────────────────────────────┤
│ PLATFORM SERVICES                                                        │
│   services/core-api ✅  (auth, users, meetings, participants, waiting room,│
│                          chat, API keys, webhooks, workers, admin)       │
│   calling ◻  privacy-relay ◻  messaging ◻  telephony ◻  numbering ◻       │
│   developer infrastructure ◻ (webhook-delivery, CPaaS APIs, SDKs)        │
├──────────────────────────────────────────────────────────────────────────┤
│ SHARED PLATFORM CONTRACTS  — packages/contracts (boundary established)   │
│   API contracts · event contracts · webhook contracts · SDK definitions  │
├──────────────────────────────────────────────────────────────────────────┤
│ INFRASTRUCTURE                                                           │
│   PostgreSQL ✅  Redis ✅  LiveKit ✅ (self-hosted target)                 │
│   telecom interconnects ◻  observability ◻  deployment (infra/) ✅/◻       │
└──────────────────────────────────────────────────────────────────────────┘
✅ implemented today   ◻ planned / architectural target only
```

### 1. Application surfaces

| Surface | Path | Status |
|---------|------|--------|
| CF Meet web app | `apps/web` | **Current.** Auth, dashboard, scheduling, pre-join, LiveKit room, waiting room, participants, chat, API keys and webhooks UI |
| Mobile apps | `apps/mobile` | Planned |
| Admin console | `apps/admin` | Planned |
| Developer portal | `apps/developer-portal` | Planned (CPaaS docs, keys, usage) |

### 2. Platform services

| Service | Path | Status |
|---------|------|--------|
| Core API | `services/core-api` | **Current.** One deployable modular monolith (see [ADR 0001](../decisions/0001-platform-monorepo-layout.md)) |
| Realtime app-to-app calling | `services/calling` | Planned |
| Privacy Relay communications | `services/privacy-relay` | Planned |
| Unified messaging | `services/messaging` | Planned |
| SIP/PSTN telephony | `services/telephony` | Planned |
| Digital Communications Numbers | `services/numbering` | Planned |
| Developer infrastructure | `services/webhook-delivery`, `services/workers` | Planned, extracted from core-api only if needed |
| Intelligence (transcription, translation) | n/a | Future |

Service boundaries and the rules for extracting a service are in
[platform-boundaries.md](platform-boundaries.md).

### 3. Shared platform contracts

`packages/contracts` holds the contracts more than one component (or an external
integrator) depends on: API contracts, event and webhook schemas, permission
identifiers, shared enums and SDK generation sources. Today the HTTP API contract
comes from core-api's utoipa annotations (`/api-docs/openapi.json`).
See [`packages/contracts/README.md`](../../packages/contracts/README.md).

### 4. Infrastructure

| Component | Status | Where |
|-----------|--------|-------|
| PostgreSQL (SeaORM, migrations in `services/core-api/migration`) | Current | `DATABASE_URL` |
| Redis (OTP, rate limiting, token blacklist, email queue) | Current | `REDIS_URL` |
| LiveKit SFU (token signing in core-api) | Current | `LIVEKIT_*`, dev config `infra/livekit/` |
| Local orchestration | Current | root `docker-compose.yml`, `infra/docker/` |
| Deployment (Vercel for web, Docker image for core-api) | Current | `infra/deployment/` |
| Telecom interconnects (SIP trunks, carriers) | Planned | n/a |
| Observability (metrics, tracing, alerting) | Planned. Today: structured `tracing` logs only | n/a |

## Current request flow

```
Browser (apps/web) ──HTTPS /api/v1──► services/core-api ──► PostgreSQL
       │                                   │       └──────► Redis
       │                                   └─ signs LiveKit join token
       └──────────── WebRTC (LiveKit URL from join response) ─────► LiveKit SFU
```

The web app and the core API are coupled **only over HTTP**. They are built,
tested and deployed independently.
