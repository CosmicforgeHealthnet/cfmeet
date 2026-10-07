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
| **In progress** | Realtime app-to-app calling — `services/calling`: platform trust foundation (tenants, applications, environments, service accounts, API credentials, scopes, principals) implemented; call API not yet |
| **Planned** | Unified messaging |
| **Planned** | Privacy Relay communications |
| **Planned** | SIP/PSTN telephony and Digital Communications Numbers |
| **Planned** | Enterprise communications, admin console, developer portal / CPaaS APIs and SDKs |
| **Future** | Intelligence: transcription, translation and meeting/call assistance |

Planned items are not implemented yet. See [`docs/architecture/platform-overview.md`](docs/architecture/platform-overview.md).

## Repository layout

```text
cfmeet/
├── apps/
│   └── web/                  Next.js CF Meet web application                         (current)
├── services/
│   ├── meeting-service/      Rust/Axum Meeting & Conference Platform service         (current)
│   └── calling/              Rust/Axum Calling service (platform trust foundation)   (active)
├── packages/
│   ├── contracts/            Contract authority: manifest, schemas, OpenAPI, validator (authority)
│   └── service-auth/         Internal service authentication library (Ed25519)       (active)
├── infra/
│   ├── livekit/              LiveKit configuration
│   ├── docker/               Container/local orchestration notes
│   └── deployment/           Deployment guide
├── docs/
│   ├── architecture/         Platform architecture and service boundaries
│   ├── api/                  API documentation entry point
│   ├── decisions/            Architecture decision records
│   ├── phases/               Delivery-phase specifications and closure reports
│   └── migration/            Monorepo migration history
├── scripts/                  dev helpers; service-boundaries/ data-ownership guard
├── .github/workflows/
├── docker-compose.yml
└── Makefile
```

## Current deployable applications/services

| Path | Responsibility | Deploys as |
|---|---|---|
| `apps/web/` | CF Meet web experience | Vercel |
| `services/meeting-service/` | Meeting/conference control plane: auth, meetings, participants, waiting room, chat, LiveKit token/control integration, API keys, webhooks, workers and related meeting-domain functionality | Docker image |
| `services/calling/` | Calling domain service. Currently the platform trust foundation: tenant / application / environment / service-account model, API credentials, scopes, limits and communication principals ([README](services/calling/README.md)) | Docker image (context: repository root, `-f services/calling/Dockerfile`) |

The current Rust service is intentionally named **meeting-service** because it is the backend for the meeting/conferencing domain, not the universal backend for all future CF Meet capabilities.

`services/calling/` is the second domain service; it is independent of `meeting-service` (own database and migrations, no shared source). Future services such as `privacy-relay`, `messaging`, `telephony` and `numbering` will be added under `services/` when their implementation phases begin. They are not scaffolded prematurely.

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
├── calling/               active (platform trust foundation)
├── privacy-relay/         future
├── messaging/             future
├── telephony/             future
├── numbering/             future
├── webhook-delivery/      future if extracted
└── workers/               future if extracted
```

See [`docs/architecture/platform-boundaries.md`](docs/architecture/platform-boundaries.md).

## Prerequisites

- Rust stable with `rustfmt` and `clippy`
- Node.js 22 and npm
- Docker with Compose v2, or local PostgreSQL, Redis and LiveKit

## Environment files

```bash
cp services/meeting-service/.env.example services/meeting-service/.env
cp services/calling/.env.example         services/calling/.env
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
- Calling service Docker build context: repository root (`docker build -f services/calling/Dockerfile .`; the context is filtered to `packages/service-auth` and `services/calling`)

The web app and meeting service remain independently deployable and communicate over published APIs only.

## CI

Workflows under `.github/workflows/` are path-aware:

- **Meeting service:** `services/meeting-service/**` → fmt, clippy, test, build and Docker image
- **Calling service:** `services/calling/**`, `packages/service-auth/**`, Calling / platform contracts → fmt, check, clippy, PostgreSQL tests (incl. internal service auth and contract conformance), architecture guards, migration smoke, startup refusal checks, build and Docker image
- **Contracts and service auth:** `packages/contracts/**`, `packages/service-auth/**` → contract validation (`make contracts-validate`), validator mutation tests, service-auth fmt / check / clippy / tests for both feature sets, private-key scan
- **Service boundaries:** `services/**`, `packages/**` → repository data-ownership guard and its mutation tests (`make service-boundaries`)
- **Frontend:** `apps/web/**` → install, lint, typecheck and Next.js build
- **Compose:** root compose/infra/meeting-service Dockerfile validation

## Documentation

- [Platform overview](docs/architecture/platform-overview.md)
- [Platform boundaries](docs/architecture/platform-boundaries.md)
- [Service data ownership](docs/architecture/service-data-ownership.md)
- [API docs](docs/api/README.md)
- [Meeting service guides](services/meeting-service/docs/README.md)
- [Calling service](services/calling/README.md)
- [Contracts](packages/contracts/README.md)
- [Deployment](infra/deployment/README.md)
- [Architecture decisions](docs/decisions/README.md)
- [Delivery phases](docs/phases/README.md)
- [Monorepo migration record](docs/migration/MONOREPO_MIGRATION.md)
