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
| **Implemented (staging)** | Realtime calling — `services/calling`: platform trust foundation (tenants, applications, environments, service accounts, API credentials, scopes, principals), the canonical Call aggregate, app voice / video / group calls, routing, public-network calls through Telephony, enterprise calling (extensions, ring groups, queues, IVR, hours, hold / transfer, voicemail, recording), customer webhooks, usage and limits |
| **Implemented (staging)** | Privacy Relay communications — `services/privacy-relay`: communication contexts, relay sessions, Contact Vault, temporary relay numbers |
| **Implemented (staging)** | SIP/PSTN telephony — `services/telephony` with the CF-owned SIP / media edge (`infra/telephony`); **real-carrier certification is pending** an external lawful interconnect (ADR 0016) |
| **Planned** | Unified messaging |
| **Planned** | Digital Communications Numbers |
| **Planned** | Admin console, developer portal / CPaaS SDKs |
| **Future** | Intelligence: transcription, translation and meeting/call assistance |

Planned items are not implemented yet. See [`docs/architecture/platform-overview.md`](docs/architecture/platform-overview.md).

## Repository layout

```text
cfmeet/
├── apps/
│   └── web/                  Next.js CF Meet web application                         (current)
├── services/
│   ├── meeting-service/      Rust/Axum Meeting & Conference Platform service         (current)
│   ├── calling/              Rust/Axum Calling service (trust foundation, Calls, enterprise) (active)
│   ├── privacy-relay/        Rust/Axum Privacy Relay + Contact Vault                  (active)
│   └── telephony/            Rust/Axum Telephony core (SIP / PSTN legs, providers)     (active)
├── packages/
│   ├── contracts/            Contract authority: manifest, schemas, OpenAPI, validator (authority)
│   ├── service-auth/         Internal service authentication library (Ed25519)       (active)
│   ├── webhooks/             Webhook signing / SSRF / retry primitives               (active)
│   └── object-storage/       Protected S3-compatible object storage client            (active)
├── infra/
│   ├── livekit/              LiveKit configuration
│   ├── telephony/            SIP / media edge (Asterisk) configuration and certification
│   ├── docker/               Container/local orchestration notes
│   └── deployment/           Deployment guide, per-service backup / restore and database roles
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
| `services/calling/` | Calling domain service: the platform trust foundation (tenant / application / environment / service-account model, API credentials, scopes, limits, communication principals) and the Calling core — the canonical, provider-independent Call aggregate, state machine, participants / sessions, legs / routes / attempts, authorizations, immutable events with a transactional outbox, the call API, and application calling (endpoints, signaling, ringing / arbitration, media provider boundary, call control) ([README](services/calling/README.md)) | Docker image (context: repository root, `-f services/calling/Dockerfile`) |

The current Rust service is intentionally named **meeting-service** because it is the backend for the meeting/conferencing domain, not the universal backend for all future CF Meet capabilities.

`services/calling/`, `services/privacy-relay/` and `services/telephony/` are independent domain services: each has its own database, migrations, image and credentials, and they talk only through authenticated APIs and events (no shared source or database). Future services such as `messaging` and `numbering` will be added under `services/` when their implementation begins; they are not scaffolded prematurely.

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
├── calling/               active (platform trust foundation + Calling core)
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
- PostgreSQL 16 (required for every backend service and its tests). Redis and LiveKit only for the meeting service / real-media certification. Docker with Compose v2 is **optional** (convenience for dependencies, image builds and certification); ordinary backend development and tests run against directly reachable services.

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
- Calling service Docker build context: repository root (`docker build -f services/calling/Dockerfile .`; the context is filtered to `packages/service-auth`, `packages/object-storage`, `packages/webhooks` and `services/calling`)
- Privacy Relay / Telephony Docker build context: repository root (`docker build -f services/privacy-relay/Dockerfile .`, `docker build -f services/telephony/Dockerfile .`; each context is filtered to `packages/service-auth` and its own service)
- Per-service PostgreSQL provisioning, backup and restore: [`infra/deployment/README.md`](infra/deployment/README.md)

The web app and meeting service remain independently deployable and communicate over published APIs only.

## CI

Workflows under `.github/workflows/` are path-aware:

- **Meeting service:** `services/meeting-service/**` → fmt, clippy, test, build and Docker image
- **Calling service:** `services/calling/**`, `packages/service-auth/**`, Calling / platform contracts → fmt, check, clippy, PostgreSQL tests (incl. internal service auth, the call state machine, concurrency races, transactional events / outbox and contract conformance), architecture guards, migration smoke, startup refusal checks, build and Docker image
- **Contracts and service auth:** `packages/contracts/**`, `packages/service-auth/**` → contract validation (`make contracts-validate`), validator mutation tests, service-auth fmt / check / clippy / tests for both feature sets, private-key scan
- **Privacy Relay / Telephony services:** their service, its shared packages and contracts (Telephony also Calling's telephony paths, Privacy Relay and `infra/telephony/**`) → fmt, check, clippy, PostgreSQL tests, migration smoke, cross-service e2e against the real binaries, build and Docker image
- **Certification:** Calling RTC (real LiveKit), Telephony SIP (real Asterisk + SIPp) and enterprise calling (Asterisk + SIPp + S3) workflows; each runs with its `*_CERT_REQUIRED=1` flag, so a missing dependency fails instead of passing as SKIPPED
- **Dependency and image security:** RustSec advisories for every Part III lockfile, image secret scan and pending Debian security updates per service image (also weekly)
- **Service boundaries:** every push → repository data-ownership guard, secret scan, per-service database isolation check and their mutation tests (`make service-boundaries`, `make secret-scan`)
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
