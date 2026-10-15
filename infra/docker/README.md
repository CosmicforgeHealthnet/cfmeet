# infra/docker

Container build and local orchestration notes.

| Artefact | Location | Notes |
|---|---|---|
| Meeting Service image | `services/meeting-service/Dockerfile` | Build context must be `services/meeting-service/` (`docker build ./services/meeting-service`). Produces the server and migration binaries. |
| Meeting-service-only compose | `services/meeting-service/docker-compose.yml` | Carried forward from the original RTC backend for service-local workflows. |
| Local platform compose | `docker-compose.yml` (repo root) | PostgreSQL, Redis, LiveKit plus `meeting-service` and `frontend` under the `app` profile. Ports bound to 127.0.0.1. Local development only. |
| Calling / Privacy Relay / Telephony images | `services/<service>/Dockerfile` | Build context is the repository root (`docker build -f services/<service>/Dockerfile .`), filtered by `services/<service>/Dockerfile.dockerignore` to that service and the shared packages it links. Non-root user, runtime stage applies Debian security updates only. Not part of the root compose file: these services run natively against PostgreSQL in development (no Docker required) — see `infra/deployment/README.md`. |
| Web app | `apps/web` | Deployed to Vercel; root compose runs it with Node for local development. |

Dockerfiles stay next to the service/application they build so components remain independently deployable. Shared base images or multi-service build tooling belong here only when they become necessary.
