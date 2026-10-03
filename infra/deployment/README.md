# Deployment

The monorepo does not change how either application is deployed. Each one
deploys on its own from its own subdirectory.

## Frontend (Vercel)

| Setting | Value |
|---------|-------|
| Git repository | `CosmicforgeHealthnet/cfmeet` |
| **Root Directory** | **`frontend`** |
| Framework preset | Next.js (auto-detected from `frontend/package.json`) |
| Install command | default (`npm install`, using `frontend/package-lock.json`) |
| Build command | default (`next build`) |
| Environment variables | `NEXT_PUBLIC_API_URL` = public backend base URL, e.g. `https://<api-host>/api/v1` |

Optional: to skip frontend deployments when only backend files change, set
*Ignored Build Step* to `git diff --quiet HEAD^ HEAD -- .` (it runs inside
`frontend/`).

The frontend build never compiles or needs the Rust backend.

## Backend (Docker)

The image is built from `backend/Dockerfile` with **`backend/` as the build context**:

```bash
docker build -t cfmeet-backend ./backend
# or
cd backend && docker build -t cfmeet-backend .
```

The image contains two binaries: `./server` (API, default `CMD`) and
`./migration` (SeaORM migrator: `./migration up`). Supply configuration at runtime
(`--env-file`, orchestrator secrets) as before; see `backend/.env.example`.

`backend/docker-compose.yml` (unchanged from `rtc_backend`) still works when run
from inside `backend/`. The root `docker-compose.yml` is for local development
only.

## Environment-driven URLs

No URLs are hardcoded by the migration. Before switching any deployment over to
this repository, check that these values are unchanged:

| Variable | App | Notes |
|----------|-----|-------|
| `DATABASE_URL` | backend | PostgreSQL |
| `REDIS_URL` | backend | Redis |
| `LIVEKIT_URL` | backend | Returned to clients in join responses, so it must be reachable from browsers |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | backend | Used to sign LiveKit join tokens |
| `FRONTEND_URLS` | backend | CORS allow-list (comma-separated) |
| `APP_URL` | backend | Links in emails / join URLs |
| `GOOGLE_REDIRECT_URL` | backend | Must match Google Cloud Console |
| `NEXT_PUBLIC_API_URL` | frontend | Backend base URL including `/api/v1` |

## Switching deployments to the monorepo

1. Vercel: point the project at `CosmicforgeHealthnet/cfmeet`, set Root Directory = `frontend`, keep the existing environment variables.
2. Backend host / registry: change the build source to `CosmicforgeHealthnet/cfmeet` with context `backend/` (Dockerfile path `backend/Dockerfile`).
3. Deploy once from the monorepo and confirm `/health` plus a full meeting join before retiring the old repositories.
