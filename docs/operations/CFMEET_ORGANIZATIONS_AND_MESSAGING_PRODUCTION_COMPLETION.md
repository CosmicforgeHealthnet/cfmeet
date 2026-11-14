# CF Meet — Session 3: Organizations and organization Messaging production completion

**Date:** 2026-11-12. **Repository:** `CosmicforgeHealthnet/cfmeet`, branch `main`. **Production:** Contabo VPS, `https://api.cfmeet.com`.
**Implementation:** `2ff0c12` (code, contracts), `021e7cd` (handover), `3e11958` (CI fix). **Released:** `3e11958bfd51d1f6283bb8269e369757ccd17b08` (release run 43331884110, certify run 43336196158, both success).
**Frontend handover:** [`docs/frontend/CFMEET_ORGANIZATIONS_AND_MESSAGING_BACKEND_HANDOVER.md`](../frontend/CFMEET_ORGANIZATIONS_AND_MESSAGING_BACKEND_HANDOVER.md).

## 1. Executive summary

* The backend gaps found by the audit are closed in code, tested and **released to production** (`3e11958`): member emails with a backend-enforced
  visibility policy, invitations by email **or CF Meet ID** (through the Communications Hub's discovery rules, never an
  account-discovery tool, never disclosing the invitee's email), and an **inbox realtime capability** so organization
  (and personal) Messaging clients no longer need to poll the conversation list.
* Several requested capabilities already existed and were **not rebuilt**: self-service organization creation
  (`POST /api/v2/organizations`, creator OWNER, idempotent, rate limited), member display names (ORG-006),
  conversation previews and unread counts (`inbox` on the member conversation list), organization-scope realtime
  session tokens, workspaces, teams, policies, membership lifecycle, last-owner protection.
* **The Organizations authority cutover is not complete and could not be completed in this execution.** Production is
  still in the non-authoritative organization mode, so every first-party organization route — and therefore every
  organization-scoped feature in this report — answers `FEATURE_DISABLED` to users. The blocking gate is **owner
  gate 1 (encrypted off-host backup + restore drill)**, which has never been satisfied: the owner's age key pair does
  not exist and `cfmeet-offsite-backup.yml` has never run. After it, the established procedure still requires the
  import, shadow mode and **7 elapsed days** of zero-mismatch observation before the irreversible switch. None of this
  was bypassed, shortened or fabricated.

## 2. Initial production state (before this session)

| Item | State | Evidence |
|---|---|---|
| Organizations authority mode (Meeting) | not authoritative (`legacy`; shadow never entered) | live `POST https://api.cfmeet.com/api/v2/organizations` → `404 FEATURE_DISABLED` "First-party organization management is not enabled" (2026-11-12 21:28 UTC); all-features closure report |
| Legacy organization dataset | 0 organizations, 0 memberships (export 2026-11-09, run 42711430322) | `CFMEET_ALL_FEATURES_ACTIVATION_CLOSURE.md` |
| Off-host backup (gate 1) | **never run** — `cfmeet-offsite-backup.yml` has 0 runs; `vars.CFMEET_BACKUP_AGE_RECIPIENTS` / `secrets.CFMEET_BACKUP_AGE_IDENTITY` not created | GitHub Actions run list (0 runs) |
| Organizations Service | deployed (`cfmeet-organizations`, private 8450), provisioning and org-event activations done in earlier sessions | coordinated-release journal |
| Personal Messaging realtime (MANAGED session devices) | ACTIVE (`MESSAGING_SESSION_DEVICES_ENABLED=true`, run 42730700547) | all-features closure |
| Concurrent release activity | another agent released `9684346` (run 29) and `f2f9693` (run 32, success); this session waited for run 32 before touching `main` | runs 29–32 |

## 3. Organizations authority-mode findings

Per-capability classification (Section 1.2):

| Capability | Classification |
|---|---|
| Organizations Service (orgs, memberships, invitations, teams, policies, audit, provisioning, outbox) | implemented, deployed, **not authoritative** |
| Meeting facades / first-party relay (`/api/v2/organizations*`) | implemented, gated on `ORGANIZATIONS_AUTHORITY_MODE=authoritative` |
| Shadow comparison, export / import / reconciliation tooling | implemented (`org-cutover-*` operations) |
| Gate 1 off-host backup + restore drill | tooling implemented (PR #55); **owner-controlled dependency missing** |
| 7-day shadow evidence | elapsed-time requirement, not started |
| Organization-scoped Enterprise Calling | implemented, `activate-enterprise-calling` requires authoritative |
| Organization Messaging relay / session tokens / member resolve | implemented, gated on authoritative + per-organization Messaging binding |

## 4. Organization cutover evidence

* Export / inventory: 0 organizations, 0 memberships (run 42711430322, 2026-11-09). Re-inventory in this session: see §17.
* Import: **not run** — `org-cutover-import` refuses without a verified off-host backup record ≤ 24 h and the owner's
  `offsite-backup-verified` confirmation (gate 1). Nothing to report for reconciliation, shadow or quiesce.
* **Empty-dataset first-time activation (Section 2.4).** With zero legacy organizations and memberships the data risk of
  the import is nil, but the runbook's gates are not only about data volume: gate 1 protects the Organizations
  database itself once users start writing to it (after `authoritative` the change is irreversible; runbook §7 forbids
  falling back to a writer whose data is older). A narrowly scoped first-time procedure is conceivable — *verify
  0/0 inventory and zero pending outbox events → take and verify the off-host backup → import the empty export →
  switch directly to `authoritative` without the 7-day shadow window (nothing to compare)* — but it (a) still needs
  gate 1, which is missing, and (b) changes an owner-approved safety procedure, which requires an explicit owner
  decision recorded in the release evidence. It was therefore **not implemented or run**. Owner decision requested:
  §19.

## 5. Self-service organization creation

Already implemented (no code change): `POST /api/v2/organizations` relays to `POST /internal/v1/organizations` with the
verified acting user; creation source SELF_SERVICE; creator OWNER; slug validation and `SLUG_TAKEN`; `Idempotency-Key`
replay (`idempotent-replayed` header) and `IDEMPOTENCY_CONFLICT`; at most 5 per user per 24 h (`RATE_LIMITED`);
`provisioningState` reported (`GET …/provisioning`, `PROVISIONING_PENDING` on scoped features); the organization
appears in `GET /api/v2/me/organizations`. Covered by the existing `organizations` service suites and
`tests/organizations_authority.rs` (`s4_*`). **Legacy-mode creation (Section 3.3): deliberately not added** — it would
create a second writer the cutover must later reconcile (ADR 0056 §2, one writer); activation of the authoritative
path is the fix. Status: `IMPLEMENTED_PENDING_ACTIVATION`.

## 6. Member identity enrichment

* **Diagnosis:** `display_identity::lookup` works; the Meeting Service returns `displayName` / `username` on every
  member item (ORG-006). The `Account 3f2a9c1e` placeholder comes from the web client, which ignores these fields
  (`apps/web/src/components/organizations/OrganizationMembers.tsx`, comment "The API serves neither display names …").
  Frontend requirement documented in the handover §3 (no frontend code changed).
* **New: `email`** on `GET /api/v2/organizations/{id}/members` — present only for an ACTIVE OWNER / ADMIN of the
  organization or a platform administrator; **absent** (not `null`) for MANAGER, MEMBER, GUEST, suspended members and
  non-members (who are refused anyway). Legacy mode admits only platform administrators and active OWNER/ADMIN to the
  list, i.e. exactly the policy. Deleted accounts: names and email absent, never invented. Display-name changes are
  read live. Membership id and account id stay separate. One batched query per page (500 ids per statement).
* Files: `services/meeting-service/src/handlers/v2/organizations.rs` (`may_see_member_emails`),
  `src/services/display_identity.rs`, `src/dto/v2.rs`.

## 7. Invitation implementation

* `POST /api/v2/organizations/{id}/invitations` accepts `identifierType` (`EMAIL` | `CF_MEET_ID`) + `identifier`
  (the `email` form is unchanged).
* **EMAIL:** trimmed, lower-cased; an account registered with that address that already holds a non-removed
  membership → `409 ALREADY_MEMBER`.
* **CF_MEET_ID:** only for callers who may invite (ACTIVE OWNER / ADMIN / MANAGER — checked against the authoritative
  member list **before** any lookup). The Meeting Service asks the Communications Hub's new private route
  `POST /internal/v1/communications/organization-invitations/resolve` (Meeting Service only; existing
  `communications-user.forward` grant, so no new production grant) which applies exactly the consumer-discovery rules
  to the inviter: discoverable public id, visible profile, no block in either direction, active account, the
  `discovery.resolve` attempt budget (`429`). Unknown, hidden, blocked, inactive, suspended and guest accounts are one
  indistinguishable `404 INVITEE_NOT_FOUND`. Self / existing member → `409 ALREADY_MEMBER`. The resolved account's
  email is sent to Organizations only, with the new optional `inviteeHint` = `CF Meet ID <id>` (Organizations 1.4.0;
  stored instead of the masked email; `@` refused), so the inviter never sees the email, not even masked; the
  response carries `identifierType` and `identifier` and no `email`.
* Unchanged and reused: one-time tokens, verified-email acceptance (another identity is refused), decline, revoke,
  7-day expiry, re-invite = resend (the previous pending invitation is superseded), audit and membership events,
  provisioning of the member's technical principal, offboarding on removal. Email delivery uses the existing central
  delivery (`sendEmail`, `emailDelivery` = `ACCEPTED` / `DISABLED` / `FAILED`); Resend is a separate owner activation
  and no delivery is claimed where it is not configured.

## 8. Conversation-list enhancements

Already implemented (MSG-007): every member conversation list item — including through the organization relay, where
Messaging injects the signed initiating principal — carries `inbox.lastActivityAt`, `inbox.lastMessage`
(`messageId`, `sequence`, `senderPrincipalId` (organization-scope principal), `createdAt`, `lifecycle`,
`securityProfile`, `attachmentCount`, `previewText`), `inbox.unreadCount` and `inbox.mentionCount`; empty
conversation → `lastMessage: null`, `unreadCount: 0`. Previews respect history windows, clear-for-me (hidden
messages), tombstones and PRIVATE_E2EE (never plaintext). Unread counts come from the existing read cursors (no second
authority). The new realtime test asserts preview, sender and unread count through the member list. Per-conversation
cost is bounded by the page size (≤ 100) and indexed lookups; a batched rewrite was not needed for correctness and was
not attempted.

## 9. Organization realtime integration

* Existing and verified in code: `POST /api/v2/organizations/{id}/messaging/session-tokens` issues an
  organization-scope MANAGED session-device token bound to the account session → `POST /v1/realtime/tickets` →
  `wss://api.cfmeet.com/v1/realtime` (same gateway as personal Messaging, no separate service). Durable per-conversation
  events with replay from the ack cursor, duplicate dropping, `sync.required`, `auth.refresh`, revocation on
  sign-out / device / membership / organization suspension (close `4401`).
* **Gap closed — conversation-list changes without polling.** Before, an event reached a connection only for
  conversations it had explicitly subscribed to, so new conversations and activity elsewhere required polling. New
  optional capability `inbox` (realtime protocol 1.4.0, advertised in `welcome.capabilities`):
  * client frame `inbox.subscribe` → `inbox.subscribed`;
  * `inbox.event {conversationId, eventSequence, eventType}` for every committed event of a conversation the
    principal is an ACTIVE member of (or was just removed from) that this connection is not subscribed to —
    identifiers only, never content; no duplicate for subscribed conversations;
  * the principal's own `read-cursor.synced` also reaches its inbox connections (cross-device unread counts);
  * `inbox.resync` after broker loss (refetch the list once).
  * Implementation: the outbox publisher attaches the conversation's ACTIVE member principals (+ the subject of a
    member event; bounded at 1024, otherwise subscribed delivery only) to the broker pointer; the hub routes by
    principal; old broker messages without recipients still parse (rolling restart safe).
* Mobile background push is a separate activation and is not implied by connected realtime.

## 10. Cross-service integration

| Integration | Result |
|---|---|
| Meeting ↔ Organizations | members, invitations (`inviteeHint`), relay unchanged otherwise |
| Meeting ↔ Communications Hub | new Meeting-only resolve route, existing grant |
| Meeting ↔ Messaging (organization relay, session tokens) | unchanged; inbox capability is Messaging-side |
| Platform Access, Calling, Developer Platform, API Gateway | unchanged (no code change) |
| Organization-scoped Enterprise Calling | `activate-enterprise-calling` requires authoritative → not activated |
| Personal vs organization isolation | organization conversations live in the organization's own FIRST_PARTY scope; inbox routing is per principal id (scope-specific); tests assert non-members receive nothing |

## 11. API inventory — new / changed

| Method | Path | Change |
|---|---|---|
| GET | `/api/v2/organizations/{id}/members` | + `email` (policy-gated) |
| POST | `/api/v2/organizations/{id}/invitations` | + `identifierType`, `identifier`; `email` optional; responses `404 INVITEE_NOT_FOUND`, `409 ALREADY_MEMBER`; response + `identifierType`, `identifier` |
| POST (internal) | `/internal/v1/communications/organization-invitations/resolve` | new, Meeting Service only, never public |
| POST (internal) | `/internal/v1/organizations/{id}/invitations` | + optional `inviteeHint` |
| WebSocket | `/v1/realtime` | + `inbox.subscribe`, `inbox.subscribed`, `inbox.event`, `inbox.resync`, `welcome.capabilities` |
| — | error codes | + `ALREADY_MEMBER` (409), `INVITEE_NOT_FOUND` (404) |

No endpoint was duplicated; no internal route is exposed publicly.

## 12. Swagger / contract updates

`packages/contracts/openapi/meeting-service.json` (regenerated by `export_openapi`), realtime protocol 1.4.0,
Organizations internal 1.4.0, Communications internal 1.5.0, `manifest.json`, all bundles
(`bundle_openapi.py`), public API catalogue (`public_api.py`), frontend endpoint reference, generated web / mobile
contract types (`apps/web/src/types/generated/meeting-service.ts`, `apps/mobile/lib/api/generated/meeting-service.ts`
— generated files only, required by the web / mobile CI freshness checks; no component, layout, navigation, style or
state code changed). Checks: validator 98 contracts OK; bundles, labels, public API, private docs, SDK, first-party
ingress and frontend-reference checks pass.

## 13. Unit / integration test results (local, PostgreSQL 16 + Redis 7)

| Service | Result |
|---|---|
| Messaging | fmt, clippy `-D warnings` clean; all 42 test binaries pass (lib 71 incl. new hub / broker / fanout tests; realtime 24 incl. `inbox_signals_cover_unsubscribed_and_new_conversations_without_content`) |
| Communications Hub | fmt, clippy clean; all suites pass incl. new `organization_invitee_resolution_follows_discovery_rules` |
| Organizations | fmt, clippy clean; all suites pass (lib 39 incl. `invitee_hints_never_carry_an_email`) |
| Meeting Service | fmt gate (no new drift); `organizations_authority` 29/29 (new: `session3_member_names_for_members_and_emails_for_owners_and_admins_only`, `session3_invitations_by_email_or_cf_meet_id_never_disclose_the_invitee`), `openapi_contract` 9, `api_contract_matrix` 13, `api_operation_coverage` 7, `api_v2_roles` 11, `idor_matrix` 1, `organization_domain_access` 6; after the CI fix the **full workspace: 83/83 test binaries pass** (incl. `architecture_boundaries`), route coverage 207/207 operations, OpenAPI export current. |
| Contracts validator | 98 contracts, 272 examples, 345 counter-examples OK; validator tests 20/20 |

## 14. Security and tenant-isolation results

* Email disclosure enforced server-side (field absent, not null); suspended ADMIN loses it; non-members refused.
* CF Meet ID resolution: only inviters may trigger a lookup (asserted: a MEMBER and a non-member cause **zero** Hub
  lookups); discovery rules and attempt budget apply; one indistinguishable `404` for unknown / hidden / blocked /
  inactive; the invitee's email never appears in the response (asserted); client-supplied `inviteeHint` refused;
  the Hub route refuses any caller except the Meeting Service.
* Realtime inbox: identifier-only (asserted: preview text never in `inbox.event`), non-members receive nothing,
  removed members get their own removal then nothing further (subscriptions end as before), revocation paths unchanged.

## 15. Production release SHA

`3e11958bfd51d1f6283bb8269e369757ccd17b08` (`main`). Release scope (base `f2f9693`, the previous completed release):
**replaced** organizations, messaging, calling, communications, meeting-service, api-gateway (calling and api-gateway
because their images copy the shared contracts / SDK inputs); **kept** platform-access, numbering, privacy-relay,
sms-gateway, telephony (revision `9684346`) and developer-platform + worker (`f2f9693`). Every replaced image carries
the revision label `3e11958…` (checked on pull and after transfer). Later commits on `main` (`991593a`, PR #68, release
tooling only; this report) change no image build inputs.

## 16. Deployment workflow URLs

| Step | Run |
|---|---|
| CI on `021e7cd` (5 red: stale SDK models, thin-handler rule) — fixed by `3e11958` | push workflows of `021e7cd` |
| CI on `3e11958` (11 push workflows, all green; the gate also requires green latest runs of untouched services) | push workflows of `3e11958` |
| Coordinated **release** | [43331884110](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43331884110) — success |
| Coordinated **certify** (read-only) | [43336196158](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43336196158) — success |
| Preceding release by another agent (waited for, not overlapped) | [43309857124](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43309857124) (`f2f9693`) |

Coordination: the other active release agent (session "CF Meet production certification audit") was messaged before
and after; it held its dispatches and its PR #68 until this release and certification completed. One collision
happened: pushing `021e7cd` moved `main` under that agent's queued certify (run 43317541707, refused at the gate,
no production change).

## 17. Production activation evidence

Release run 43331884110 (VPS):

* resource gate: MemAvailable 5365 MiB, 4533 MiB remaining after release, shortfall 0;
* backup manifest `/opt/cfmeet-core/backups/43331884110-1/manifest.json`: 16 databases, isolated restore tested for
  organizations, platform-access and meeting-service (local, on-host backup — **not** the off-host gate-1 evidence);
* migrations run before each swap (organizations, messaging + messaging-communications, calling + calling-communications,
  communications, all eight Meeting histories); histories after release: organizations 4 (latest
  `m20261110_000004_messaging_service_binding`), messaging 31, calling 47, meeting-service (`lifecycle_delivery_migrations`
  `m20261112_000001_email_delivery_and_presence` …), no new migration in this change;
* verification: 30/30 PASS (`running` for every container, migrations for all ten databases, CORS preflight from the web
  origin, public Swagger, `/internal/v1/organizations` and `/internal/v1/admin/event-delivery` not public,
  `organizations_no_dead_events`, `provider_features_off`), violations `[]`.

Certify run 43336196158 (read-only, 13 services): every service PASS for container state (no OOM, restart count 0),
image, readiness, database, migrations, service authentication (unauthenticated internal probes answer 401),
restart stability and upstreams (worker: N/A for listener and migrations; gateway: no database); platform checks:
core identity PASS, Organizations event delivery PASS, provider safe defaults PASS; public API and gateway
certification step PASS; certification verdict PASS.

Configuration observed by certify (non-secret values):

| Service | Relevant flags |
|---|---|
| Meeting Service | `ORGANIZATION_EVENTS_ENABLED=true`, `FEATURE_CONFERENCE_REALTIME_ENABLED=true`, `NOTIFICATIONS_EMAIL_ENABLED=true`, `EMAIL_PROVIDER=smtp`; **no `ORGANIZATIONS_AUTHORITY_MODE`** (default `legacy`) |
| Messaging | `MESSAGING_SESSION_DEVICES_ENABLED=true` (MANAGED realtime), `MESSAGING_E2EE_DEVICES_ENABLED=false`, `MESSAGING_PUSH_PROVIDER=none` |
| Organizations | `ORGANIZATIONS_PROVISIONING_ENABLED=true`, `ORGANIZATIONS_EVENT_DESTINATIONS=cfmeet.meeting-service` |
| Excluded capabilities | SMS provider, number inventory / porting, PSTN inbound / outbound, callback relay, SIP trunking / edge, privacy relay, contact vault, relay number pool: **disabled** (unchanged) |

Release journal (certify): last operations `release 43309857124 (f2f9693)`, `release 43331884110 (3e11958)`; the only
organization cutover entry is `org-cutover-export 42711430322` (2026-11-09). No import, shadow or authoritative step
exists — consistent with §4.

Activations performed in this session: **none** besides the release. No eligible activation was pending that did not
depend on the authoritative cutover (`activate-first-party-features`, provisioning and org events were already
recorded); `activate-enterprise-calling` refuses without the authoritative mode.

Live public check (2026-11-12 21:28 UTC, and unchanged by the release): `POST /api/v2/organizations` →
`404 FEATURE_DISABLED` "First-party organization management is not enabled"; `GET /api/v2/me/organizations` → `401`
unauthenticated; `/health` healthy.

## 18. End-to-end acceptance results (J1–J11)

| Journey | Result | Evidence |
|---|---|---|
| J1 Organization creation | **not runnable in production** (FEATURE_DISABLED until authoritative); passes against the relay with a contract-faithful Organizations double | live `FEATURE_DISABLED`; `organizations_authority::s4_*`; Organizations service suites |
| J2 Member directory (names, permitted emails) | integration PASS; production not runnable (members of first-party organizations only exist after the cutover; the legacy directory has 0 organizations) | `session3_member_names_for_members_and_emails_for_owners_and_admins_only` |
| J3 Invite by email → accept → member | integration PASS (relay + Organizations acceptance rules); production not runnable | `session3_invitations_…`, `s11_organization_invitation_email_from_the_authoritative_answer`, Organizations invitation suites |
| J4 Invite by CF Meet ID | integration PASS across Meeting ↔ (fake) Hub and the real Hub resolver | `session3_invitations_…`, Communications `organization_invitee_resolution_follows_discovery_rules` |
| J5 Organization Messaging: conversation, send, preview, unread | integration PASS (real Messaging over real WebSocket and PostgreSQL); organization relay gated in production | `inbox_signals_cover_unsubscribed_and_new_conversations_without_content`; `closure_organization_messaging_runs_in_the_organization_scope_as_the_member` |
| J6 Realtime between two members | integration PASS (two principals, inbox + subscribed delivery) | realtime suite 24/24 |
| J7 Reconnect, missed events, no duplicates | integration PASS (existing replay / ack tests; inbox resync documented) | `offline_devices_replay_missed_events_in_canonical_order_from_their_ack`, `committed_messages_survive_a_broker_outage` |
| J8 Access revocation | integration PASS (member removal ends subscription and reaches the inbox; suspension / revocation close `4401`) | `removed_members_stop_receiving_while_messages_are_flowing`, `revocation_and_suspension_close_established_connections`, new inbox test |
| J9 Workspace / team access | integration PASS (existing) ; production gated | `s8_*`, `org006_*` |
| J10 Multi-organization isolation | integration PASS | `session3_…` (other organization's list), `session_devices_never_cross_tenants`, inbox non-member silence |
| J11 Regression (personal Meetings, Calling, Messaging, Hub, Developer Platform) | **PASS in production** at the infrastructure level: release verification 30/30, certify 13/13 services; Meeting full workspace 83/83 binaries locally and in CI; Messaging 42/42, Communications, Organizations suites | runs 43331884110, 43336196158 |

No authenticated production journey was run for the new behaviour: every organization-scoped journey is refused by
design until the cutover, and the personal-scope realtime `inbox` capability has no live acceptance harness yet (its
behaviour is proven by the real-WebSocket integration test; a live check needs a controlled acceptance account and a
new acceptance operation).

## 19. Remaining owner / external requirements

1. **Gate 1 (owner):** create the age key pair(s); set `vars.CFMEET_BACKUP_AGE_RECIPIENTS` and
   `secrets.CFMEET_BACKUP_AGE_IDENTITY`; dispatch `cfmeet-offsite-backup.yml` and let its restore drill pass
   ([backup-and-restore.md](backup-and-restore.md)).
2. Then: `org-cutover-export` → `org-cutover-import` (`confirm=offsite-backup-verified`) → `org-cutover-shadow`.
3. **Either** 7 days of zero-mismatch shadow evidence (runbook §5), **or** an explicit, recorded owner decision approving
   an empty-dataset first-time procedure (§4) — engineering will implement it as a separate, tested operation only
   after that decision.
4. Quiesce (block the three legacy write routes at the ingress), then
   `org-cutover-authoritative` with `confirm=offsite-backup-verified,quiesced` (irreversible).
5. `activate-enterprise-calling`; per-organization service accounts and Calling / Messaging bindings (owner decision,
   ADR 0058 §1).
6. Resend activation for invitation emails (owner procedure: [resend-email-owner-handoff.md](resend-email-owner-handoff.md)).
7. Frontend: show `displayName` / `email` in the member list; identifier invitations; `inbox` realtime (handover).
8. Engineering follow-up (not built): an in-app list of invitations addressed to the signed-in account.

## 20. Final per-feature verdict

| Feature | Verdict | Reason |
|---|---|---|
| Organizations Service authoritative (cutover) | **BLOCKED_OWNER_ACTION** | gate 1 off-host backup missing; then import, shadow, 7-day evidence (or an owner-approved empty-dataset procedure), quiesce |
| Seven-day shadow evidence | BLOCKED_EXTERNAL_DEPENDENCY | elapsed time; cannot start before shadow mode |
| Self-service organization creation | IMPLEMENTED_PENDING_ACTIVATION | deployed; refused with FEATURE_DISABLED until authoritative |
| Workspaces, teams, policies, roles, membership lifecycle | IMPLEMENTED_PENDING_ACTIVATION | deployed; gated on authoritative |
| Member display names | IMPLEMENTED_PENDING_ACTIVATION | backend serves them (existing); gated route; frontend still shows ids (handover §3) |
| Member email for OWNER / ADMIN | IMPLEMENTED_PENDING_ACTIVATION | deployed in `3e11958`; gated route |
| Invitations by email (incl. resend / revoke / accept / decline) | IMPLEMENTED_PENDING_ACTIVATION | deployed; gated; email via SMTP (`NOTIFICATIONS_EMAIL_ENABLED=true`), Resend separate |
| Invitations by CF Meet ID | IMPLEMENTED_PENDING_ACTIVATION | deployed in Meeting + Communications Hub (existing grant); gated route |
| Organization Messaging conversation list previews / unread counts | IMPLEMENTED_PENDING_ACTIVATION | existing `inbox` summary; organization relay gated |
| Organization Messaging realtime (session tokens + inbox capability) | IMPLEMENTED_PENDING_ACTIVATION | deployed; organization session tokens gated on authoritative + Messaging binding |
| Personal Messaging realtime `inbox` capability | IMPLEMENTED_PENDING_ACTIVATION | deployed and enabled by default for every connection that sends `inbox.subscribe`; not yet exercised by a live authenticated acceptance (§18) — clients must also adopt it |
| Organization-scoped Enterprise Calling | BLOCKED_OWNER_ACTION | `activate-enterprise-calling` requires authoritative; per-organization bindings are owner decisions |
| In-app list of invitations for the recipient | not implemented (follow-up) | §19.8 |
| Production health of all 13 services after the release | **ACTIVE_VERIFIED** | release verification 30/30, certify 13/13 (runs 43331884110, 43336196158) |
| Excluded capabilities (SMS / phone OTP, PSTN / SIP, number provisioning / porting, Privacy Relay) | not activated (excluded) | certify: disabled |
