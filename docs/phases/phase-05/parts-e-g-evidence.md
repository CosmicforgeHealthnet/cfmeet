# Part V Parts E, F & G — implementation evidence register

Directive: *CF Meet — Part V Full Implementation Directive: Parts E, F & G* (specification
Version 2.0, 18 October 2026). Branch: `staging` only (no other remote branch created,
nothing merged into `main`, nothing deployed, no history rewritten). Decisions:
[ADR 0049](../../decisions/0049-messaging-realtime-gateway.md),
[ADR 0050](../../decisions/0050-messaging-attachments.md),
[ADR 0051](../../decisions/0051-messaging-search-moderation-compliance.md); security
architecture, AD register and the §96 audit:
[`messaging-security-architecture.md`](../../architecture/messaging-security-architecture.md).

**Every AD-01…AD-10 decision remains PENDING. Production E2EE stays disabled**
(`MESSAGING_E2EE_DEVICES_ENABLED` defaults off outside local / test). A push to
`staging` deploys nothing (`deploy-backend.yml` deploys only from `main`).

## 1. Commits on `staging`

| Commit | Scope |
|---|---|
| `e23017b` | messaging-crypto: public MLS framing inspection, client group operations, E2EE attachment encryption, device-local search index |
| `61190a6` | Milestone 0 (prerequisites absent on `staging`): membership, message store, MLS Delivery Service, durable per-conversation event log, blocks, abuse controls, policy v3 |
| `04dbcfb` | Part E — realtime gateway |
| `06df918` | Part F — attachments, encrypted media, scanning, quotas, legal holds |
| `1f02edc` | Part G — search, reports, moderation, retention / purge, exports, data deletion, regulated contexts, audit API; real-binary realtime certification step |
| this register's commit | cross-target build fix of the attachment content id (`uuid` builder from OS randomness), contract-validator caching in tests, load measurements, this register |

## 2. Delivery by part

### Part E — Realtime messaging (§58–§68)

| § | Requirement | Delivered | Evidence |
|---|---|---|---|
| 58–59 | Authenticated gateway, short-lived client credentials | Messaging Client Access Tokens (EdDSA JWS, bound to tenant / app / env / principal / device / session); single-use seconds-long tickets in `Sec-WebSocket-Protocol`; TLS / Origin / limits / re-validation | `tests/realtime.rs`, `platform_access_e2e` step 9b (real binary) |
| 60 | Subscriptions with dynamic authorization | ACTIVE membership at subscribe; revoked on removal right after `member.removed` | `removed_members_stop_receiving_while_messages_are_flowing` |
| 61 | Durable vs ephemeral | Durable events only from the committed outbox; typing / presence ephemeral | `committed_messages_survive_a_broker_outage` |
| 62 | Typing | Coalesced, throttled, TTL, cleared on disconnect, never persisted | `typing_is_ephemeral_…` |
| 63 | Presence privacy | HIDDEN = OFFLINE, co-member only, opt-in last seen | `presence_respects_hidden_users_…` |
| 64 | Reconnect / replay | Resume from ack, gap fill in canonical order, `sync.required` beyond the window | `offline_devices_replay_…` |
| 65 | Backpressure | Bounded queues; resumable 1013 close with cursors | `a_slow_consumer_is_disconnected_resumably_…` |
| 66 | Multi-instance fan-out | Redis pub/sub pointers + presence; database catch-up | `tests/realtime_redis.rs` (real Redis, two instances) |
| 67 | Ordering | Per-conversation event cursor; duplicates dropped | `concurrent_senders_are_delivered_gap_free_in_one_order` |
| 68 | Mobile push boundary | Registrations + metadata-only leased jobs (DB CHECK); **no APNs / FCM adapter**: jobs end `NOT_CONFIGURED` | `push_payloads_are_metadata_only_…` |

### Part F — Attachments (§69–§80)

| § | Requirement | Delivered | Evidence |
|---|---|---|---|
| 69 | Reuse object storage | `packages/object-storage` (signed PUT bound to size / SHA-256 / type, signed GET with forced headers, list) | `s3_live` + `real_object_storage_end_to_end` (real MinIO, forged upload refused) |
| 70 | E2EE client encryption | `cf_messaging_crypto::attachment` (XChaCha20-Poly1305 STREAM); key only in the MLS descriptor | `private_e2ee_attachment_round_trip_with_real_client_encryption` |
| 71 | Upload intent / finalization | Server keys, state machine with trigger, re-verification of size and digest | `managed_attachment_lifecycle_…`, `integrity_and_content_type_security` |
| 72 | Self-hosted scanning, fail closed, no E2EE claim | ClamAV `INSTREAM`; `NOT_APPLICABLE_ENCRYPTED` (CHECK) | `scanning_fails_closed_and_quarantines`, `real_clamav_detects_eicar` (real clamd) |
| 73–75 | Images / files / voice notes / video | Per-kind allow-lists, magic bytes, active-content refusal, declared (unverified) durations, waveform | `integrity_and_content_type_security` |
| 76 | Authorized short-lived downloads | Grants re-authorized each time; proxied read with `nosniff`, `no-store`, CSP; removed members refused | `managed_attachment_lifecycle_…` |
| 77 | Retention / hold-aware deletion | Deletion queue with `HELD`, dead-letter, orphan reconciliation | `legal_holds_preserve_objects_…`, `orphaned_objects_are_reconciled` |
| 78 | Quotas | Atomic reservations (budget, concurrency, daily volume) | `quotas_are_atomic_and_released` (20 concurrent → exactly 3) |
| 79 | Content-type security | Allow-lists, double extensions, sanitized names, forced download | as above |
| 80 | Future media processing | `attachments::processing` contract only (no transcoding implemented) | — |

### Part G — Search, moderation, compliance (§81–§96)

| § | Requirement | Delivered | Evidence |
|---|---|---|---|
| 81 | E2EE local search | `cf_messaging_crypto::local_search` (sealed device index); server refuses `LOCAL_SEARCH_ONLY` | crypto unit tests, `managed_search_is_membership_aware_…` |
| 82–83 | MANAGED server search | PostgreSQL FTS, membership / window / delete-for-me aware, bounded, rate-limited | same |
| 84 | Permalinks | Resolution re-checks membership every time | `tests/messaging.rs` |
| 85 | Reports | Voluntary sealed E2EE disclosure, `REPORTER_ATTESTED_UNVERIFIABLE`; MANAGED references policy-gated | `e2ee_reports_carry_only_voluntary_disclosure_…` |
| 86 | Moderation actions | Remove / mute / unmute / remove member / restrict / lift / dismiss; append-only (trigger) + audit | `moderation_mutes_restricts_and_respects_policy` |
| 87 | Blocks | Pair-locked blocks (Milestone 0) | `tests/messaging.rs` |
| 88 | Abuse controls | Velocity limits, abuse signals, automatic restriction (Milestone 0), report / upload / search limits | `tests/messaging.rs`, `tests/attachments.rs` |
| 89–90 | Retention / purge | Policy expiry, irreversible purge (trigger), attachments deleted | `retention_expires_and_purges_but_legal_holds_preserve` |
| 91 | Legal holds | Conversation / principal / environment; administration-only, audited, never decrypting | `tests/attachments.rs`, `tests/compliance.rs` |
| 92 | Regulated profiles | `managedDisclosureAcknowledged`; `centralArchiveRequired` ⇒ MANAGED only | `regulated_contexts_require_disclosure_…` |
| 93 | Exports | MANAGED-only (`E2EE_EXPORT_NOT_AVAILABLE`), leased worker, grants, expiry, audit | `exports_are_managed_only_authorized_and_expire` |
| 94 | Data deletion | Principal erasure / conversation purge, holds win (`PARTIALLY_HELD`) | `data_deletion_requests_respect_holds` |
| 95 | Admin audit | `GET /v1/messaging/audit` filters / paging / tenant scope | `regulated_contexts_require_disclosure_and_audit_is_queryable` |
| 96 | Final security audit | §6 of the security architecture document | — |

## 3. APIs (contract `cfmeet.messaging.http-api` 1.6.0; every route in the served OpenAPI, parity tested both ways)

| Area | Routes | Authentication / scope |
|---|---|---|
| Realtime | `POST /v1/messaging/client-tokens`, `POST /v1/messaging/client-sessions/{id}/revoke`, `GET`/`PUT /v1/messaging/presence/settings`, `PUT`/`DELETE /v1/messaging/devices/{id}/push-registration` | API credential + acting principal; `messages.read` / `messages.send` |
| Realtime (device-direct) | `POST /v1/realtime/tickets`, `GET /v1/realtime` | client token / single-use ticket |
| Attachments | `POST …/attachments`, `POST …/{id}/finalize`, `GET …/{id}`, `POST …/{id}/download-grants`, `GET …/{id}/content` | `attachments.write` / `attachments.read`, acting member |
| Search | `GET /v1/conversations/{id}/search`, `GET /v1/messaging/search` | `messages.read`, acting principal |
| Reports | `POST /v1/conversations/{id}/reports` | `messages.send`, acting member |
| Moderation | `GET /v1/messaging/reports`, `GET …/reports/{id}`, `GET`/`POST /v1/messaging/moderation/actions` | `moderation.manage`, administration only |
| Compliance | `…/legal-holds` (list / create / get / release), `…/deletion-requests` (create / get), `GET /v1/messaging/audit` | `conversations.manage`, administration only |
| Exports | `POST /v1/messaging/exports`, `GET …/{id}`, `POST …/{id}/download-grants` | `messaging.export`, administration only |

Tenant isolation: every query is scoped by tenant / application / environment from the
credential; foreign resources are `NOT_FOUND` (tested for attachments, reports, holds,
exports, deletion requests, audit and search).

## 4. Database (migrations 3–7, `messaging_` tables only, no secret / plaintext column names)

| Migration | State |
|---|---|
| `m20261105_000003_membership_messages` | member lifecycle, messages (CHECK: profile-consistent content, purge irreversible), MLS groups / Welcomes / pending removals, ack / read cursors, hidden markers, blocks, restrictions, abuse signals |
| `m20261106_000004_realtime` | client sessions, tickets (hash only), presence settings / last seen, push registrations / jobs (metadata-only CHECK) |
| `m20261107_000005_attachments` | attachments (profile CHECK, key format, terminal-state trigger), storage usage, upload days, object-deletion queue |
| `m20261108_000006_legal_holds` | legal holds (never deleted; release-only trigger) |
| `m20261109_000007_moderation_compliance` | reports (sealed evidence, profile-consistent evidence CHECK), moderation actions (append-only trigger), exports, deletion requests |

CI migration smoke runs up / reset / up and the per-service role isolation check.

## 5. Testing

| Gate | Result (local, PostgreSQL 16; CI run on `1f02edc`) |
|---|---|
| `messaging` service suites | architecture 6, attachments 10, compliance 7, conversations 15, devices 11, foundation 7, messaging 10, migrations 3, outbox 3, realtime 14, realtime_redis 1 (real Redis), startup 3, transparency 4, platform_access_e2e 1 (real Platform Access + real Messaging binaries), 38 unit — all pass locally and in CI |
| Real infrastructure | MinIO (`pgsty/minio` pinned digest): forged upload refused by the store, signed GET with forced download, unsigned GET refused; clamd 1.x with an EICAR test signature: EICAR quarantined, clean text passes |
| messaging-crypto | 16 unit + 7 OpenMLS integration (+ RFC 9420 vectors in CI); Android / iOS / wasm32 compile-only |
| object-storage | unit + `s3_live` against MinIO |
| Contracts | validator: 53 contracts OK (`cfmeet.messaging.common` / `http-api` 1.6.0, events 1.3.0, realtime protocol 1.0.0) |
| Guards | service boundaries, operations registry (10 Messaging responsibilities), secret scan, licence review, backend crypto graph — pass |
| CI on `1f02edc` | every Messaging job green except `crypto core` (Android / iOS / wasm `cargo check`: `Uuid::new_v4` needs a feature those builds do not enable) — fixed in this register's commit and re-verified locally for all three targets |

Not observed: one local full-suite run hung inside `tests/realtime.rs` while the
machine was also compiling (killed after 10 minutes); 6 subsequent full runs and every
test individually passed, and CI passed. Recorded as an unexplained, unreproduced event.

## 6. Load and resilience measurements (`tests/load.rs`, real measurements)

Machine: 4 vCPU Intel Xeon @ 2.10 GHz, 15 GiB RAM, PostgreSQL 16.15 on the same host,
release build, in-process HTTP router (no network hop for REST), real TCP WebSockets,
one run each. These numbers describe this machine only; they are not production capacity
claims.

| Measurement | Result |
|---|---|
| MANAGED send, 20 concurrent senders in ONE conversation, 1 000 messages | 215 msg/s; latency p50 89 ms, p95 121 ms, p99 177 ms (per-conversation ordering lock) |
| MANAGED send, 20 independent conversations, 1 000 messages | 351 msg/s; p50 54 ms, p95 72 ms, p99 110 ms |
| PRIVATE_E2EE send with real client MLS encryption, 500 messages (sequential) | 86 msg/s; p50 10.3 ms, p95 19.0 ms, p99 26.4 ms; receiving device fetched + decrypted all 500 in 0.09 s |
| Realtime fan-out, 1 sender → 50 subscribed WebSockets, 100 messages (5 000 deliveries, in order, no duplicates) | send-to-receive p50 35 ms, p95 52 ms, p99 65 ms, max 73 ms |
| MANAGED search over 5 000 messages, 60 queries | p50 6.3 ms, p95 9.7 ms, p99 10.7 ms |

Resilience is exercised functionally (not timed): broker outage with database catch-up,
slow-consumer close and resume, draining, Redis-backed two-instance fan-out, scanner
outage (VERIFYING → retry → READY / REJECTED), storage outage during finalization, held
deletions resuming after release, leased workers.

During measurement a 65-second fan-out result was traced to the TEST CLIENT re-compiling
every JSON schema per received frame; the validators are now cached (server unchanged).

## 7. Security (§96)

See `docs/architecture/messaging-security-architecture.md` §6 for the restriction-by-
restriction enforcement table and residual risks. Summary: no plaintext substitute for
E2EE, no server-side E2EE keys or decryption path, no E2EE scanning or scan claims, no
E2EE search terms on the server, no E2EE exports, holds never decrypt, private bucket
with short-lived re-authorized grants, append-only moderation and audit, no gate
disabled, no equivalence claim with any other messenger.

## 8. Blockers and open items

| Item | Status |
|---|---|
| AD-01…AD-10 owner decisions | PENDING — production E2EE disabled |
| Independent cryptographic / security assessment (AD-10) | NOT STARTED |
| APNs / FCM / Web Push adapters | NOT IMPLEMENTED (jobs end `NOT_CONFIGURED`) |
| Media processing (thumbnails, transcoding, duration verification) | Contract only |
| Key-transparency witness / auditor | Open (heads `UNWITNESSED`) |
| Mobile / browser runtime certification | Compile-only |
| ClamAV signature coverage | Deployment-dependent (CI loads only EICAR) |
| E2EE history for new devices (backup / transfer, AD-07) | Not implemented |
| Production deployment | Not performed (out of scope) |

## 9. Final declaration

| Area | Declaration |
|---|---|
| Part E — Realtime messaging | **PASS** (software, tested incl. real Redis and real binaries); push delivery **PARTIAL** (no provider adapters) |
| Part F — Attachments | **PASS** (tested incl. real MinIO and real clamd); media processing **PARTIAL** (contract only) |
| Part G — Search, moderation, compliance | **PASS** |
| APIs / OpenAPI / tenant isolation | **PASS** |
| CI extended, no gate weakened | **PASS** (pending the re-run for this register's commit) |
| Load / resilience evidence | **PASS** (real measurements on one machine; no capacity claim) |
| External security certification | **BLOCKED** (assessment not started) |
| Production E2EE | **BLOCKED** (AD-01…AD-10 pending) |
| **Production readiness** | **NOT APPROVED** |
