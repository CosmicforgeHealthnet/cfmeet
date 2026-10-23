# Delivery phases

Each CF Meet delivery phase has its own folder for specification, decisions, implementation notes and acceptance/closure evidence.

```text
docs/phases/
├── phase-01/
├── phase-02/   ← Meeting & Conference Platform
├── ...
└── phase-10/
```

## Phase 2 implementation locations

Phase 2 — Meeting & Conference Platform — is implemented primarily in:

- `services/meeting-service/` — backend/control plane
- `apps/web/` — web meeting experience
- `packages/contracts/` — shared meeting API/event/permission contracts when they cross component boundaries
- `infra/livekit/` — LiveKit/media configuration
- `docs/phases/phase-02/` — implementation specification and acceptance evidence

Future calling, Privacy Relay, messaging, telephony and numbering services are outside the Phase 2 meeting-service implementation unless the Phase 2 specification explicitly defines an integration boundary.

The repository migration/restructure that preceded Phase 2 is recorded in [`../migration/MONOREPO_MIGRATION.md`](../migration/MONOREPO_MIGRATION.md).

## Part VI — Developer & Enterprise Platform

Chapters 1–5 (foundation) are recorded in [`phase-06/`](phase-06/README.md):
`services/developer-platform/`, `services/api-gateway/`,
`packages/contracts/platform-developer/`, `sdks/`, `infra/api-gateway/` and
[ADR 0055](../decisions/0055-developer-platform-and-api-gateway-foundation.md).
