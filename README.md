# CF Meet

**CosmicForge Tech Limited — unified communications platform**

CF Meet is evolving from a real-time meeting product into CosmicForge's unified communications platform: meetings and conferencing, realtime calling, messaging, Privacy Relay communications, telephony, Digital Communications Numbers, enterprise communications and developer communications APIs.

This monorepo contains the platform's applications, domain services, shared contracts, infrastructure and documentation.

## Current vs planned capabilities

| Status | Capability |
|---|---|
| **Current** | Video meetings: scheduled and instant meetings, pre-join, LiveKit rooms, waiting room/admission, participant list and media state, screen share, in-meeting chat |
| **Current** | Accounts: email/password with verification, Google OAuth, password reset/change-password |
| **Current** | Developer basics: API keys, external meeting create/join API, webhook registration, OpenAPI/Swagger |
| **Implemented (software)** | Realtime calling — `services/calling`: platform trust foundation (tenants, applications, environments, service accounts, API credentials, scopes, principals), canonical Call aggregate, app voice/video/group calls, routing, public-network calls through Telephony, enterprise calling (extensions, ring groups, queues, IVR, hours, hold/transfer, voicemail, recording), customer webhooks, usage and limits |
| **Implemented (software)** | Privacy Relay communications — `services/privacy-relay`: communication contexts, relay sessions, Contact Vault and temporary relay numbers |
| **Implemented (software)** | SIP/PSTN telephony — `services/telephony` with the CF-owned SIP/media edge (`infra/telephony`); **real-carrier certification remains pending** an external lawful interconnect (ADR 0016) |
| **Planned** | Unified messaging |
| **Planned** | Digital Communications Numbers |
| **Planned** | Admin console, developer portal / CPaaS SDKs |
| **Future** | Intelligence: transcription, translation and meeting/call assistance |

Part III software is implemented and certified. Full Part III production acceptance remains pending on real lawful carrier certification and the remaining external scale gates. See `docs/architecture/part-l-final-closure-register.md`.

## Repository layout

```text
cfmeet/
├── apps/
│   ├── web/                  Next.js CF Meet web application
│   └── mobile/               Expo React Native application
├── services/
│   ├── meeting-service/      Rust/Axum Meeting & Conference Platform service
│   ├── calling/              Rust/Axum Calling service (trust foundation, Calls, enterprise)
│   ├── privacy-relay/        Rust/Axum Privacy Relay + Contact Vault
│   └── telephony/            Rust/Axum Telephony core (SIP/PSTN legs, providers)
├── packages/
│   ├── contracts/            Contract authority: manifest, schemas, OpenAPI, validator
│   ├── service-auth/         Internal service authentication library (Ed25519)
│   ├── webhooks/             Webhook signing / SSRF / retry primitives
│   └── object-storage/       Protected S3-compatible object storage client
├── infra/
│   ├── livekit/              LiveKit configuration
│   ├── telephony/            SIP/media edge (Asterisk) configuration and certification
│   ├── docker/               Container/local orchestration notes
│   └── deployment/           Deployment guide, per-service backup/restore and database roles
├── docs/
│   ├── architecture/         Platform architecture, boundaries, certification and runbooks
│   ├── api/                  API documentation entry point
│   ├── decisions/            Architecture decision records
│   ├── phases/               Meeting-platform delivery specifications and closure reports
│   ├── migration/            Monorepo migration history
│   ├── DEPLOYMENT.md         Deployment guide
│   ├── INFRASTRUCTURE.md     Infrastructure guide
│   ├── BACKEND.md            Backend deployment options
│   ├── VERCEL.md             Web deployment guide
│   ├── MOBILE.md             Expo/EAS deployment guide
│   ├── ENVIRONMENT.md        Environment variables reference
│   └── TROUBLESHOOTING.md    Common issues and solutions
├── scripts/                  dev helpers, service-boundary guard and secret scan
├── .github/workflows/
├── docker-compose.yml
├── docker-compose.prod.yml
└── Makefile
```

## Current deployable applications/services

| Path | Responsibility | Deploys as |
|---|---|---|
| `apps/web/` | CF Meet web experience | Vercel |
| `apps/mobile/` | CF Meet mobile experience | Expo / EAS |
| `services/meeting-service/` | Meeting/conference control plane: auth, meetings, participants, waiting room, chat, LiveKit token/control integration, API keys, webhooks, workers and related meeting-domain functionality | Docker image |
| `services/calling/` | Calling domain service: tenant/application/environment/service-account trust, credentials/scopes/limits, canonical provider-independent Call state, app calling, routing, public-network control, enterprise calling, webhooks and usage | Docker image (repository-root context, `-f services/calling/Dockerfile`) |
| `services/privacy-relay/` | Communication Contexts, Privacy Relay Sessions, Contact Vault and temporary relay numbers | Docker image (repository-root context, `-f services/privacy-relay/Dockerfile`) |
| `services/telephony/` | Public-network leg execution, provider adapters, provider events and reconciliation; drives the SIP/media edge in `infra/telephony/` | Docker image (repository-root context, `-f services/telephony/Dockerfile`) |

The meeting service remains the backend for the meeting/conferencing domain; it is not the universal backend for all CF Meet capabilities. Calling, Privacy Relay and Telephony are independent domain services with their own databases, migrations, images and credentials. They communicate through authenticated APIs/events rather than shared source or databases.

Future services such as `messaging` and `numbering` are added only when their implementation begins.

## Phase 2 implementation home

**Phase 2 — Meeting & Conference Platform** is implemented primarily in:

- `services/meeting-service/`
- `apps/web/`
- `apps/mobile/`
- `packages/contracts/`
- `infra/livekit/`
- `docs/phases/phase-02/`

Phase 2 does not own Calling, Telephony, Privacy Relay, Numbering or Messaging.

## Service evolution

```text
services/
├── meeting-service/       current
├── calling/               active
├── privacy-relay/         active
├── messaging/             future
├── telephony/             active
├── numbering/             future
├── webhook-delivery/      future if extracted
└── workers/               future if extracted
```

See `docs/architecture/platform-boundaries.md`.

## Prerequisites

- Rust stable with `rustfmt` and `clippy`
- Node.js 22 and npm
- PostgreSQL 16
- Redis and LiveKit where required by the meeting service or real-media certification
- Docker with Compose v2 is optional for normal backend development; it is used for convenience, packaging and certification

## Environment files

```bash
cp services/meeting-service/.env.example services/meeting-service/.env
cp services/calling/.env.example         services/calling/.env
cp services/privacy-relay/.env.example   services/privacy-relay/.env
cp services/telephony/.env.example       services/telephony/.env
cp apps/web/.env.example                 apps/web/.env.local
cp apps/mobile/.env.example              apps/mobile/.env
```

## Local development

```bash
make deps
make migrate
make meeting-service
curl http://localhost:8080/health
```

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
- Mobile: `apps/mobile` via Expo/EAS
- Meeting service: `services/meeting-service/Dockerfile`
- Calling: repository-root build context with `services/calling/Dockerfile`
- Privacy Relay: repository-root build context with `services/privacy-relay/Dockerfile`
- Telephony: repository-root build context with `services/telephony/Dockerfile`
- Per-service PostgreSQL provisioning/backup/restore: `infra/deployment/README.md`

## CI

Workflows under `.github/workflows/` are path-aware and include:

- meeting service;
- Calling;
- Privacy Relay;
- Telephony;
- contracts and service-auth;
- real RTC certification;
- Asterisk/SIP certification;
- enterprise calling certification;
- dependency/image security;
- service-boundary, database-isolation and secret-scan checks;
- frontend;
- mobile;
- compose/deployment validation.

## Documentation

Platform / architecture:

- [Platform overview](docs/architecture/platform-overview.md)
- [Platform boundaries](docs/architecture/platform-boundaries.md)
- [Service data ownership](docs/architecture/service-data-ownership.md)
- [Architecture decisions](docs/decisions/README.md)
- [API docs](docs/api/README.md)

Part III:

- [Part F certification](docs/architecture/part-f-certification.md)
- [Part G certification](docs/architecture/part-g-certification.md)
- [Part H security certification](docs/architecture/part-h-security-certification.md)
- [Part I reliability certification](docs/architecture/part-i-reliability-certification.md)
- [Part J persistence/deployment certification](docs/architecture/part-j-persistence-deployment-certification.md)
- [Part K production acceptance](docs/architecture/part-k-production-acceptance.md)
- [Part L closure register](docs/architecture/part-l-final-closure-register.md)

Operations:

- [Deployment](infra/deployment/README.md)
- [Provider/carrier outage runbook](docs/architecture/provider-outage-runbook.md)
- [Secret rotation](docs/architecture/secret-rotation-runbook.md)
- [Security deployment checklist](docs/architecture/security-deployment-checklist.md)

Additional deployment guides:

- [Deployment index](docs/README.md)
- [Infrastructure](docs/INFRASTRUCTURE.md)
- [Backend](docs/BACKEND.md)
- [Vercel](docs/VERCEL.md)
- [Mobile](docs/MOBILE.md)
- [Environment](docs/ENVIRONMENT.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)

Other:

- [Meeting service guides](services/meeting-service/docs/README.md)
- [Calling service](services/calling/README.md)
- [Privacy Relay service](services/privacy-relay/README.md)
- [Telephony service](services/telephony/README.md)
- [SIP/media edge](infra/telephony/README.md)
- [Contracts](packages/contracts/README.md)
- [Delivery phases](docs/phases/README.md)
- [Monorepo migration record](docs/migration/MONOREPO_MIGRATION.md)
