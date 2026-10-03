# Deployment

Each application deploys on its own from its own subdirectory. Neither
deployment builds or needs the other.

> **Layout change:** before the platform layout restructure, these roots were
> `frontend/` and `backend/`. They are now `apps/web` and `services/core-api`.
> Update the Vercel Root Directory and the image build context accordingly.

## Web app: `apps/web` (Vercel)

| Setting | Value |
|---------|-------|
| Git repository | `CosmicforgeHealthnet/cfmeet` |
| **Root Directory** | **`apps/web`** |
| Framework preset | Next.js (auto-detected from `apps/web/package.json`) |
| Install command | default (`npm install`, using `apps/web/package-lock.json`) |
| Build command | default (`next build`) |
| Environment variables | `NEXT_PUBLIC_API_URL` = public core API base URL, e.g. `https://<api-host>/api/v1` |

Optional: to skip web deployments when only other directories change, set
*Ignored Build Step* to `git diff --quiet HEAD^ HEAD -- .` (it runs inside
`apps/web/`). Once `apps/web` consumes `packages/contracts`, also include that path.

The web build never compiles or needs the Rust core API.

## Core API: `services/core-api` (Docker)

The image is built from `services/core-api/Dockerfile` with **`services/core-api/` as the build context**:

```bash
docker build -t cfmeet-core-api ./services/core-api
# or
cd services/core-api && docker build -t cfmeet-core-api .
```

The image contains two binaries: `./server` (API, default `CMD`) and
`./migration` (SeaORM migrator: `./migration up`). Supply configuration at runtime
(`--env-file`, orchestrator secrets) as before; see `services/core-api/.env.example`.

`services/core-api/docker-compose.yml` (unchanged from `rtc_backend`) still works when run
from inside `services/core-api/`. The root `docker-compose.yml` is for local development
only.

## Environment-driven URLs

No URLs are hardcoded by the migration. Before switching any deployment over to
this repository, check that these values are unchanged:

| Variable | App | Notes |
|----------|-----|-------|
| `DATABASE_URL` | core-api | PostgreSQL |
| `REDIS_URL` | core-api | Redis |
| `LIVEKIT_URL` | core-api | Returned to clients in join responses, so it must be reachable from browsers |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | core-api | Used to sign LiveKit join tokens |
| `FRONTEND_URLS` | core-api | CORS allow-list (comma-separated) |
| `APP_URL` | core-api | Links in emails / join URLs |
| `GOOGLE_REDIRECT_URL` | core-api | Must match Google Cloud Console |
| `NEXT_PUBLIC_API_URL` | web | Core API base URL including `/api/v1` |

## Switching deployments to the monorepo

1. Vercel: point the project at `CosmicforgeHealthnet/cfmeet`, set Root Directory = `apps/web`, keep the existing environment variables.
2. Core API host / registry: change the build source to `CosmicforgeHealthnet/cfmeet` with context `services/core-api/` (Dockerfile path `services/core-api/Dockerfile`).
3. Deploy once from the monorepo and confirm `/health` plus a full meeting join before retiring the old repositories.
