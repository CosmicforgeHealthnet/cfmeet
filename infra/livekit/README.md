# infra/livekit

LiveKit configuration.

| File | Purpose |
|------|---------|
| `livekit.dev.yaml` | **Local development only.** Used by the root `docker-compose.yml` (`livekit` service). Uses LiveKit's public dev key pair (`devkey` / `secret`), binds to 127.0.0.1, and uses UDP 50000–50100. |

The core API (`services/core-api`) talks to LiveKit only by signing join tokens
with `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` and returning `LIVEKIT_URL` to
clients. Production LiveKit (self-hosted or cloud) is configured through those
environment variables. Production LiveKit server configuration will go in this
directory when it is version-controlled.
