# Phase 2 — Meeting & Conference Platform

This folder is the documentation home for the Phase 2 implementation programme.

## Status

**PHASE 2 BACKEND — COMPLETE.** Production-ready backend: Swagger 1.1.0, 187 operations
(49 v1, 136 v2), 22 migrations, production pre-cutover certification PASS. Public
`api.cfmeet.com` is temporarily still routed to the legacy backend until the frontend
compatibility deployment is confirmed — a deployment / cutover dependency, not unfinished
backend implementation. See
[`docs/phase2-certification/16-deployment-status.md`](../../phase2-certification/16-deployment-status.md).

## Implementation ownership

| Concern | Repository location |
|---|---|
| Backend/control plane | `services/meeting-service/` |
| Web UI | `apps/web/` |
| Shared contracts | `packages/contracts/` |
| LiveKit/media configuration | `infra/livekit/` |
| Phase specification, decisions, test evidence and closure reports | `docs/phases/phase-02/` |

## Service rule

`services/meeting-service/` remains one deployable modular monolith throughout Phase 2 unless an explicit architecture decision approves a split. Phase 2 work should extend internal meeting-domain modules rather than creating participant/auth/permission/waiting-room microservices by default.

## Scope note

Future platform services such as calling, Privacy Relay, messaging, telephony and numbering are separate domain boundaries. They are not to be implemented inside the Meeting Service merely because they are part of the wider CF Meet roadmap.

The full Phase 2 implementation specification should be added to this folder before implementation begins.

## Backend certification and closure (Parts M–N)

The Phase 2 backend closure evidence package (requirements matrix, migration, test,
system E2E, load, real RTC provider, recording / storage, security, failure / recovery,
staging, rollout, rollback, runbook and contract reports) lives in
[`docs/phase2-certification/`](../../phase2-certification/01-summary.md).
