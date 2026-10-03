# CF Meet

**CosmicForge Tech Limited: unified communications platform**

CF Meet started as CosmicForge's real-time video meeting product and is growing into
the company's **unified communications platform**: meetings and conferencing,
realtime calling, messaging, Privacy Relay communications, telephony, Digital
Communications Numbers, enterprise communications and developer communications
APIs.

This monorepo holds the platform's applications, services, shared contracts,
infrastructure configuration and documentation.

## Capabilities

| | Capability |
|---|-----------|
| **Current** | Video meetings: scheduled and instant meetings, pre-join, LiveKit-based rooms, waiting room / admission, participant list and media state, screen share, in-meeting chat |
| **Current** | Accounts: email/password with verification, Google OAuth, password reset |
| **Current** | Developer basics: API keys, external meeting create/join API, webhook registration, OpenAPI/Swagger |
| Planned | Realtime app-to-app calling |
| Planned | Unified messaging |
| Planned | Privacy Relay communications |
| Planned | SIP/PSTN telephony and Digital Communications Numbers |
| Planned | Enterprise communications, admin console, developer portal / CPaaS APIs and SDKs |
| Future | Intelligence: transcription, translation |

Planned items are **not implemented**. See
[`docs/architecture/platform-overview.md`](docs/architecture/platform-overview.md).

## Repository layout

```
cfmeet/
├── apps/
│   └── web/              Next.js CF Meet web app                      (current)
├── services/
│   └── core-api/         Rust/Axum core API: one deployable modular monolith (current)
├── packages/
│   └── contracts/        Shared API / event / webhook contracts (boundary; generation planned)
├── infra/
│   ├── livekit/          LiveKit config (local dev)
│   ├── docker/           Container build & local orchestration notes
│   └── deployment/       Deployment guide
├── docs/
│   ├── architecture/     Platform overview and boundaries
│   ├── api/              API documentation entry point
│   ├── decisions/        Architecture decision records (ADRs)
│   ├── phases/           Per-phase specifications and reports
│   └── migration/        Monorepo consolidation record
├── scripts/              Repository-wide automation
├── .github/workflows/    Path-aware CI
├── docker-compose.yml    Local dependencies (+ optional full stack)
└── Makefile              Root developer commands
```

| Path | Origin | Deploys as | Build root |
|------|--------|------------|------------|
| `services/core-api/` | `CosmicforgeHealthnet/rtc_backend` (via `backend/`) | Docker image (`services/core-api/Dockerfile`) | `services/core-api/` |
| `apps/web/` | `CosmicforgeHealthnet/rtc_frontend` (via `frontend/`) | Vercel | Root Directory = `apps/web` |

The web app and the core API communicate **only over HTTP** (`NEXT_PUBLIC_API_URL`
→ core API `/api/v1`). Neither imports the other's source or build output, and
each builds and deploys independently. Future services and apps are documented
in [`docs/architecture/platform-boundaries.md`](docs/architecture/platform-boundaries.md)
and get created only when real work begins.

More docs:
[core API guides](services/core-api/docs/README.md) ·
[API](docs/api/README.md) ·
[deployment](infra/deployment/README.md) ·
[contracts](packages/contracts/README.md) ·
[ADRs](docs/decisions/README.md) ·
[migration record](docs/migration/MONOREPO_MIGRATION.md)

## Prerequisites

- Rust (stable) with `rustfmt` and `clippy`
- Node.js 22 and npm (`apps/web` uses `package-lock.json`)
- Docker with Compose v2 (for PostgreSQL, Redis and LiveKit), or local installs of each

## Environment files

Secrets are never committed. Copy the examples and fill them in locally:

```bash
cp services/core-api/.env.example services/core-api/.env
cp apps/web/.env.example          apps/web/.env.local
```

| File | Purpose |
|------|---------|
| `services/core-api/.env.example` | `DATABASE_URL`, `REDIS_URL`, `JWT_SECRET`, `LIVEKIT_URL` / `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`, Google OAuth, SMTP, `FRONTEND_URLS` (CORS), `APP_URL` |
| `apps/web/.env.example` | `NEXT_PUBLIC_API_URL` (core API base URL, e.g. `http://localhost:8080/api/v1`) |

`.env`, `.env.*` and `.env.local` are git-ignored everywhere. Only `*.env.example`
files are tracked.

## Local development

### 1. PostgreSQL, 2. Redis, 3. LiveKit

```bash
make deps            # docker compose up -d postgres redis livekit
```

This starts, bound to `127.0.0.1` only:

| Service | Address | Defaults |
|---------|---------|----------|
| PostgreSQL 15 | `localhost:5432` | user `postgres`, password `password`, db `cosmicforge_rtc` (override with `POSTGRES_*`) |
| Redis 7 | `localhost:6379` | no auth |
| LiveKit (dev mode) | `ws://localhost:7880` | key `devkey`, secret `secret` ([`infra/livekit/livekit.dev.yaml`](infra/livekit/livekit.dev.yaml)) |

Matching values for `services/core-api/.env`:

```dotenv
DATABASE_URL=postgresql://postgres:password@localhost:5432/cosmicforge_rtc
REDIS_URL=redis://127.0.0.1:6379
LIVEKIT_URL=ws://localhost:7880
LIVEKIT_API_KEY=devkey
LIVEKIT_API_SECRET=secret
FRONTEND_URLS=http://localhost:3000
APP_URL=http://localhost:3000
```

The dev LiveKit key pair is public and for local use only.

### 4. Core API (`services/core-api`)

```bash
make migrate         # cd services/core-api && cargo run -p migration -- up
make backend         # cd services/core-api && cargo run --bin backend   (alias: make core-api)
curl http://localhost:8080/health
```

Swagger UI: <http://localhost:8080/swagger-ui/>. OpenAPI: <http://localhost:8080/api-docs/openapi.json>.

### 5. Web app (`apps/web`)

```bash
make frontend        # cd apps/web && npm ci (first run) && npm run dev   (alias: make web)
```

Open <http://localhost:3000>.

### Everything at once

```bash
make dev             # deps + migrations + core API + web app (Ctrl+C stops the apps)
docker compose --profile app up -d --build   # or: full stack in containers
```

## Root commands

| Command | What it does |
|---------|--------------|
| `make help`     | List all targets |
| `make deps` / `make deps-down` | Start / stop PostgreSQL, Redis, LiveKit |
| `make migrate`  | Apply core API migrations |
| `make backend` (`core-api`) | Run the core API |
| `make frontend` (`web`) | Run the web app dev server |
| `make dev`      | Start deps, then core API and web app together |
| `make test`     | `cargo test --workspace` + web typecheck (no web test suite exists yet) |
| `make lint`     | `cargo fmt --check`, `cargo clippy`, `npm run lint` |
| `make build`    | `cargo build --workspace` + `next build` |

Each target runs the app's own toolchain inside `services/core-api/` or
`apps/web/`, so you can also `cd` into either directory and work directly.

## CI

Workflows in [`.github/workflows`](.github/workflows) are path-filtered:

- **Backend** (`services/core-api/**`): `cargo fmt --check`, `cargo clippy`, `cargo test`, `cargo build`, `docker build` with `services/core-api/` as context
- **Frontend** (`apps/web/**`): `npm ci`, `npm run lint`, `tsc --noEmit`, `next build`
- **Compose config** (`docker-compose.yml`, `infra/**`, core-api Dockerfile): `docker compose config`

`packages/contracts/**` will be added to the consuming workflows once something
consumes it. `cargo fmt --check` and `npm run lint` currently fail on code imported
unchanged from the original repositories. They report but don't block (see
[`docs/migration/MONOREPO_MIGRATION.md`](docs/migration/MONOREPO_MIGRATION.md)).
