# API documentation

## Core API (`services/core-api`)

The authoritative HTTP API description is the OpenAPI 3.0 document generated
from the Rust code (utoipa annotations, collected in
`services/core-api/src/swagger/mod.rs`). Get it from a running instance:

| Endpoint | Content |
|----------|---------|
| `GET /api-docs/openapi.json` | OpenAPI document |
| `GET /swagger-ui/` | Swagger UI |
| `GET /health` | Health check |

Base path for versioned routes: `/api/v1` (`auth`, `meetings`, `participants`,
`api-keys`, `api`, `users`, `webhooks`, `admin`).

Narrative guides for individual areas (meetings, participants, chat,
LiveKit, OAuth, email queue, and more) live with the service in
[`services/core-api/docs/`](../../services/core-api/docs/README.md).

How shared contracts will be exported and consumed is described in
[`packages/contracts`](../../packages/contracts/README.md).
