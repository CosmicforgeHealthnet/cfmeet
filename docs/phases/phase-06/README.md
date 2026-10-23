# Phase 06 — CF Meet Part VI: Developer & Enterprise Platform

Chapters 1–5 (foundation). Decision record: [ADR 0055](../../docs/decisions/0055-developer-platform-and-api-gateway-foundation.md).
Staging only: nothing is merged to `main` or deployed.

## 1. Repository baseline

| Item | Value |
|---|---|
| Audit date | 2026-10-23 |
| `origin/main` at audit | `5c2430c19449bbde28993dda2bd88fe1ffde9e92` (Merge PR #28: restore Vercel frontend deployments) |
| `origin/staging` at audit | `1ddfd70ad3dac7e52b3b53a913707ad224d48335` (ancestor of `main`; 45 commits behind, 0 ahead) |
| Synchronization | `staging` fast-forwarded to `main` (`git merge --ff-only`): no history rewritten, no commit lost |
| Synchronized SHA (Part VI base) | `5c2430c19449bbde28993dda2bd88fe1ffde9e92` |
| Existing domain services | meeting-service, calling, platform-access, numbering, privacy-relay, telephony, sms-gateway, messaging (unchanged) |

Existing checks executed on the synchronized base together with the Part VI changes:
`packages/service-auth` tests (both feature sets), the contract validator and its tests,
Swagger bundle check, service-boundary guard and its mutation tests, operations registry,
secret scan, database isolation script, and Platform Access's full suite (it links the
changed shared registry). See [`validation.md`](validation.md).

## 2. What Chapters 1–5 implement

| Requirement | Where | Status |
|---|---|---|
| Developer Platform service (config, startup, own DB + migrations, API + worker binaries, health / readiness, service-auth, typed errors, logging, correlation, Platform Access client boundary) | `services/developer-platform/` | Done |
| API Gateway (Axum runtime, probes, typed config, Platform Access credential auth, authoritative context, short-lived signed context, header stripping, default-deny REST/WebSocket routing model, correlation, Redis config, safe errors / timeouts / 503) | `services/api-gateway/` | Done |
| Identities and least-privilege permissions | `packages/service-auth` (`cfmeet.developer-platform`, `cfmeet.api-gateway`, `developer-request.forward`) | Done |
| Contracts registered in the manifest | `packages/contracts/platform-developer/` (4 contracts), `cfmeet.platform.common` 1.10.0 | Done |
| SDK workspace / generation scaffolding | `sdks/typescript`, `sdks/dart`, `sdks/generate.py` | Scaffold only |
| Local runtime, networking, database configuration | `infra/api-gateway/`, `infra/deployment/postgres/*` (developer-platform role set) | Done (local only) |
| Makefile targets, targeted CI | `Makefile` (`developer-platform-*`, `api-gateway-*`, `sdks-check`), `.github/workflows/developer-platform.yml`, `api-gateway.yml` | Done (validation only, no deploy) |
| Feature flags (default off), env examples | `DEVELOPER_PLATFORM_GATEWAY_API_ENABLED`, `API_GATEWAY_PUBLIC_ROUTING_ENABLED`, `API_GATEWAY_WEBSOCKETS_ENABLED`; `services/*/.env.example` | Done |
| Boundary guards | `scripts/service-boundaries` (`GATEWAY_OWNS_DATA`, `IDENTITY_HIERARCHY_DUPLICATED`, `PRODUCT_ENDPOINT_IN_PLATFORM`), operations registry entry | Done |
| Developer Console boundary | [`developer-console-boundary.md`](developer-console-boundary.md); `apps/web/src/app/dashboard/developers/` unchanged | Defined |
| Deployment integration requirements | [`deployment-integration.md`](deployment-integration.md) | Recorded, not performed |

Unchanged on purpose: `frontend.yml`, Vercel, `deploy-backend.yml`, the production VPS,
production secrets, live Nginx routing, the root compose files, every domain service's
code, contracts and Swagger documents (two Swagger bundles were regenerated only because
the shared `cfmeet.platform.common` service-identity description changed).

## 3. Explicitly NOT implemented (later sections)

| Section | Work |
|---|---|
| 50 | Developer Platform functional API: projects / applications UX, API key management through Platform Access, usage, logs, webhooks management, console features |
| 51 | The public route catalogue (every domain service route behind the gateway), receiver-side gateway permissions in each domain service, WebSocket forwarding, edge / Nginx migration |
| 52 | SDK functionality (typed clients, auth helpers, retries, pagination, WebSockets, publishing) |
| 53–57 | Quotas / rate limiting on Redis, enterprise features and the remaining Part VI chapters |
| Part VII | Payments, carrier integration, expansion — out of scope |

## 4. Validation evidence

See [`validation.md`](validation.md).
