# Phase 2 — Meeting & Conference Platform

This folder is the documentation home for the Phase 2 implementation programme.

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
