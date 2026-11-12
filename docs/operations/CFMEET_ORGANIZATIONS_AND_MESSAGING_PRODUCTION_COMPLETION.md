# CF Meet — Session 3: Organizations and organization Messaging production completion

**Date:** 2026-11-12. **Repository:** `CosmicforgeHealthnet/cfmeet`, branch `main`. **Production:** Contabo VPS, `https://api.cfmeet.com`.
**Implementation commit:** `2ff0c12` (+ handover `021e7cd`). **Release:** see §15–§17.
**Frontend handover:** [`docs/frontend/CFMEET_ORGANIZATIONS_AND_MESSAGING_BACKEND_HANDOVER.md`](../frontend/CFMEET_ORGANIZATIONS_AND_MESSAGING_BACKEND_HANDOVER.md).

## 1. Executive summary

* The backend gaps found by the audit are closed in code, tested and released: member emails with a backend-enforced
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
| Meeting Service | fmt gate (no new drift); `organizations_authority` 29/29 (new: `session3_member_names_for_members_and_emails_for_owners_and_admins_only`, `session3_invitations_by_email_or_cf_meet_id_never_disclose_the_invitee`), `openapi_contract` 9, `api_contract_matrix` 13, `api_operation_coverage` 7, `api_v2_roles` 11, `idor_matrix` 1, `organization_domain_access` 6. The full workspace ran in CI (§15). |
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

_Filled after the release (§17)._

## 16. Deployment workflow URLs

_Filled after the release._

## 17. Production activation evidence

_Filled after the release._

## 18. End-to-end acceptance results (J1–J11)

_Filled after the release._

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

_Filled after the release._
