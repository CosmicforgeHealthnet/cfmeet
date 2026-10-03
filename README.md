# CF Meet

**CosmicForge Tech Limited**

CF Meet is CosmicForge's real-time video meeting platform. This monorepo contains
the RTC control-plane API and the web client. They are two separately deployable
applications that share version control, CI, documentation and local tooling.

## Repository layout

```
cfmeet/
├── backend/     Rust/Axum RTC control plane (PostgreSQL, Redis, LiveKit, SeaORM)
├── frontend/    Next.js CF Meet client (LiveKit Components, React Query)
├── infra/       deployment/infrastructure configuration (local LiveKit config)
├── scripts/     repository-wide automation
├── docs/        technical, architecture and deployment documentation
├── .github/     path-aware CI workflows
├── docker-compose.yml   local development dependencies (+ optional full stack)
└── Makefile     root developer commands
```

| Path | History | Deploys as | Build context |
|------|---------|------------|---------------|
| `backend/`  | imported from `CosmicforgeHealthnet/rtc_backend`  | Docker image (`backend/Dockerfile`) | `backend/` |
| `frontend/` | imported from `CosmicforgeHealthnet/rtc_frontend` | Vercel | Root Directory = `frontend` |

The two applications talk to each other only over HTTP (`NEXT_PUBLIC_API_URL` →
backend `/api/v1`). The frontend never imports backend source, and the backend
never depends on frontend build output. Keep it that way.

Detailed backend documentation lives in [`backend/docs/`](backend/docs/README.md).
Deployment notes: [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).
Migration record: [`docs/MONOREPO_MIGRATION.md`](docs/MONOREPO_MIGRATION.md).

## Prerequisites

- Rust (stable) with `rustfmt` and `clippy`
- Node.js 22 and npm (the frontend uses `package-lock.json`)
- Docker with Compose v2 (for PostgreSQL, Redis and LiveKit), or local installs of each

## Environment files

Secrets are never committed. Copy the examples and fill them in locally:

```bash
cp backend/.env.example  backend/.env
cp frontend/.env.example frontend/.env.local
```

| File | Purpose |
|------|---------|
| `backend/.env.example`  | `DATABASE_URL`, `REDIS_URL`, `JWT_SECRET`, `LIVEKIT_URL` / `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET`, Google OAuth, SMTP, `FRONTEND_URLS` (CORS), `APP_URL` |
| `frontend/.env.example` | `NEXT_PUBLIC_API_URL` (backend base URL, e.g. `http://localhost:8080/api/v1`) |

`.env`, `.env.*` and `.env.local` are git-ignored everywhere; only `*.env.example`
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

Matching values for `backend/.env`:

```dotenv
DATABASE_URL=postgresql://postgres:password@localhost:5432/cosmicforge_rtc
REDIS_URL=redis://127.0.0.1:6379
LIVEKIT_URL=ws://localhost:7880
LIVEKIT_API_KEY=devkey
LIVEKIT_API_SECRET=secret
FRONTEND_URLS=http://localhost:3000
APP_URL=http://localhost:3000
```

The dev LiveKit key pair is public and for local use only. Use LiveKit Cloud or
your own LiveKit deployment with real credentials anywhere else.

### 4. Backend

```bash
make migrate         # cd backend && cargo run -p migration -- up
make backend         # cd backend && cargo run --bin backend
curl http://localhost:8080/health
```

Swagger UI is served at <http://localhost:8080/swagger-ui/>.

### 5. Frontend

```bash
make frontend        # cd frontend && npm ci (first run) && npm run dev
```

Open <http://localhost:3000>.

### Everything at once

```bash
make dev             # deps + migrations + backend + frontend (Ctrl+C stops the apps)
```

Or run the full stack in containers:

```bash
docker compose --profile app up -d --build
```

## Root commands

| Command | What it does |
|---------|--------------|
| `make help`     | List all targets |
| `make deps` / `make deps-down` | Start / stop PostgreSQL, Redis, LiveKit |
| `make migrate`  | Apply backend migrations |
| `make backend`  | Run the backend API |
| `make frontend` | Run the Next.js dev server |
| `make dev`      | Start deps, then backend and frontend together |
| `make test`     | `cargo test --workspace` + frontend typecheck (no frontend test suite exists yet) |
| `make lint`     | `cargo fmt --check`, `cargo clippy`, `npm run lint` |
| `make build`    | `cargo build --workspace` + `next build` |

Each target just runs the app's own toolchain inside `backend/` or `frontend/`, so
you can always `cd` into either directory and work as before.

## CI

Workflows in [`.github/workflows`](.github/workflows) are path-filtered:

- **Backend** (`backend/**`): `cargo fmt --check`, `cargo clippy`, `cargo test`, `cargo build`, `docker build` with `backend/` as context
- **Frontend** (`frontend/**`): `npm ci`, `npm run lint`, `tsc --noEmit`, `next build`
- **Compose config** (`docker-compose.yml`, `infra/**`): `docker compose config`

`cargo fmt --check` and `npm run lint` currently fail on code imported unchanged
from the original repositories. They report but don't block until those are cleaned up
(see [`docs/MONOREPO_MIGRATION.md`](docs/MONOREPO_MIGRATION.md)).
