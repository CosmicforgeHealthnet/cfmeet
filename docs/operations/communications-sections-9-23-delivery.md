# Sections 9–23 — unified Communications delivery record

Status vocabulary: IMPLEMENTED (code), TESTED (named CI run passed), DEPLOYED (running in
production, verified), BLOCKED_EXTERNAL, DEFERRED, NOT_RUN. A passed build is never
reported as a deployment.

## Baseline (Part B)

| Item | Value |
|---|---|
| Audit start | 2026-11-05, `main` = `staging` = `53a1d6c` |
| Section 8 during the audit | open: public realtime ingress 502 (`messaging-section-8-delivery.md`, "Open defect") |
| Section 8 at the time of writing | closed by the other session: `53e26d8` "Record the realtime ingress root cause, repair and independent verification" on `main` and `staging` |
| Work branch | `feature/sections-9-23` in an isolated worktree; the concurrent Section 8 checkout and its nginx/TLS work were not touched |
| Production images at baseline | Messaging `ghcr.io/cosmicforgehealthnet/cfmeet-messaging@sha256:11f4aa75…`, Communications image `sha256:b90ada26…`, Calling `sha256:4ca17362…`, meeting-service `sha256:0be1b722…`, Platform Access `sha256:d7189f63…`, scan gateway `3637794` (from the Section 8 verify receipts) |
| Latest main CI at baseline | Hub 42088185332, Calling 42088185429, backend 42088185153, contracts 42088185466, TypeScript SDK 42088185433, Messaging 42098873520: all success |
| Production E2EE | BLOCKED_EXTERNAL (issue 13); `MESSAGING_E2EE_DEVICES_ENABLED=true` still refused in production |
| Push providers | BLOCKED_EXTERNAL (no APNs/FCM/Web Push credentials) |
| Phone OTP / matching | DEFERRED (owner decision); activation stays phone-free |
| Off-server DR | DEFERRED (owner decision); total VPS loss can lose data and keys |

Audit corrections to earlier records:
- Messaging push dispatch **does** re-check membership at dispatch time
  (`realtime/push.rs` `dispatch_due`/`authorized`, commit `3f63a0e`). The Section 8
  inventory row "IMPLEMENTED_NEEDS_REPAIR" is superseded.
- The Account relay is a generic catch-all; the per-operation registry is the Hub's
  dispatch router. New public operations need no Account change.

## Section 9 — feature inventory and classification

Classes: `REUSE_ACTIVE`, `REUSE_NEEDS_INTEGRATION`, `IMPLEMENTED_NOT_ACTIVE`,
`NEW_REQUIRED` (implemented in this program unless marked), `DEVICE_ONLY`,
`BLOCKED_EXTERNAL`, `DEFERRED`. Routes are relative to the owner's public prefix.

### Messaging (owner: Messaging unless noted)

| Feature | Route | Class |
|---|---|---|
| Direct / group conversations, list, members, roles, bans, invitations | `/v1/conversations…`, `/members`, `/bans`, `/invitation/*` | REUSE_ACTIVE for MANAGED contexts; IMPLEMENTED_NOT_ACTIVE for personal PRIVATE_E2EE (BLOCKED_EXTERNAL) |
| Send, receive, pagination, replies, threads, edit, delete, reactions, pins | `/messages…`, `/thread`, `/reactions`, `/pins` | same as above |
| Delivery/read indicators, unread counts | `read-cursor`, realtime `ack`, `/unread` | same |
| Typing, presence | WebSocket frames; `/presence/settings` | REUSE_ACTIVE (realtime route repaired in Section 8) |
| Voice notes, documents, camera/gallery metadata | attachments intent/finalize/grants | REUSE_ACTIVE (MANAGED); private files are client-encrypted |
| Shared media / documents index | Hub `GET /conversations/{id}/media` → Messaging MS-2 | NEW_REQUIRED |
| Conversation search | `/conversations/{id}/search`, `/messaging/search` | REUSE_ACTIVE for MANAGED; PRIVATE_E2EE: DEVICE_ONLY |
| Starred messages | Hub `/saved-messages` | NEW_REQUIRED |
| Clear chat for me | Hub `/conversations/{id}/clear-for-me` | NEW_REQUIRED |
| Disappearing messages | conversation `retentionSeconds` (stricter only), purge worker, legal holds | REUSE_ACTIVE; device deletion DEVICE_ONLY |
| Chat notification preferences | Hub `/conversations/{id}/preferences`, `/me/notification-preferences` | NEW_REQUIRED |
| Advanced chat privacy | Hub `/me/privacy`; presence settings | REUSE_ACTIVE |
| Export | operator exports (`/messaging/exports`); decrypted private export | REUSE_ACTIVE (operator); DEVICE_ONLY (private) |
| Translation, chat lock, themes, media visibility | none | DEVICE_ONLY |

### Calling (owner: Calling)

| Feature | Route | Class |
|---|---|---|
| Voice/video from a conversation | Hub `POST /conversations/{id}/calls` → CL-1 | NEW_REQUIRED (activation needs launch credential, see blockers) |
| Voice/video from Contacts | Hub `POST /contacts/{id}/calls` → CL-1 | NEW_REQUIRED (same) |
| From the Calls section / call-back | `actions.callBackContactId` + contact launch; direct Calling API | REUSE_ACTIVE |
| Incoming ringing, accept/decline/cancel, multi-device arbitration | Calling signaling and `/v1/calls/{id}/*` | REUSE_ACTIVE |
| Missed calls, history, duration | Calling history; Hub `GET /calls/history` | REUSE_ACTIVE + NEW_REQUIRED facade |
| Group calls, transfer, reconnection, media admission, recording consent, voicemail | Calling | REUSE_ACTIVE |
| Call activity cards in conversations | Hub timeline cards | NEW_REQUIRED |

### Contacts and profiles (owner: Hub)

| Feature | Route | Class |
|---|---|---|
| Exact-ID search, QR/invite discovery, requests, accept/decline/cancel, connections, details, aliases, block, profile reports, privacy, public profile, ID rotation, revocable invites | existing `/api/v2/communications/*` | REUSE_ACTIVE |
| Favorites | `/contacts/favorites`, `/contacts/{id}/favorite` | NEW_REQUIRED |
| Custom lists | `/contact-lists…` | NEW_REQUIRED |
| Groups in common | `/contacts/{id}/common-groups`, `/conversations/{id}/shared-groups` | REUSE_ACTIVE + NEW_REQUIRED |
| Shared media summary | `/conversations/{id}/media` | NEW_REQUIRED |
| Message abuse reports | Messaging `/conversations/{id}/reports` | REUSE_ACTIVE |
| Notification preferences per contact | `PATCH /contacts/{id}` `notifications` | REUSE_ACTIVE |
| Private phone handling, phone discovery | none | DEFERRED (no field is ever exposed) |

### Devices and settings

| Feature | Route | Class |
|---|---|---|
| Linked devices, registration, revocation | Hub `/devices…` over Messaging devices + Calling endpoints | NEW_REQUIRED |
| Notification targets | Hub `/devices/{id}/push` → Messaging push registration | NEW_REQUIRED facade |
| Delivery diagnostics | Hub `/me/notification-delivery-status` | NEW_REQUIRED; providers BLOCKED_EXTERNAL |
| Realtime sync, offline replay, multi-device state | Messaging cursors; Hub `/sync`, `/devices/{id}/sync-state` | REUSE_ACTIVE + NEW_REQUIRED |
| Storage usage | Hub `/me/storage-summary` → MS-3 | NEW_REQUIRED |
| Recovery info, session security, capabilities | Hub `/me/recovery-state`, `/me/capabilities`; Account logout/revoke-all | NEW_REQUIRED + REUSE_ACTIVE |

### Meetings (owner: Meetings)

| Feature | Route | Class |
|---|---|---|
| Instant/scheduled meetings, invitations, admission, media credentials, meeting chat, guests, preactivation | existing `/api/v2/*` | REUSE_ACTIVE (unchanged) |
| Meetings shared into conversations, activity cards | Hub `/conversations/{id}/meetings*` → MT-1/MT-2 | NEW_REQUIRED |
| Joining from the unified app | existing resolve/join with `meetingCode` | REUSE_ACTIVE |

## Section 10 — Calling + Messaging integration (design as implemented)

- Launch: Hub authenticates via the Account relay, resolves the contact or the
  conversation's ACTIVE members server-side, runs the full personal policy (relationship,
  both-direction blocks in Hub/Calling/Messaging, recipient privacy, account state,
  operator hold), rate-limits (120/hour), then calls Calling CL-1 under the user's own
  delegated principal. Calling re-runs its policy check before idempotent replay, so a
  direct Calling request cannot bypass relationship policy either.
- The Hub stores `communication_call_links` (call id, origin, conversation/contact,
  kind, client request id) and per-viewer timeline cards. No LiveKit secret, media URL
  or signaling credential is stored or proxied.
- Standalone calls never create a conversation (contact-origin calls have no card).
- Timeline (U04) is an **activity overlay**: call and meeting cards with
  `anchorSequence`, merged by clients with the Messaging history they already page. No
  message metadata is copied (APPROVED_CONTRACT_REVISION of "combine Messaging event
  references"). Missed outcomes are Calling's canonical per-participant outcome, read
  live (CL-2) for non-terminal cards older than 5 s; `stale=true` when Calling is
  unreachable; `source_version` makes out-of-order refreshes harmless.

## Section 14 — persistence

Hub migration `m20261106_000001_unified` (`sql/002_unified.sql`) in a **separate history
table** `communications_unified_seaql_migrations`. The previous image's readiness check
(exactly one foundation row) still passes, so rollback after migrating is safe. New:
contact lists/members, conversation preferences, clear-for-me watermarks, saved message
references, notification preferences, device registry, call links, timeline entries,
meeting links, change feed, operator holds; one nullable `favorite_rank` column.
Invariants: owner-scoped list membership trigger, monotonic watermark and sync
positions, terminal device revocation, immutable device bindings, unique idempotent call
and meeting links, per-viewer entry keys, version triggers, 30-day change-feed
retention. No cross-service foreign keys, no content, keys or push tokens.

Owner migrations: meeting-service `m20261105_000001_communications_meeting_facade`
(Communications-owned history, idempotency for MT-1). Calling and Messaging: none.

## Section 15 — public API reconciliation

29 operations were ALREADY_IMPLEMENTED (Sections 4–7). This program adds 44
(IMPLEMENTED_THIS_PROGRAM); public Communications Swagger now has 73, the public
catalogue 572.

| IDs | Status |
|---|---|
| A01, A02, A09–A14 | ALREADY_IMPLEMENTED |
| A03–A08 (phone challenges, change, delete) | DEFERRED; not exposed (no fake verification) |
| D01 (phone matches) | DEFERRED |
| D02–D19 | ALREADY_IMPLEMENTED |
| U01–U05, U08–U14 | IMPLEMENTED_THIS_PROGRAM |
| U06, U07 (`/conversations/direct`, `/conversations/group`) | APPROVED_CONTRACT_REVISION: served by canonical Messaging create (`counterpartCfMeetId`, member `cfMeetId`); a second creation path was not added |
| C01–C20 | IMPLEMENTED_THIS_PROGRAM |
| N01–N11 | IMPLEMENTED_THIS_PROGRAM; plus `PUT /devices/{id}/sync-state` (acknowledgement) |

## Section 16 — private operations

| # | Route | Status |
|---|---|---|
| 1 | `policy/check` | ALREADY_IMPLEMENTED (now also applies operator holds and account-wide notification switches) |
| 2 | `identities/resolve` | ALREADY_IMPLEMENTED as `principals/resolve` (name kept; purpose-bound) |
| 3–5, 7 | `events/{calling,messaging,meetings,devices}` | IMPLEMENTED (`communications-events.publish`, family/type allowlists, payload-bound dedupe); producers not yet wired, not granted in production |
| 6 | `events/accounts` | ALREADY_IMPLEMENTED as `account-events` |
| 8 | `readiness` | IMPLEMENTED (`communications-observability.read`) |
| 9 | `reconciliation/range` | IMPLEMENTED (`communications-backfill.run`, ≤31 days) |
| 10, 12 | `verification/callback`, `admin/phone-recovery` | IMPLEMENTED as fail-closed 503 `DEPENDENCY_NOT_CONFIGURED` (phone deferred) |
| 11 | `admin/metrics` | IMPLEMENTED (observability scope; aggregate counts only) |
| 13 | `admin/suspend` | IMPLEMENTED (`communications-profile.suspend`, sensitive, audited); not granted in production |

## Section 17 — errors and events

Errors: see the [handoff error catalogue](../api/communications-unified-handoff.md#7-error-catalogue);
existing codes are kept, new codes added. Event families: the Hub's owner outbox keeps
its existing `communications.connection.*`, `communications.block.changed`,
`communications.account.changed` events (consumed by Calling/Messaging for revocation).
Consumer-visible change events are the per-user change feed domains:

| Spec family | Implemented as |
|---|---|
| contact.requested / accepted / declined, connection.revoked | outbox `connection.requested/connected/none/revoked` (existing) |
| activation.changed | outbox `account.changed` (existing) |
| block.changed | outbox (existing) |
| profile.updated, privacy.changed, preferences.changed, timeline.changed, device.revoked, meeting.linked, call.linked | change-feed domains PROFILE, PRIVACY*, CONVERSATION_PREFERENCE / NOTIFICATION_PREFERENCE, TIMELINE, DEVICE, MEETING_LINK |
| call.outcome_changed | owner event `events/calling` invalidates cards; outcome read from Calling |
| notification.delivery_failed | Messaging push job state via `/me/notification-delivery-status` |
| phone.changed | DEFERRED |

\* Privacy/block feed entries for pre-existing operations are a follow-up; those
resources are re-read on bootstrap.

## Section 18 — certification matrix

Filled in with run IDs as they complete (see "Evidence" below).

| ID | Status | Note |
|---|---|---|
| T01 | PASS (Sections 4–7 receipts) | re-run in the real-service journey |
| T02–T04 | NOT_RUN / DEFERRED | phone binding deferred |
| T05, T06, T16 | PASS (Sections 4–7) | |
| T07–T09 | BLOCKED_DEPENDENCY | production PRIVATE_E2EE approval (issue 13) |
| T10 | PASS in CI only (Messaging suites) | production personal chats blocked as above |
| T11 | PASS (Sections 4–7 production real media) | Hub-launched calls: pending launch credential |
| T12, T25 | PASS (Sections 4–7 / Enterprise receipts) | |
| T13 | pending CI | Hub PostgreSQL suite `unified.rs` |
| T14 | pending CI | Hub + meeting-service suites |
| T15 | pending CI | device link/revoke suites |
| T17 | pending CI | favorites, lists, mute, clear-for-me, saved |
| T18 | partial | Messaging retention/legal-hold suites; device deletion DEVICE_ONLY |
| T19 | pending CI | contracts, SDK, migrations |
| T20 | PASS (Section 8 storage/scanner receipts) | |
| T21 | BLOCKED_DEPENDENCY | physical devices and push credentials |
| T22 | partial | fail-closed outage tests in Hub suites; no production fault injection |
| T23 | NOT_RUN | no load tests on the shared production VPS (standing owner rule) |
| T24 | partial | Messaging deletion/export APIs; phone handling deferred |
| T26 | pending | rollback-safe migration history verified in tests; production rollback not exercised |

## Section 19 — capacity

Measured state is in `messaging-section-8-delivery.md` (host ~5 GiB available of
11.7 GiB, shared scanner, LiveKit 54 MiB, Calling 13 MiB, Messaging 9 MiB). The new Hub
work adds tables and per-request owner calls only; no worker, no new container, no new
memory reservation. Controlled Calling (10–100 calls) and Messaging (10–1,000 msg/s)
load levels are **NOT_RUN**: the owner's standing rule forbids load tests on the shared
production VPS, and no isolated load environment exists. Planning assumptions are not
presented as measured capacity.

## Section 21 — fixed decisions preserved

Email/password and Google signup unchanged; CF Meet ID and QR flows unchanged; phone OTP
deferred; display names may repeat; exact IDs decide identity; accepted contacts are
the default boundary; enterprise contexts separate; PRIVATE_E2EE target with no MANAGED
downgrade; Meetings independent; owners authoritative; shared scanning kept; no new
antivirus, paid provider, carrier calling or AWS; frontend untouched.

## External blocker register

| Blocker | Effect | Needed from |
|---|---|---|
| Production E2EE assessment (issue 13) | personal chats, T07–T09 | independent assessor + owner approval |
| Push provider credentials | background/locked notifications, T21 | owner (APNs key, FCM project, VAPID) + physical devices |
| `CALLING_COMMUNICATIONS_LAUNCH_CREDENTIAL_ID` | Hub call launch answers 503 until set | a Platform Access credential id (non-secret) of the personal scope's first-party service account with `calls:write` |
| Phone verification provider | A03–A08, D01, T02–T04 | owner decision |
| Isolated load environment | T23, Section 19 load levels | owner decision (no VPS load tests) |
| Off-server backups | DR | owner decision (deferred) |

## Evidence

(Run IDs, image digests and production receipts are appended below as they complete.)
