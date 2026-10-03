# CF Meet Meeting Service

The Rust/Axum control-plane service for the **CF Meet Meeting & Conference Platform**.

This service owns the current meeting-domain backend: authentication required by meetings, meeting lifecycle, participants, waiting room/admission, in-meeting chat, API keys, webhook registration, workers, LiveKit integration and related persistence.

It is intentionally named `meeting-service` because it is **not** the backend for every future CF Meet communications capability. Calling, Privacy Relay, messaging, telephony and numbering will receive their own service boundaries when those phases begin.

During Phase 2 this service remains **one deployable modular monolith**. Do not split meetings, participants, permissions, waiting room or scheduling into separate deployed microservices unless a later architecture decision explicitly requires it.

## Quick start

```bash
# From the monorepo root
make deps
make migrate
make meeting-service
```

Or from this directory:

```bash
cargo run -p migration -- up
cargo run --bin backend
```

Health check:

```bash
curl http://localhost:8080/health
```

## Phase 2

Phase 2 — Meeting & Conference Platform — backend work is implemented here.
The corresponding web experience lives in `apps/web/`, shared contracts in `packages/contracts/`, LiveKit infrastructure in `infra/livekit/`, and the implementation specification/acceptance evidence in `docs/phases/phase-02/`.

## Documentation

See [`docs/`](docs/) for detailed guides:

| Guide | Description |
|---|---|
| [Quick Start](docs/QUICK_START.md) | Get up and running |
| [Docker Testing](docs/DOCKER_TESTING_GUIDE.md) | Docker setup and commands |
| [Database Schema](docs/DATABASE_SCHEMA.md) | Database structure |
| [Migrations](docs/MIGRATIONS.md) | Database migrations |
| [API Integration](docs/API_INTEGRATION.md) | API usage |
| [Swagger Guide](docs/SWAGGER_GUIDE.md) | API documentation |
| [OAuth Setup](docs/OAUTH_SETUP.md) | Google OAuth configuration |
| [LiveKit](docs/LIVEKIT.md) | WebRTC integration |
| [Meetings](docs/MEETINGS.md) | Meeting functionality |
| [Participants](docs/PARTICIPANTS.md) | Participant management |
| [Chat](docs/CHAT.md) | Chat system |
| [Email Queue](docs/EMAIL_QUEUE.md) | Email processing |
| [Logging](docs/LOGGING.md) | Logging configuration |

## Tech stack

- Rust / Axum
- SeaORM / PostgreSQL
- Redis
- LiveKit
- JWT + Google OAuth
