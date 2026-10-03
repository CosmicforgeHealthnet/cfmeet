# API documentation

## Meeting service (`services/meeting-service`)

The authoritative HTTP API description for the current Meeting & Conference Platform service is the OpenAPI 3.0 document generated from the Rust code (utoipa annotations collected in `services/meeting-service/src/swagger/mod.rs`).

| Endpoint | Content |
|---|---|
| `GET /api-docs/openapi.json` | OpenAPI document |
| `GET /swagger-ui/` | Swagger UI |
| `GET /health` | Health check |

Base path for versioned routes: `/api/v1`.

Narrative guides for meetings, participants, chat, LiveKit, OAuth, email queue and related capabilities live in [`services/meeting-service/docs/`](../../services/meeting-service/docs/README.md).

The current API still contains several account/admin/developer endpoints inherited from the original RTC backend. Their presence does not make this service the universal CF Meet backend. Future platform domains receive their own service boundaries as their phases begin.

Shared/exported contract strategy is described in [`packages/contracts`](../../packages/contracts/README.md).
