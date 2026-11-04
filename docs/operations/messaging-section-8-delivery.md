# Section 8 — Messaging production activation and delivery closure

## Baseline before production configuration changes

Audit date: 2026-11-04. Starting local/staging `547313a656ef86b8e91c1e481251d6887f5004be`;
starting remote main `de033cd8e88dd9469f41191b74f60c6b0416837e`. Local main was
fast-forwarded, preserving the intervening author's frontend changes without editing them.
Only main/staging are used. No frontend, mobile, Vercel or telecom changes are in scope.

The previously queued Calling cleanup run **41865047261** completed successfully,
including its real PostgreSQL security tests; Hub run **41865047319** also succeeded.
Latest Unified Messaging run **41874453663**: the two real backend integration jobs
passed; its browser-client job failed. That failure is not certified as a backend pass
and frontend repair/testing is outside this assignment.

Read-only VPS inspection **41927794291** succeeded before any runtime changes:
Messaging image ID `sha256:df59d317ea2dd7b224c22fda2adbedd16d9352e12342def6bd7c4b8a85afb67f`
(accepted source `787bb9a58bd34add53f74a2bbf52aaa141b317d8`). Private network only,
no published Messaging ports. Existing service-auth, transparency and client-token keys
are read-only mounts. Native Communications scope and Account bridge remain configured.
No attachment S3, ClamAV, managed-keyring, realtime Redis, metrics listener, witness or
checkpoint settings occur in the effective Messaging environment. Push provider credentials
are absent. More detailed redacted inspection is implemented in the existing VPS workflow;
it must complete before configuration changes. No customer content is inspected.

## Exact reusable capability inventory

Classification describes the starting production state, not the existence of passing
unit tests. Canonical Messaging implementation is reused throughout.

| Capability | Baseline classification | Implementation and existing tests |
|---|---|---|
| Account login/principal bridge, short-lived device sessions | IMPLEMENTED_AND_ACTIVE (issuance requires enrollment) | Account `services/unified_messaging/{identity,relay,gateway}.rs`, `handlers/v2/unified_messaging.rs`; Messaging `identity/`, `realtime/sessions.rs`; `client_tokens_rest`, `unified_messaging_e2e` |
| Communications identity/contact/privacy enforcement | IMPLEMENTED_AND_ACTIVE | `communications.rs`, conversation/message/membership authoritative entry points; Hub real-five-service acceptance |
| Direct/group create/reuse, lifecycle, membership/roles/invitations | IMPLEMENTED_NOT_ACTIVATED for personal PRIVATE_E2EE | `conversations/`, `membership/`; `conversations`, `messaging`, `engagement`, `authorization_matrix` |
| Send/list/edit/delete/reply/reactions/pins/read/unread/events | IMPLEMENTED_NOT_ACTIVATED for personal PRIVATE_E2EE | `messages/`, `engagement/`, `conversations/sequence.rs`; `messaging`, `engagement` |
| MLS/OpenMLS client encryption, epochs, tamper/replay/transplant checks | BLOCKED_EXTERNAL_DEPENDENCY for production activation | `packages/messaging-crypto`, `mls/`, `messages/e2ee.rs`; crypto vectors/property/compatibility suites; `messaging` |
| Device PoP, KeyPackages, multiple devices, revocation/rotation | IMPLEMENTED_NOT_ACTIVATED | `devices/`, `http/devices.rs`, `realtime/sessions.rs`; `devices`, `history_transfer`, `backups` |
| Local private-key custody, authenticated history transfer, optional encrypted backups | FRONTEND_DEVICE_RESPONSIBILITY plus implemented opaque backend relay | crypto `device`, `group`, `history_transfer`, `backup`; backend `devices/history_transfer.rs`, `backups.rs`; corresponding tests |
| Signed KT heads/inclusion/consistency/rollback monitor | IMPLEMENTED_NEEDS_INTEGRATION | `transparency/`, `operations.rs`; `transparency`, `transparency_witness`, `operations`, `disaster_recovery` |
| Independent KT witness/cryptographic assessment | BLOCKED_EXTERNAL_DEPENDENCY | security architecture AD-01–AD-10, ADR 0045; no independent acceptance evidence found |
| MANAGED sealed messages/search/moderation | IMPLEMENTED_NEEDS_INTEGRATION | `messages/{managed,keys}.rs`, `search/`, `moderation/`; `managed_keys`, `compliance`; no production keyring, never a personal E2EE substitute |
| WebSocket tickets, durable replay/acks, ordering, offline/multi-device sync | IMPLEMENTED_NOT_ACTIVATED for un-enrolled personal devices | `realtime/`; `realtime`, `client_tokens_rest` |
| Multi-instance Redis fan-out and presence | IMPLEMENTED_NEEDS_INTEGRATION | `realtime/{broker,ephemeral}.rs`; `realtime_redis`; production currently in-process |
| PostgreSQL outbox/retry/dead-letter/replay | IMPLEMENTED_AND_ACTIVE | `events/outbox.rs`, `workers/`, `operations.rs`; `outbox`, `operations`, `realtime` |
| Attachment intent/finalize/grants/quotas/retention/derivatives | IMPLEMENTED_NEEDS_INTEGRATION | `attachments/`, `packages/object-storage`; `attachments`, `query_plans`; no production S3 config |
| Private attachment crypto/thumbnail handling | FRONTEND_DEVICE_RESPONSIBILITY with implemented opaque storage | crypto `attachment.rs`; backend never scans/decrypts PRIVATE_E2EE; `attachments` |
| Managed attachment malware scanning | IMPLEMENTED_NEEDS_INTEGRATION | `attachments/scanner.rs`; real clamd/EICAR CI; deployment absent |
| APNs/FCM/Web Push adapters/retries/token cleanup | IMPLEMENTED_NEEDS_INTEGRATION | `realtime/{push,push_adapters}.rs`; `push_adapters` verifies local fake providers, not physical delivery |
| Queued push recipient authorization | IMPLEMENTED_NEEDS_REPAIR | `realtime/push.rs` checks device registration/status but not current conversation membership at dispatch |
| Per-device push toggle | IMPLEMENTED_AND_ACTIVE for enrolled devices | `push::register/unregister`; no provider means NOT_CONFIGURED, never delivered |
| Per-contact notification preferences | IMPLEMENTED_NEEDS_INTEGRATION | Hub `communication_contact_aliases.notifications`; pending delivery integration |
| Meeting chat/guest/admission compatibility | IMPLEMENTED_AND_ACTIVE under independent existing rules | Account `services/unified_messaging/`, Messaging `integrations/meeting.rs`; `integrations`, Account real-service journeys |
| Metrics/readiness/worker health/operator recovery | IMPLEMENTED_NEEDS_INTEGRATION | `metrics.rs`, `operations.rs`, `messaging-admin`; `operations`, `load`; listener not configured |
| Contracts/SDKs/public routing | IMPLEMENTED_NEEDS_INTEGRATION verification | bundled Messaging 96 operations/77 paths, Account authenticated relay allowlist; contracts need live route comparison |
| Off-server disaster recovery | BLOCKED_EXTERNAL_DEPENDENCY / DEFERRED_BY_OWNER | local restore tests are not VPS-loss recovery evidence |
| Phone OTP/phone matching | MISSING / DEFERRED | deliberately excluded |

Messaging already has 29 canonical forward migrations plus a separate Communications
feature migration history. No duplicate message/conversation/device service is required.
PostgreSQL remains authoritative; Redis and push are never delivery persistence.

## Production cryptographic activation verdict

**BLOCKED_EXTERNAL_DEPENDENCY.** The repository's security decision register still marks
AD-01–AD-10 pending and records no independent cryptographic assessment. Internal review
and automated tests cannot supply that assessment or owner decisions. Production
`MESSAGING_E2EE_DEVICES_ENABLED=true` must continue to be rejected. No override, guard
removal, MANAGED substitution, fake witness or production E2EE certification is authorized.
Authorized local/test crypto testing and safe operational repairs can proceed independently.

## Delivery evidence

Work in progress. Implemented/tested/deployed claims will be recorded separately with
exact CI and VPS receipts. Sections 9–10 are not started. Phone verification and off-server
backups remain deferred; loss of this VPS can lose databases, storage and recovery keys.
