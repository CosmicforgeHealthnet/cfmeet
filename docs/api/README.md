# API documentation

## Unified CF Meet Swagger explorer

The **Meeting Service** Swagger UI at `/swagger-ui/` provides a service selector for the
versioned OpenAPI contracts checked into `packages/contracts`. It uses its own
`/api-docs/openapi.json` for meetings and serves peer-domain specifications from
`/api-docs/*.json`. The meeting schema and all existing meeting routes are unchanged.

| Swagger selector | JSON document | Contract authority | Operation scope |
|---|---|---|---|
| Meetings | `/api-docs/openapi.json` | `services/meeting-service/src/swagger` | Meeting Service |
| Calling | `/api-docs/calling.json` | `calling/http/v1.openapi.json` | Calling + platform ingress |
| Platform Access (internal) | `/api-docs/platform-access.json` | `platform-access/internal/v1.openapi.json` | Service-auth only |
| Numbering / DCNs | `/api-docs/numbering.json` | `numbering/internal/v1.openapi.json` | Customer + internal Numbering |
| Privacy Relay | `/api-docs/privacy-relay.json` | `privacy-relay/http/v1.openapi.json` | Customer ingress + Contact Vault |
| Privacy Relay (internal) | `/api-docs/privacy-relay-internal.json` | `privacy-relay/internal/v1.openapi.json` | Service-auth only |
| Telephony (internal) | `/api-docs/telephony.json` | `telephony/internal/v1.openapi.json` | Service-auth only |
| SMS Gateway | `/api-docs/sms-gateway.json` | `sms-gateway/http/v1.openapi.json` | Customer SMS + signed provider callbacks |

**Discovery versus deployment:** The Swagger selector is a read-only *documentation
aggregation*. It does not proxy or publish the peer services at the Meeting Service host.
Actual API calls still require a separately deployed and reachable target service,
its designated ingress, and its existing customer/service/provider authentication.
Do not use Meeting Service's URL as an assumed Numbering or SMS Gateway API base.
Only claim that the public Swagger page has changed after deploying the updated Meeting
Service image and verifying the running `/swagger-ui/` and `/api-docs/numbering.json`.

**Internal APIs:** Their schemas are discoverable for engineering review; they are not
customer APIs and cannot be called without service-auth. The original service-specific
`GET /openapi.json` endpoints remain the canonical served contracts for Numbering and
SMS Gateway. Real carrier certification and production deployment are separate gates.


## Meeting service (`services/meeting-service`)

The authoritative HTTP API description for the current Meeting & Conference Platform service is the OpenAPI 3.0 document generated from the Rust code (utoipa annotations collected in `services/meeting-service/src/swagger/mod.rs`).

| Endpoint | Content |
|---|---|
| `GET /api-docs/openapi.json` | OpenAPI document |
| `GET /swagger-ui/` | Swagger UI |
| `GET /health` | Health check |

Base paths for versioned routes: `/api/v1` (legacy-compatible) and `/api/v2` (Phase 2
meeting domain).

### Current contract

**PHASE 2 BACKEND — COMPLETE.** The production-ready backend publishes OpenAPI **1.1.0**:
**187 operations** — 49 v1, 136 v2 and 2 unversioned (`/`, `/health`). The same document is
checked in as [`packages/contracts/openapi/meeting-service.json`](../../packages/contracts/openapi/meeting-service.json)
and CI fails when it differs from the code.

The server advertised in Swagger ("Try it out") is `SWAGGER_SERVER_URL`, defaulting to
`https://api.cfmeet.com`; staging sets `https://api.staging.cfmeet.com`.

| Where | What it serves |
|---|---|
| Checked-in contract | 1.1.0 · 187 operations |
| Staging — `https://api.staging.cfmeet.com/swagger-ui/` | 1.1.0 · 187 operations |
| Production-ready backend — `127.0.0.1:18081` on the production host | 1.1.0 · 187 operations |
| Public — `https://api.cfmeet.com/swagger-ui/` | legacy 1.0.0 · 51 operations until the cutover |

Public `api.cfmeet.com` is temporarily still routed to the legacy backend until the frontend
compatibility deployment is confirmed. This is a deployment / cutover dependency, not
unfinished backend implementation; the public Swagger becomes 1.1.0 at the Nginx cutover.
Details: [`docs/phase2-certification/16-deployment-status.md`](../phase2-certification/16-deployment-status.md).

Narrative guides for meetings, participants, chat, LiveKit, OAuth, email queue and related capabilities live in [`services/meeting-service/docs/`](../../services/meeting-service/docs/README.md).

The current API still contains several account/admin/developer endpoints inherited from the original RTC backend. Their presence does not make this service the universal CF Meet backend. Future platform domains receive their own service boundaries as their phases begin.

Shared/exported contract strategy is described in [`packages/contracts`](../../packages/contracts/README.md).


## Part III communications (`services/calling`)

The Calling service publishes the **public Part III developer documentation** directly from
the authoritative contract files in `packages/contracts`. The Swagger page intentionally
contains only customer/client-facing Calling and Privacy Relay APIs. Service-to-service
Privacy Relay operations, Telephony execution APIs, provider callbacks and operator
interfaces remain internal and are not listed in public Swagger.

| Endpoint | Content |
|---|---|
| `GET /swagger-ui/` | Part III Swagger UI with a Calling / Privacy Relay selector |
| `GET /api-docs/calling/openapi.json` | Public-only Calling OpenAPI (83 paths, 127 operations including `/health`) |
| `GET /api-docs/privacy-relay/openapi.json` | Public Privacy Relay OpenAPI (19 paths, 22 operations) |

That is **149 documented public Part III operations** including the Calling health check
(**148 product/customer operations** when health is excluded).

The source contracts remain:

- `packages/contracts/calling/http/v1.openapi.json` — the full Calling contract,
  including internal routes used for conformance testing. The runtime public document
  removes every `/internal/*` path and internal-only security schemes before serving it.
- `packages/contracts/privacy-relay/http/v1.openapi.json` — the public Privacy Relay
  contract.
- `packages/contracts/privacy-relay/internal/v1.openapi.json` and
  `packages/contracts/telephony/internal/v1.openapi.json` — internal contracts only;
  deliberately **not** exposed in public Swagger.

The OpenAPI documents use a relative `/` server so Swagger "Try it out" targets the same
public API host that served the documentation. Deployment/gateway routing must therefore
send the documented public paths to their owning service while keeping internal paths
cluster-only.
