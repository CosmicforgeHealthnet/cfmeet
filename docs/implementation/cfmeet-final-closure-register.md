# CF Meet — final closure register (recovery ledger)

Mandate: final master closure (all 18 sections, 149 findings, 22 journeys). Reference commit `ab349836` (`staging`, 2026-11-08). This file is the resume point after any editor interruption: read it, then `git log origin/staging` and the branches named below.

Rules kept: no production change, no organization cutover, no force-push, owner/external gates stay disabled and are listed in §4.

## 1. Recovery baseline (2026-11-08)

| Item | State |
|---|---|
| `origin/staging` | `ab349836` — 50/50 CI runs green (push matrix + drill + Phase 2 + Hub) |
| `origin/main` | `586b88cb` (no production change by this programme) |
| Open PRs | #39 `staging` → `main` (not touched; production release needs authorization) |
| Worktrees | all clean; unmerged work: `blueprint/s17-web-comms2` (web Messages/Contacts UI, local only), `blueprint/s17-org-relay` (teams/policies relay WIP, local only) |
| Section 18 matrix | PASS 6 · PARTIAL 13 · NOT_COVERED 1 (J-10) · EXTERNAL_BLOCKED 2 |

Confirmed open gaps (checked in code, not only in reports):

1. Organizations outbox never delivers: `services/organizations/src/main.rs` passes `DisabledDispatcher`; `store::emit` writes only the `cfmeet.reconciliation` record destination.
2. No organization-scoped Messaging and no offboarding revocation (J-10).
3. No real-process organization lifecycle journey (J-04/05/06/14/17; organization legs of J-07/J-12 rest on a fake Organizations).

## 2. Workstreams

| # | Workstream | Branch | State | Commits | CI |
|---|---|---|---|---|---|
| W1 | Organizations event delivery (producer + Meeting ingress) | `blueprint/final-org-events` | CODE COMPLETE, local tests green; branch CI running | `1ca9b010`, `7c759c09`, `04f8c5ce`, `2b945505` | 32/33 on `7c759c09` (Meeting fmt + guard array, fixed in `2b945505`); rerun pending |
| W2a | Organization offboarding worker | same | CODE COMPLETE, local tests green | `7c759c09` | as W1 |
| W2b | Organization-scope Messaging relay + realtime tokens | same | CODE COMPLETE, local tests green | `04f8c5ce`, `f129f442` | pending |
| W3 | Real-process organization lifecycle certification (J-04/05/06/07/10/12/14/17/19) | same | J-04/05/06/10/14/19 passed in CI (42536543145); J-17 passed (42543307766); J-07 found a real ICS export bug (fixed `bde8e596`); full rerun pending | `3aa7a562`…`bde8e596` | pending on `bde8e596` |
| W4 | Account / contact / phone gaps (J-01/02/03/13) | same | CODE COMPLETE: positive password reset, QR route, PROVIDER_BLOCKED label, DP console session revocation | `5dbf9772`, `adf25521`, `6f71808b` | pending |
| W5 | Section 17 recovered work: web Messages/Contacts, ORG-006 teams/policies relay, organization Messages web view, member resolve | same | CODE COMPLETE, local tests green | `62104239`, `f2ea1ab2`, `77fcd86b`, `ca4072cc` | pending |
| W6 | RTC Egress capture on staging builds (J-12 media) | same | PASSED (42541119111) | `rtc capture workflow` | done |
| W7 | Documents C (API inventory) and G (activation runbook) | same | DRAFTED | `250d6dd7`, `3b41cc2f` | n/a |

## 3. Workstream records

### W1 — Organizations event delivery

- **Functionality:** every organization event is written in the change's transaction once as the `cfmeet.reconciliation` record (never sent) and once per subscribed consumer (`outbox::subscribers`; the Meeting Service). `HttpEventDispatcher` delivers over signed service HTTP to `POST /internal/v1/organization-events`; 4xx → dead letter, outage → exponential backoff; claims never overtake an undelivered earlier row of the same aggregate. Operator status and audited dead-letter replay.
- **Consumer (Meeting Service):** dedupe on `eventId` (digest; conflicting body → 409), monotonic fences per aggregate (older → STALE), projection refresh for lifecycle / policy / ownership / scope events (shadow / authoritative mode), desired member access for membership events.
- **Services:** organizations, meeting-service, packages/service-auth, packages/contracts.
- **APIs:** Organizations `GET /internal/v1/admin/event-delivery`, `POST /internal/v1/admin/event-delivery/replay` (`organization.administer`); Meeting `POST /internal/v1/organization-events` (`meeting-organization-events.apply`).
- **Contracts:** platform common 1.16.0 (new permission), organizations internal 1.3.0.
- **Migrations:** Meeting additive history `organization_integration_migrations` (`m20261110_000001_organization_integration`: inbox, fences, member access, scope environments), binary `organization-integration-migration`, wired into image, CI, certification stacks, deploy scripts, backup catalog; required at startup only when `ORGANIZATION_EVENTS_ENABLED=true`.
- **Config (off by default):** Organizations `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service`, `CF_MEETING_SERVICE_BASE_URL`, signing key; Meeting `ORGANIZATION_EVENTS_ENABLED=true` + trust bundle granting `cfmeet.organizations` `meeting-organization-events.apply`.
- **Tests (local, real PostgreSQL/Redis):** organizations full suite; service-auth 39+; meeting `organization_events` 3/3.

### W2 — Organization offboarding and organization-scope Messaging

- **Offboarding worker:** applies desired access (removal / suspension → principal SUSPENDED; re-admission → ACTIVE) to the member's principals in the organization's own scope through Platform Access. Messaging and Calling resolve principals per request and revalidate realtime sessions (≤ 30 s), so organization reads / sends / calls / subscriptions stop; the personal principal is a different principal and is untouched. Retries forever with backoff; a newer version arriving mid-pass stays due.
- **Organization Messaging:** `/api/v2/organizations/{id}/messaging/relay/{path}` (5 methods) and `/session-tokens`, the personal allowlist run in the organization's FIRST_PARTY triple (ADR 0056 §4). Gate: membership, organization status and `features.messaging` decided by Organizations per request; Messaging binding per organization (`service-bindings/messaging`, Organizations migration `m20261110_000004`).
- **No new store:** Messaging, its delegated surface and realtime gateway are reused; no duplicate authority.
- **Contracts:** meeting v2 operations 188 → 194 (gateway EXCLUDED `FIRST_PARTY_ORGANIZATIONS`), OpenAPI snapshot, route manifest and first-party ingress record regenerated.
- **Tests (local):** meeting `organizations_authority` 24/24 (organization scope + member principal, allowlist refusal, session binding with organization-environment reference, fail-closed cases), `openapi_contract` 9/9, `api_contract_matrix` 13/13, migration `destructive_guard` 3/3.
- **Owner activation steps (kept off):** create a Messaging service account in each organization triple and bind it (`PUT .../service-bindings/messaging`); grant `messaging-application.delegate` for organization triples in Messaging's trust bundle (already granted to Meeting); set `ORGANIZATION_CALLING_SCOPE=organization` + `CF_MESSAGING_BASE_URL` on Meeting; bootstrap the organization environment's Messaging policy to allow MANAGED.

## 4. Owner approvals and external gates (kept disabled)

| Gate | Affected operation | State |
|---|---|---|
| Production Organizations authority migration | `ORGANIZATIONS_AUTHORITY_MODE` beyond `legacy` in production | not approved |
| Production deployment | any `main` promotion / deploy workflow | not approved |

## 5. Consolidation closure checkpoint (2026-11-09)

- Branch audit: 40 remote heads; 38 fully contained in `staging` (36 ancestors, 4 `s18-sip-race*` patch-identical to `e9950455`), `blueprint/final-org-events` the only unique work (35 commits, fast-forward). See `cfmeet-branch-consolidation-report.md`.
- Integration branch for the closure: `claude/cfmeet-consolidation-closure-vk2pe3` (temporary; fast-forwarded into `staging`, then deleted).
- Fixed: J-07 disclosure (`a48b0413`), J-12 harness (`84b981b1`), SEC-004 (`1d8bf9c2`), ORG-011/MTG-016 (`89efdc91`). Added: J-08 (`3b8b7f42`), J-11 (`d7959fd7`), J-18 real-schema drill (`42b415fd`).
- Part VI certification green on `84b981b1` (run 42579359634): `ORGANIZATION_LIFECYCLE_REAL_E2E … PASS`, `ORGANIZATION_CUTOVER_REAL_E2E … PASS`.
- Final documents: closure report (A), certification report (D), audit closure register (E), frontend handover + endpoint reference (C/B), runbook update (F), branch report (G).
- W1–W7 above: all merged; their "pending" CI notes are superseded by the runs in the certification report.
