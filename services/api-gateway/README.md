# services/api-gateway — public API Gateway (Part VI foundation)

Customer ingress of the CF Meet platform ([ADR 0055](../../docs/decisions/0055-developer-platform-and-api-gateway-foundation.md)).
Foundation only: the Section 51 route catalogue is not implemented.

Request pipeline (`src/http/ingress.rs`):

1. new server-generated request / correlation ids (returned as `X-Request-Id`,
   `X-Correlation-Id`; signed into every upstream hop);
2. **default deny** — a path outside the validated route table is `NOT_FOUND` before any
   credential is examined; the table refuses internal, provider-callback, operator,
   Platform Access, Privacy Relay and Telephony targets by construction (`src/routing.rs`);
3. a client `X-CF-Service-Authorization` is refused; every other client header claiming
   authority (`X-CF-*`, `X-Tenant-*`, `X-Forwarded-*`, …) is dropped — upstream requests
   are built from scratch;
4. `Authorization: Bearer` verified by **Platform Access** (`platform-credential.authenticate`);
   tenant / application / environment / service account come only from its authority chain;
5. route scope check (Platform Access scopes), protocol check (WebSocket forwarding is
   Section 51: `PROTOCOL_NOT_ENABLED`), bounded JSON body;
6. forward with a fresh **short-lived (60 s) signed service token** carrying the
   `DelegatedPlatformContext` for exactly one upstream audience, under the route deadline.
   The domain service keeps final resource authorization; its status and JSON are relayed.

The gateway owns **no database** and no communications data. Redis is configured only
(`API_GATEWAY_REDIS_URL`) for future shared quotas. Flags default off:
`API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_WEBSOCKETS_ENABLED`.

```bash
make api-gateway               # services/api-gateway/.env
make api-gateway-test          # unit, routing, ingress, startup (no database)
make api-gateway-integration   # real Platform Access + Developer Platform (TEST_DATABASE_URL)
make api-gateway-lint
```
