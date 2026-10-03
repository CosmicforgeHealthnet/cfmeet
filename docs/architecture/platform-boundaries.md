# Platform boundaries

This document defines where code for each platform capability lives, now and
later. Anything marked *planned* is a target boundary only: **no code, no
placeholder project and no deployable unit exists for it yet**. Do not create
empty services or apps to "reserve" these paths. Create them when real work
starts.

## Services (`services/`)

```
services/
├── core-api/           ✅ current Rust/Axum backend (modular monolith)
├── calling/            ◻ planned: realtime app-to-app calling
├── privacy-relay/      ◻ planned: Privacy Relay (masked-identity) communications
├── messaging/          ◻ planned: unified messaging
├── telephony/          ◻ planned: SIP/PSTN telephony
├── numbering/          ◻ planned: Digital Communications Numbers
├── webhook-delivery/   ◻ only if extracted from core-api
└── workers/            ◻ only if extracted from core-api
```

### core-api is a modular monolith

`services/core-api` stays **one deployable application**. Its domains are internal
modules (see `src/handlers`, `src/routes`, `src/services`, `src/models`,
`src/workers`):

| Domain | Lives in core-api today |
|--------|-------------------------|
| Auth (JWT, Google OAuth, OTP, password reset) | ✅ |
| Users / admin | ✅ |
| Meetings (scheduled, instant, auto-start/end workers) | ✅ |
| Participants, waiting room, media state, screen share | ✅ |
| Chat | ✅ |
| API keys and external `/api/v1/api` endpoints | ✅ |
| Webhooks (registration) | ✅ |
| Email queue worker | ✅ |

Do **not** split these into separate deployed services
(meeting-service, participant-service, auth-service, permission-service,
organization-service, …) at this stage. New domains (for example organizations
or permissions) start as modules inside core-api.

### When to extract a service

Extract a module into its own `services/<name>/` only when a concrete reason exists:

1. **Scaling:** its load profile differs sharply (for example media/telephony signalling or webhook fan-out).
2. **Security / isolation:** it handles regulated data or credentials that need a smaller blast radius (telephony credentials, number inventory, Privacy Relay identity mapping).
3. **Deployment cadence or runtime:** it needs a different release cycle, language or runtime (for example a SIP stack).
4. **Failure isolation:** its failure must not take down meetings.

Record each extraction as an ADR in `docs/decisions/`, and define its API and
event contracts in `packages/contracts` **before** the split.

Expected early candidates: `telephony` and `numbering` (carrier integrations,
compliance), `privacy-relay` (identity isolation), and `webhook-delivery`
(retry/fan-out load).

## Application surfaces (`apps/`)

```
apps/
├── web/                ✅ current CF Meet web app (Next.js)
├── admin/              ◻ planned: internal administration console
├── developer-portal/   ◻ planned: CPaaS developer portal
└── mobile/             ◻ planned: mobile clients
```

No placeholder Next.js or mobile projects are generated. Each app talks to
platform services over their published APIs only, and must never import service
source code.

## Shared packages (`packages/`)

```
packages/
└── contracts/          ◐ boundary established; generation strategy documented
```

See [`packages/contracts/README.md`](../../packages/contracts/README.md). Additional
shared packages (UI kit, client SDKs) get added only when there are at least two
real consumers.

## Dependency rules

| From → To | Allowed? |
|-----------|----------|
| apps/* → services/* (source) | ❌ Never. HTTP/WebSocket APIs only |
| services/* → apps/* (source or build output) | ❌ Never |
| services/* → services/* (source) | ❌ Never. APIs or events only |
| apps/*, services/* → packages/contracts (generated types) | ✅ That is its purpose |
| packages/contracts → apps/* or services/* | ❌ Never. Contracts depend on nothing |

## Infrastructure (`infra/`)

| Path | Contents |
|------|----------|
| `infra/livekit/` | LiveKit configuration (local dev today) |
| `infra/docker/` | Container build / local orchestration notes |
| `infra/deployment/` | Deployment guide (Vercel root `apps/web`, image context `services/core-api`) |
