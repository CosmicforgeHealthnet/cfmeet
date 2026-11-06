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

## Media-signaling timeout root cause, capacity and release readiness — 2026-11-05 (continued)

No production-changing operation ran in this phase: no `messaging-configure`, no
`communications-release`, no container, firewall or LiveKit change. Production still runs
the verified `787bb9a` images. Evidence comes from read-only `media-diagnose` runs
[42008205649](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42008205649),
[42009330067](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42009330067),
[42010061145](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42010061145) and
[42012266631](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42012266631).
They print only redacted message templates, counts, timings and resource figures (unit-tested
redaction), plus isolated GitHub-runner reproductions. Status of this work: IMPLEMENTED and
TESTED in CI; not DEPLOYED.

### Measured VPS state (read-only)

| Area | Measurement |
|---|---|
| Host | 6 vCPU, 11.7 GiB RAM, 4 GiB swap (~0.3 GiB used, swappiness 10), uptime 425 h, cgroup v2, Docker 29.1.3 |
| Memory | MemAvailable 5.0 GiB now; sysstat minimum 4.72 GiB across both days; memory PSI 0; no swap-in/out in any window |
| CPU | load ~4–5; idle ≥ 54% in every failure window (sysstat); steal 0; iowait ≤ 4.6%; CPU PSI "some" avg300 16% (contention, not saturation) |
| OOM / kernel | 0 OOM kills, 0 conntrack-full, 0 hung tasks since 2026-11-01 |
| Disk | / 84.7% used, 29.5 GiB free; images 144 GB (45.6 GB reclaimable); 107 exited containers, 60 retained failed/rollback containers |
| Workload | 76 running containers from several products; existing ClamAV daemons `admin-server-mail-clamav` and `admin-server-staging-mail-clamav` at ~949 MiB each (3 GiB limits), plus `karrygo-media-scanner` at 683 MiB |
| Network | UDP InErrors/RcvbufErrors 0, NoPorts 4616; TCP ListenOverflows 0, ListenDrops 85; conntrack 1148/262144; hop latency p50/max: public TLS 0.10/0.25 s, gateway 401 upgrade 0.07/0.10 s, loopback Calling 0.008 s, LiveKit 0.003 s |
| Active calls | 0 at inspection |
| LiveKit | v1.13.9, `livekit/livekit-server@sha256:d0c04791bf63ca8dcea123571827d56ba9504bd57c8d5de60ee03b5408cc3154`; Docker bridge network; published 3478/udp, 5349/tcp, 7880/tcp, 7881/tcp; rtc 50000–60000/udp **not published**; `use_external_ip` true; node_ip = public DNS address; embedded TURN enabled; **two API keys** (Meetings and Calling) |
| Log rotation | none on `lk-server`, `cfmeet-calling`, `cfmeet-messaging`, `cfmeet-core-db` and most other CF Meet containers |

### Correlation of the failed releases

LiveKit per-window counts (`starting RTC session` / `participant active` / never connected).
Each never-connected participant was removed with reason `SIGNAL_SOURCE_CLOSE`:

| Run | Image | Outcome | Sessions / active / never connected |
|---|---|---|---|
| 41794968776 | 787bb9a | FAIL | 4 / 2 / 2 |
| 41795985590 | 787bb9a | PASS (control) | 31 / 24 / 7 |
| 41972828392 | 0a5d9dd | FAIL | 17 / 14 / 3 |
| 41975667093 | 0a5d9dd | FAIL | 29 / 23 / 6 |
| 41977529784 | 0a5d9dd | FAIL | 7 / 3 / 4 |
| 41979206960 | 787bb9a | PASS (control) | 26 / 19 / 7 |
| 41980193898 | 0a5d9dd | FAIL | 21 / 16 / 5 |
| 41981794294 | 0a5d9dd | FAIL | 9 / 6 / 3 |
| 41992435685 | 0a5d9dd | FAIL | 2 / 0 / 2 |

About 20–30% of media joins never connect, in passing and failing runs and with both images.
**The `0a5d9dd` candidate did not introduce the defect.** Enterprise acceptance often still
passed because `join()` retries with a fresh credential. The Communications harness has no
retry, so one dropped join failed the release. The two 429 failures (public-ID rotation rate
limit) and the URLError/extension timeouts are separate, transient harness or rate-limit
events; they are not counted as this defect.

Anonymized per-participant LiveKit timelines show the exact stage. The sequence is
`starting RTC session` → Calling's admission `UpdateParticipant` (+0.04–0.17 s) →
`participant closing reason=SIGNAL_SOURCE_CLOSE, sessionDuration 0` (+0.1–0.4 s), before any
ICE. Successful joins show `UpdateParticipant` → `participant active` instead. nginx recorded
the WebSocket upgrades (101), so signaling worked. The client SDK then waited for
`wait_pc_connection timed out` (15.7 s) and retried, past the acceptance's 20 s connect budget.
Ruled out by measurement: CPU, memory, swap or OOM pressure; disk; TLS, nginx or loopback
latency; connection limits; active calls; and the Communications policy service (no admission
refusals logged in those windows).

### Root cause

Production LiveKit is shared with Meetings and holds two API keys. LiveKit v1.13.9 signs
**refreshed participant tokens with an arbitrary configured key**: the first entry of a Go map.
The upstream fix, [livekit/livekit#5088](https://github.com/livekit/livekit/pull/5088)
"Sign refresh tokens with the join token's API key", merged on 2026-10-28 and is not in any
image used here. LiveKit refreshes at session start and again after Calling's admission
permission grant. Calling's media gateway can verify only Calling's key, so when a refresh was
signed with the Meetings key it **closed the signaling relay**. Why it is intermittent: the
map's first entry varies per call. Roughly 1/8 of refreshes pick the less-favoured key, so two
refreshes per join give 1 − (7/8)² ≈ 23%, matching the 20–30% observed.

Isolated reproduction (no production access): run
[42012267771](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42012267771)
`refresh-keys`. It used the identical production image digest `sha256:d0c04791…`, a
Calling-key join-only join and a server permission grant. 67/80 refreshes were signed with the
other application's key, and 39/40 joins received at least one. The current `latest` image
(`sha256:25cd902d…`) gave 70/80 and 40/40. The skew is mirrored relative to production (which
key the map favours depends on the key names), but the defect is the same. CI never saw it
because the certification LiveKit had a single key.

Not the root cause, but recorded: LiveKit's WebRTC UDP range is unpublished in bridge mode, so
host-network clients fall back to ICE-TCP 7881. The UFW default deny also blocked container →
host UDP (29 blocks to 50000–60000, 20 to 3478 since 2026-11-04). The isolated replica of that
exact topology ([42009530386](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42009530386))
connected 30/30 with bidirectional audio (p50 0.44 s) over TCP, and the UDP-mux variant 30/30
(p50 0.38 s). It degrades media quality for real users (TCP/TURN-TLS instead of UDP) and is a
recommended follow-up, not a release blocker. TURN allocation-quota messages after 06:15 on
5 Nov come from the host-network acceptance client gathering candidates on dozens of Docker
bridges.

### Repairs

| Repair | Commit | Status |
|---|---|---|
| Gateway refresh handling: reseal refreshes verifiable with Calling's key as before. Otherwise accept a refresh only if HS256, LiveKit-issued (never edge-issued), unexpired and bound to the same room, identity and generation, and mint an edge credential from the connection with the **admitted permissions re-derived from the database** (never the unverified token's grants; join-only when nothing is admitted). Admission is re-checked at every refresh; both relay exits are now logged | `9f61237`, `4ca0cb5` | IMPLEMENTED, TESTED (unit, clippy); certification below |
| Certification LiveKit holds a second application key, so the real gateway is certified against the production key layout | `36fe5ef` | IMPLEMENTED |
| Isolated topology and refresh-key reproductions; `rtc_client` `refresh_issuers` (key names only) | `36fe5ef` | TESTED |
| Read-only `media-diagnose` operation (resources, kernel, sysstat, containers, network, LiveKit facts/version/timelines, nginx, SDK templates) with redaction tests | `6594575`…`be363bb` | TESTED |
| Reconnect harness diagnostics (classified, redacted) and transfer completion checks (earlier) | `0eb2f18`, `0d92389` | TESTED |

Acceptance thresholds are unchanged: the 20 s connect budget, real-media frame checks,
authorization denials, transfer removal, reconnect and rollback criteria. No test was relaxed;
the refreshed-token re-entry security test is unchanged.

### Test results

| Check | Run | Result |
|---|---|---|
| Real Calling gateway + LiveKit certification, two-key LiveKit, **before fix** (`be363bb`) | [42012267576](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42012267576) (attempt 2) | **FAIL** at the first real media join through the gateway (`rtc_certification.rs:589`, connect `ok: false`), reproducing production |
| Same, first fix (`9f61237`, join-only fallback) | [42012774740](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42012774740) | FAIL: all enterprise media scenarios passed; refreshed-token round 1 held an unprivileged refresh (fixed by `4ca0cb5`) |
| Same, **after fix** (`4ca0cb5`) | [42013987193](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42013987193) main, [42013981872](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42013981872) staging | **PASS**: extension, SIMULTANEOUS and ORDERED ring groups, queue, transfer and failed transfer with real media; all 3 refreshed-token re-entry rounds still refused 401 at the gateway and at LiveKit |
| LiveKit refresh-key reproduction (production digest), topology replicas | [42012267771](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42012267771) | PASS (evidence above) |
| Calling unit tests `media::` (21), `cargo fmt --check`, `cargo clippy --all-targets -D warnings` | local, `4ca0cb5` | PASS |
| Production tooling unit tests (redaction, persistence, transfer, reconnect, gates) | local, `9bbcc3a` | PASS (65 tests) |
| Full CI on `4ca0cb5` (main) | Calling service 42013987273, Enterprise calling certification 42013987261, Dependency and image security 42013987359, Privacy Relay 42013987220, Telephony 42013987244, Communications Hub 42013987235, Part VI cross-service 42013987320 | **PASS** |
| Communications end-to-end journeys on `4ca0cb5` | main [42013987468](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42013987468) FAIL; staging [42013982061](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42013982061) PASS on the identical commit | **Pre-existing intermittent defect, not this change.** The same assertion (`numbering/tests/communications_e2e.rs:3290`, Telephony leg read as empty right after the PSTN invitation rings, before any media or token refresh) failed before this work on `b8c3de3` ([41865047465](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/41865047465)). Likely a read-before-write race in the test; not fixed here (outside the media-signaling scope) |
| Unified Messaging integration journeys on `4ca0cb5` | [42013987269](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42013987269) | Both real-process backend jobs PASS; the shipped web client in Chromium job FAIL, the known frontend failure recorded in the baseline (frontend out of scope) |

### Messaging configuration preservation verdict

**PRESERVED (TESTED).** `messaging-configure` builds the configured container through the new
pure `runtime_config()`: the exact running image, production E2EE=false, keyring,
storage-secret and KT-checkpoint mounts, S3/Redis/ClamAV/metrics/checkpoint environment, memory
and CPU limits, and log rotation. A later coordinated release clones the running container, so
all of it carries forward unchanged with only the image and the Communications URLs, trust
bundle and personal scope replaced. Rollback renames the *same* original container back into
place, and the failed candidate is retained.

`test_messaging_config_persistence.py` (13 tests) proves:

- configure output;
- refusal of E2EE;
- retention across one and repeated image replacements without drift;
- exact-container rollback after a failed release and after a failed configure (by container
  identity);
- isolated staging receives no production keyring, storage secret, Redis URL, ClamAV, S3, push
  or production KT checkpoint, uses an isolated KT log ID, a stage database and a 768 MiB limit,
  and leaves the production definition untouched;
- every runtime dependency is explicitly classified for staging.

Mutation checks confirmed that the tests fail if staging keeps Redis, if rollback starts the
wrong container, or if a release drops mounts. Repair: a scanner created by a failed configure
run is now stopped (retained for diagnosis) instead of holding memory.

### ClamAV capacity

Documented by [ClamAV](https://docs.clamav.net/manual/Installing/Docker.html): about 1.2 GiB to
load signatures; about 2.4 GiB briefly during each daily reload (old and new signatures
overlap); freshclam test loads add more; minimum 3 GiB, 4 GiB preferred.
`ConcurrentDatabaseReload no` avoids the doubling but blocks scans during reload. Messaging
fails closed (attachments stay VERIFYING and retry), so that mode is safe. Measured on this
host, the other products' daemons sit at ~949 MiB each.

At the measured minimum of 4.72 GiB available:

- **Default mode:** a CF Meet daemon peaking around 2.4 GiB plus freshclam leaves about 1.3 GiB.
- **Low-memory mode:** it still leaves under the 3 GiB that a coordinated release requires for
  isolated candidates (`stage_release` refuses below that).

**Adequate headroom is not demonstrated.** The configure gate now requires scanner limit
3 GiB + release margin 3 GiB + media reserve 0.5 GiB (6.5 GiB available) unless a scanner
already exists. It caps the scanner at 1 CPU with low CPU shares (tests cover 3, 4.72 and
6.4 GiB refused; 7 GiB proceeds). Safe capacity options, owner decision:

1. Add RAM, to at least 16 GiB.
2. Reclaim memory: about 5 idle `cfmeet-comm-stage-*` candidates still run with 768 MiB limits.
3. Share one host clamd across products.
4. Run the CF Meet scanner in `ConcurrentDatabaseReload no` mode after option 1 or 2.

Also recommended:

- Docker log rotation for CF Meet containers.
- Pruning the 45.6 GB of reclaimable images (disk at 84.7%).

### GO / NO-GO

| Operation | Decision | Basis |
|---|---|---|
| `messaging-configure` | **NO-GO** | Scanner headroom not demonstrated (4.72–5.0 GiB available vs 6.5 GiB required); the gate now refuses it. Push credentials absent. Attachment storage, Redis and metrics could be configured without the scanner, but that requires a scanner-optional configure path that does not exist yet; none is claimed |
| Next coordinated release (`communications-stage` then `communications-release`) | **CONDITIONAL GO, not yet** | The root cause is fixed in Calling code at `4ca0cb5`, so the `0a5d9dd` images cannot be released: they contain the defect. Required in order: (1) certification green on the two-key LiveKit; (2) a new immutable image set built from ≥ `4ca0cb5`; (3) isolated `communications-stage` PASS on those digests; (4) then one controlled `communications-release` with its automatic rollback. No retry-until-green |
| Production E2EE | **BLOCKED** | Issue 13 open; no independent assessment or owner approval |
| Phone OTP, off-server DR | **DEFERRED** | Owner decision |

## Gateway release and Messaging capacity recovery — 2026-11-05 (final phase)

### Outcome

**The corrected backend is DEPLOYED.** Production now runs the images built from `914c6c0`,
which contain the media-gateway refresh fix (`9f61237` + `4ca0cb5`) and the PSTN e2e ordering
fix. The release passed on its first and only attempt, with no rollback; independent
verification passed. **`messaging-configure` stays NO-GO**: measured headroom is 5.02 GiB
available against the 6.5 GiB gate. Production E2EE stays disabled, MANAGED is not used as a
replacement, and phone OTP and off-server disaster recovery remain DEFERRED.

### Commits

| Commit | Content |
|---|---|
| `9f61237`, `4ca0cb5` | Gateway: a refresh LiveKit signed with another API key no longer drops an admitted connection. The fallback credential carries the database-admitted permissions |
| `914c6c0` | **Image source.** Scoped `resource-recover`, bounded release logs (json-file 20 MB × 5 when unset), PSTN leg ordering in `communications_e2e` |
| `d980689`, later | Spare Meetings image rule, memory-by-owner inventory, this report. Infra and docs only: service code is identical to `914c6c0` (`git diff 914c6c0 -- services packages` is empty) |

### Gateway security review

- **Frame source:** refresh frames arrive only on the gateway's own upstream connection to
  LiveKit. Clients cannot inject downstream frames; client simulate requests are dropped.
- **Re-admission first:** every refresh re-runs the authoritative database admission
  (`admitted_permissions`: AUTHORIZED session, current generation, live call state,
  Communications policy for personal calls). Any refusal closes the connection.
- **Verified path:** a refresh verifiable with Calling's key is resealed exactly as before.
- **Fallback path:** an unverifiable refresh is only a signal. It must be HS256, LiveKit-issued
  (never edge-issued), unexpired, and bound to the same room, identity and current generation
  (stale generations are refused). The credential is minted from the connection with the
  database-admitted permissions. The token's own grants (`roomAdmin`, `canPublish`, …) are never
  copied, and the credential is join-only when nothing is admitted.
- **No escalation path:** an edge credential only ever yields room, identity and generation to
  the gateway. LiveKit receives a fresh 30 s join-only upstream token, and media rights are
  applied server-side from the database.
- **Revocation:** a removed session's saved refreshed token is still refused (401) through the
  gateway and directly at LiveKit, in all three certification rounds.

### PSTN ringing assertion (pre-existing defect, fixed)

Telephony's `accept_dcn_inbound` (`services/telephony/src/inbound.rs`) calls Calling's
`offer_dcn_inbound`, and Calling creates the call and rings the endpoint. Only after Calling's
response does Telephony insert the leg with `calling_call_id` and move it to RINGING, in one
transaction. The e2e test read the leg once, immediately after observing the invitation, and
got an empty row. It now waits with the suite's bounded `eventually()` helper and requires the
**identical** `RINGING/PSTN_TO_APP/<dcn>/<revision>/SYNTHETIC_TEST` value. Communications
end-to-end journeys passed on `914c6c0` on both main
([42020871918](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42020871918)) and
staging ([42020869086](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42020869086)).
Main had failed three times before. The Chromium web-client job of Unified Messaging remains
the separate known frontend failure (out of scope).

### Images (built by [42020907394](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42020907394) from `914c6c0216b18f16f8ca4e73f54935eea3a8dccf`)

| Service | Registry digest | Image ID on VPS |
|---|---|---|
| calling | `sha256:11a8d79a0669b9676309f3ad9a3395013f8433736178da5331e69323079493d6` | `sha256:822ed097af5f42fa34f0a4220720aa5ed868534ab28f56e06919769e1da95400` |
| messaging | `sha256:c4762273b871358a3969e1fca0a20f077f1829d4493039e48f61ef9c6d4cc682` | `sha256:d3d4bb186c4bea8c720d4785b677128f98ebc33f1d937d26e84493d2002a0bd2` |
| communications | `sha256:b517ca1fd08dd77443ab3d9f1550066325bbff82cbedda0f08cff548fa6b4410` | `sha256:b39b7908dbd199a97fff4669aeacf128efb8bfccb14fd75633311b04184c5626` |
| meeting-service | `sha256:5f4ff8ccdbf5bb6841b2e1f2430eeba9828aaf164119f07a075d3447a545fe1e` | `sha256:9cb00a9bd2281dc2b82434929d8b44fbbf517a1f99aacf0b5b91709d0989eec8` |
| platform-access | `sha256:a2e164489177f736446cbee25b01d638b7d7647f88f6d9caa64afd0b3ae657b4` | `sha256:ffa0920f5f0000a88bccaabebf6fcfecebd17abaf7240605dea0ab70434127e8` |
| api-gateway (built, not deployed) | `sha256:acbe2b046ab1552aee3f933ad4ffd045828503f94d5cf4684a24239928d6fffd` | — |

LiveKit is unchanged: v1.13.9, `sha256:d0c04791…`, two keys.

### CI for the released code

- **On the service code, at `4ca0cb5`** (the only service-tree change since then is the e2e
  test): Calling RTC certification on a two-key LiveKit (main and staging), Calling service,
  Enterprise calling certification, Telephony, Privacy Relay, Communications Hub, Part VI
  cross-service, and Dependency and image security all PASS.
- **On `914c6c0`:** Communications end-to-end (main and staging), Numbering service, Part VI
  cross-service, Dependency and image security, Compose config and the image build all PASS.

### Production deployment evidence

| Step | Run | Result |
|---|---|---|
| `communications-stage` on the exact digests (isolated databases, no published ports, production two-key LiveKit) | [42024930198](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42024930198) | **14/14 PASS**: native voice/video ringing, accept and **bidirectional real frames**; direct-conversation reuse and managed message (isolated TEST application only); contacts, block/unblock, ID rotation, Meetings available |
| **One** controlled `communications-release` | [42025871400](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42025871400) | **PASS, no rollback.** Communications 16/16 in `production`. Enterprise 21/21: voice/video real media, `real_media_reconnection`, hold/resume, camera denial, decline, cancel, missed history, group, IVR, `transferred_party_removed_from_media`, `internal_transfer_real_media`, extensions, ring groups, queues, tenant isolation, scope denial. **No media join or reconnect retries were needed.** One harness `GET /v1/calls/…` read timed out once and was recovered by the bounded read retry (writes are never replayed). Regression (Meetings health, TLS readiness, both LiveKit keys) PASS |
| Independent `communications-verify` | [42027660513](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42027660513) | **14/14 PASS** in production with new fixtures; running image IDs equal the accepted release |
| `messaging-inspect` | [42028131735](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42028131735) | Messaging image `d3d4bb18…`, source `914c6c0`; E2EE false; log rotation 20 MB × 5 applied by the release; dependencies unchanged (not configured); 0 devices, 0 conversations, 0 dead outbox |

Feature flags after the release are unchanged: app and new calls, enterprise, recording and
voicemail enabled; public inbound/outbound, callback relay and synthetic interconnect disabled.
The previous `787bb9a` containers are retained as `*-communications-rollback-42025871400` for
rollback.

Device revocation: Calling cross-endpoint credential denial and removed-session refusal pass
(enterprise acceptance and certification). Messaging device enrollment and revocation are not
production-testable because production E2EE devices stay disabled; they are covered by the
isolated Messaging suites only. **BLOCKED** for production, per the E2EE gate.

### Resource recovery ([42020871662](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42020871662), [42028326692](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42028326692))

Before acting, inventory [42020369672](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42020369672)
recorded the starting state.

**What changed:**

- **Stopped (retained, never removed):** the 5 `cfmeet-comm-stage-*` candidates, verified at
  run time: candidate label, no published ports, 0 active stage calls, 0 active stage database
  sessions. They were stopped again after the release's stage run restarted them.
- **Removed (never forced):** 14 CF Meet image tags.
  - 7 old `cfmeet-meeting-service:prod-*` builds.
  - Stale media/API probe images (rebuilt on demand from their content hash).
  - The unused 2.55 GiB `cfmeet-calling-web-probe:1.56.1`.

  Only CF Meet-owned images that were unused and unreferenced by any rollback journal, evidence
  record or env file. The first run also removed `prod-6c39351` (25 Oct), the newest *unused*
  Meetings build, which the rule intended to keep as a spare. Meetings rollback was unaffected
  (it uses retained rollback containers, whose images are preserved), and the rule is corrected
  in `d980689`.
- **Log rotation:** a validated `copytruncate` logrotate rule (100 MB × 5, compressed, no
  restarts) for the long-lived `lk-server`, `cfmeet-core-db`, `cfmeet-core-tls`,
  `cfmeet-calling-loopback`, `cfmeet-calling-redis` and `cfmeet-calling-artifacts`. Containers
  recreated by releases now get Docker json-file rotation.

**Kept:**

- All running production containers.
- 50 failed, 10 rollback and 32 retired containers as deployment evidence.
- Volumes, secrets and evidence files.
- Every other product's containers and images: 154 unused tagged and about 600 dangling images
  are not CF Meet's. Their roughly 45 GB reclaimable is an owner decision across products.

| Measurement | Before (inventory) | After cleanup | After release + cleanup |
|---|---|---|---|
| MemAvailable | 4.84 GiB | 4.99 GiB | **5.02 GiB** |
| Disk free | 29.52 GiB (84.7%) | 30.58 GiB (84.1%) | 30.09 GiB (84.4%) |
| CF Meet real container memory | — | — | 0.71 GiB |
| Other products' container memory | — | — | 4.51 GiB (ClamAV daemons 0.93 + 0.92 + 0.67 GiB) |
| Load average (6 vCPU) | 4.5–6.0 | 4.8–5.3 | 4.5–5.7 |
| Swap used | 0.3 GiB | 0.3 GiB | 0.3 GiB |

Stopping the stage candidates freed little memory because they were idle (their 768 MiB values
are limits, not reservations). CF Meet already uses under 1 GiB of real memory, so no further
CF Meet-side reallocation can create the missing headroom.

### Messaging capacity decision

Gate: scanner limit 3 GiB + coordinated-release margin 3 GiB + media reserve 0.5 GiB =
**6.5 GiB available**. Measured: 5.02 GiB after cleanup; 4.72 GiB under the post-release
workload and the 2-day sysstat minimum. **Deficit: 1.5–1.8 GiB.** One other product's ClamAV was
at 11.75% CPU during measurement (consistent with its own reload): overlapping reload peaks are
real.

Options considered, none of which changes another product's security boundary:

- **Low-memory scanner mode** (`ConcurrentDatabaseReload no`, 2 GiB limit). The requirement
  becomes 5.5 GiB, still above 5.02 GiB, so it is insufficient.
- **Sharing another product's clamd.** It crosses product isolation, so it is rejected.
- **Configuring storage, Redis and metrics without a scanner.** This needs a new scanner-optional
  configure path. MANAGED attachments would then stay VERIFYING (fail closed), so it is limited
  value. It is an owner decision and not implemented.

Configuration survival remains **TESTED** (13 persistence tests). This release preserved
Messaging's environment and mounts and added log rotation as designed.

**Decision: `messaging-configure` NO-GO.** Minimum additional RAM is **+2 GiB**: it covers the
1.8 GiB deficit at the measured minimum, with no margin for growth. **Recommended +4 GiB
(16 GiB total)**, giving about 9 GiB available, about 6 GiB remaining at a scanner reload peak,
and a coordinated release still possible. After an upgrade, rerun `resource-inventory`. Run
`messaging-configure` only when it reports at least 6.5 GiB available; the gate enforces this
anyway. Push providers remain BLOCKED on credentials; production E2EE remains BLOCKED (issue 13).

### Status summary

| Item | Status |
|---|---|
| Media-gateway refresh fix | IMPLEMENTED, TESTED, **DEPLOYED** (`914c6c0` images) |
| PSTN e2e ordering fix | IMPLEMENTED, TESTED |
| Transfer completion and reconnect acceptance | TESTED in production (release 42025871400) |
| Scoped resource recovery and log rotation | IMPLEMENTED, executed on the VPS |
| Messaging dependencies (storage, ClamAV, Redis, metrics) | BLOCKED: capacity (+2 GiB minimum, +4 GiB recommended) |
| Push providers | BLOCKED: credentials |
| Production PRIVATE_E2EE | BLOCKED: independent assessment and owner approval |
| Phone OTP, off-server DR | DEFERRED |
| Section 8 | **PARTIALLY COMPLETE.** Calling media restored and the backend released; Messaging infrastructure activation waits on capacity |


## Shared security scanning and Messaging infrastructure activation — 2026-11-05

The owner replaced the "+2 GiB RAM / dedicated ClamAV" requirement with a shared,
authenticated scanning gateway in front of an antivirus engine that already runs on the
VPS. The earlier rejection of "sharing another product's clamd" was about crossing product
isolation through a raw, unauthenticated port; the gateway keeps that isolation. Runbook:
[shared-security-scanning.md](shared-security-scanning.md).

### Scanner backend selection (read-only audit, runs 42077171046 and 42077986241)

| Engine | Class | Limit / peak | Memory events | Reload | Threads / stream | Signatures | Decision |
|---|---|---|---|---|---|---|---|
| `admin-server-mail-clamav` (1.4.6) | production mail | 3072 / 1846 MiB | 0 OOM | `ConcurrentDatabaseReload no` | 8 / 25M, `AlertExceedsMax yes` | daily 28173, 60 h | **Selected** |
| `admin-server-staging-mail-clamav` | staging | 3072 / 1846 MiB | 0 | no | 8 / 25M | 28173, 60 h | Excluded (staging) |
| `karrygo-media-scanner` (1.5.4) | production KarryGo | 1536 / 1536 MiB | 52,772 `memory.max` hits, 277 MiB swap | default | default | 28174, 11 h | Excluded (no headroom) |

Host during the audit: 5.33 GiB available of 11.68 GiB, swap 0.29 GiB, CPU pressure
`some` ≈ 9 % (300 s), memory pressure 0. Mail engine: 0.02 % CPU median, 951 MiB, benign
stdin scans 154–199 ms. LiveKit 54 MiB, Calling 13 MiB, Messaging 9 MiB.

Finding for the mail owner: its signatures are one daily version behind KarryGo's (28173
vs 28174) and its `freshclam.dat` is 156 h old, although `Checks 12`. If its database passes
72 h the gateway refuses to scan (`SIGNATURES_STALE`) and MANAGED attachments stay
`VERIFYING`. The threshold is not relaxed to keep scanning.

### Implementation

| Component | Status |
|---|---|
| `packages/scan-gateway-contract`: verdicts, reasons, `ScanResponse`, Ed25519 signed-verdict format and caller verification | IMPLEMENTED, TESTED (6 unit tests) |
| `services/security-scan-gateway`: authenticated single-use service tokens, per-product registry and limits, replay guard, bounded streaming `INSTREAM`, freshness gate, signed verdicts, private metrics, probe and keygen binaries | IMPLEMENTED, TESTED (4 unit + 16 integration tests; real-engine test in CI) |
| `cfmeet.security-scan` identity, `security-scan.submit` (Messaging only) | IMPLEMENTED, TESTED (service-auth suite, new least-privilege test) |
| Messaging `GatewayScanner` behind the existing `AttachmentScanner` (no new state machine) | IMPLEMENTED, TESTED (5 adapter tests; full MANAGED lifecycle through the real gateway: clean → READY and byte-exact download, EICAR → QUARANTINED; 23/23 attachment tests) |
| `scan-gateway-configure / verify / disable` operations | IMPLEMENTED, TESTED (7 unit tests) |
| `messaging-configure` refactor: scanner mode `gateway` or `quarantine`; no antivirus engine; memory gate 3.5 GiB instead of 6.5 GiB; stage candidates never get the gateway | IMPLEMENTED, TESTED (persistence suite, 77 tooling tests total) |
| PRIVATE_E2EE | Unchanged: ciphertext is never sent to a scanner; production E2EE stays disabled (issue 13) |

No public Messaging API changed. The gateway API is internal only and absent from consumer
Swagger. No frontend, mobile, Vercel or `frontend.yml` change.

### Deployment (production, 2026-11-05)

| Step | Run | Result |
|---|---|---|
| Image set `eeaf61a` (incl. gateway) | [42092034545](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42092034545) | built, revision-labelled |
| Gateway image `3637794` (probe refusal fix) | [42098873874](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42098873874) | built |
| `scan-gateway-configure` attempts | 42091033220, 42094833977, 42095995749 | FAILED and rolled back each time: probe missing from image; oversized body → 502 via nginx; direct oversized refusal seen as a closed upload. Each fixed in code, never by relaxing a check |
| `communications-stage` `eeaf61a` | [42097040034](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42097040034) | PASS |
| One controlled `communications-release` `eeaf61a` | [42098810080](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42098810080) | **PASS, no rollback**. Previous containers retained as `*-communications-rollback-42098810080` |
| `scan-gateway-configure` `3637794` | [42102547990](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42102547990) | **PASS 26/26**, regression PASS |
| `messaging-configure` | 42103449496 | FAILED in the storage probe (two URLs counted as grants); rolled back to the exact previous container and nginx |
| `messaging-configure` | [42104592956](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42104592956) | **PASS**: scanner mode `gateway` (active in the running binary), storage, Redis, metrics, keyring, KT checkpoint; regression PASS; rollback container `cfmeet-messaging-runtime-rollback-42104592956` |
| `communications-verify` | [42105498094](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42105498094) | PASS |
| `resource-inventory` | [42105503611](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42105503611) | PASS (stage candidates found running after the stage run; stopped by `resource-recover`) |
| `messaging-verify` | [42105526737](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42105526737) | **FAILED**: Swagger, admin/metrics not public PASS; public `POST /v1/realtime/tickets` 502 (see "Open defect") |
| `resource-recover` | [42114254870](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42114254870) | PASS: stage candidates restarted by the stage run stopped again (retained) |
| Independent `scan-gateway-verify` | [42113919022](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42113919022) | **PASS 26/26**, regression PASS |

Running images (`docker inspect` image IDs, from the verify receipts): messaging
`sha256:60608afbfc61…` (registry `ghcr.io/cosmicforgehealthnet/cfmeet-messaging@sha256:11f4aa7581bb40355263f84970830638de83ee25d57bfb8f7efad2b2482ca365`),
calling `sha256:4ca17362520f…`, communications `sha256:b90ada269a7b…`, meeting-service
`sha256:0be1b722742c…`, platform-access `sha256:d7189f63d6aa…`, security scan gateway
`sha256:3cc6f387f958…` (`cfmeet-security-scan-gateway:3637794dd997f02a8dca26f44b4b3f7af3d8b683`).
Verdict public key: `scan-verdict-20261105` / `1H7LXDzDHbeusoZ4etnNXOIPg9QrKKGm-Vanv2pjX-s`.

### Production acceptance of the shared scanner (42102547990, 42113919022)

Each run checks both paths (`cosmicforge-scan-gateway:8099` and Messaging's
`https://api.cfmeet.com:8449`), signing with Messaging's own service key.

| Check | Result |
|---|---|
| Unauthenticated, other-environment, replayed tokens | 401 ×3 per path |
| Clean fixture | CLEAN, signature and binding verified (request, caller, environment, digest, length) |
| EICAR | INFECTED `Eicar-Test-Signature`, signed and bound |
| Digest mismatch / empty / oversized (25 MiB + 1) | 422 / 422 / 413 (unsigned) |
| 10 sequential small scans | p50 10–25 ms, p95 12–72 ms, all CLEAN |
| Burst of 6 × 512 KiB in parallel | 6 CLEAN, 0 OVERLOADED, max 51–499 ms |
| 25 MiB − 1 KiB file | CLEAN in 314–366 ms |
| Engine port from CF Meet's network (`admin-server-mail-clamav:3310`, alias) | unreachable |
| Backend network members | `admin-server-mail-clamav`, `cosmicforge-scan-gateway` only |
| Gateway published ports | none |

Resources measured around each probe run: gateway peak **8 MiB** of its 64 MiB limit
(1 MiB idle), 0.4–0.6 CPU-seconds. Mail engine memory 949–951 MiB before and after
(limit 3072, peak 1849), **0 OOM kills**, healthy; 1.0–1.4 engine CPU-seconds. LiveKit
0.08–0.26 CPU-seconds in the same window. Host MemAvailable 4.95–5.09 GiB, swap
unchanged at 0.29 GiB. **Additional host memory reserved: none**: the dedicated 3 GiB
daemon and the 6.5 GiB gate are gone.

Signature freshness: the mail engine moved from daily 28173 (60.7 h) to **28174** (13 h)
during the session. The earlier stale-signature concern is resolved for now. The
gateway still enforces 72 h.

Failure and rollback evidence: three gateway configure failures and one Messaging configure
failure in production each rolled back automatically, leaving the previous state.
Messaging returned to its exact previous container and nginx. The CI suites exercise engine
outage, stale signatures, timeouts, overload, malformed replies, truncation and replay.

### Messaging attachment lifecycle

- CI ([Messaging 42085103681](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42085103681),
  `MESSAGING_SCAN_GATEWAY_REQUIRED=1`): the real gateway in front of the real ClamAV
  engine. A clean MANAGED file goes READY and downloads byte-exact; EICAR goes QUARANTINED
  and is never downloadable; an unavailable scanner gives VERIFYING. 23/23 attachment
  tests.
- Production: storage probe PASS (encrypted upload/download, private bucket, unsigned
  denied, signed grant works, expired grant denied, Calling bucket denied, probe object
  deleted). The scanner is active in the running binary.
- A production MANAGED attachment through the public API was **not** run. Personal MANAGED
  conversations are deliberately refused (422, "managed not activated as personal E2EE
  substitute"), and no isolated production application fixture is attached to Messaging
  attachments. It is not claimed.

### Open defect: public realtime route

`messaging-configure` activated the existing public realtime ingress (`/v1/realtime`,
`/v1/realtime/tickets` → private proxy `:8444`). It answers **502**: host nginx 1.24
(OpenSSL 3.0.13) rejects the private proxy's Let's Encrypt Generation Y chain (leaf, YE1,
Root YE cross-signed by X2, X2 cross-signed by X1) with "unable to get local issuer
certificate".

Evidence, all from `messaging-ingress` diagnostics (42108774026 … 42113422356):

- `openssl s_client` with the bundle alone, and with the directory, returns code 0.
- `curl` from the host returns 401.
- A loopback nginx using the same binary and the same directives returns 401.
- The live `nginx -T` shows the expected directives (depth 5, system bundle); workers
  reload; nothing unusual is inherited.

Root cause was not identified in that session. It is identified and repaired below
("Public realtime ingress repair").

## Public realtime ingress repair — 2026-11-05 (closes the open defect)

### Outcome

`POST /v1/realtime/tickets` and `GET /v1/realtime` now reach Messaging over verified TLS.
From the public internet, an unauthenticated ticket request gets Messaging's own **401**,
not 502. Certificate, hostname and SNI verification are unchanged, and depth is now set
explicitly. No public port was opened and no service image was redeployed.
`messaging-verify`, `scan-gateway-verify` and `communications-verify` all pass.

### Root cause (two defects, the second hid the fix for the first)

1. **Insufficient verify depth.** The routes installed by `messaging-configure` 42104592956
   (`cf4dd69`) had no `proxy_ssl_verify_depth`, so nginx used its default of 1. The private
   proxy presents leaf → YE1 → Root YE (cross-signed by X2) → X2 (cross-signed by X1). The
   trust anchor, ISRG Root X2 in the system bundle, sits two intermediates above the leaf.
   Reproduced with the host's own nginx 1.24 / OpenSSL 3.0.13 on loopback, using the live
   directives: default depth and depth 1 give **502** with `(20:unable to get local issuer
   certificate)`, the exact live error. Depths 2, 3, 4 and 5 give **401**. `openssl
   s_client -verify_depth` has the same boundary (1 fails, 2 passes).
2. **Reload race in the deployment code.** `systemctl reload nginx` only signals the master.
   Each repair attempt (`messaging-ingress` 42108003678 … 42113422356) probed the route right
   after the reload, while the previous worker generation was still accepting requests with
   the old configuration. The probe then failed and the route was rolled back, so the
   corrected depth was never live afterwards. Evidence from the host error log: every run
   logged exactly one 502 in the same second as its install reload, followed 2–5 s later by
   the rollback reload. In run 42113422356 the failing worker (PID 1920696) had been started
   at 22:28:26 by the *previous* run's rollback. This is why the loopback nginx, `curl` and
   `s_client` all succeeded while "live nginx" kept failing. The earlier code comment
   saying depth 3 fails was a product of this race and has been corrected.

Ruled out: the systemd environment (no `Environment=` or drop-ins), alternate masters
(port 443 is owned only by the systemd master; the other nginx masters are containers),
trust-store content (the bundle alone verifies), SNI and routing (the error log shows the
expected upstream).

### Correction (commit `1f1e213`)

| Change | Where |
|---|---|
| `reload_host_nginx()`: `nginx -t`, reload, then return only once none of the pre-reload workers still accepts connections (old workers retitle themselves "is shutting down"). Bounded at 60 s, fails closed. | `production.py` |
| Realtime routes: `proxy_ssl_verify on`, `proxy_ssl_verify_depth 5` (measured minimum 2), `proxy_ssl_name api.cfmeet.com`, `proxy_ssl_server_name on`, system bundle; no `proxy_ssl_verify off` anywhere | `messaging_runtime.nginx_config` |
| No Docker address in the realtime routes: host nginx connects to `https://127.0.0.1:18444`, a loopback-only **TCP relay** (`cfmeet-messaging-ingress`, nginx `stream`) that resolves `cfmeet-core-tls:8444` through Docker DNS on each connection. The relay never terminates TLS, so host nginx still verifies the private proxy's own chain and name. It reuses the private proxy's own nginx image (nothing pulled); read-only root, all capabilities dropped, no-new-privileges, UID 101, 32 MiB, 32 PIDs, 0.25 CPU, rotated logs, `unless-stopped`; networks `cfmeet-messaging-ingress-net` (bridge for the loopback port) and `cfmeet-core-private` | `messaging_runtime.ensure_relay` |
| Pre-check through the relay with nginx's exact name, SNI, depth and trust file before any host change | `messaging_runtime.verifies` |
| Probe checks Messaging's own refusals (error envelope, realm header, and the `private, no-store` header only this location adds) for tickets and WebSocket upgrades | `messaging_runtime.ingress_probe` |
| Exact rollback of site, routes and relay when the install, the probe or the regression fails; `messaging-configure` uses the same path | `restore_ingress`, `restore_relay`, `_configure` |
| Relay listed as long-lived infrastructure | `resource_recovery.LONG_LIVED` |

The attachment route (`/cfmeet-messaging-attachments/` → MinIO) is unchanged. It still
renders the storage container's address at install time; see "Remaining risks".

### Tests and CI

| Run | Result |
|---|---|
| Tooling unit tests (106, incl. 27 new in `test_messaging_ingress.py`) | PASS on every run below |
| [42122365294](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42122365294) `Messaging realtime ingress` (branch), plus 42122587267 (`main`) and 42122586290 (`staging`) | **PASS** |
| Compose config 42122587766 (`main`) | PASS |

`infra/calling/ingress-ci/realtime_ingress.py` runs the deployment functions against
Ubuntu 24.04's nginx 1.24.0 / OpenSSL 3.0.13 (the production build) as host nginx and the
private proxy's image by digest (`nginx@sha256:0e52bd05…`) with its 8444 server block.
The certificate chain has the Generation Y shape. A CI stand-in serves Messaging's realtime
contract (status codes, envelope, single-use tickets, WebSocket upgrade).

| CI check | Result |
|---|---|
| Chain needs depth 2 (`verifies` at depth 1 fails), like production | PASS |
| Previously live directives (default depth) | **502** + "upstream SSL certificate verify error" (production failure reproduced) |
| Rendered configuration: full `ingress_probe` | PASS |
| Valid client token → ticket 201 → WebSocket **101**, subprotocol and accept key verified, one frame round-tripped | PASS |
| Ticket replay | 401 `INVALID_TICKET` |
| Flip bad ↔ good config 3× each way with `reload_host_nginx`, request immediately after each | always the new behaviour |
| Wrong `proxy_ssl_name`; untrusted root | 502 / 502 |
| Private proxy recreated with a **different Docker address** | route recovers by itself; host nginx master, relay and routes untouched |
| Relay confinement, loopback-only port, no certificate or key in the relay, reuse without restart, rollback removal | PASS |

The production workflow now runs this test before every `messaging-ingress` and
`messaging-configure`.

### Production deployment and verification

| Step | Run | Result |
|---|---|---|
| Baseline | read-only | public ticket route 502; live routes without depth; no relay; port 18444 free; 5.2 GiB available |
| `messaging-ingress` (`1f1e213`) | [42122645079](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42122645079) | **PASS**: real-nginx gate PASS; relay created; trust `system-bundle`; probes below; regression PASS. No rollback needed |
| Independent `messaging-verify` | [42123078284](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42123078284) | **PASS 15/15**, incl. `public_realtime_routes_present_and_unauthenticated_access_denied`; activation verdict `BLOCKED_EXTERNAL_DEPENDENCY` (E2EE) |
| Independent `scan-gateway-verify` | [42123256240](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42123256240) | **PASS** 24 checks, 0 failures; engine healthy, 949 MiB, 0 OOM; gateway publishes no ports |
| Calling/Meetings `communications-verify` | [42123390637](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42123390637) | **PASS 14/14**, incl. native voice/video ringing with bidirectional media frames and `meetings_remain_independently_available`; regression PASS |

Production probe (deploy run) and an independent repeat **from outside the VPS**:

| Request | Answer |
|---|---|
| `POST /v1/realtime/tickets`, no credential (HTTP/1.1 and HTTP/2) | **401** `AUTHENTICATION_FAILED`, `WWW-Authenticate: Bearer realm="cf-meet-messaging"` |
| Same with a forged `CF-Client-Token` | 401 `INVALID_CLIENT_TOKEN` |
| Same with a refused `Origin` | 401 `ORIGIN_NOT_ALLOWED` |
| Preflight from a refused origin | 403, no `Access-Control-Allow-Origin` |
| Other methods (`DELETE`/`GET` tickets, `POST /v1/realtime`) | 403 (nginx `limit_except`) |
| `wss` upgrade, subprotocol, no ticket | 401 `TICKET_REQUIRED` |
| `wss` upgrade, well-formed unknown ticket | 401 `INVALID_TICKET` (Messaging's ticket store lookup) |
| `wss` upgrade without subprotocol | 400 `SUBPROTOCOL_REQUIRED` |
| TCP 18444 and 8444 on the public address | not reachable |

Deployed state: relay running at 1.9 MiB; both realtime routes `proxy_pass
https://127.0.0.1:18444` with verify on, depth 5, the api.cfmeet.com name and SNI; six
current workers and no old generation; site file byte-identical (include already
present); no upstream TLS errors after the install. Rollback material:
`/opt/cfmeet-core/messaging/nginx-{site,snippet}-before-42122645079`. Running images are
unchanged from the release (messaging `sha256:60608afbfc61…`, calling `4ca17362520f…`,
communications `b90ada269a7b…`, meeting-service `0be1b722742c…`, platform-access
`d7189f63d6aa…`, gateway `3cc6f387f958…`). Production E2EE is still disabled (0 devices,
0 conversations).

**Not run in production:** valid ticket issuance, a WebSocket 101 upgrade and session
behaviour. A client token requires an enrolled device of the acting principal
(`require_own_device`), and production device enrollment is refused by the E2EE guard
(issue 13). These paths are proven through the exact ingress configuration in CI, and
Messaging's own session suites cover ticket and session semantics.

### Remaining risks

- **Private proxy certificate is pinned to `archive/api.cfmeet.com/fullchain4.pem`**
  (expires 2026-12-21), not certbot's `live/` link. After certbot renews (around
  2026-11-21) the private proxy keeps presenting the old certificate. All private TLS
  routes, including this one, fail closed when it expires unless the mount is moved to the
  new version. This belongs to the TLS proxy release tooling and is outside this change.
- The attachment route and the Calling artifacts route still render MinIO's Docker
  address at install time. If MinIO restarts with a different address they fail closed
  until the relevant configure operation is re-run.
- The relay depends on the private proxy keeping the container name `cfmeet-core-tls`.

### Final status

| Item | Status |
|---|---|
| Shared security scan gateway (auth, isolation, limits, streaming, signed verdicts, metrics) | IMPLEMENTED, TESTED, **DEPLOYED, VERIFIED** |
| Scanner backend: existing production mail ClamAV through a dedicated internal network | DEPLOYED, VERIFIED (owner unaffected: healthy, 0 OOM, memory unchanged) |
| No new antivirus engine, no host memory reservation | VERIFIED |
| Messaging `GatewayScanner` (signed CLEAN only) | IMPLEMENTED, TESTED, **DEPLOYED** (release 42098810080) |
| Messaging attachment storage, Redis realtime broker/presence, private metrics, managed keyring, KT checkpoint | **DEPLOYED, VERIFIED** (messaging-configure 42104592956) |
| Managed attachment lifecycle through the real gateway | TESTED (CI); production API journey not run (no isolated managed fixture) |
| Public realtime route | **DEPLOYED, VERIFIED** (messaging-ingress 42122645079; messaging-verify 42123078284). Valid-ticket WebSocket upgrade proven in CI only (production enrollment blocked by issue 13) |
| Calling / Meetings / LiveKit | regression PASS after every change; release acceptance PASS |
| Production PRIVATE_E2EE | **BLOCKED_EXTERNAL** (issue 13); devices disabled |
| Push providers | **BLOCKED_EXTERNAL** (no credentials) |
| Phone OTP, phone matching, carrier calling, off-server DR | **DEFERRED** |

## Remaining risks closed — 2026-11-06

The three remaining risks above are resolved in production. Full evidence is in [cfmeet-sections-8-23-final-closure.md](cfmeet-sections-8-23-final-closure.md) section 8.

| Former risk | Resolution | Status |
|---|---|---|
| Private proxy pinned to `archive/…/fullchain4.pem` | Read-only `live/` and `archive/` directory mounts ([42185815717](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42185815717)). A certbot deploy hook activates renewals, a daily monitor runs (warn at 45/30 days, alert at 14/7), and operators are notified on issue #38 ([42188709330](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42188709330), [42189603354](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42189603354)). Runbook: [tls-certificate-automation.md](tls-certificate-automation.md) | PRODUCTION_VERIFIED |
| Storage routes rendered MinIO's Docker address | Loopback relay `cfmeet-storage-ingress` on `127.0.0.1:19000`, which resolves the store by name ([42186633760](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42186633760)) | PRODUCTION_VERIFIED |
| Relay depends on the `cfmeet-core-tls` name | The durable switch keeps the name through an alias swap; the CI proof covers container restart and recovery of a stopped proxy | ENGINEERING_CLOSED |

Latest `messaging-verify`: [42193577972](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42193577972), 15 of 15 PASS. Activation verdict `BLOCKED_EXTERNAL_DEPENDENCY`. Latest `scan-gateway-verify`: [42192822658](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/42192822658), PASS.

Current status:

- Production PRIVATE_E2EE: still BLOCKED_EXTERNAL (#13).
- Push providers: still BLOCKED_EXTERNAL.
- Phone and off-server DR: still DEFERRED_BY_OWNER.
