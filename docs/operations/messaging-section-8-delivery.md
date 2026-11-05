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

**BLOCKED_EXTERNAL_DEPENDENCY.** ADR 0052 selects AD-01–AD-10 for engineering development
while explicitly withholding production cryptographic approval. The older security
architecture uses PENDING terminology; GitHub issue 13 remains OPEN with no independent
assessment or release approval attached. Internal review and automated tests cannot
supply that assessment or production decisions. Production
`MESSAGING_E2EE_DEVICES_ENABLED=true` must continue to be rejected. No override, guard
removal, MANAGED substitution, fake witness or production E2EE certification is authorized.
Authorized local/test crypto testing and safe operational repairs can proceed independently.

## Delivery evidence

Partially complete; see the recovery and closure status below. Implemented/tested/deployed claims are recorded separately with
exact CI and VPS receipts. Sections 9–10 are not started. Phone verification and off-server
backups remain deferred; loss of this VPS can lose databases, storage and recovery keys.

Read-only detailed inspection **41928266782** succeeded: all 29 canonical Messaging
migrations applied, 65 tables including two Communications feature tables, zero devices,
zero conversations, zero pending/dead-letter events. `/health`, `/ready` and private
`/openapi.json` returned 200. Runtime production E2EE=false; push=none; realtime TLS=true.
The inspected registry evidence identifies the running image as
`ghcr.io/cosmicforgehealthnet/cfmeet-messaging@sha256:0124976ae113b139b47a2cd7e5af61488c698ce7020c49668b2ec38bcef3cc6b`.
Resource limits/log rotation were unset; no new runtime configuration has yet been claimed
deployed by this report. Public probes found `/v1/realtime` and `/v1/realtime/tickets` 404.

The Messaging public SDK bundle's 96 operations include 16 application administration/
compliance operations intentionally filtered out of consumer Swagger. Served Swagger has
80 operations/64 paths, not 96. This distinction is preserved and documented; no admin
operations are exposed through the native relay. The native map now covers 71 operations
(65 allowlisted relay operations plus six existing identity bridges), including 33 added
allowlist entries for existing canonical operations. See
[native integration map](messaging-native-api-map.md) and
[internal security assessment](messaging-section-8-security-review.md).

Implementation to date: public-application-frame rejection; canonical MLS sender/AAD
binding; staged client commits and restart/conflict recovery; delayed push membership,
block/privacy/mute checks; revocation cleanup and token-rotation race protection; failed
privacy-read refusal; fail-closed KT checkpoint recovery and request-boundary checks;
concurrent identity creation repair; native API/Swagger integration; bounded metrics;
production inspection/configuration/verification in the existing VPS workflow. Database
schema changes are unnecessary for these repairs; existing forward migration histories
are retained. Runtime dependency preparation preserves the exact running image and
production E2EE=false, with a private rollback container and separate stage checkpoint.

Contact integration closure: existing conversation creation now accepts an exclusive
`counterpartCfMeetId` alternative; existing member addition accepts `cfMeetId`. Private
Hub resolution is purpose-bound (DIRECT_CONVERSATION or GROUP_INVITATION) and then
canonical Messaging authorization runs unchanged. Public discovery still exposes no
principal IDs. Tests cover dual identifiers, chat privacy, group invitations, and blocks.
HTTP contract 1.14.0 and private Hub contract 1.2.0 retain the existing operation counts.

## Recovery and closure status — 2026-11-05

Statuses: IMPLEMENTED (code on main), TESTED (named evidence passed), DEPLOYED (running in
production, verified by inspection), BLOCKED, DEFERRED. Nothing below is claimed beyond
the cited run.

### 1. Interruption recovery findings

The interrupted agent's last commit was `0d92389` (transfer media correction), pushed to
both main and staging. Its production run **41981794294** started but the agent never read
its result. That run failed. Its `PREVIOUS_ACCEPTANCE` line is the stale result of the
earlier run 41980193898 (transfer "Remote playable media not received"), not a new
failure. In 41981794294, Communications acceptance passed and the enterprise harness then
failed earlier than transfer, at voice reconnection ("Current session reconnect refused",
no error recorded). So the transfer fix was never exercised in production.

### 2. Surviving local changes

Recovery ran in a fresh cloud clone, not the original laptop. It had no uncommitted,
untracked or unpushed work. Local staging was a fast-forward behind origin and was
fast-forwarded. The laptop's three locally changed files (about 1,836 additions in the
earlier screenshot) could not be inspected from here. Every Section 8 commit is on
GitHub, so they are probably that history, but this is **unverified**. Check `git status`
on the laptop before discarding anything there.

### 3. Repository and production SHAs

- main = staging = `0eb2f189f4846749c8cab63558e6965643462d27`. This adds the reconnect
  classification below. No service code has changed since `0a5d9dd`.
- Release candidate images are built from `0a5d9dd381010252d280698fd37a9481c0513dd9`. The
  release has not been accepted.
- Production still runs the images built from `787bb9a58bd34add53f74a2bbf52aaa141b317d8`:
  Messaging image ID `sha256:df59d317ea2dd7b224c22fda2adbedd16d9352e12342def6bd7c4b8a85afb67f`, Calling image ID `sha256:58c969d3856884aea0ec54fb0fc71a8019fdf93e49f4ed96595d587c5200adac` (inspections 41974553591 and
  41978504593). The verified rollback state is preserved.

### 4. Section 8 work: completed vs incomplete

| Area | Status |
|---|---|
| MLS PrivateMessage framing, sender/device AAD binding, staged commits | IMPLEMENTED, TESTED (Messaging CI 41970089662 on `0a5d9dd`, 41971644251 on `ef2a2e5`); not DEPLOYED |
| Device/notification authorization, delayed push membership/block checks | IMPLEMENTED, TESTED (41970089662; the earlier failure 41968530981 was repaired by `0a5d9dd`); not DEPLOYED |
| Privacy/block enforcement, KT checkpoint fail-closed, concurrent identity repair | IMPLEMENTED, TESTED; not DEPLOYED |
| CF Meet ID contacts, native route map, Swagger/SDK 1.14.0 | IMPLEMENTED, TESTED (isolated staging 41972295095); not DEPLOYED |
| Release-candidate isolation (brokers, storage, push, KT checkpoint, keyring) | IMPLEMENTED, TESTED (staging 41972295095 and unit tests) |
| Production rollout of `0a5d9dd` | BLOCKED: see section 5 |
| Messaging production dependencies | BLOCKED: tooling IMPLEMENTED, never executed (section 6) |
| Production PRIVATE_E2EE | BLOCKED_EXTERNAL_DEPENDENCY (section 7) |
| Phone OTP/matching; off-server disaster recovery | DEFERRED |

### 5. Transfer acceptance outcome

The `0d92389` correction was reviewed against Calling. `complete_transfer_if_joined`
retires the transferring party, and `enforce_removals` then removes it from the provider
room. So the harness correctly waits for: CONNECTED, the departing client disconnecting,
the old identity leaving the caller's remote list, the target identity being present,
and then playable frames both ways. Three regression tests pass. It is IMPLEMENTED and
TESTED locally. It is **not yet TESTED in production**, because no production run has
reached the transfer phase since the fix.

`0eb2f18` closes the reconnect diagnostic gap. Calling admission still admits a
DISCONNECTED session that holds the current generation, and the resealed credential
lasts 900 s, so a refusal was not expected. The step now records a redacted error class
and category. An authorization refusal (401/403/not admitted) fails at once. A transport
fault gets one more attempt with the same current credential. Two failures still fail.
Four regression tests cover this. Criteria are unchanged.

Production run **41992435685** (`communications-release`, image `0a5d9dd`) then failed
earlier still, in Communications acceptance: `RTC operation failed: connect`,
`TimeoutError`, `connection_timeout: true`, `ice_checks: false`, `signal_error: false`,
`unauthorized: false`. It rolled back automatically, and regression passed afterwards.

**Classification: an intermittent media-signaling timeout before ICE, on the on-VPS
acceptance path (client → public TLS origin → nginx → Calling gateway → LiveKit). It is
not an authorization refusal and not a regression of `0a5d9dd`.** The same symptom
affected `787bb9a` (41794968776, 41797479652) before it passed (41795985590). There have
been 7 consecutive `0a5d9dd` release failures (41972828392, 41974711025, 41975667093,
41977529784, 41980193898, 41981794294, 41992435685). They are mostly media-connect or
transport timeouts. One was a 429 from public-ID rotation rate limits. The read-only
diagnosis 41978504593 found firewall/self-origin/nginx markers clean. Releases are not
retried blindly. Next safe steps: collect the Calling gateway and LiveKit timing for the
failing connect (server logs during acceptance), and check VPS CPU and memory pressure
while candidates run.

### 6. Messaging production dependency status

The latest inspection (41969121121) shows the following.

- **Configured:** communications policy, realtime signing, transparency signing.
- **Not configured:**
  - attachment storage
  - malware scanner
  - Redis fan-out
  - metrics listener
  - managed keyring
  - KT checkpoint
  - witnesses
  - APNs/FCM/Web Push (`MESSAGING_PUSH_PROVIDER=none`, no credentials)
- **Runtime:** E2EE=false; 0 devices, 0 conversations, 0 pending/dead outbox; no published
  ports; no memory/CPU limits.

`messaging-configure` exists, but **no production run of it exists**. It is BLOCKED
pending an owner decision: ClamAV reserves about 3 GiB on a VPS with about 5 GiB
available, and VPS load is a candidate cause of the media timeouts above. Push stays
BLOCKED until provider credentials exist.

### 7. E2EE approval status

[Issue 13](https://github.com/CosmicforgeHealthnet/cfmeet/issues/13) is OPEN and
unchanged since 2026-10-18. It has no independent assessment, witness, licence review or
owner sign-off. The production validator still refuses `MESSAGING_E2EE_DEVICES_ENABLED=true`.
**BLOCKED_EXTERNAL_DEPENDENCY.** MANAGED is not presented as PRIVATE_E2EE.

### 8. CI runs and results

- Compose config 41981793757: success. Service boundaries 41981793742 (main) and
  41981790517 (staging): success, on `0d92389`.
- Messaging CI: 41970089662 (staging `0a5d9dd`) success; 41971644251 (main `ef2a2e5`)
  success.
- Unified Messaging integration journeys: the last five runs were cancelled by the
  owner, so they provide **no evidence**.
- Production workflow: see sections 5 and 9.

### 9. Real-service acceptance evidence

| Journey | Evidence | Status |
|---|---|---|
| Two authenticated accounts, identity, accepted relationship, block/unblock, ID rotation, Meetings regression, direct conversation reuse + managed message (TEST app), native voice/video frames | staging 41972295095 (`0a5d9dd`, 14/14); production 41795985590 and 41798927157 (`787bb9a`) | TESTED (isolated for `0a5d9dd`; production for `787bb9a`) |
| Enterprise Calling 20/20, incl. reconnection, IVR and internal transfer | production 41795985590 and 41979206960 (`787bb9a`) | TESTED on `787bb9a`; not on `0a5d9dd` |
| Recording 12/12; voicemail 7/7 | 41799294360, 41799339221 (`787bb9a`) | TESTED |
| Transfer completion with removal of the departing peer (new criteria) | none in production yet | BLOCKED (section 5) |
| Production E2EE device enrollment/revocation, bidirectional E2EE messaging, groups | E2EE gate | BLOCKED; only isolated CI suites cover them |
| Attachments, ClamAV, Redis realtime/offline replay in production | dependencies not configured | BLOCKED; CI uses real PostgreSQL/Redis/MinIO/ClamAV (41970089662) |
| Worker/restart/outage recovery | CI suites only | TESTED in isolation; not in production |

### 10. Image digests and deployment status

Candidate `0a5d9dd` registry digests:

- platform-access `sha256:c5879a85113099882b1328bb92c6d865565fbc86bda1e113348fedd9df049191`
- meeting-service `sha256:5d057e4b28aa56f4a64b452179493e7c175eaaf678587dd5cdb9e9aaa3fd7708`
- calling `sha256:df97addabe089c488d557648efc011c56663002b7c9bce2c04460996fef86ac1`
- messaging `sha256:ca79a71a5761ebdf277d6d0e3b6b8c72ba9dbecfee9230a68bc3df547ea5070a`
- communications `sha256:4402bb9d590b820feec63d2523677ef6906e0556b683e25ea5c9ebaf592af44e`

These were transferred and verified by image ID, but are **not DEPLOYED**: every release
attempt rolled back. Production keeps `787bb9a`; Messaging is registry digest
`sha256:0124976ae113b139b47a2cd7e5af61488c698ce7020c49668b2ec38bcef3cc6b`.

### 11. Remaining risks and blockers

1. Intermittent pre-ICE media-signaling timeouts in on-VPS acceptance. This blocks
   certifying the release.
2. Messaging dependencies are unconfigured, and there is a memory headroom trade-off.
3. There are no push provider credentials.
4. E2EE has no external certification or owner approval.
5. Unified Messaging journeys have been cancelled rather than run.
6. Off-server recovery is deferred, so loss of the VPS can lose data and keys.

### 12. Closure verdict

**Section 8 is PARTIALLY COMPLETE.** Engineering work is IMPLEMENTED and TESTED in CI and
in isolated staging. The production rollout, production dependency activation and
production E2EE are not done. Sections 9–10 are not started.
