# Deployment

Each application/service deploys independently from its own subdirectory.

## Web app: `apps/web` (Vercel)

| Setting | Value |
|---|---|
| Git repository | `CosmicforgeHealthnet/cfmeet` |
| Root Directory | `apps/web` |
| Framework | Next.js |
| Environment | `NEXT_PUBLIC_API_URL` = public Meeting Service API base URL, e.g. `https://<api-host>/api/v1` |

## Meeting service: `services/meeting-service` (Docker)

The image is built from `services/meeting-service/Dockerfile` with **`services/meeting-service/` as the build context**:

```bash
docker build -t cfmeet-meeting-service ./services/meeting-service
# or
cd services/meeting-service && docker build -t cfmeet-meeting-service .
```

The image contains the API server and SeaORM migration binary. Runtime configuration remains environment-driven; see `services/meeting-service/.env.example`.

`services/meeting-service/docker-compose.yml` remains available for service-local workflows. The root `docker-compose.yml` is the preferred local platform orchestration file.

## Environment-driven configuration

| Variable | Component | Notes |
|---|---|---|
| `DATABASE_URL` | meeting-service | PostgreSQL |
| `REDIS_URL` | meeting-service | Redis |
| `LIVEKIT_URL` | meeting-service | LiveKit URL returned/used for meeting media |
| `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` | meeting-service | LiveKit token/control credentials |
| `FRONTEND_URLS` | meeting-service | CORS allow-list |
| `APP_URL` | meeting-service | App/join URLs used by backend flows |
| `GOOGLE_REDIRECT_URL` | meeting-service | OAuth callback |
| `NEXT_PUBLIC_API_URL` | web | Meeting Service API base URL including `/api/v1` |

## Monorepo deployment roots

- Web: `apps/web`
- Meeting Service: `services/meeting-service/`

The earlier `services/core-api/` name was replaced before Phase 2 so the service's domain responsibility is explicit.

Before retiring the legacy `rtc_backend` / `rtc_frontend` repositories, deploy both the web app and meeting service successfully from `cfmeet` and verify `/health` plus a full meeting join.
