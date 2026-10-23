# ADR 0055 — Developer Platform and public API Gateway foundation (Part VI §§1–5)

**Status:** Accepted (foundation; staging only — not deployed, live Nginx routing unchanged)
**Date:** 2026-10-23
**Scope:** CF Meet Part VI — Developer & Enterprise Platform, Chapters 1–5

## Context

Part VI opens CF Meet to developers and enterprises (first-party CosmicForge products and
external customers alike) through one published API. Eight domain services already exist
(Meeting Service, Calling, Platform Access, Numbering, Privacy Relay, Telephony, SMS
Gateway, Messaging), each authenticating customers through Platform Access and owning its
own database. Part VI must add a control plane and a single customer ingress without
creating a second identity authority, a universal communications backend or any coupling
between databases.

## Decisions

1. **Two new independently buildable services.**
   * `services/developer-platform` (`cfmeet.developer-platform`) — the developer /
     enterprise control plane: API and worker binaries, its OWN PostgreSQL database
     (`devplat_*`, history `developer_platform_seaql_migrations`), refuses to start with
     pending migrations.
   * `services/api-gateway` (`cfmeet.api-gateway`) — the public customer ingress. It has
     **no database** (a `DATABASE_URL` is refused; the boundary guard rejects tables and
     database drivers: `GATEWAY_OWNS_DATA`).
2. **Platform Access stays the only identity authority.** Tenants, applications,
   environments, service accounts, API credential verifiers, scopes and principals live in
   Platform Access only. The gateway authenticates every credential through
   `platform-credential.authenticate`; the Developer Platform re-validates every forwarded
   context through `platform-identity.read` and stores canonical Platform Access ids only
   (`devplat_environment_refs`: immutable tenant → application → environment references, no
   names, statuses, scopes or credentials). Guard rule `IDENTITY_HIERARCHY_DUPLICATED`
   rejects identity tables in Part VI services; `IDENTITY_AUTHORITY_ELSEWHERE` already
   rejects credential verifier machinery. There is no second credential authority.
3. **Signed request context, receiver-authoritative.** The gateway forwards with a fresh
   Ed25519 service token (`packages/service-auth`, 60 s default lifetime, exactly one
   audience) carrying the `DelegatedPlatformContext` built from Platform Access's authority
   chain. Tenant / project / environment ids in headers, query or body are never authority.
   Upstreams verify the token with their own trust bundle and keep **final resource
   authorization**. New registry entries (contract `cfmeet.platform.common` 1.10.0):
   * `developer-request.forward` — receiver `cfmeet.developer-platform`, grantable to
     `cfmeet.api-gateway` only;
   * `platform-credential.authenticate` additionally grantable to `cfmeet.api-gateway`;
   * `platform-identity.read` additionally grantable to `cfmeet.developer-platform`.
   The gateway receives no permission (it has no `/internal` surface and no trust bundle).
4. **Client authority headers are never forwarded.** Upstream requests are built from
   scratch through the shared internal client (method, `/internal/v1` path, validated
   query, JSON body, `Idempotency-Key`, the signed token). A client
   `X-CF-Service-Authorization` is refused; `X-CF-*`, `X-Tenant-*`, `X-Forwarded-*` and
   similar headers are dropped (names logged, never values).
5. **Default deny.** Only routes of a validated route table are served, and only when
   `API_GATEWAY_PUBLIC_ROUTING_ENABLED=true`; anything else is `NOT_FOUND` before a
   credential is examined. Route construction refuses Platform Access, Telephony, Privacy
   Relay, the operator console and the gateway itself as upstreams, public patterns outside
   `/v1/` or containing `internal`, `admin`, `operator`, `provider-callbacks`, `callbacks`,
   `metrics`, and upstream paths outside `/internal/v1/` or under administration. The route
   model covers REST and WebSocket; WebSocket routes require
   `API_GATEWAY_WEBSOCKETS_ENABLED` and their forwarding is Section 51
   (`PROTOCOL_NOT_ENABLED`). The foundation catalogue has one route,
   `GET /v1/developer-platform/context`.
6. **Safe failure.** Authority unreachable → 503; throttled → 429 with `Retry-After`;
   upstream deadline → 504 `UPSTREAM_TIMEOUT`; connection failure → 502
   `UPSTREAM_UNAVAILABLE`; upstream not configured → 503; body limit → 413. Error messages
   are constant and never echo input or upstream detail.
7. **Correlation.** Each customer request gets server-generated `X-Request-Id` and
   `X-Correlation-Id`; the correlation id is signed into every hop.
8. **Shared quotas later.** `API_GATEWAY_REDIS_URL` (TLS required outside local / test) is
   configuration only; quotas are not enforced in this foundation.
9. **Same contracts for everyone.** First-party and external tenants use the same routes,
   contracts and SDKs; they differ only by Platform Access scopes and trust
   classification. No product-specific endpoint (`PRODUCT_ENDPOINT_IN_PLATFORM`).
10. **Contracts and SDKs.** `packages/contracts/platform-developer/` registers
    `cfmeet.developer-platform.common`, `cfmeet.developer-platform.internal-api`,
    `cfmeet.api-gateway.common` and `cfmeet.api-gateway.http-api` (experimental).
    `sdks/typescript` and `sdks/dart` are scaffolds generated by `sdks/generate.py` from
    the public contracts only.
11. **Existing applications.** `apps/web/src/app/dashboard/developers/` remains the future
    Developer Console (no new frontend); its boundary is the published gateway API. The
    eight domain services, their contracts and Swagger documentation are unchanged.

## Not decided here / out of scope

The Section 50–57 functional work (Developer Platform resources and APIs, the Section 51
route catalogue and domain-service gateway permissions, Section 52 SDK functionality,
quotas, usage, console features), deployment to staging/production, Nginx routing,
payments, carrier integration and Part VII.

## Consequences

* The gateway can be scaled and restarted independently and holds no state to back up.
* Each domain service that joins the gateway needs its own receiver-side permission
  granting `cfmeet.api-gateway` (Section 51) — the gateway can never grant itself access.
* Platform Access sees one extra caller per ingress request (credential authentication);
  read caching remains a Platform Access concern.
