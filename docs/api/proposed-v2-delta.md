# CF Meet — proposed API delta (Blueprint v1.0, Section 1)

**Status:** proposal, checked against the registered-operation inventory before any handler is written. Exact names are frozen per section when the OpenAPI contract for that section is committed; a route listed here is not an implemented API.
**Inventory used:** [`docs/audits/cfmeet-api-inventory.csv`](../audits/cfmeet-api-inventory.csv) — 1,066 registered operations (650 public, 166 internal service, 94 delegated mirror, 100 admin/operator, 42 infrastructure, 9 provider callbacks, 5 WebSocket). Still exact on `staging` `0587b37a`: no source change since the audit baseline.
**Collision method:** every proposed `METHOD path` was normalised (path parameters → `{}`) and compared to all 1,066 inventory rows; same-method rows sharing the last two path segments were listed as *similar* and reviewed by hand. Script and output are reproducible from the inventory (see the Section 1 record).

Classification: `EXISTING` (reuse as is), `EXTEND` (same operation, new fields/behaviour), `FACADE` (existing route, new authority behind it), `NEW` (no existing operation can do the job), `INTERNAL` (signed service-to-service), `CONDITIONAL` (only if a later inventory check proves a gap), `DEPRECATE`.

## 1. Account authority (meeting-service) — Section 2

| Method | Path | Class | Exact match in inventory | Why an existing operation cannot serve |
|---|---|---|---|---|
| GET | `/api/v2/me/sessions` | NEW | none | No session model exists (ACC-007); logout is all-or-nothing |
| POST | `/api/v2/me/sessions/{sessionId}/revoke` | NEW | none (similar: Hub `devices/{id}/revoke` revokes a Hub link, not an account session) | Per-session revoke needs a session id the JWT does not carry today |
| POST | `/api/v1/auth/oauth/google/exchange` | NEW | none | Replaces tokens in the callback URL (ACC-005); one-time code → token pair |
| GET | `/api/v2/me/security-events` | CONDITIONAL | none | User-facing slice of the new account audit table, only if the web settings page needs it |
| GET | `/api/v2/admin/audit-events` | NEW (ADMIN) | none | No account audit store exists (SEC-013) |
| POST | `/api/v1/auth/login`, `/refresh`, `/logout`, `/register`, `/forgot-password`, `/reset-password`, `/change-password`, `/verify-email`, `/resend-verification`, OAuth callback | EXTEND | yes (all EXISTING) | Token types, session ids, revocation epoch, uniform responses, rate limits; contract shapes stay compatible |
| POST | `/internal/v1/user-sessions/verify` | EXTEND | yes (INTERNAL) | Must enforce the revocation epoch and token type (ACC-002) |

## 2. First-party organizations (owner: `services/organizations`; public ingress: meeting-service `/api/v2` relay) — Sections 4–8

Inventory facts: meeting-service exposes exactly 5 organization routes; Developer Platform exposes 22 `/v1/organizations*` routes for **API customer workspaces** (a different authority by ADR 0056 — similarity is name-only, not a duplicate).

| Method | Path | Class | Exact match | Note / justification |
|---|---|---|---|---|
| GET | `/api/v2/me/organizations` | FACADE | **yes** (meeting-service, gateway-routed, in SDKs) | Contract preserved; extended additively with `createdBy`, `viewerRole`, suspended status (ORG-003, ORG-008) |
| GET | `/api/v2/organizations/{orgId}/members` | FACADE + EXTEND | **yes** (meeting-service) | Adds display name, status, pagination (ORG-006) |
| PUT | `/api/v2/organizations/{orgId}/members/{userId}` | FACADE → DEPRECATE for direct add | **yes** | Kept for compatibility; adding a new member requires an invitation (ORG-005); retirement per Section 16 |
| POST | `/api/v2/admin/organizations`, PATCH `/api/v2/admin/organizations/{id}` | FACADE (operator) | **yes** | Operator paths keep working through the new authority |
| POST | `/api/v2/organizations` | NEW | none | Self-service creation (ORG-002); idempotency key; returns provisioning state |
| GET | `/api/v2/organizations/{orgId}` | NEW | none (DP `GET /v1/organizations/{id}` = developer workspace) | Detail with createdAt/createdBy/profile (ORG-004) |
| PATCH | `/api/v2/organizations/{orgId}` | NEW | none (admin PATCH is status-only, `dto/v2.rs:821-825`) | Versioned profile edit |
| POST, GET | `/api/v2/organizations/{orgId}/invitations` | NEW | none | Email-bound hashed invitations (ORG-005) |
| POST | `/api/v2/organization-invitations/accept`, `/decline` | NEW | none | Token in body only, never in the URL |
| POST | `/api/v2/organizations/{orgId}/invitations/{invitationId}/revoke` | NEW | none | |
| PATCH | `/api/v2/organizations/{orgId}/members/{memberId}` | NEW | none | Versioned role/status change with last-owner rule |
| POST | `/api/v2/organizations/{orgId}/members/me/leave` | NEW | none | Self-leave (forbidden today, ORG-008) |
| POST | `/api/v2/organizations/{orgId}/ownership-transfer` | NEW | none | Atomic transfer (ORG-008) |
| POST | `/api/v2/organizations/{orgId}/suspend`, `/reactivate`, `/archive` | NEW | none | Lifecycle (ORG-008) |
| GET | `/api/v2/organizations/{orgId}/audit-events` | NEW | none | ORG-007 |
| GET | `/api/v2/organizations/{orgId}/provisioning` | NEW | none | Truthful readiness (Section 7) |
| GET | `/api/v2/admin/organizations` | NEW (ADMIN) | none | Operator listing (ORG-004) |
| GET, POST | `/api/v2/organizations/{orgId}/teams` | NEW | none | Section 6 |
| GET, PATCH | `/api/v2/organizations/{orgId}/teams/{teamId}`; POST `…/archive` | NEW | none | |
| GET, POST | `/api/v2/organizations/{orgId}/teams/{teamId}/members`; PATCH/DELETE `…/{memberId}` | NEW | none | |
| GET, PATCH | `/api/v2/organizations/{orgId}/policies` | NEW | none | Organization-level constraints; replaces the writer-less `profile_policy` (ORG-009) |
| GET | `/api/v2/organizations/{orgId}/roles/capabilities` | CONDITIONAL | none | Only if the workspace capability read cannot serve admin UIs |
| GET | `/api/v2/me/workspaces` | NEW | none | Personal + active organizations with context summaries (Section 8) |
| GET | `/api/v2/me/workspaces/{workspaceId}/capabilities` | NEW | none (Hub `GET /api/v2/communications/me/capabilities` is personal communications only) | Server-derived features per context |

Internal (signed, `services/organizations`), all NEW: `GET /internal/v1/organizations/{id}`, `POST /internal/v1/organizations/memberships/resolve`, `POST /internal/v1/organizations/authorization/evaluate`, `POST /internal/v1/organizations/scopes/resolve`, `GET /internal/v1/organizations/reconciliation/status`, plus the migration import and shadow-compare operations (operator-only, Section 4). Event replay is CONDITIONAL on the event transport chosen in Section 3.

## 3. Meetings and calendar (meeting-service) — Section 10

| Method | Path | Class | Exact match | Justification |
|---|---|---|---|---|
| GET | `/api/v2/me/conferences?from&to&status&role&organizationId&cursor` | NEW | none | No cross-space occurrence query (MTG-005); replaces N+1; organization calendar is a filter, not a route |
| GET | `/api/v2/conferences/{id}/calendar.ics` | NEW | none | No ICS (MTG-007) |
| GET | `/api/v2/meeting-spaces/{id}/calendar.ics` | NEW | none | Series export |
| GET | `/api/v2/organizations/{orgId}/meeting-spaces` | CONDITIONAL | none | Only if the agenda filter cannot serve org managers (ORG-011) |
| POST | `/api/v2/conferences/{id}/reschedule`, `/cancel`; PUT `/api/v2/meeting-spaces/{id}/recurrence[/exceptions]` | EXTEND | yes | MTG-009/010/014 semantics |

## 4. Personal onboarding, notifications, calling, messaging — Sections 9, 11–14

| Method | Path | Class | Note |
|---|---|---|---|
| POST | `/api/v2/me/phone-verification/challenges`, `/confirm`, `/change` | NEW / PROVIDER_GATE | Disabled until an approved provider exists; returns PROVIDER_BLOCKED |
| GET, DELETE | `/api/v2/me/phone-verification[/status]` | NEW | Masked status only |
| — | Hub contacts, public ID/QR, invites, requests, blocks, privacy, notification preferences, delivery status, devices, inbox (73 ops) | EXISTING | Reused; no new contacts or notification-preference API |
| GET | `/api/v2/me/notifications` | CONDITIONAL | Hub `GET /api/v2/communications/inbox` and `/activity` are checked first |
| GET | `/v1/enterprise/recordings` | NEW | Recording discovery; today only by id (CAL-017) |
| — | `/api/v2/messaging/relay/*path` | EXTEND | Add PATCH, query allowlist parity, `after` cursor (MSG-001…003); no new message API |

## 5. Developer platform and legacy — Sections 15–16

No new customer endpoints. Existing `/v1/organizations*`, projects, environments, credentials, webhooks and usage are activated as they are. `DEPRECATE` candidates (after measured migration only): 23 v1 meeting write operations, 8 legacy developer API-key/webhook routes, 10 Calling webhook/usage/limits copies after the Part VI cut-over.

## 6. Running total (proposal only)

Counted against the 1,066 baseline: about 31 NEW public first-party operations for organizations/workspaces, 5 NEW account operations, 3 NEW meeting operations, 4 NEW phone operations (provider-gated), 1 NEW calling operation, about 6 NEW internal Organizations operations, plus CONDITIONAL items. These numbers are not an API count: the real before/added/modified/deprecated/after figures are recomputed from the routers at the end of each section (blueprint Appendix B).
