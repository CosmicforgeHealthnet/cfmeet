# API documentation

## Labeled Swagger operations (all non-Meeting services)

Non-Meeting service API operations are grouped by named OpenAPI `tags`, not
the Swagger UI catch-all `default` tag. This includes *both* external and
internal engineering catalogs; the public/private documentation routing remains
a separate audience-specific filter.

- Calling: **136 operations**, including Call Management, Call Controls,
  Calling Devices, Calling Signaling, Call History, Recording & Consent, IVR,
  Voicemail, Call Queues, SIP Devices, Enterprise Policies and internal events.
- Messaging: **99 operations** grouped by Conversations, Messages, Membership,
  Attachments, MLS Group Encryption, Device Keys & E2EE, Realtime, Presence,
  Search, Moderation, SMS Bridges and Compliance; Messaging internal: **7**.
- Numbering / DCNs: **98 operations**, including Search, Reservations,
  Assignments, Catalog, KYC, Porting, Risk, and administrative/internal sections.
- Platform Access internal: **37 operations** grouped by identity, authorization,
  credentials and administration.
- Privacy Relay: **22 operations**; Privacy Relay internal: **25**.
- SMS Gateway: **11 operations**; SMS Gateway internal: **4**.
- Telephony internal: **17 operations**.
- Platform internal HTTP conventions: **0 operations**, components-only by design.

**Total: 456 non-Meeting source-documented operations**. Each has an
`operationId`, concise `summary`, meaningful `description`, and exactly
one `tag` with a top-level section definition.

The authoritative files in `packages/contracts/**` and the committed
self-contained `packages/contracts/bundled/**` variants contain identical
operation labels. Both are checked by
`python3 scripts/contracts/check_swagger_labels.py` and
`python3 scripts/contracts/bundle_openapi.py --check` during CI.

This is **documentation-only**. It does not modify endpoint URLs, route
handlers, authentication/authorization, schemas, SDK behavior, frontend
deployments, production E2EE, or public/private access controls. The public
Swagger will show the improved group headings **only after the changed Meeting
Service documentation image is deployed and verified**.


## Public and private API documentation

**Public:** `https://api.cfmeet.com/swagger-ui/` remains the default read-only
Swagger explorer. It includes filtered customer/developer contracts for Meetings,
Calling, Messaging, Numbering / DCNs, Privacy Relay and SMS Gateway. The JSON
returned by `/api-docs/*.json` is filtered **on the server**: removing entries
from the dropdown alone is not a security or documentation split.

| Public selector | Public JSON | Source (unchanged) |
|---|---|---|
| Meetings | `/api-docs/openapi.json` | Meeting Service generated OpenAPI |
| Calling | `/api-docs/calling.json` | `calling/http/v1.openapi.json` |
| Numbering / DCNs | `/api-docs/numbering.json` | `numbering/internal/v1.openapi.json` (public operations only) |
| Privacy Relay | `/api-docs/privacy-relay.json` | `privacy-relay/http/v1.openapi.json` |
| SMS Gateway | `/api-docs/sms-gateway.json` | `sms-gateway/http/v1.openapi.json` |
| Messaging | `/api-docs/messaging.json` | `messaging/http/v1.openapi.json` |

At this commit's contract baseline, **476** HTTP operations are intended for
the public documentation view (193 Meetings, 125 Calling, 52 Numbering,
20 Privacy Relay, 6 SMS, 80 Messaging). This is **NOT** the frontend team's
API consumption count, nor a certification that each service is deployed.
Server-side filtering removes internal-only routes, provider callbacks, privileged
admin routes, and select operational compliance endpoints. These APIs continue
to exist and remain described in the canonical contracts.

**Private engineering catalog:** use the offline generator:

```bash
python3 scripts/swagger/test_private_docs.py
python3 scripts/swagger/build-private-docs.py --out /tmp/cfmeet-private-docs-new
```

This produces a separate, **complete 12-document** static catalog from
`packages/contracts/openapi` and `packages/contracts/bundled`. It includes
**661** source-documented HTTP operations, including Platform Access, Messaging
internal, SMS Gateway internal, Privacy Relay internal, Telephony and Platform
internal HTTP. The generator refuses to write inside the repository and refuses
non-empty output destinations to prevent accidental publication.

The **recommended first deployment is loopback-only, behind SSH and Nginx
HTTP authentication**, using
`infra/deployment/private-api-docs.nginx.conf.example`. This lets authorized
engineers access the full catalog without publishing internal API definitions.
For a dedicated `internal-docs.cfmeet.com` domain, provision TLS plus an
independently verified VPN/SSO access gate first; the hostname is **not
created automatically** by this code change.

**Deployment ordering:**

1. Install and validate the private catalog **in its own protected site**,
   including all 12 documents and access-denied checks from unauthorized hosts.
2. Verify the private reader's access and audit logs; fail closed if authentication
   or network restrictions are missing.
3. Only then deploy the updated Meeting Service that filters public documentation.
4. From an unauthenticated network, verify public selector shows exactly six
   services and old `/api-docs/platform-access.json`,
   `/api-docs/privacy-relay-internal.json` and
   `/api-docs/telephony.json` return 404.
5. Verify every public JSON renders and Swagger still has Try-it-out disabled.
   Verify production feature/API routes, auth, and Nginx routing were not changed.
6. Keep a rollback of the previous docs/Meeting image. Do not expose the private
   catalog on the public API hostname.

**Security:** documentation visibility is NOT endpoint authorization. Existing
application/service credentials, tenant-scoped authorization, network controls,
and provider callback validation remain mandatory. The public Swagger host is
not a proxy for all services: use each service's real authorized ingress for API
traffic.


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
