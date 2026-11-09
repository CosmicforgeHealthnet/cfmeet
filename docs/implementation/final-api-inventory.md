# CF Meet — final API inventory (final closure, Document C)

Source of truth: the served OpenAPI contracts in `packages/contracts/` (each service serves exactly its checked-in contract; route-table ↔ contract identity is tested per service) and the gateway route catalogue `infra/api-gateway/route-manifest.json`. Counts are operations (method + path), computed from those files on the closure branch.

## 1. Operations per service contract

| Owner | Contract | Version | Visibility | Operations | Deprecated |
|---|---|---|---|---:|---:|
| Meeting Service | `cfmeet.meeting-service.http-api` (v1 51 + v2 206) | 1.5.0 | public / first-party | 259 | 7 |
| Meeting Service | `cfmeet.meeting-service.communications-internal` | 1.1.0 | internal | 4 | 0 |
| Organizations | `cfmeet.organizations.internal-api` | 1.3.0 | internal | 51 | 0 |
| Platform Access | `cfmeet.platform-access.internal-api` | 1.8.0 | internal | 73 | 0 |
| Calling | `cfmeet.calling.http-api` | 1.16.0 | public | 147 | 0 |
| Calling | `cfmeet.calling.communications-internal` | 1.1.0 | internal | 8 | 0 |
| Messaging | `cfmeet.messaging.http-api` | 1.16.0 | public | 101 | 0 |
| Messaging | `cfmeet.messaging.internal-api` | 1.0.0 | internal | 7 | 0 |
| Messaging | `cfmeet.messaging.communications-internal` | 1.2.0 | internal | 11 | 0 |
| Communications Hub | `cfmeet.communications.http-api` | 1.3.0 | public | 73 | 0 |
| Communications Hub | `cfmeet.communications.communications-internal` | 1.4.0 | internal | 9 | 1 |
| Developer Platform | `cfmeet.developer-platform.http-api` | 1.6.0 | public | 73 | 0 |
| Developer Platform | `cfmeet.developer-platform.internal-api` | 1.9.0 | internal | 55 | 0 |
| API Gateway | `cfmeet.api-gateway.http-api` | 1.1.0 | public | 7 | 0 |
| Numbering | `cfmeet.numbering.internal-api` | 1.13.0 | internal | 98 | 0 |
| Privacy Relay | `cfmeet.privacy-relay.http-api` | 1.2.0 | public | 22 | 0 |
| Privacy Relay | `cfmeet.privacy-relay.internal-api` | 1.4.0 | internal | 25 | 0 |
| SMS Gateway | `cfmeet.sms-gateway.http-api` | 1.3.0 | public | 11 | 0 |
| SMS Gateway | `cfmeet.sms-gateway.internal-api` | 1.0.0 | internal | 4 | 0 |
| Telephony | `cfmeet.telephony.internal-http-api` | 1.2.0 | internal | 17 | 0 |
| **Total** | | | | **1,055** | **8** |

Not counted above (no OpenAPI path object): 5 realtime WebSocket endpoints (Messaging `/v1/realtime`, Meeting conference realtime, Calling signaling, plus their ticketed variants) and the Meeting Service internal routes served outside its public contract (`/internal/v1/user-sessions/verify`, **`/internal/v1/organization-events`**, `/internal/rtc/*`).

## 2. Public routes by gateway class (`route-manifest.json`)

| Class | Meaning | Operations |
|---|---|---:|
| `PUBLIC_CUSTOMER` | developer / customer APIs routed by the API Gateway | 576 |
| `EXCLUDED` | first-party app APIs served directly by their owner (account-session, organizations, workspace relays); never gateway-routed | 123 |
| `OPERATOR_ADMIN` | operator-only administration | 38 |
| `INTERNAL_SERVICE` | signed service-to-service routes present on a public listener | 33 |
| `PUBLIC_INFRASTRUCTURE` | health / readiness / documents | 17 |
| `PROVIDER_CALLBACK` | signed provider callbacks (LiveKit, SMS, carrier) | 4 |

Per domain: meetings — 133 customer, 112 excluded (first-party), 13 operator, 1 infrastructure; calling — 125 customer, 16 internal, 4 excluded; messaging — 96 customer, 2 excluded; communications — 73 customer; developer-platform — 73 customer; numbering — 52 customer, 25 operator, 17 internal; privacy-relay — 17 customer, 5 excluded; sms — 6 customer; gateway — 1 customer, 6 infrastructure.

## 3. Operations added or extended by the final closure

| Service | Operation | Purpose |
|---|---|---|
| Organizations | `GET /internal/v1/admin/event-delivery` | per-consumer delivery status (operator) |
| Organizations | `POST /internal/v1/admin/event-delivery/replay` | dead-letter replay, audited (operator) |
| Organizations | `GET`/`PUT`/`DELETE /internal/v1/admin/organizations/{id}/scopes/primary/service-bindings/messaging` | Messaging binding per organization scope |
| Organizations | `POST /internal/v1/organizations/scopes/resolve` (extended) | `serviceBindings.messaging` |
| Meeting (internal) | `POST /internal/v1/organization-events` | signed organization event ingress |
| Meeting v2 | `GET`/`POST`/`PUT`/`PATCH`/`DELETE /api/v2/organizations/{id}/messaging/relay/{path}` | organization-scope Messaging (J-10) |
| Meeting v2 | `POST /api/v2/organizations/{id}/messaging/session-tokens` | organization-scope MANAGED realtime |
| Meeting v2 | `POST /api/v2/organizations/{id}/messaging/members/resolve` | colleagues → organization-scope principals (active members only) |
| Meeting v2 | 11 teams / team members / policies operations under `/api/v2/organizations/{id}/` | Section 6 through the Organizations relay (ORG-006) |
| Communications | `PhoneVerification.status` | canonical `PROVIDER_BLOCKED` (was `DEFERRED`) |

Meeting v2 operations: 188 → 206. Gateway `EXCLUDED` (first-party organization) operations: 105 → 123. No operation was removed; no duplicate authority was introduced (organization conversations reuse Messaging; teams and policies stay owned by Organizations).

## 4. Deprecated compatibility operations (kept until verified migration)

The 7 deprecated Meeting Service operations are the legacy v1 create paths (`POST /api/v1/meetings`, `POST /api/v1/meetings/instant`, `POST /api/v1/api/meetings`; v2 creation replaces them, PLT-004) and the legacy meeting webhooks (`GET`/`POST /api/v1/webhooks`, `PATCH`/`DELETE /api/v1/webhooks/{id}`; never delivered, replaced by Developer Platform webhooks, PLT-003). The deprecated Communications internal operation is `POST /internal/v1/communications/block-events` (owner block metadata ingest, superseded by the canonical block rules). None is deleted before its verified migration.
