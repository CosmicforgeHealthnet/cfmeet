# CF Meet

**CosmicForge Tech Limited — unified communications platform**

CF Meet is evolving from a real-time meeting product into CosmicForge's unified communications platform: meetings and conferencing, realtime calling, messaging, Privacy Relay communications, telephony, Digital Communications Numbers, enterprise communications and developer communications APIs.

This monorepo contains the platform's applications, domain services, shared contracts, infrastructure and documentation.

## Current vs planned capabilities

| Status | Capability |
|---|---|
| **Current** | Video meetings: scheduled and instant meetings, pre-join, LiveKit rooms, waiting room/admission, participant list and media state, screen share, in-meeting chat |
| **Current** | Accounts: email/password with verification, Google OAuth, password reset |
| **Current** | Developer basics: API keys, external meeting create/join API, webhook registration, OpenAPI/Swagger |
| **Planned** | Realtime app-to-app calling |
| **Planned** | Unified messaging |
| **Planned** | Privacy Relay communications |
| **Planned** | SIP/PSTN telephony and Digital Communications Numbers |
| **Planned** | Enterprise communications, admin console, developer portal / CPaaS APIs and SDKs |
| **Future** | Intelligence: transcription, translation and meeting/call assistance |

Planned items are not implemented yet.

## Repository layout

```text
cfmeet/
├── apps/
│   ├── web/                  Next.js web application (deploys to Vercel)
│   └── mobile/               Expo React Native app (deploys via EAS)
├── services/
│   └── meeting-service/      Rust/Axum backend API (deploys to VPS via Docker)
├── packages/
│   └── contracts/            Shared API / OpenAPI contracts
├── infra/
│   └── livekit/              LiveKit local development config
├── docs/
│   ├── README.md             Deployment documentation index
│   ├── INFRASTRUCTURE.md     Ubuntu VPS deployment guide
│   ├── BACKEND.md            Backend deployment options
│   ├── VERCEL.md             Vercel deployment guide
│   ├── MOBILE.md             Expo/EAS deployment guide
│   ├── ENVIRONMENT.md        Environment variables reference
│   └── TROUBLESHOOTING.md    Common issues and solutions
├── scripts/
├── .github/workflows/
├── docker-compose.yml
└── Makefile
```

## Current deployable applications/services

| Path | Responsibility | Deploys as |
|---|---|---|
| `apps/web/` | CF Meet web experience | Vercel |
| `services/meeting-service/` | Meeting/conference control plane: auth, meetings, participants, waiting room, chat, LiveKit token/control integration, API keys, webhooks, workers and related meeting-domain functionality | Docker image |

The current Rust service is intentionally named **meeting-service** because it is the backend for the meeting/conferencing domain, not the universal backend for all future CF Meet capabilities.

Future services such as `calling`, `privacy-relay`, `messaging`, `telephony` and `numbering` will be added under `services/` when their implementation phases begin. They are not scaffolded prematurely.

## Phase 2 implementation home

**Phase 2 — Meeting & Conference Platform** is implemented primarily in:

- `services/meeting-service/` — backend/control-plane implementation
- `apps/web/` — meeting/conference user experience
- `packages/contracts/` — shared API/event/permission contracts
- `infra/livekit/` — media-plane configuration and infrastructure
- `docs/phases/phase-02/` — implementation specification, decisions and acceptance evidence

Phase 2 does **not** belong in future calling, telephony, numbering or messaging services.

## Service evolution

The current meeting service remains one deployable **modular monolith** during Phase 2. We do not split auth, participants, waiting room or permissions into separate microservices merely for structure.

Future domain services are created only when a real boundary exists, for example:

```text
services/
├── meeting-service/       current
├── calling/               future
├── privacy-relay/         future
├── messaging/             future
├── telephony/             future
├── numbering/             future
├── webhook-delivery/      future if extracted
└── workers/               future if extracted
```

See the [docs](docs/README.md) folder for deployment guides.

## Prerequisites

- Rust stable with `rustfmt` and `clippy`
- Node.js 22 and npm
- Docker with Compose v2, or local PostgreSQL, Redis and LiveKit

## Environment files

```bash
cp services/meeting-service/.env.example services/meeting-service/.env
cp apps/web/.env.example                 apps/web/.env.local
```

`services/meeting-service/.env.example` documents PostgreSQL, Redis, JWT, LiveKit, OAuth, SMTP, CORS and application URL configuration. `apps/web/.env.example` documents `NEXT_PUBLIC_API_URL`.

## Local development

Start dependencies:

```bash
make deps
```

Run migrations and the meeting service:

```bash
make migrate
make meeting-service
curl http://localhost:8080/health
```

`make backend` remains as a temporary compatibility alias for `make meeting-service`.

Run the web app:

```bash
make frontend
```

Run everything together:

```bash
make dev
# or
docker compose --profile app up -d --build
```

Swagger UI: `http://localhost:8080/swagger-ui/`
OpenAPI: `http://localhost:8080/api-docs/openapi.json`

## Deployment roots

- Vercel Root Directory: `apps/web`
- Meeting service Docker build context: `services/meeting-service/`
- Meeting service Dockerfile: `services/meeting-service/Dockerfile`

The web app and meeting service remain independently deployable and communicate over published APIs only.

## CI

Workflows under `.github/workflows/` are path-aware:

- **Meeting service:** `services/meeting-service/**` → fmt, clippy, test, build and Docker image
- **Frontend:** `apps/web/**` → install, lint, typecheck and Next.js build
- **Compose:** root compose/infra/meeting-service Dockerfile validation

## Documentation

- [Deployment Guide](docs/README.md) - Complete deployment documentation
- [Infrastructure Setup](docs/INFRASTRUCTURE.md) - Ubuntu VPS deployment guide
- [Backend Deployment](docs/BACKEND.md) - Meeting service deployment
- [Vercel Deployment](docs/VERCEL.md) - Web app deployment
- [Mobile Deployment](docs/MOBILE.md) - Expo/EAS deployment
- [Environment Variables](docs/ENVIRONMENT.md) - Configuration reference
- [Troubleshooting](docs/TROUBLESHOOTING.md) - Common issues
- [Meeting Service Docs](services/meeting-service/docs/README.md) - API documentation
- [OpenAPI Contracts](packages/contracts/README.md) - API contracts
