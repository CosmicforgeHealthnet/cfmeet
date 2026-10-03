# infra/docker

Container build and local orchestration notes.

| Artefact | Location | Notes |
|----------|----------|-------|
| Core API image | `services/core-api/Dockerfile` | Build context **must** be `services/core-api/` (`docker build ./services/core-api`). Produces `./server` and `./migration`. |
| Core-API-only compose | `services/core-api/docker-compose.yml` | Carried over unchanged from `rtc_backend`; run it from inside `services/core-api/`. |
| Local platform compose | `docker-compose.yml` (repo root) | PostgreSQL, Redis, LiveKit (default), plus `backend` and `frontend` under the `app` profile. Ports bound to 127.0.0.1. Local development only. |
| Web app | none | `apps/web` is deployed to Vercel and has no Dockerfile. Locally the root compose runs it with `node:22-alpine` + `npm run dev`. |

Dockerfiles stay next to the application they build, so each application stays
independently buildable. Put shared base images or multi-service build tooling
in this directory if and when it is needed.
