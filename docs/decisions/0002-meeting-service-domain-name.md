# ADR 0002: Rename `core-api` to `meeting-service` before Phase 2

- **Status:** Accepted
- **Date:** 2026-10-03

## Context

The first platform-layout restructure placed the imported Rust RTC backend under `services/core-api/`. That name is too broad for the expanded CF Meet roadmap because future capabilities—calling, Privacy Relay, messaging, telephony, numbering and developer communications—will have their own domain boundaries.

The existing Rust backend is primarily the control plane for the current meeting/conferencing product and is the implementation base for Phase 2 — Meeting & Conference Platform.

Calling it `core-api` could incorrectly imply that all future communications capabilities should be added to the same service.

## Decision

1. Rename `services/core-api/` to `services/meeting-service/` without changing business behavior.
2. Treat `services/meeting-service/` as the primary backend implementation home for Phase 2.
3. Keep the service as one deployable modular monolith during Phase 2; meeting subdomains are internal modules rather than separate microservices.
4. Keep `apps/web/` as the Phase 2 web experience, `packages/contracts/` as the shared contract boundary and `infra/livekit/` as the media infrastructure/configuration boundary.
5. Add future services—such as `calling`, `privacy-relay`, `messaging`, `telephony` and `numbering`—only when their implementation phases begin or a concrete extraction reason exists.
6. Preserve original imported history and perform the rename as a normal Git history step; do not rewrite published history.

## Consequences

- CI, root tooling, local Docker orchestration and deployment documentation use `services/meeting-service/`.
- The Vercel web root remains `apps/web`.
- The backend Docker build context becomes `services/meeting-service/`.
- `make meeting-service` is the preferred local command; `make backend` remains a compatibility alias for now.
- Some internal binary/package names inherited from the old RTC backend may continue to use `backend` until a separate cleanup is justified; directory/domain naming is authoritative.
- Phase 2 documentation and acceptance evidence belong under `docs/phases/phase-02/`.

## Supersession note

ADR 0001 remains the historical record of the monorepo restructure. This ADR supersedes only its `core-api` service naming, not the broader decisions to use `apps/`, `services/`, `packages/`, `infra/` and a modular-monolith-first approach.
