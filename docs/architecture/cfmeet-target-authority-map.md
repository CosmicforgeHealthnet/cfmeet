# CF Meet — target authority map (Blueprint v1.0)

**Status:** Section 1 deliverable. Frozen target; current state verified against `staging` = `0587b37a` (code identical to the audit baseline `586b88cb`, `git diff 586b88cb 0587b37a` touches only `docs/audits/`).
**Decision record:** [ADR 0056](../decisions/0056-first-party-organizations-and-workspace-boundaries.md). **Ownership rules enforced by:** [`service-data-ownership.md`](service-data-ownership.md) and `scripts/service-boundaries/check_service_boundaries.py`.

## 1. Entities and their single writer

| Entity | Canonical writer (target) | Writer today | Stable key | Readers (how) | Change in this programme |
|---|---|---|---|---|---|
| Human CF Meet account, credentials, sessions | meeting-service (account authority) | same | `users.id` | every service via verified bearer → actor; Developer Platform via session bridge | Section 2 hardening; session table; no move |
| First-party business organization + profile | **organizations** (new) | meeting-service `organizations` | `organizationId` (existing UUIDs preserved) | Meeting, Calling, Messaging, Hub via signed internal API / events | Sections 4–5: migrate, then single writer |
| Organization membership, invitations, ownership | **organizations** | meeting-service `organization_memberships` (no invitations) | `(organizationId, userId)` | same | Sections 4–5 |
| Teams, role grants, organization policy | **organizations** | — (profile_policy JSONB only, no writer) | `teamId`; grant id | same | Section 6 |
| Organization ↔ technical scope mapping, provisioning saga | **organizations** | — | mapping id; workflow id | Calling, Messaging (scope resolve) | Section 7 |
| Technical tenant / application / environment / principal / credential | platform-access | same | PA ids | all customer-facing services | possibly a FIRST_PARTY tenant delegation path (Section 7) |
| Developer workspace, developer members/roles, projects, plans, usage, central webhooks | developer-platform | same | EXTERNAL `tenantId` | API Gateway, Developer Portal | Section 15 activation only |
| Meeting Spaces, conferences, schedules, recurrence, admission, calendar | meeting-service | same | v2 ids | Hub (owner APIs) | Section 10 agenda/ICS; org filters through Organizations |
| Calls, enterprise PBX config, recordings, voicemail | calling | same | call id; `(tenant, app, env)` | Hub (owner APIs) | Section 12 per-organization scope and actor |
| Conversations, messages, devices, MLS/E2EE | messaging | same | conversation id | Hub, meeting relay | Section 13 relay parity, MANAGED realtime |
| Contacts graph, public ID/QR, blocks, privacy, unified timeline | communications (Hub) | same | profile id | first-party app via meeting relay | Section 14 organization-aware views, revocation fan-out |
| Numbers, SMS, relay, telephony | numbering / sms-gateway / privacy-relay / telephony | same | — | — | provider gated, unchanged |
| Backups and restore drills | infrastructure owner (`infra/deployment/backup`) | partial (no meeting/communications/developer-platform coverage, SEC-010) | — | — | Section 2 |

## 2. Identity and context flow (target)

```
Browser / mobile ──bearer──▶ meeting-service /api/v2 (account authority: verifies user, revocation epoch, account status)
                                 │  signed service token (acting userId, 60 s, one audience)
                                 ├──▶ organizations      /internal/v1/...  organizations, members, invitations, teams, policy, provisioning
                                 ├──▶ communications     (existing relay, 73 ops)
                                 └──▶ messaging          (existing relay)
Calling / Messaging / Meeting / Hub ──signed──▶ organizations  /internal/v1/organizations/{memberships,authorization,scopes}/…
organizations ──signed──▶ platform-access (delegation: scope create/verify/suspend)
organizations outbox ──signed, at-least-once──▶ owner projections (meeting-service org projection, Hub, Calling, Messaging)
Developer Portal ──session bridge──▶ meeting-service /internal/v1/user-sessions/verify ; developer roles checked by developer-platform
External apps ──PA credential──▶ api-gateway ──signed delegated context──▶ owner services
```

Rules: the client supplies only the *requested* context (organization id in the path or query). Membership, role, capability, organization status and technical scope are resolved server-side for every request. Client-supplied `tenantId`, `memberRole` or `actingPrincipalId` are never authority.

## 3. Current → target gaps by authority

| Authority | Current gap (audit ID) | Target section |
|---|---|---|
| Account | revocation epoch not enforced on v1 + bridge (ACC-002), token types (ACC-001), Google pre-hijack (ACC-004), no sessions (ACC-007), no audit (SEC-013), no backups (SEC-010) | 2 |
| Organizations | three models (ORG-001), admin-only creation (ORG-002), no detail/edit/invite/audit/archive (ORG-003…008) | 3–6 |
| Org ↔ technical scope | shared enterprise scope (CAL-007) | 7, 12 |
| Workspace | no workspace list or capabilities | 8 |
| Meetings | no agenda/ICS/notifications (MTG-005…007), schedule edge cases | 10–11 |
| Messaging | relay contract defects, no MANAGED realtime (MSG-001…004) | 13 |
| Hub | no organization-aware views, revocation fan-out gaps (ACC-008/010, DATA-005/006) | 14 |
| Developer | gateway and Developer Platform not deployed (PLT-005), legacy keys/webhooks (PLT-002/003) | 15–16 |

## 4. Non-negotiable boundary checks (carried into every section)

1. One active writer per entity; migration windows use a mutation guard, never dual writes.
2. No cross-service database reads, writes or foreign keys (boundary script in CI).
3. No second credential, password or identity store.
4. Organization roles never grant developer, Platform Access operator or personal-data rights.
5. Removal and suspension fail closed in every consumer, including when Organizations is unreachable.
6. External gates (E2EE, push, SMS, PSTN, numbers) are reported as gated, never simulated as live.
