# ADR 0001: Platform monorepo layout and modular-monolith core API

- **Status:** Accepted
- **Date:** 2026-10-03

## Context

CF Meet was consolidated from `rtc_backend` and `rtc_frontend` into one monorepo
with `backend/` and `frontend/` directories. The platform scope now covers
meetings, realtime calling, Privacy Relay, messaging, SIP/PSTN telephony,
Digital Communications Numbers, enterprise communications and developer APIs.
`backend/` + `frontend/` does not leave room for several services, several
application surfaces or shared contracts.

## Decision

1. Adopt the layout `apps/` (application surfaces), `services/` (platform
   services), `packages/` (shared contracts), `infra/`, `docs/`, `scripts/`.
2. Rename in place with `git mv`: `backend/` → `services/core-api/`,
   `frontend/` → `apps/web/`. Published history is not rewritten.
3. `services/core-api` stays **one deployable modular monolith**. Domains are
   internal modules. Services are extracted only for scaling, security/isolation,
   runtime/cadence or failure-isolation reasons, each recorded in its own ADR.
4. `packages/contracts` is the single home for cross-component contracts. Each
   contract has exactly one authority (today: core-api's utoipa-generated OpenAPI),
   and consumers use generated code, not hand-copied types.
5. Future services and apps are documented, not scaffolded.
6. Applications stay independently deployable: Vercel Root Directory `apps/web`,
   Docker build context `services/core-api`.

## Consequences

- Deployment settings must change once (Vercel Root Directory, image build context).
- `git log --follow` is needed to trace a file's pre-rename history.
- CI path filters follow the new directories, and contract changes will trigger
  consumer workflows once real coupling exists.
- Hand-written TS types in `apps/web/src/types` remain technical debt until
  contract generation is implemented.
