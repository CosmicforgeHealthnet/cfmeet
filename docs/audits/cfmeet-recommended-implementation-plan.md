# CF Meet — Recommended Implementation Plan (PROPOSAL ONLY)

**Audit phase:** Phase 1 — audit only · **Baseline:** `586b88cb121d07ac0821f5d334820bae3868f6c2` · **Date:** 2026-11-06

> **This is a proposal for owner review. Nothing in it has been executed.** No code, contract, migration, configuration or deployment was changed during the audit. Each workstream needs explicit owner approval, its own validation and delivery on `staging`, and a separate instruction before anything reaches `main` or production.

**Inputs:**
- [Executive report](cfmeet-platform-comprehensive-audit.md)
- [Feature gap register](cfmeet-feature-gap-register.md) (all finding IDs)
- [Meetings & calendar](cfmeet-meetings-calendar-audit.md)
- [Organizations](cfmeet-organizations-audit.md)
- [Data & architecture](cfmeet-data-and-architecture-gaps.md)
- [Frontend coverage](cfmeet-frontend-backend-coverage.md)
- [User journeys](cfmeet-user-journey-coverage.md)
- [Duplication report](cfmeet-api-duplication-report.md)

## 0. Owner decisions required first (PRODUCT_DECISION)

These decisions gate whole workstreams. Building before they are made risks rework.

| # | Decision | Options | Gates | Findings |
|---|---|---|---|---|
| D-1 | **Organization authority model** | (a) Meeting-service `organizations` becomes the product organization; the Developer Platform org and Platform Access tenant link to it 1:1. (b) Developer Platform org becomes canonical; meeting orgs become a projection. (c) Keep them separate and document the boundary. | WS-5, WS-8 (per-org calling scope), WS-10 | ORG-001, PLT-010, DATA-004, CAL-007 |
| D-2 | **Who may create organizations** | Platform admin only (today) vs self-service with limits and approval | WS-5 | ORG-002 |
| D-3 | **Calendar scope** | In-app agenda only vs + ICS export/invitations vs + Google/Outlook sync (spec: optional) | WS-4 | MTG-005, MTG-007 |
| D-4 | **Notification channels** | Email invitations/reminders (SMTP exists) vs + push (needs APNs/FCM/Web Push credentials) | WS-4, WS-9 | MTG-006, CAL-003, ACC-020 |
| D-5 | **Realtime for MANAGED chat without an E2EE device** | Allow realtime tokens for MANAGED-only principals vs keep polling until PRIVATE_E2EE activation | WS-7 | MSG-004 |
| D-6 | **Account lifecycle and compliance** | Account deletion/erasure, data export, retention, MFA requirement | WS-1 (MFA), WS-9 | ACC-011, ACC-013, DATA-009 |
| D-7 | **Part VI rollout** | Deploy the API Gateway and Developer Platform now (and point SDK/docs at them) vs defer and correct the public docs | WS-10 | PLT-005, PLT-004 |
| D-8 | **Mobile scope for Phase 2** | Full v2 parity vs meetings-only vs pause mobile | WS-6 | FE-021, MTG-001 |
| D-9 | **Legacy retirement window** | Timeline for `FEATURE_LEGACY_MEETING_WRITE_DISABLED` and retiring the legacy developer API | WS-6, WS-10 | MTG-017, PLT-002/003 |

## 1. Workstreams (smallest coherent batches)

Ordering principle: data protection and P0 security first, then the owner-visible Meetings defects, then the domain completions. Each workstream can be delivered and validated on its own.

### WS-1 — Account authority security hardening (P0/P1) · BACKEND (+web callback) · Complexity MEDIUM

- **Scope:**
  - **ACC-002:** enforce `credentials_not_before` in the v1 `middleware/auth.rs` and in `session_bridge.rs`.
  - **ACC-003:** revocation must not depend on `CF_COMMUNICATIONS_BASE_URL`.
  - **ACC-001:** add a token `typ` claim, refuse refresh tokens as bearers, add rotation with reuse detection and an absolute lifetime (ACC-017).
  - **ACC-004:** on Google sign-in, refuse to auto-activate a pending password account (or clear its password); check `aud`, `email_verified` and `iss`; persist the Google `sub`.
  - **ACC-005:** replace URL tokens with a one-time code exchange.
  - **ACC-006:** fix the username collision on Google sign-up.
  - **SEC-001:** fail fast when `JWT_SECRET` is missing or weak.
  - **SEC-004:** bind guest refresh to a live participant.
  - **SEC-007:** fail closed on revocation-check errors.
  - **ACC-014 / ACC-015:** uniform auth responses; per-IP plus per-account rate limits.
  - **SEC-013 / DATA-015:** append-only account security audit table.
  - **DATA-002 / ACC-023:** email normalization.
- **Proposed APIs:**
  - `POST /api/v1/auth/oauth/google/exchange` (one-time code → tokens)
  - `GET /api/v2/admin/audit-events` (read-only)
- **Migrations:**
  - `account_audit_events` (additive)
  - case-insensitive email unique index. This needs a **duplicate detection and merge step first**: MEDIUM–HIGH risk.
- **Contract changes:** token claims (backwards-compatible acceptance window), OAuth callback shape (the web callback page changes together).
- **Security:** dual-accept window for old tokens; force re-login after deploy if the owner prefers.
- **Validation:** new meeting-service tests covering forgot/reset/verify/change-password and OAuth, which have none today (INV-002).
- **Regression risks:** login/refresh for web and mobile; the Developer Console session bridge.

### WS-2 — Data protection and release safety (P0/P1) · INFRASTRUCTURE · Complexity MEDIUM

- **Scope:**
  - **SEC-010:** extend `infra/deployment/backup/backup.sh` to meeting-service (history table `seaql_migrations`), communications and developer-platform. Schedule backups through a GitHub Actions workflow with off-host copies. Add a restore drill.
  - **SEC-011 / DATA-013:** pre-migration snapshot in the release workflows.
  - **SEC-018:** separate the meeting-service runtime role from the migration (DDL) role.
  - **SEC-015:** ship only `up` in production migration images.
  - **SEC-016:** alert rules as code.
  - **SEC-017:** TLS automation for every public lineage.
- **APIs / migrations:** none (role grants only).
- **Validation:** restore drill into an isolated database. No production disruption: read-only dumps.
- **Dependencies:** owner approval of the off-host storage target.

### WS-3 — Meetings web fixes, the defects the owner noticed (P1) · FRONTEND · Complexity LOW–MEDIUM

- **Scope:**
  - **MTG-004:** show `latestConference.scheduledStartAt` (with the meeting timezone) instead of `createdAt` on the meetings list and dashboard.
  - **FE-001:** read `access.*` and `consents.*` in pre-join, so consent-gated and invite-only joins work.
  - **FE-007:** artifact field names.
  - **MTG-012:** use the space/occurrence timezone for entry and display.
  - **MTG-013:** host "Start" control in pre-join.
  - **MTG-015:** recurrence builder (no raw RRULE), with an occurrence preview.
  - **MTG-016 / ORG-010:** organization picker and the ORGANIZATION_ONLY policy.
  - **MTG-019 / FE-014:** remove the dead v1 branch.
- **APIs:** none. All data already exists.
- **Contract work:** generate the web v2 types from `packages/contracts/openapi/meeting-service.json` (or add contract tests) and run the web v2 test suites in CI (FE-015).
- **Validation:** browser E2E for Journey C/D against real services (new).

### WS-4 — Scheduling correctness, calendar and notifications · BACKEND + FRONTEND · Complexity MEDIUM–HIGH

- **Scope:**
  - **MTG-005:** cross-space agenda API.
  - **MTG-008:** virtual recurrence expansion beyond the 30-day horizon.
  - **MTG-009:** reschedule keeps the end time and enforces the create-time rules.
  - **MTG-010:** reject past starts, expire stale occurrences, unify canonical-occurrence selection.
  - **MTG-011:** consistent summary views.
  - **MTG-014:** series edits keep or re-anchor exceptions.
  - **MTG-018:** pagination; remove the N+1.
  - **MTG-006 (per D-3/D-4):** invitation emails, reschedule/cancel notifications, reminder worker.
  - **MTG-007:** ICS export.
  - Web day/week/month calendar views on the new API.
- **Proposed APIs (new, justified: no existing endpoint lists occurrences across spaces):**
  - `GET /api/v2/me/conferences?from&to&status&role&organizationId&cursor` — agenda for personal, organization, upcoming and past views. It replaces N+1 calls to `GET /meeting-spaces/:id/conferences`. Organization calendar = `organizationId` filter, not a separate endpoint.
  - `GET /api/v2/conferences/:id/calendar.ics` and `GET /api/v2/meeting-spaces/:id/calendar.ics` (series), per D-3.
  - Reused unchanged: `POST /conferences/:id/reschedule`, `/cancel`, `PUT /meeting-spaces/:id/recurrence[/exceptions]`.
- **Migrations:** index on `conference_sessions(scheduled_start_at)`; reminder/outbox table; exception ↔ rule-version link (MEDIUM: backfill).
- **Security:** the agenda must include only spaces where the caller is a member, invitee, registrant or organization member with the correct policy. Do not reproduce the MTG-011 summary leak.
- **Dependencies:** D-3, D-4; WS-5 for organization filters.

### WS-5 — Organizations completion · BACKEND + FRONTEND · Complexity MEDIUM (after D-1, D-2)

- **Scope:**
  - **ORG-003:** expose `created_by` (and the creator's display name), render `created_at`.
  - **ORG-004:** detail and edit endpoints.
  - Profile fields: description, type, operating/registered location, optional legal incorporation date (distinct from workspace creation), creation source.
  - **ORG-005:** invitations with acceptance.
  - **ORG-006:** member list with identity; align the messaging directory and exclude inactive users.
  - **ORG-007:** audit.
  - **ORG-008:** archive, ownership transfer, last-owner guard.
  - **ORG-009:** profile-policy write.
  - **ORG-011:** organization calendar via WS-4.
  - Minimal admin listing; web organization detail page; mobile per D-8.
- **Proposed APIs (meeting-service, new):**
  - `GET /api/v2/organizations/:id`
  - `PATCH /api/v2/organizations/:id`
  - `POST /api/v2/organizations` (only if D-2 allows self-service)
  - `POST|GET /api/v2/organizations/:id/invitations`
  - `POST /api/v2/organization-invitations/:token/accept|decline`
  - `POST /api/v2/organizations/:id/invitations/:iid/revoke`
  - `POST /api/v2/organizations/:id/ownership-transfer`
  - `POST /api/v2/organizations/:id/archive`
  - `GET /api/v2/organizations/:id/audit-events`
  - `PATCH /api/v2/organizations/:id/profile-policy`
  - `GET /api/v2/admin/organizations`
- **Reuse instead of new:** member listing is an extension of `GET /organizations/:id/members` (add identity fields). If D-1 chooses the Developer Platform as canonical, most of these already exist there (`/v1/organizations*`: detail, PATCH, invitations, ownership transfer, close) and should be **reused through a projection, not rebuilt**.
- **Migrations:** additive nullable profile columns; `organization_invitations`; `organization_audit_events`; ownership constraint (verify existing data first).
- **Security:** invitation tokens hashed; enumeration-safe errors (ORG-015); organization admin authority over organization meetings (ORG-011).

### WS-6 — Mobile v2 migration · FRONTEND · Complexity HIGH (scope per D-8)

- **Scope:**
  - **MTG-001 / FE-002 / FE-022:** move every meeting call to v2.
  - **MTG-003 / FE-003:** fix the 35 non-existent paths (table in the meetings audit).
  - **MTG-002 / FE-004:** real live room with a LiveKit SDK and correct id mapping.
  - **FE-005:** keep `participantToken`; stop re-POSTing join.
  - **FE-006:** environment-driven API URL; no cleartext.
  - **ACC-012:** remove credential logging; fix verify routing.
  - **FE-019:** v2 error envelope.
  - **MSG-012:** in-meeting chat on v2.
- **APIs:** none new.
- **Afterwards:** enable `FEATURE_LEGACY_MEETING_WRITE_DISABLED` (D-9) and retire the 23 legacy v1 write routes per the duplication report.

### WS-7 — Messaging first-party completeness · BACKEND + FRONTEND · Complexity MEDIUM

- **Scope:**
  - **MSG-001:** event replay parameter (`after`). Change the relay allowlist and the web, or accept both keys. Contract-first.
  - **MSG-002:** add PATCH to the meeting relay.
  - **MSG-003:** relay query allowlist.
  - **MSG-004 (D-5):** realtime for MANAGED.
  - **MSG-007:** inbox summary (title, last activity, unread). Extend the existing conversation list response; avoid a new endpoint if possible.
  - **MSG-005 / MSG-008:** web UI for receipts, typing, reactions, edits, replies, search, block/report, history paging, tombstones.
  - **MSG-014 / FE-016:** fix #37 (open the panel in the journey, or render the controls outside the toggle).
- **Validation:** extend the browser journey to GROUP chats and to members added after creation.

### WS-8 — Calling lifecycle and web calling · BACKEND + FRONTEND · Complexity MEDIUM

- **Scope:**
  - **CAL-001:** auto-terminate when ≤1 party remains, plus an abandoned-call timeout (product rule).
  - **CAL-002:** any participant can end or leave; hold/resume rules.
  - **CAL-004:** global ringing, ringtone, caller identity.
  - **CAL-005:** mount recording consent and abuse report.
  - **CAL-006:** bind voicemail/forwarding to the user's principal.
  - **CAL-008 / CAL-020:** production guards.
  - **CAL-013:** route Privacy Relay contacts correctly, or hide them.
  - **CAL-015:** media auto-reconnect.
  - **CAL-017:** recording discovery.
  - **CAL-014:** group calls and Hub launch.
- **Per-organization calling scope (CAL-007)** follows D-1.
- **Proposed API:** `GET /v1/enterprise/recordings` (list/search with filters), justified because recordings are currently findable only by id.
- **External:** incoming-call push (CAL-003) needs APNs VoIP / FCM / Web Push credentials. Build it behind a flag; activation is BLOCKED_EXTERNAL.

### WS-9 — Sessions, devices, Communications Hub client · BACKEND + FRONTEND · Complexity MEDIUM–HIGH

- **Scope:**
  - **ACC-007:** session model with listing and per-device logout.
  - **ACC-008:** device revoke invalidates its session.
  - **ACC-010:** Hub account events for users without a profile; deactivate Platform Access principals.
  - **MSG-006 / ACC-009 / FE-020:** web UI for contacts, CF Meet ID/QR, invites, requests, blocks, privacy, notification preferences, devices. All 73 Hub operations already exist.
  - **ACC-020:** enforce the notification preferences.
  - **ACC-011 / DATA-009 (per D-6):** deletion and export.
- **Proposed APIs:**
  - `GET /api/v2/me/sessions`
  - `POST /api/v2/me/sessions/:id/revoke`
  - conditional on D-6: `POST /api/v2/me/deletion-request`, `POST /api/v2/me/data-exports`, `GET /api/v2/me/data-exports/:id`
- **Migrations:** `account_sessions` (with a token `sid` claim; dual-accept window); erasure procedure (HIGH risk, after the legal/retention decision).

### WS-10 — Developer platform and legacy developer API · BACKEND + INFRASTRUCTURE · Complexity MEDIUM

- **Scope:**
  - **PLT-001:** fix the `contact-access-grants` scope expression; validate scopes in the generator.
  - **PLT-002:** hash the legacy API keys and make the quota atomic, or freeze issuance.
  - **PLT-003:** stop advertising legacy meeting webhooks (label or retire).
  - **PLT-004 / FE-013:** fix the Developers page.
  - **PLT-005 (D-7):** deploy the gateway and Developer Platform via GitHub Actions, then run the webhook/usage cut-over and retire the 10 Calling copies.
  - **PLT-006 / PLT-007:** Swagger consistency and hardening.
  - **PLT-008:** credential last-used and expiry warnings.
  - **PLT-009:** operator audit read.
  - **INV-001:** manifest ↔ router CI gate.
- **APIs:** none new for customers; operator audit read endpoints.

### WS-11 — Contract parity and end-to-end test gates (cross-cutting) · SHARED_CONTRACT · Complexity MEDIUM

- **Scope:**
  - **FE-015:** generated client types or contract tests for web and mobile.
  - Browser and mobile E2E for Journeys C, D, E, F, I in CI (CAL-022, MSG-010).
  - Tests for the 200 operations with no test reference (INV-002), prioritizing auth.
  - Gateway manifest drift gate (INV-001).
- **Why it is separate:** every major client defect in this audit (FE-001, FE-007, MTG-003, MSG-001) was a contract drift that a parity gate would have caught.

## 2. Suggested sequence

| Phase | Workstreams | Rationale |
|---|---|---|
| 1 (immediate) | WS-2, WS-1, WS-3 | P0 data protection and account security; the owner-visible meetings display and consent defects (low effort, high impact) |
| 2 | WS-4, WS-7 (MSG-001/002/003, #37), WS-8 (CAL-001/002/005/006/008), WS-11 (parity gate) | Close the broken journeys C, D, E, F, I |
| 3 (after D-1/D-2/D-8) | WS-5, WS-6 | Organization model and mobile depend on owner decisions |
| 4 | WS-9, WS-10, remaining WS-7/WS-8 | Hub client, sessions, Part VI rollout, legacy retirement |

## 3. Missing API estimate (evidence-backed)

**24 new endpoints are justified by confirmed gaps, plus about 4 more if MFA is approved (D-6).** Of these, 1 is conditional on D-2, 3 on D-6 and 2 on D-3. Everything else in the gap list is closed by **reusing or fixing existing endpoints**: relay PATCH, query allowlists, response fields, member identity, inbox fields, the organization calendar via the agenda filter, and notification preferences via the existing Hub operations.

| # | Endpoint | Workstream | Why an existing endpoint cannot be reused |
|---|---|---|---|
| 1 | `GET /api/v2/me/conferences` | WS-4 | No cross-space occurrence query exists (inventory: no `/me/conferences`, `/calendar` or `/agenda` route) |
| 2–3 | `GET /api/v2/conferences/:id/calendar.ics`, `GET /api/v2/meeting-spaces/:id/calendar.ics` | WS-4 (D-3) | No ICS anywhere |
| 4 | `GET /api/v2/organizations/:id` | WS-5 | Only `/me/organizations` and member routes exist |
| 5 | `PATCH /api/v2/organizations/:id` | WS-5 | The admin PATCH changes status only |
| 6 | `POST /api/v2/organizations` | WS-5 (D-2) | Creation is admin-only today |
| 7–11 | organization invitations: create, list, accept, decline, revoke | WS-5 | Membership is added directly by UUID |
| 12 | `POST /api/v2/organizations/:id/ownership-transfer` | WS-5 | — |
| 13 | `POST /api/v2/organizations/:id/archive` | WS-5 | — |
| 14 | `GET /api/v2/organizations/:id/audit-events` | WS-5 | No audit store |
| 15 | `PATCH /api/v2/organizations/:id/profile-policy` | WS-5 | Storage and readers exist, but there is no writer |
| 16 | `GET /api/v2/admin/organizations` | WS-5 | No admin listing |
| 17–18 | `GET /api/v2/me/sessions`, `POST /api/v2/me/sessions/:id/revoke` | WS-9 | No session model |
| 19 | `POST /api/v1/auth/oauth/google/exchange` | WS-1 | Replaces tokens in the URL |
| 20 | `GET /api/v2/admin/audit-events` | WS-1 | No account audit store |
| 21–23 | deletion request, data export create, data export status | WS-9 (D-6) | No deletion or export path |
| 24 | `GET /v1/enterprise/recordings` | WS-8 | Recordings are findable only by id |

## 4. Out of engineering control (track separately)

| Blocker | Unblocks | Reference |
|---|---|---|
| Independent E2EE cryptographic review and owner risk acceptance | PRIVATE_E2EE in production | #13, MSG-013 |
| APNs / FCM / Web Push credentials | Incoming-call push, offline message push | CAL-003, MSG-011 |
| PSTN carrier contract | Phone calling, SIP trunk, Privacy Relay numbers | CAL-010, CAL-016 |
| Number provider (and adapter) | Real numbers / DCNs | CAL-011 |
| SMS carrier (and adapter) | SMS, phone verification | CAL-012 |
| Key-transparency witnesses | KT witness checkpoints | MSG-013 |
