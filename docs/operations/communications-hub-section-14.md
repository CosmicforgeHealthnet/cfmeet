# Communications Hub and revocation — Blueprint v1.0 Section 14

Engineering record for staging. Not a production release authorization; nothing here
changes production until an operator deploys it. PRIVATE_E2EE stays blocked (independent
review #13); no change here enables or weakens it.

## What changed

| Finding | Change | Where |
|---|---|---|
| ACC-010 | Account-wide credential ends (password change/reset, deactivation, admin suspension/deactivation, logout-everywhere) reach Calling and Messaging **without a Hub profile**: the Hub reads the principal from Account and fans `communications.account.changed` out durably (`fanOut: PRINCIPAL`). Account also queues the durable Messaging `revoke-all` for the bridge principal on each of them (reasons `PASSWORD_CHANGED` / `ACCOUNT_DISABLED`, previously logout only) and mirrors the current account status onto the Platform Access principal from its outbox worker | Hub `events.rs`; Meeting `services/communications.rs` (`credentials_ended`, `mirror_principal_status`), `handlers/auth.rs`, `handlers/users.rs`, `handlers/admin.rs`; Platform Access `POST /internal/v1/principals/account-status` |
| ACC-008 | Devices belong to sign-ins (ADR 0057 amendment A1): a Hub device records the account session that linked it; ending the session (`logout`, `POST /api/v2/me/sessions/{id}/revoke`) queues `ACCOUNT_SESSION_ENDED` and the Hub revokes its devices at their owners (`ACCOUNT_SESSION_ENDED`); revoking a bound device ends its sign-in through the Account relay. Surface: the existing `/api/v2/me/sessions*` + `/api/v2/communications/devices*` | Hub `devices.rs`, `api.rs`; Meeting relay (`DeviceOperation`, `end_bound_session`, `session_ended`), `handlers/v2/account_sessions.rs`; `packages/communications-policy` (`VerifiedAccount.accountSessionRef`) |
| DATA-005 / DATA-006 | Authority decision: owners are authoritative, the Hub device table is an index bound to sign-ins, Hub `COMMUNICATIONS` blocks are the personal-block authority, owner-sourced bridge rows and `block-events` deprecated; single read paths documented | [communications-device-and-block-authority.md](../architecture/communications-device-and-block-authority.md) |
| MSG-016 | Inbox lists only current memberships (checked with Messaging, like `activity`); `block-events` marked deprecated (no producer; removal at the next contract major) | Hub `sync.rs`, `events.rs`, contract 1.3.0 |
| MSG-009 | Late-activated members get meeting invitation cards on first timeline read (bounded by `visibleFromSequence`); chat lines stay client-composed — disposition in the [handoff §11](../api/communications-unified-handoff.md) | Hub `calls.rs` (`materialize_meeting_cards`) |
| MSG-010 | Real-process CI journey: Hub facades (conversation context, call launch, timeline cards, instant meeting, share, inbox), device ↔ session binding, revocation without a Hub profile, Platform Access mirror — real Account, Platform Access, Hub, Calling (LiveKit), Messaging | `certification/tests/personal_communications.rs` (`hub_facades_device_sessions_and_revocation_fan_out_across_real_owners`), workflow "Communications Hub foundation" (real-services job) |
| Section 13 follow-up | `capabilities.messaging.available` follows MANAGED messaging; `managed.realtime` is `SESSION_DEVICE` only when Messaging has `MESSAGING_SESSION_DEVICES_ENABLED` and client-token keys; E2EE readiness reported separately (`securityReady`, `privateE2ee`) | Messaging `communications.rs` (`managed_capability`), Hub `policy.rs` (`messaging_capability`) |
| ACC-009 / MSG-006 / FE-020 | REUSED_EXISTING_IMPLEMENTATION + frontend handoff (journey → endpoint table) | [handoff §11](../api/communications-unified-handoff.md) |

## Contracts

`cfmeet.communications.communications-internal` 1.3.0 (+bundled), `cfmeet.communications.http-api` / `public-api` / `swagger-api` 1.2.0, `cfmeet.messaging.communications-internal` 1.2.0 (+bundled), `cfmeet.platform-access.internal-api` 1.8.0 (+bundled; 1.7.0 is Section 15), `cfmeet.platform-access.identity` 1.4.0. SDKs regenerated (TS/Dart models). Meeting-service: no new operation (v2 count unchanged at 181); one description changed (`v2_revoke_my_session`). No new service-auth permission: the Platform Access mirror reuses `platform-principal.ensure` (already granted to Meeting), so no trust-bundle change.

## Database

* Hub: new third history `communications_device_session_seaql_migrations`, migration `m20261108_000001_device_session_binding` — `communication_devices.account_session_ref` (nullable, immutable once set, CHECK 64 hex), partial index, revocation reason `ACCOUNT_SESSION_ENDED`. Applied by the existing `communications-migration up` (it runs every history). The previous Hub image checks only the foundation and unified histories, so it still starts after this migration (rollback safe; the column is ignored by it). Backup catalog lists the new optional history.
* Meeting, Messaging, Calling, Platform Access: no migration.

## Release steps (owner-gated; production untouched by this section)

1. Platform Access image (new route) and Hub migration + image, then Messaging image, then Meeting image. An older Hub refuses `ACCOUNT_SESSION_ENDED` and the device session reference (Account's outbox keeps retrying until the Hub is upgraded; device links from a newer Account fail with 400 against an older Hub); an older Platform Access answers 404 to the mirror, which Account treats as "nothing to mirror".
2. No new configuration. `MESSAGING_SESSION_DEVICES_ENABLED` remains the Section 13 release decision; until it is set, capabilities report `managed.realtime: POLLING`.
3. Rollback: images can roll back in any order; the Hub migration stays (forward-only, additive). Device bindings recorded by the new image are ignored by the old one.

## Not in this section

* Realtime nudge for Hub timeline changes and call cards for members without a Hub profile at launch time — both closed later by the MSG-009 audit closure below.
* Auto-linking Messaging devices / Calling endpoints into the Hub registry at registration (client step in the handoff).
* Removal of the deprecated `block-events` route and the dead Meeting `audio_video_devices` table (later contract / legacy-retirement steps).
* Web and mobile clients (Section 17).

## Audit closure MSG-009 (after Section 14)

| Change | Where |
|---|---|
| **Call cards for parties without a profile at launch.** A conversation call's members are all call parties (Calling rings them all); a member without a Hub profile now gets an owed card (`communication_pending_call_cards`, references and safe state only). It is written once — moved into the timeline in one statement — the first time that principal reads its timeline or activity with a profile, due for an immediate refresh from Calling under its own delegation, and announced on its change feed. Matched by principal only; every read that shows a card still checks membership. Pruned after 30 days (call history keeps the call). | Hub `calls.rs` (`launch`, `materialize_call_cards`), `events.rs` (`maintain`) |
| **Realtime nudge.** Every distinct timeline change — a call started (`CALL_STARTED`), each Calling state version first observed by any viewer (`CALL_UPDATED`), a meeting shared (`MEETING_SHARED`) — is deduplicated (`communication_timeline_nudges`), bumps the conversation's monotonic version (`communication_timeline_versions`) once and queues `communications.timeline.changed` for Messaging ONLY, in the same transaction, through the durable owner outbox. Messaging publishes `timeline.changed` to the conversation's members (live and replay; see [messaging-section-13.md](messaging-section-13.md)). Replayed launches / shares and repeated sweeps nudge nothing. When a call's state moves for one viewer, the other parties' cards of that call are made due at once (outcomes are per party), so every party converges on its next read or the next sweep. | Hub `calls.rs` (`timeline_nudge`, `refresh_call_cards`), `meetings.rs` (`record_meeting`), `store.rs` (`messaging_event`) |
| Tests | `services/communications/tests/unified.rs` `call_cards_reach_parties_without_a_profile_and_timeline_changes_are_nudged_once`; Messaging `tests/realtime_receipts_timeline.rs` `hub_timeline_nudges_reach_members_once_per_version` |

Database: a FOURTH separate history `communications_timeline_realtime_seaql_migrations`, migration `m20261109_000001_timeline_realtime` (three additive tables), applied by `communications-migration up`. The previous image checks foundation + unified + device-session exactly (`device_session_ready`), so it still starts after this migration; the backup catalog lists the new optional history. Contract: `cfmeet.messaging.communications-internal` 1.3.0 documents the event. Calls launched outside the Hub still have no card (Calling has no producer); chat lines stay client-composed (handoff §11).
