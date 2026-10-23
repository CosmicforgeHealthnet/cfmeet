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

Parts A–D (meeting core, admission, roles, participants) live under `/api/v2` behind
`FEATURE_MEETING_V2_ENABLED`. The legacy `/api/v1` meeting API is unchanged and keeps working.
See [`docs/PHASE2_MEETING_DOMAIN.md`](docs/PHASE2_MEETING_DOMAIN.md).

### Part VI — platform credentials and meeting webhooks

With `MEETING_PLATFORM_ACCESS_ENABLED=true`, a customer's platform credential can call the
machine-callable `/api/v2` operations through the public API Gateway (`meetings.read` /
`meetings.write`; meeting spaces, instant meetings, conference lifecycle, participants and
waiting room, invitations, attendance — `src/services/platform/operations.rs`). The gateway
forwards without the credential; this service has Platform Access re-verify it live and
acts as the application environment's machine caller, which sees and manages ONLY meetings
owned by that environment (`meeting_spaces.platform_*`; anything else is 404). With
`MEETING_SERVICE_PUBLIC_EVENTS_DESTINATION=developer-platform`, lifecycle events of
environment-owned meetings are published from the transactional outbox to the Developer
Platform as platform envelopes (default `none`). See `src/services/platform/mod.rs`.

### Local development and tests (no Docker required)

Use a normal local PostgreSQL (and optionally Redis) install:

```bash
cargo run -p migration -- up                    # legacy + Phase 2 additive schema + idempotent backfill
cargo run --bin phase2_backfill -- --dry        # migration evidence (counts / exceptions)

# Unit tests only
cargo test --lib

# Full suite: unit + DB integration + API + security + concurrency + migration tests.
# Each test gets its own database cloned from a migrated template.
TEST_DATABASE_URL=postgres://postgres@127.0.0.1:5432/postgres \
TEST_REDIS_URL=redis://127.0.0.1:6379 \
cargo test --workspace
```

Without `TEST_DATABASE_URL` the database-backed suites **fail** (they are never silently
skipped). `CFMEET_SKIP_DB_TESTS=1` skips them explicitly with a printed `SKIPPED` line.
`TEST_REDIS_URL` is only needed by the legacy v1 regression test (legacy auth uses Redis);
the v2 suites deliberately run with Redis unreachable.

OpenAPI contract export (checked in CI):

```bash
cargo run --bin export_openapi > ../../packages/contracts/openapi/meeting-service.json
```

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
