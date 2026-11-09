# ADR 0053 — Meeting chat migration to Unified Messaging and the CF Meet identity bridge

Status: Accepted for `staging` (Part V Part H, §97–§101, §104). Builds on ADR 0045–0052.
Engineering direction only — not a production release authorization.

## Context

Meeting Service owns CF Meet accounts, meeting admission, roles and chat policy. Its v2
chat stored main-room, breakout and private messages in `conference_messages` behind a
`MessageStore` port designed to be replaced. Part V makes Messaging the authority for
conversations and messages, reachable only through authenticated published contracts
(never another service's database), without relabelling legacy server-readable chat as
end-to-end encrypted and without exposing service credentials to clients.

## Decisions

1. **Authenticated boundary.** Meeting calls Platform Access and Messaging with
   `cf-service-auth` Ed25519 service tokens (`cfmeet.meeting-service`). CF Meet's own
   FIRST_PARTY application environment (tenant / application / environment / service
   account ids) is signed into every token as the delegated platform context and
   re-validated by the receiver on every call. Meeting is granted exactly
   `platform-principal.ensure` (Platform Access) and `messaging-application.delegate` +
   `messaging-meeting.sync` (Messaging); the service-auth registry refuses bundles that
   grant them to anyone else. No API credential, password or service key leaves Meeting.
2. **Identity bridge (§104).** Meeting verifies its own login (HS256 access token with the
   revocation store checked; it fails closed when the store cannot be checked) and maps
   the account to its `NATIVE_CF_USER` principal via `POST /internal/v1/principals/ensure`
   (FIRST_PARTY, full authority only; reference `cfmeet-user:<account id>`, never an email
   or number). Guests get a `TEMPORARY_EXTERNAL_PARTICIPANT` principal per participant.
   Device registration (client-held MLS keys with proof of possession), KeyPackages,
   device revocation, client-token issue and session revocation are relayed to Messaging's
   `/internal/v1/delegated/**` surface with the principal as the SIGNED initiating
   principal (an unsigned `CF-Acting-Principal` header can only repeat it). The client
   receives at most a short-lived, device-bound Messaging client token. Logging out of CF
   Meet queues a durable "revoke all Messaging sessions" job.
3. **Meeting conversations (§98).** Each conference resolves idempotently to one MEETING
   conversation (`meetingRef = conference:<id>`, context namespace `meeting`), MANAGED
   because meeting chat is server-readable by design and disclosed as such. Membership
   follows admission (ADMITTED → ACTIVE with role, REMOVED / BANNED / LEFT), sent with a
   source version drawn from a sequence under the conference link lock and fenced in
   Messaging (`messaging_sync_fences`), so a delayed older update never restores access.
   Former participants are recorded as terminal memberships that never grant access, so
   their history keeps its true author. Ended conferences close their conversation.
4. **Migration state machine (§100).** `LEGACY_PRIMARY → SHADOW_VALIDATION →
   MESSAGING_PRIMARY → LEGACY_RETIRED`, with `SHADOW_VALIDATION → LEGACY_PRIMARY` (abort)
   and `MESSAGING_PRIMARY → SHADOW_VALIDATION` (rollback). Transitions are version-checked,
   audited (append-only) and platform-admin only. Cutover requires a PASSED parity run of
   the current shadow period, nothing left unmirrored and a second named approver;
   retirement needs an approver and is final (also a database trigger).
5. **Shadow, provenance and rollback (§100–§101).** In shadow mode legacy stays
   authoritative and every main-room message is mirrored in order, idempotently, as
   `LEGACY_MANAGED` with its original timestamp; legacy deletions become moderator
   removals; parity compares presence, content digest and deletion state and records every
   mismatch. A Messaging outage only defers mirroring. On rollback every Messaging-native
   message is copied back (labelled `MESSAGING_ROLLBACK`) before an abort is allowed.
6. **Compatibility adapter (§99).** `UnifiedMessageStore` implements Meeting's existing
   `MessageStore` port: `/api/v2/conferences/{id}/messages` keeps its contract, Meeting
   keeps authorization (roles, policy, restrictions, breakout rules) and Messaging stores
   and orders the canonical message. Meeting keeps ids only, never a second body copy; once
   Messaging is primary the database refuses new main-room `conference_messages` rows and
   legacy v1 `chat_messages` rows (`CHAT_MIGRATED`). Imported messages keep their legacy id
   in the Meeting API. A Messaging outage answers `503 CHAT_UNAVAILABLE` for chat only.

## Scope limits (documented PARTIAL)

* Breakout and private chat stay in `conference_messages` (Meeting authority) in every
  state; they are not mirrored or migrated in this increment.
* The realtime fan-out of meeting chat stays Meeting's outbox (its payload carries the
  body, as before); Messaging push notifications are not sent for meeting chat
  (`notify: false`).
* Web / mobile clients are not changed here; real-device runtime certification remains a
  Part K gate.

## Amendment (Blueprint v1.0 Section 13) — superseded scope and relay parity

* **Superseded scope limit (MSG-015).** The first scope limit above no longer describes the
  code: breakout and private chat migrate under the same state machine to their own
  MEETING conversations (`unified_messaging/scoped.rs`, `store.rs`: "used for EVERY meeting
  chat scope"; the migration status reports `scope.migrated = MAIN_ROOM_CHAT, ANNOUNCEMENTS,
  BREAKOUT_CHAT, PRIVATE_CHAT` and `scope.legacy = []`).
* **Client relay parity (MSG-001/002/003).** `/api/v2/messaging/relay/{path}` relays every
  allowlisted method including PATCH (MANAGED edit, member role, conversation update /
  retention / title) and exactly the Messaging contract's query parameters per operation
  (`scope`, `reason`, `threadRootId`, `lifecycleState`, `contextClass`, `orderBy`). The
  replay cursor is forwarded as Messaging's canonical `after`; the WebSocket spelling
  `afterEventSequence` is accepted as a compatibility alias and rewritten. A unit test
  walks the bundled Messaging contract against the allowlist and an integration test
  walks it through the real router.
* **Identity bridge addition.** `POST /api/v2/messaging/session-tokens` issues
  MANAGED-realtime session-device tokens bound to the verified account session (ADR 0049
  amendment A1); signing out of that session revokes them.
* **Organization conversations (built in the final closure, 2026-11-09; supersedes the
  earlier "integration point, not built" note).** Organization conversations are not a
  `context` inside the personal scope: they live in the **organization's own FIRST_PARTY
  technical scope** (ADR 0056 §4), reached through
  `/api/v2/organizations/{id}/messaging/relay/{path}` (the same allowlist as the personal
  relay), `/api/v2/organizations/{id}/messaging/session-tokens` (MANAGED realtime) and
  `/api/v2/organizations/{id}/messaging/members/resolve` (active colleagues → their
  organization-scope principals). Membership, organization status and `features.messaging`
  are decided by the Organizations Service per request; the Messaging binding is per
  organization scope (`service-bindings/messaging`). Offboarding suspends the member's
  organization principal (organization events → Meeting `organization_access` worker →
  Platform Access), which stops reads, sends and realtime while personal Messaging is
  untouched. Certified across real processes in `organization_lifecycle.rs` (J-10).

## Consequences

The journey is proven across real Platform Access and Messaging processes
(`services/meeting-service/tests/unified_messaging_e2e.rs`, CI
`unified-messaging-e2e.yml`). Production cutover is an operator decision with its own
approval; nothing in this ADR enables it.
