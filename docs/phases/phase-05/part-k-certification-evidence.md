# CF Meet Part V — Part K (§§146–160): certification evidence

**Status:** SOFTWARE CERTIFICATION EVIDENCE on `staging`. This is not a production pass and
not a release authorization. `main` is unchanged (`689728c`). Nothing was deployed. No
carrier route, paid telecom action, production E2EE or external integration was turned on.
**Prior evidence:** [Parts H–J](parts-h-j-evidence.md), [Parts E–G](parts-e-g-evidence.md),
[Workstreams A–D](workstreams-a-d-evidence.md).

## 1. Git

| | SHA |
|---|---|
| Starting baseline (`origin/staging`) | `7847c3f` |
| Scenarios 4 + 5, real processes | `b5034c0` |
| Scenario 9 + CI job | `9b87338` |
| Dependency audit coverage, SBOM, Meeting Service patch updates | `295aa87` |
| Scenario 4 test-timing fix (found by CI on `295aa87`) | `5269b89` |
| This evidence record | the commit that adds this file (documentation only) |
| `main` | `689728c`, unchanged; nothing was merged or pushed to it |

## 2. The three mandatory integration gaps, now closed

All three run in `services/messaging/tests/cross_service_e2e.rs`. Each service is a
**real binary** with its **own PostgreSQL database**, migrated by **its own migrations**. The
services talk only over signed internal HTTP, and each receiver's trust bundle grants exactly
the permission the integration needs. The test reads each database only to collect evidence.
CI job: *Unified Messaging integration journeys → "Privacy Relay / SMS Gateway / Calling ↔
Messaging (real processes)"*, with `CROSS_SERVICE_E2E_REQUIRED=1`, so a missing binary fails
the job instead of skipping it.

| # | Scenario | Real processes | Result (local + CI `9b87338`) |
|---|---|---|---|
| 4 | Privacy Relay revocation | Platform Access, **Privacy Relay**, **Messaging** | **PASS** |
| 5 | Managed SMS Bridge | Platform Access, **Messaging**, **SMS Gateway** (deterministic synthetic carrier) | **PASS** |
| 9 | Webhook privacy | Platform Access, **Messaging**, **Calling** (+ local customer receiver) | **PASS** |

CI: <https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39304680035>, job
`118238550796`: `test result: ok. 3 passed; 0 failed; 0 ignored`.

**Scenario 4 asserts:**
* Before Privacy Relay speaks, a conversation cannot claim the context
  (`CONTEXT_AUTHORITY_UNKNOWN`).
* Privacy Relay's own worker pushes ACTIVE, and the conversation can then be created. CI took
  313 ms to propagate; the local run took 382–453 ms.
* **Suspend** locks the conversation and sending is refused. **Resume** unlocks it.
* **Revoke** closes the conversation (CI 142 ms; local 273–355 ms). It cannot be reopened,
  and the context cannot be claimed again (`CONTEXT_AUTHORITY_WITHDRAWN`).
* All 4 versions are applied exactly once (Privacy Relay jobs `APPLIED`; Messaging authority
  is `REVOKED:4`).
* Another tenant can neither claim the context nor read the conversation.
* **Messaging outage:** the revocation is committed in Privacy Relay. The job stays `PENDING`
  with `UNAVAILABLE` and backoff, and is never reported as applied. After Messaging restarts,
  the retry closes the conversation.
* No contact value or participant identity is stored in Messaging.

**Scenario 5 asserts:**
* The bridge is refused without the explicit, versioned disclosure acknowledgement.
* The bridge is `MANAGED` with `endToEnd: false`.
* A `PRIVATE_E2EE` conversation, created with a real OpenMLS client device, is refused
  (`E2EE_CONVERSATION_NOT_BRIDGEABLE`).
* **Outbound:** in-app message → gateway → carrier `ACCEPT` → signed `DELIVERED` receipt →
  Messaging shows `DELIVERED` (CI 2.3 s). The transport state is separate from in-app state
  (`inAppStateIndependent`).
* **Duplicate receipt and duplicate inbound event:** answered `DUPLICATE`, and exactly one SMS
  message is stored.
* **Inbound:** carrier → gateway → Messaging as a `channel: SMS` message from the
  TELEPHONE_ENDPOINT principal.
* **Unknown outcome:** `TIMEOUT_AFTER_CREATE` gives gateway `UNKNOWN`, which is never reported
  as delivered or failed. Read-back reconciliation recovers `SUBMITTED` after 30.0 s (the
  gateway's fixed delay).
* **Opt-out:** after `STOP`, the next bridged message is `BLOCKED / RECIPIENT_OPTED_OUT`, while
  in-app delivery still succeeds.
* **Spend:** after `START`, a two-segment message is `BLOCKED / SPEND_LIMIT_EXCEEDED`.
* **Close:** no further outbound transports are created.
* **Privacy:** no phone digits, `dsms-` provider reference or provider id appear in any public
  Messaging response or in Messaging's bridge or transport rows. The gateway binding row holds
  no plaintext number.
* *Not under test here:* Numbering is a routing stub inside the test. Its contract shapes come
  from the gateway's real Numbering client, and the gateway↔Numbering pair is certified
  separately by `services/numbering/tests/communications_e2e.rs`.

**Scenario 9 asserts:**
* A credential without `webhooks.manage` cannot subscribe.
* Messaging relays ids-only events, and Calling delivers `conversation.created`,
  `conversation.member_added` and `message.created` to the customer endpoint. The first
  delivery was visible after 1.0 s in CI.
* Every request is signed `v1=HMAC-SHA256(secret, "v1:{ts}:{deliveryId}:" + body)` and
  verified. A forged body does not verify.
* A `503` is retried by Calling's own worker with the same `CF-Webhook-Id`. The test only
  brings `next_attempt_at` forward; attempts are recorded as `RETRY/503`, then `SUCCEEDED`.
* No body contains the message text, `ciphertext`, `text`, `epoch`, `device`, `keyPackage`,
  `whsec_`, the API credential, or the other tenant's ids.
* The stored public events are also free of content.
* The relay is drained (everything `RELAYED`), and the second tenant's endpoint received
  nothing.
* The journey runs in the `local` runtime. That is the only runtime in which Calling accepts
  a loopback (non-public) webhook endpoint. `test`, `staging` and `production` keep the strict
  endpoint policy.
* *Limitation:* the webhook content check uses a MANAGED message. That no E2EE ciphertext is
  relayed is proven in-process (`tests/webhook_relay.rs`, Calling `tests/webhooks.rs`), not
  in this real-process chain.

## 3. Requirements matrix §§146–160

`PASS` means implemented, with the required evidence executed and verified in CI on
`9b87338` or locally (stated). Suites are `services/messaging/tests/*` unless named otherwise.

| § | Requirement | Status | Evidence |
|---|---|---|---|
| 146 | Domain: conversation lifecycle, version conflicts, membership / roles, sequencing, idempotency, edit / delete, reactions, pins, receipts, context expiry / revocation, retention resolution | PASS | `conversations` (15, incl. `concurrent_lifecycle_updates_on_one_version_have_one_winner`, `message_sequences_are_unique_and_gap_free_under_concurrency`), `messaging`, `engagement`, `integrations`, `compliance`; scenario 4 (revocation) |
| 147 | Database: fresh and upgrade migrations, constraints, rollback / reapply, role isolation, restore, ownership | PASS | `migrations`; CI "Migration smoke (up / reset / up)"; "Per-service database roles with the real migrations" (`verify-service-isolation.sh`); `disaster_recovery`; service-boundaries guard (per-service table ownership) |
| 148 | Realtime: durable commit before delivery, ordering, duplicate suppression, multi-device fan-out, cursors, reconnect / replay, backpressure, session revocation, multi-node | PASS | `realtime` (real WebSocket clients against the real router: replay from ack, broker outage, concurrent senders gap-free, backpressure, revocation); `realtime_redis` (two gateway instances, real Redis); `platform_access_e2e` step 9b (real binary handshake) |
| 149 | Isolation / IDOR across tenant, application, environment, conversation | PARTIAL | `conversations_are_isolated_by_tenant_application_environment_and_membership`, `foundation::tenant_identity_never_comes_from_the_request`, cross-tenant checks in scenarios 4 / 9. **Gap:** no single exhaustive per-route IDOR matrix for every Messaging API family (Calling has one: `api_authorization_matrix`) |
| 150 | Attachments: real S3 storage, scanner, checksum, scan pass / fail, unauthorized denial, signed-read expiry, retention, deletion, legal hold, orphan reconciliation | PASS (MANAGED) / PARTIAL (encrypted media) | `attachments` against **real MinIO and real clamd** (EICAR): `scanning_fails_closed_and_quarantines`, `legal_holds_preserve_objects…`, `attachments_fail_closed_without_storage_or_policy`, `orphaned_objects_are_reconciled`. E2EE attachments are client-encrypted blobs; server-side **media processing** for them exists only as an interface → not certified |
| 151 | Meeting + Messaging + Platform Access (real processes; legacy endpoints; closure policy; migration parity; rollback; no duplicate authoritative storage) | PASS (software) / PARTIAL (scope) | `services/meeting-service/tests/unified_messaging_e2e.rs` (real PA + Messaging binaries), CI job "Meeting ↔ Messaging ↔ Platform Access". **Breakout / private legacy Meeting chat is not migrated** |
| 152 | External developer (synthetic EXTERNAL tenant, scoped credentials, client tokens, send / read, attachments, search, replay, webhooks, no secret to client, no cross-tenant visibility) | PARTIAL | `platform_access_e2e` (real PA + Messaging, real devices, client tokens), scenarios 4 / 5 / 9 (EXTERNAL tenants, webhooks, scope denial). Attachments and search are certified in-process (real MinIO / PostgreSQL), not inside one real-process external journey |
| 153 | First-party with explicit scopes, same contracts, no privileged bypass | PASS | UM E2E (FIRST_PARTY tenant with explicit scopes); `integrations::delegation_is_refused_without_a_valid_first_party_scope` |
| 154 | Privacy Relay, Managed SMS Bridge, webhook privacy (real processes) | PASS (software) | §2 above. The real carrier edge is BLOCKED_EXTERNAL |
| 155 | Concurrency: sends, direct dedup, edits, member removal, legal holds, attachment finalization, multi-node | PARTIAL | `concurrent_direct_creations_produce_one_conversation`, `concurrent_lifecycle_updates…`, `message_sequences…under_concurrency`, `realtime::concurrent_senders…`, `outbox::concurrent_publishers_deliver_each_event_once`, `realtime_redis`. **Not exercised under concurrent load:** legal-hold placement against purge and attachment finalization |
| 156 | Capacity measurements | PARTIAL | §5. Measured on a 4-vCPU dev host only. No attachment, worker / outbox or WebSocket-reconnect-storm benchmark; no staging hardware |
| 157 | Fault injection: PostgreSQL, Redis, object storage, scanner, Platform Access, authority, SMS Gateway, workers, webhooks | PARTIAL | Exercised: Redis broker outage (`committed_messages_survive_a_broker_outage`), object storage / policy missing (`attachments_fail_closed…`), scanner failure (`scanning_fails_closed…`), Platform Access / auth failure (`foundation::customer_routes_fail_closed`), crashed worker lease (`outbox::a_crashed_claim…`), Messaging down during revocation (scenario 4), carrier timeout / unknown (scenario 5), webhook 503 (scenario 9), SMS transport refusals (`sms_bridge`). **Not exercised:** PostgreSQL loss during traffic (only restore in `disaster_recovery`) and SMS Gateway process loss mid-send against real Messaging |
| 158 | Security: auth forgery / replay / revocation / scope escalation, WS Origin, tampering, MLS vectors, key substitution, device revocation, downgrade, backup, XSS / CSP, dependency / licence / SBOM / image / secret scans, log leakage | PARTIAL / FAIL | See §6. FAIL: open RustSec advisories in `services/meeting-service`. NOT_TESTED: XSS / CSP / Trusted Types (no web messaging client exists in the repository) |
| 159 | Compliance: retention precedence, legal holds, exports, purge completeness, healthcare restrictions | PASS (software) | `compliance` (`retention_expires_and_purges_but_legal_holds_preserve`, `exports_are_managed_only_authorized_and_expire`, `data_deletion_requests_respect_holds`, regulated contexts) |
| 160 | Production acceptance gates | see §7 | — |

## 4. New and modified code and tests

| File | Change |
|---|---|
| `services/messaging/tests/cross_service_e2e.rs` | **new**: real-process harness (Platform Access, Messaging, peers on fixed or free ports, restart on the same port, per-journey trust bundles, OpenMLS client device) and scenarios 4, 5, 9 |
| `.github/workflows/unified-messaging-e2e.yml` | new job `cross-service` (scenarios 4, 5, 9; binaries required); triggers on privacy-relay / sms-gateway / calling / webhooks paths |
| `.github/workflows/dependency-security.yml` | RustSec audit extended to `platform-access`, `sms-gateway`, `numbering`, `meeting-service` (previously unaudited); new SBOM job |
| `scripts/sbom/cargo_lock_sbom.py` | **new**: CycloneDX 1.5 SBOM from every tracked `Cargo.lock` (877 components from 13 lockfiles; registry checksums kept) |
| `services/meeting-service/Cargo.lock` | semver-compatible security updates: bytes 1.12.1, rustls 0.23.46, rustls-webpki 0.103.15, time 0.3.55, lettre 0.11.23, anyhow 1.0.104, event-listener 5.4.2. `cargo check --workspace --all-targets` OK |

No production code changed: the three journeys found no software defect in the services.
One **test** defect was found by CI on `295aa87`: scenario 4 asserted that Privacy Relay's job
row was `APPLIED` the moment Messaging showed the conversation closed. Privacy Relay records
`APPLIED` only after Messaging's answer, so the check could run before that write. Both
checks now wait (bounded, 20 s) for the job status, and no assertion was relaxed. After the
fix, three consecutive local runs passed (9/9). No
assertion was weakened, no job disabled, no test skipped.

## 5. Capacity measurements (synthetic; not a production capacity claim)

**Machine:** 4 vCPU Intel Xeon @ 2.10 GHz, 15 GiB RAM, PostgreSQL 16.15 (local), Redis
7.0.15. **Command:**
`cargo test --release --test load -- --ignored --nocapture --test-threads 1`. The real router
runs in-process over real PostgreSQL, with real TCP WebSocket clients and real OpenMLS
client-side encryption. Only correctness is asserted (every message accepted, delivered once,
in order).

| Workload | Configuration | Result |
|---|---|---|
| MANAGED send, one conversation | 20 senders, 1,000 messages | 157 msg/s; p50 126 ms, p95 150 ms, p99 202 ms |
| MANAGED send, independent conversations | 20 conversations, 1,000 messages | 210 msg/s; p50 92 ms, p95 117 ms, p99 152 ms |
| E2EE send (real MLS, incl. client encryption) | 500 messages | 85 msg/s; p50 11 ms, p95 15 ms; receive + decrypt all 0.08 s |
| Realtime fan-out | 100 messages × 50 WebSocket receivers = 5,000 deliveries | send→receive p50 34 ms, p95 43 ms, p99 49 ms (2.19 s total) |
| MANAGED search | 5,000 indexed messages, 60 queries | p50 7.8 ms, p95 9.4 ms |
| Cross-service propagation (CI runner) | scenarios 4 / 5 / 9 | authority 142–313 ms; SMS delivered visible 2.3 s; first webhook 1.0 s |

**Not measured:** attachment upload / finalize throughput, worker / outbox throughput,
WebSocket reconnection storms, large-group (> 50) fan-out, staging hardware.

## 6. Security and cryptography

| Check | Status | Evidence |
|---|---|---|
| Service-token forgery / replay / wrong audience / scope | PASS | `foundation`, Privacy Relay `internal_auth`, Calling `internal_auth`; the real-process trust bundles grant one permission per edge |
| Customer scope escalation | PASS | scenario 9 (no `webhooks.manage` → 403); scope tests in `foundation` / `integrations` |
| WebSocket Origin / ticket / session revocation | PASS | `realtime` (allowed origins, single-use tickets, revalidation) |
| MLS published vectors (RFC 9420) | PASS | Messaging "crypto core" job, `cargo test --all-features` with the pinned, digest-checked vectors |
| Key substitution, device revocation, membership races, downgrade | PASS (server) | `devices`, `transparency`, `messaging`; production E2EE refused by config (`operations`) |
| External key-transparency witness | BLOCKED_EXTERNAL | heads are signed but UNWITNESSED |
| Secure device backup / recovery | PARTIAL | server backup / restore PASS (`disaster_recovery`); user device recovery incomplete |
| XSS / CSP / Trusted Types | NOT_TESTED | no web messaging client in the repository |
| RustSec advisories | **FAIL** (meeting-service) / PASS (others, local `cargo audit`) | meeting-service: 13 open after patch updates (rustls 0.21.12 ×2, rustls-webpki 0.101.7 ×3, h2 0.3.27, idna 0.5.0, zip 0.6.6 ×3, rkyv 0.7.46, libcrux-kem ×2 [not compiled, existing ignore]). These need major upgrades of the reqwest 0.11 / old TLS chain, zip and rkyv users |
| Licence review, image scan, secret scan | PASS | Messaging licence steps; Dependency and image security; `scan_secrets.py` clean |
| SBOM | PASS (generated) | new SBOM job; local run: 877 components |
| Logs / telemetry leakage | PASS | `architecture` (no key / content logging); bounded metric labels |
| Independent cryptographic assessment | BLOCKED_EXTERNAL | not performed |
| Real web / Android / iOS MLS runtime | BLOCKED_EXTERNAL | compile-only targets in CI |

Production E2EE remains **disabled and fails closed**. No E2EE production certification is
claimed from server-only tests.

## 7. §160 production acceptance gates

| Gate | Status | Reason |
|---|---|---|
| Domain correctness | PASS | §3 (146) |
| Migrations, constraints, DB role isolation | PASS | §3 (147) |
| Tenant / IDOR isolation, every API family | PARTIAL | no exhaustive per-route Messaging matrix |
| Realtime delivery guarantees | PASS | §3 (148) |
| MANAGED attachments with real storage and scanner | PASS | §3 (150) |
| Encrypted-media processing | PARTIAL | interface only |
| Meeting migration (real processes) | PASS (software) | UM E2E |
| Breakout / private Meeting chat migration | PARTIAL | still legacy |
| External developer journey | PARTIAL | attachments / search not in a single real-process journey |
| First-party integration | PASS | §3 (153) |
| Privacy Relay revocation (real processes) | PASS | scenario 4 |
| SMS Bridge software (real processes, synthetic carrier) | PASS | scenario 5 |
| SMS public carrier edge | BLOCKED_EXTERNAL | requires a real carrier, credentials and authorization |
| Webhook privacy (real processes) | PASS | scenario 9 |
| Concurrency | PARTIAL | legal-hold and attachment-finalization races not load-tested |
| Capacity | PARTIAL | dev-host numbers only; no staging hardware |
| Fault injection | PARTIAL | PostgreSQL-loss and SMS Gateway process-loss cases not executed |
| Authentication / authorization security | PASS | §6 |
| MLS conformance (server, vectors) | PASS | §6 |
| Real-device MLS runtime | BLOCKED_EXTERNAL | no device lab |
| Independent cryptographic review | BLOCKED_EXTERNAL | not performed |
| KT external witness | BLOCKED_EXTERNAL | no witness operator |
| Push provider, media processing | BLOCKED_EXTERNAL / PARTIAL | provider credentials / processing pipeline |
| Device recovery | PARTIAL | incomplete |
| Web XSS / CSP / Trusted Types | NOT_TESTED | no web messaging client |
| Dependency advisories (all lockfiles) | **FAIL** | meeting-service, 13 open |
| Licence / SBOM / image / secret scans | PASS | §6 |
| Compliance (retention, holds, exports, purge, healthcare) | PASS | §3 (159) |
| Production E2EE disabled, fail-closed | PASS | `operations` |
| Hosted staging deployment | NOT_TESTED | no authorized hosted staging; isolated CI / local processes used |
| Owner production approval | BLOCKED_EXTERNAL | not given |

**Totals (31 gates):** PASS 14 · PARTIAL 8 · FAIL 1 · BLOCKED_EXTERNAL 6 · NOT_TESTED 2 (a mixed "X / Y" status is counted by its first state).

| Declaration | State |
|---|---|
| SOFTWARE COMPLETE | PARTIAL: breakout / private chat migration, encrypted-media processing, device recovery |
| INTEGRATION READY | YES (software): every Part V cross-service edge passes with real processes |
| EXTERNAL EDGE CERTIFIED | NO: carrier, crypto review, device runtime, KT witness, push |
| PRODUCTION PASS | NO |

**Production readiness: NOT APPROVED.**

## 8. Outstanding external dependencies

1. Public SMS carrier: credentials, an authorized route and a carrier certification run.
2. Independent cryptographic assessment of the E2EE design and implementation.
3. Real web / Android / iOS MLS runtime certification on devices.
4. External key-transparency witness.
5. Push provider credentials and delivery certification.
6. Authorized hosted staging environment for deployed-system tests.
7. Explicit owner production approval.

## 9. Certification command reference

```sh
# PostgreSQL 16 and Redis 7 locally; build the peers once (debug)
for s in platform-access privacy-relay sms-gateway calling; do (cd services/$s && cargo build --workspace --bins --locked); done
export TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres TEST_REDIS_URL=redis://127.0.0.1:6379
S=$PWD/services
export PLATFORM_ACCESS_BIN=$S/platform-access/target/debug/platform-access PLATFORM_ACCESS_MIGRATION_BIN=$S/platform-access/target/debug/platform-access-migration
export PRIVACY_RELAY_BIN=$S/privacy-relay/target/debug/privacy-relay PRIVACY_RELAY_MIGRATION_BIN=$S/privacy-relay/target/debug/privacy-relay-migration
export SMS_GATEWAY_BIN=$S/sms-gateway/target/debug/sms-gateway SMS_GATEWAY_MIGRATION_BIN=$S/sms-gateway/target/debug/sms-gateway-migration
export CALLING_BIN=$S/calling/target/debug/calling CALLING_MIGRATION_BIN=$S/calling/target/debug/calling-migration

# Scenarios 4, 5, 9 (real processes; ~40 s). CROSS_SERVICE_E2E_LOGS=1 shows service logs.
(cd services/messaging && CROSS_SERVICE_E2E_REQUIRED=1 cargo test --locked --test cross_service_e2e -- --nocapture)
# Scenarios 1, 2, 6, 7 (Meeting ↔ Messaging ↔ Platform Access)
(cd services/meeting-service && MESSAGING_BIN=$S/messaging/target/debug/messaging MESSAGING_MIGRATION_BIN=$S/messaging/target/debug/messaging-migration \
  cargo test --locked --test unified_messaging_e2e -- --nocapture)
# Domain / realtime / attachments / compliance suites (attachments need MinIO + clamd, see messaging.yml)
(cd services/messaging && cargo test --locked --workspace --no-fail-fast)
# Capacity (release, ignored by default)
(cd services/messaging && cargo test --release --locked --test load -- --ignored --nocapture --test-threads 1)
# Dependencies, SBOM, guards
(cd services/meeting-service && cargo audit --ignore RUSTSEC-2023-0071 --ignore RUSTSEC-2026-0330 --ignore RUSTSEC-2026-0331)
python3 scripts/sbom/cargo_lock_sbom.py sbom.cdx.json $(git ls-files '*Cargo.lock')
python3 scripts/service-boundaries/check_service_boundaries.py && python3 scripts/secret-scan/scan_secrets.py
```

## 10. CI on `9b87338` (verified step by step, not by badge)

| Workflow | Run | Result |
|---|---|---|
| Unified Messaging integration journeys (both jobs, incl. new cross-service) | [39304680035](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39304680035) | success; cross-service 3/3 passed |
| Messaging (crypto core, service checks, service test with MinIO / clamd / Redis / real PA, migration / isolation, image) | [39304679826](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39304679826) | success; every step success; workspace run 162 passed / 0 failed / 5 ignored (the `load` benchmarks). In that run the cross-service journeys return early because their peer binaries are not built there; the dedicated job above is the evidence |
| Dependency and image security on `9b87338` | [39304679601](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39304679601) | success |
| Dependency and image security on `295aa87` (extended audit + SBOM) | [39305785130](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39305785130) | **failure, as expected**: 16 / 17 jobs succeed (incl. new SBOM, platform-access, sms-gateway, numbering audits); `RustSec advisories (services/meeting-service)` fails on the 13 advisories in §6 |
| Unified Messaging journeys on `295aa87` | [39305785127](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39305785127) | failure: the scenario-4 test race described in §4 (fixed in `5269b89`); the Meeting job succeeded |
| Service boundaries, Meeting service on `295aa87` | 39305785146 / 39305785138 | in progress when this record was written; superseded runs are re-triggered by the final push. A queued or running run is not counted as success |
