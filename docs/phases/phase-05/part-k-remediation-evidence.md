# Part V — Part K remediation evidence

Scope: the Part V "Post-Part K Engineering Completion Directive" (Milestones 0–15). This
report records what was changed on `staging` after the Part K certification commit, what
was executed to prove it, and what still cannot be closed from the repository. Every
number below comes from a recorded run; nothing is estimated.

**Production verdict: NOT APPROVED** (§K). Six gates need external parties or
authorization that do not exist yet, three gates remain PARTIAL, and hosted staging has not
been authorized.

---

## A. Git evidence

| Item | Value |
|---|---|
| Starting SHA (`staging`, Part K final) | `111865484b61170a7eedabbd321a38196d51a541` (`1118654`, 2026-10-19 19:02 UTC) |
| Last code SHA before this report | `1a2249f` (CI in §I) |
| Final SHA | the commit that adds this report (docs only); see §I for its dispatched runs |
| Remote branches | `main`, `staging` only (verified through the GitHub API) |
| `main` | `689728c9f03126bf3caa6e73b9e600283eb95f18`, unchanged since 2026-10-18 (before this work); still an ancestor of `staging` |
| History | no force-push, no rebase of published history, no merge into `main` |
| Deployment | nothing deployed (no production, no hosted staging, no production migration, production E2EE still disabled, no live carrier traffic, no billable operation) |
| Change size | 19 code commits + this report; 118 files (most of the line count is generated OpenAPI bundles and lockfiles) |

Commits (oldest first):

| Commit | Milestone | Summary |
|---|---|---|
| `b6e15c9` | M12 | Self-contained Swagger bundles (generator + `--check`); Platform Access `BOT_OR_SERVICE` principals |
| `6e13434` | M1 | Meeting Service: every RustSec vulnerability removed |
| `992afe9` | M2 | Messaging per-route authorization / IDOR matrix; bans contract status corrected |
| `c502ad4` | M3 | Breakout and private meeting chat migrated to Unified Messaging |
| `7f5545a` | M4 | Real-process fault injection: PostgreSQL loss, SMS Gateway death |
| `ca6adef` | M4/M13 | **Defect fixed:** legal-hold creation could be overtaken by an in-flight purge; finalization races |
| `747d58e` | M4 | Redis outage across two gateway instances |
| `d5b9131` | M5 | Benchmarks: large fan-out, reconnection storm, attachment pipeline, outbox |
| `3961bfa` | M8 | APNs, FCM, Web Push adapters (verifying local fakes) |
| `ce3155e` | M9 | Key-transparency witness cosigning, reference witness, quorum verification |
| `4816abb` | M8 | Web Push content encoding via `ring` (service-boundary rule kept) |
| `a91e5d3` | M7 | Authenticated device-to-device history transfer |
| `0adebe4` | M7 | Meeting Service lockfile follows the crypto-package edge |
| `3cca63e` | M6 | Server thumbnails of MANAGED images; never PRIVATE_E2EE |
| `ed5d62c` | M10 | Web app: nonce CSP, Trusted Types, security headers (Chromium-verified) |
| `2dafda5` | M10 | Client crypto core executed in Chromium (wasm32, Web Workers, strict CSP) |
| `521ea57` | M11 | Complete EXTERNAL developer journey on real processes |
| `ee3fc39` | M6 | Server-side measurement of MANAGED media (duration, dimensions) |
| `1a2249f` | M12 | Swagger UI renders every bundle in Chromium with the schema host blocked |

Database migrations added (all reversible, all run by the migration smoke test up / reset / up):

| Service | Migration | Purpose |
|---|---|---|
| Meeting Service | `m20261020_000001_unified_messaging_scoped_chat` | breakout / private chat scope links, fenced membership sync, cutover guard for every scope |
| Messaging | `m20261116_000014_kt_witnesses` | witness state, append-only cosignatures |
| Messaging | `m20261117_000015_history_transfers` | history-transfer sessions (sealed bytes only while SEALED) |
| Messaging | `m20261118_000016_attachment_derivatives` | derivatives table, MANAGED-only CHECK + trigger |
| Messaging | `m20261119_000017_attachment_inspection` | INSPECTION derivatives and their metadata |

---

## B. Dependency security (Milestone 1)

### B.1 Meeting Service — the 13 findings of the Part K register

`cargo audit` on `services/meeting-service/Cargo.lock`, with the repository's ignore list
(`RUSTSEC-2023-0071` everywhere; `RUSTSEC-2026-0330/0331` for the crypto-linked lockfiles).

| Advisory | Crate | Vulnerable version | Now | Origin | Remediation |
|---|---|---|---|---|---|
| RUSTSEC-2026-0346 | rustls | 0.21.12 | 0.23.46 only | reqwest 0.11, tokio-tungstenite 0.20 (livekit-api 0.4) | reqwest 0.11 → 0.12; livekit-api 0.4 → 0.6.4 / livekit-protocol 0.6 → 0.7 (recorder adapted) |
| RUSTSEC-2026-0349 | rustls | 0.21.12 | 0.23.46 only | same | same |
| RUSTSEC-2026-0098 | rustls-webpki | 0.101.7 | 0.103.15 only | same | same |
| RUSTSEC-2026-0099 | rustls-webpki | 0.101.7 | 0.103.15 only | same | same |
| RUSTSEC-2026-0104 | rustls-webpki | 0.101.7 | 0.103.15 only | same | same |
| RUSTSEC-2026-0258 | h2 | 0.3.27 | 0.4.20 | reqwest 0.11 / hyper 0.14 | reqwest 0.12 |
| RUSTSEC-2024-0421 | idna | 0.5.0 | 1.1.0 only | validator 0.18 | validator 0.18 → 0.20 |
| RUSTSEC-2026-0235 | rkyv | 0.7.46 | absent | optional rust_decimal feature recorded in the lockfile | rust_decimal 1.40 → 1.43 (no archived data existed: nothing persisted with rkyv) |
| RUSTSEC-2026-0367 | zip | 0.6.6 | absent | utoipa-swagger-ui build script (extracts a ZIP at build time) | utoipa-swagger-ui removed; Swagger UI served by the Meeting Service itself (swagger-ui-dist 5.17.14 pinned by version and SHA-384 SRI, restrictive CSP) |
| RUSTSEC-2026-0368 | zip | 0.6.6 | absent | same | same |
| RUSTSEC-2026-0369 | zip | 0.6.6 | absent | same | same |
| RUSTSEC-2026-0330 | libcrux-kem | 0.0.9 | 0.0.9 in the lockfile, **compiled for no target** | OpenMLS optional backend | documented exception kept (not expanded): `cargo tree -i libcrux-kem --target all -e normal,build` is empty; `dependency-security.yml` fails the job if it is ever linked |
| RUSTSEC-2026-0331 | libcrux-kem | 0.0.9 | same | same | same |

Also removed: `oauth2` 4.4 (declared, never used). Untrusted ZIP extraction: none remains in
any Meeting Service path (the only ZIP use was the build script above).

Verification at the time of the change: workspace tests 632 passed plus the library re-run
133 / 133; Meeting Service, Unified Messaging integration and Dependency security workflows
green on every later commit (§I).

### B.2 Every Rust lockfile now (2026-10-20, same ignore list)

| Lockfile | Vulnerabilities | Warnings (not vulnerabilities) |
|---|---|---|
| services/meeting-service | **0** (was 11 unignored) | unmaintained: async-std 1.13.2 (RUSTSEC-2025-0052), instant 0.1.13 (RUSTSEC-2024-0384), proc-macro-error 1.0.4 (RUSTSEC-2024-0370), proc-macro-error2 2.0.1 (RUSTSEC-2026-0173); yanked: spin 0.9.8 |
| services/messaging | 0 | unmaintained: proc-macro-error2 2.0.1 |
| packages/messaging-crypto | 0 | unmaintained: proc-macro-error2 2.0.1 |
| packages/messaging-crypto/web-harness (new) | 0 | unmaintained: proc-macro-error2 2.0.1 |
| services/privacy-relay | 0 | unmaintained: atomic-polyfill 1.0.3 (RUSTSEC-2023-0089) |
| calling, numbering, platform-access, sms-gateway, telephony, contracts validator, object-storage, service-auth, webhooks | 0 | none |

Advisory exceptions (unchanged in number and scope): `RUSTSEC-2023-0071` (rsa, Marvin
attack: no RSA private-key operation on attacker-observable timing; FCM signing uses
`ring`, guarded by an architecture test) and `RUSTSEC-2026-0330/0331` (libcrux-kem, not
compiled). New dependencies added in this work: `image` 0.25 (PNG + JPEG only) and its
codec crates for Messaging thumbnails; `wasm-bindgen` 0.2.129 in the test-only web harness;
`playwright` 1.56.1 (dev-only) in three test packages. All pass the licence gate
(`scripts/messaging/check_licenses.py`: 406 packages for Messaging, 157 for the harness).
The CycloneDX SBOM is regenerated from every committed lockfile by the `sbom` job of
`dependency-security.yml` on every run (artifact `cfmeet-backend-sbom`).

---

## C. API authorization (Milestone 2)

`services/messaging/tests/authorization_matrix.rs` executes every customer operation of
`cfmeet.messaging.http-api` against a fresh fixture (real OpenMLS devices, object storage,
an SMS bridge, compliance records, client sessions). For each operation it sends every
applicable denial with the positive call's own path, query and body, asserts the exact
status and error code, asserts no identifier or content is echoed, and compares a
fingerprint of every table before and after (no side effect: no row, event, grant, cursor
or delivery change). Then the positive call must succeed with a status the contract
documents. A coverage test fails if a published operation is missing.

| | Operations | Denial cases |
|---|---|---|
| Customer API (every published operation, incl. the three new in this work) | 84 | 850 |
| Internal API (Meeting, Privacy Relay, SMS Gateway surfaces) | 7 | 49 |

Denials executed per customer operation where applicable: missing / invalid / revoked
credential; a client token on a customer route; insufficient scope; another tenant's ids
with its own principal (IDOR); another tenant acting as our principal; non-member;
removed member; banned member; inactive principal; principal-restricted credential; plain
member on admin operations; another principal's device; acting principal on
administration-only operations. Internal: no token, customer credential, untrusted issuer,
rogue key, wrong peer, missing and forged delegated scope.

Justified exclusions (recorded in the matrix itself): the three MLS Delivery Service
routes (`mls/commits`, `mls/state`, `mls/key-package-claims`) skip RemovedMember /
BannedMember because the E2EE fixture has no such member; both cases are exercised on the
MANAGED routes and in `tests/messaging.rs`. Expired credentials and suspended tenants are
Platform Access states and reach Messaging as the same revoked / inactive outcomes tested
here (and in `platform_access_e2e`, where Platform Access revocation propagates to a
running Messaging). Concurrent revocation is covered by `realtime::removed_members_stop_receiving_while_messages_are_flowing` and the M11 journey.

The full per-route table (method, path, scopes, acting rule, resource kind, positive
status, every denial and its result) is `evidence/authorization-matrix.txt`.
Defect found: `POST /v1/conversations/{id}/bans` answered 201 while the contract said 200
(contract corrected).

---

## D. Meeting migration (Milestone 3)

Breakout-room and private (participant-to-participant) chat now follow the same
LEGACY_PRIMARY → SHADOW_VALIDATION → MESSAGING_PRIMARY → LEGACY_RETIRED state machine as
main-room chat (`c502ad4`).

| Aspect | Behaviour |
|---|---|
| Ownership | Meeting Service keeps participation, breakout admission, host permissions and policy; Messaging owns conversations, messages, membership, events, retention |
| Mapping | one Messaging MEETING conversation per breakout room and per participant pair (`conference_chat_scope_links`); private chats owned by the environment's non-human system principal (Platform Access `BOT_OR_SERVICE`), so only the two parties are members; breakout chats contain the assigned admitted participants plus hosts visiting the room |
| Membership | version-fenced sync (`conference_chat_scope_member_sync`): moving rooms, leaving, removal and closure update Messaging membership before the send transaction |
| Shadow / parity | mirroring and parity checks cover every scope |
| Cutover | a database guard refuses legacy rows of every scope after cutover (no duplicate authoritative writes) |
| Rollback | copy-back while still possible; DOWN migration restores the previous schema exactly |
| Provenance | imported history stays `LEGACY_MANAGED`, never relabelled E2EE |
| Tests | real-process e2e (`services/meeting-service/tests/unified_messaging_e2e.rs`, real Platform Access + Messaging binaries): shadow, parity, cutover, DB guard, per-principal direct Messaging access, room moves, removal, rollback, close-all |

Limitation: for PRIVATE_E2EE meeting conversations the backend removes authorization; the
cryptographic removal is the client's MLS commit (not claimed by a database update).

---

## E. Reliability (Milestones 4 and 13)

| Scenario | Test | What is asserted |
|---|---|---|
| PostgreSQL loss during concurrent sends | `cross_service_e2e` (Unified Messaging workflow): Messaging reaches its database through a cuttable TCP gate | `/ready` 503 during the cut; every acknowledged message durable; retries with the same `clientMessageId` never duplicate; gap-free sequence; receiver sees each message once, in order; security profile unchanged |
| SMS Gateway killed mid-send | `cross_service_e2e` | submission durably AUTHORIZED then process killed; no false terminal state while down; one transport per message; one provider submission each after restart; duplicate carrier receipts rejected; DELIVERED reaches Messaging |
| Redis loss between two gateway instances | `realtime_redis::a_redis_outage_loses_nothing_committed_and_fanout_resumes` | sends still commit; the other instance's client still receives through database catch-up; nothing redelivered after Redis returns; live fan-out resumes; E2EE ciphertext decrypts in order throughout |
| Legal hold vs in-flight purge | `compliance::a_legal_hold_is_never_overtaken_by_an_in_flight_purge` | **defect found and fixed** (`ca6adef`): a hold could be acknowledged while a purge that had already checked "not held" committed; now a per-environment advisory lock serialises holds against purges, erasure and object deletion (the test fails on the old code) |
| Attachment finalization races | `attachments::concurrent_finalization_and_expiry_settle_exactly_once` | 16 parallel finalizes, and 8 finalizes racing 4 expiry workers: one outcome, quota committed / released once, usage metered once, one deletion queued |
| Derivative render vs attachment deletion | `attachments::managed_images_get_safe_thumbnails…` | a render finishing after deletion removes its own output; the thumbnail object is deleted with the attachment |
| Previously certified (re-run green) | outbox crashed-claim recovery, broker outage, object storage / policy missing, scanner failure, Platform Access failure, webhook receiver 503, carrier timeout / unknown, KT rollback after restore, backup / restore | `outbox`, `realtime`, `attachments`, `foundation`, `cross_service_e2e`, `operations`, `disaster_recovery` |

Not separately exercised (residual, §J): a Privacy Relay process outage (Messaging-down
during revocation is covered) and a deliberate retry storm against a live provider.

---

## F. Performance (Milestone 5)

Host: 4 vCPU Intel Xeon @ 2.10 GHz, 15 GiB RAM; PostgreSQL 16.15 and Redis 7.0.15 on the
same host; release build; tests run one at a time. Raw output:
`evidence/load-measurements-2026-10-20.txt`. **Development-host numbers, not a production
capacity guarantee.** Each benchmark also asserts correctness (every delivery once and in
order, each event exactly once, every upload READY); all 9 passed.

| Workload | Size | Throughput | p50 / p95 / p99 (ms) | Errors |
|---|---|---|---|---|
| MANAGED send, 20 senders, one conversation | 1 000 messages | 179 msg/s | 109 / 134 / 168 | 0 |
| MANAGED send, 20 independent conversations | 1 000 messages | 63 msg/s | 107 / 1 119 / 2 590 | 0 |
| E2EE send with real MLS client encryption | 500 messages | 104 msg/s | 9.3 / 12.1 / 13.9 | 0 |
| Realtime fan-out, 50 receivers | 100 messages, 5 000 deliveries | — | 36.8 / 52.1 / 70.2 | 0 |
| Realtime fan-out, 250 receivers | 40 messages, 10 000 deliveries | 4 235 deliveries/s | 98.4 / 125.5 / 150.3 | 0 |
| Reconnection storm, 200 clients, 20 missed messages each | 200 reconnects | — | reconnect 1 250 / 1 806 / 1 841; caught up 1 619 / 2 055 / 2 090 | 0 |
| MANAGED search, 5 000 indexed messages | 60 queries | — | 6.3 / 8.1 / 8.5 | 0 |
| Attachment pipeline, 256 KiB, concurrency 16 (in-memory store, always-clean scanner) | 200 uploads | 59 uploads/s, 14.8 MiB/s verified | intent 55.8 / 106 / 140; finalize+verify+scan 54.9 / 87.2 / 118; end-to-end incl. send and grant 256 / 348 / 436 | 0 |
| Outbox, 4 concurrent publishers | 5 000 events | 1 948 events/s (1 300 / 1 200 / 1 300 / 1 200 per publisher) | — | 0 duplicates |

Real MinIO and real clamd are exercised for correctness (`attachments`, M11 journey), not
benchmarked; the attachment benchmark measures Messaging's own work. Hosted-capacity
certification: NOT_TESTED (no authorized environment).

---

## G. External integration

| Edge | Software status | Real-world status |
|---|---|---|
| SMS Bridge | PASS with the deterministic synthetic carrier (real processes; gateway death recovered) | public carrier: BLOCKED_EXTERNAL (route, credentials, authorization) |
| Push | APNs (ES256 provider tokens), FCM HTTP v1 (RS256 service-account OAuth), Web Push (RFC 8291, checked against the RFC 8291 Appendix A vector; RFC 8292 VAPID; HTTPS allowlist, no redirects) behind the existing `PushProvider`; metadata-only payloads; truthful outcomes (unregistered handles disable the registration, 429 / 5xx retry); tested against local fakes that verify signatures and decrypt | live provider certification: BLOCKED_EXTERNAL (Apple / Google / browser-vendor credentials) |
| Key-transparency witness | cosignature input + quorum verification in the client package; reference witness (`messaging-kt-witness` binary) that verifies consistency proofs and refuses forks / rollbacks with durable state; log-side submitter that verifies every cosignature and raises `KT_ROLLBACK_DETECTED` (appends fail closed); `witnessStatus: WITNESSED` only with a quorum; three witnesses over real HTTP in `transparency_witness` | independent witness operators: BLOCKED_EXTERNAL (locally run witnesses are not independent) |
| History recovery | device-to-device transfer: target signs a single-use HPKE transfer key, both devices show the same comparison code, source seals (RFC 9180, transfer context as AAD) and signs, target verifies and opens; server stores opaque bytes only while SEALED, deletes them on completion / cancel / expiry / revocation; no server key, no escrow | real-device certification: part of gate 20 |
| Client crypto runtime | the real Rust client core runs in Chromium 141 (wasm32, one device per Web Worker, strict CSP + Trusted Types): registration KeyPackages pass Messaging's own checks; group / Welcome; encrypt / decrypt; tampered and replayed ciphertext refused; altered KeyPackage refused; epoch change without pre-join history; offline delivery; 300 KB attachment; history transfer with forged-source, cross-tenant, tampered and stale-key refusals; device removal; nothing persisted in browser storage | physical Android / iOS devices and the shipped web client: BLOCKED_EXTERNAL / not built (the web app does not yet integrate the crypto core) |
| External developer | §G.1 | — |

### G.1 External developer journey (Milestone 11)

`platform_access_e2e::external_developer_journey_with_attachments_search_and_webhooks`, on
the real Platform Access binary, the real Messaging binary, real MinIO and real clamd (CI;
locally run both with clamd and without it, where policy marks scanning not required):
EXTERNAL tenant → application → SANDBOX environment → scoped credential (kept on the test's
"customer backend") → principals with devices → policy set over the API → MANAGED group →
client token → single-use ticket → WebSocket → message sent and received live → image
uploaded with the signed grant → finalized (clamd CLEAN) → metadata → download grant
(forced attachment disposition, bytes equal) → server thumbnail and its grant → MANAGED
search (one and across conversations) → PRIVATE_E2EE search refused (`LOCAL_SEARCH_ONLY`)
→ lifecycle events relayed to the platform-event ingress with ids only (no text, file name
or bytes; the Calling delivery hop is `cross_service_e2e` scenario 9) → member removal
revokes the live subscription and every later read, grant and search → revoked device gets
no client token → another tenant reaches nothing. Result: PASS.

---

## H. Swagger / OpenAPI (Milestone 12)

| Item | Result |
|---|---|
| Root cause | contracts reference shared schemas by canonical `$id` (`https://schemas.cfmeet.com/...`), a naming authority that is not served; Swagger UI tried to fetch them |
| Documents checked | 11: calling http; messaging http and internal; numbering internal; platform internal-http; platform-access internal; privacy-relay http and internal; sms-gateway http and internal; telephony internal. The Meeting Service's own generated document is checked statically by its unit test (it has no shared-schema references) |
| Resolution | `scripts/contracts/bundle_openapi.py` inlines every referenced schema (recursively, local refs re-based) into each document's `components/schemas`; sources untouched; bundles registered in the manifest as authority `generated` |
| Static check | `bundle_openapi.py --check` (no external `$ref`, every local `$ref` resolves, bundles current) in `contracts.yml`; Meeting Service unit test `every_served_document_resolves_all_references_locally` |
| Browser check (new, `1a2249f`) | `scripts/contracts/swagger-render`: the Swagger UI the Meeting Service serves (version + SRI read from its source) renders every bundle in Chromium with `schemas.cfmeet.com` blocked; Swagger UI's own resolver resolves each whole document: **446 operations, 840 schemas, 0 resolver errors, 0 blocked requests**; a negative control with an external `$ref` must report "Could not resolve reference" (it does) |
| Serving | Meeting Service `/swagger-ui` with pinned, SRI-checked assets, restrictive CSP, the bundled peer documents; no internal authentication was relaxed and no credential is exposed |
| Live hosted page | **not verified**: requires an authorized deployment of this commit and a check of the deployed URL |

---

## I. CI

All runs below are on `staging`, verified job by job through the GitHub API (not by badge).

Last code commit with a full workflow set: **`ee3fc39`** — 16 of 16 workflows succeeded:
Messaging ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056811)),
Unified Messaging integration journeys ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056879)),
Dependency and image security ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056799)),
Meeting service ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056882)),
Service boundaries ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056947)),
Contracts and service auth ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056841)),
Platform Access ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056844)),
Privacy Relay ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056849)),
SMS Gateway ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056926)),
Numbering ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056789)),
Calling ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056804)),
Calling RTC certification ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056762)),
Enterprise calling certification ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056810)),
Communications end-to-end journeys ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056907)),
Telephony ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056768)),
Telephony SIP certification ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39350056881)).

Earlier commits: Frontend green on `ed5d62c` ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39344491032));
the new `client crypto runtime in Chromium (wasm32)` job green on `2dafda5`
([job](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39346895304/job/118381555186)).
On `1a2249f`, Contracts and service auth, including the new `Swagger UI renders every
bundle` job, succeeded ([run](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/39352656207)).
One run was **cancelled**, not failed: Service boundaries on `2dafda5`, superseded by the
next push (concurrency group); it passed on `521ea57` and `ee3fc39`.

The commit adding this report changes documentation only, so path-filtered workflows would
not start on it. Every workflow except `deploy-backend.yml` (deployment) and `mobile.yml`
(EAS builds / store submission: external and billable) is dispatched manually on the final
SHA; their results are reported with the final response and on issue #14, not claimed here
in advance.

Security findings in CI: none open. Warnings: the unmaintained / yanked crates of §B.2;
pre-existing ESLint errors in `apps/web` (reported, non-blocking, unchanged by this work).

---

## J. Outstanding items (no hidden gaps)

| # | Item | Owner | Kind | Evidence needed | Blocking authority | Status | Next action |
|---|---|---|---|---|---|---|---|
| 1 | Public SMS carrier | SMS Gateway | external | a certification run on a lawful route | owner + carrier | BLOCKED_EXTERNAL | contract a carrier; provide credentials in a sandbox; run the bridge suite against it |
| 2 | Independent cryptographic assessment | Messaging / crypto package | external | assessor report | owner | BLOCKED_EXTERNAL | commission a review of ADRs 0045–0052, `packages/messaging-crypto`, the MLS Delivery Service and the history-transfer protocol |
| 3 | Independent KT witnesses | Messaging | external | ≥ quorum of witnesses run by independent operators | owner + operators | BLOCKED_EXTERNAL | recruit operators; each runs `messaging-kt-witness` with its own key; configure `MESSAGING_KT_WITNESSES_FILE` |
| 4 | Real-device MLS runtime | clients | external + software | runs on physical Android / iOS devices and in the shipped web client | owner (device lab) | BLOCKED_EXTERNAL | integrate the crypto core into `apps/web` / `apps/mobile`; run the Chromium journey's steps on devices |
| 5 | Push provider certification | Messaging | external | deliveries through Apple / Google / a browser push service | owner (accounts) | BLOCKED_EXTERNAL | provision sandbox credentials; enable the adapters in a sandbox environment |
| 6 | Hosted staging | all services | authorization | deployed-system smoke, capacity and rollback runs | owner | NOT_TESTED | authorize an environment; run the deployment verification against it |
| 7 | Owner production approval | owner | authorization | explicit approval after gates pass | owner | BLOCKED_EXTERNAL | — |
| 8 | Capacity on production-like hardware | Messaging | software + environment | §F repeated on staging hardware with real MinIO / clamd under load | owner (environment) | PARTIAL | run `tests/load.rs` and an attachment load against hosted staging |
| 9 | Media: transcoding; derivative bytes not counted in storage quotas | Messaging | software | — | — | PARTIAL | transcoding is not required by any current policy and is not claimed; thumbnails are bounded (≤ 320 px PNG) but excluded from customer quotas — decide whether to meter them |
| 10 | Web build / update integrity | apps/web | software | every served script pinned | — | PARTIAL | Next.js 16 SRI is ignored by Turbopack and, under webpack, pinned only 5 of 11–17 scripts per page (measured); not adopted. Options: upstream support, or a hash-pinned self-hosted loader |
| 11 | Web app CSP for the crypto core | apps/web | software (future) | — | — | open by design | when the web app integrates the wasm core it must add `'wasm-unsafe-eval'` (shown necessary in the Chromium harness) |
| 12 | Privacy Relay outage and retry-storm fault tests | Messaging / Privacy Relay | test coverage | — | — | not exercised | add a cuttable gate in front of Privacy Relay in `cross_service_e2e` |
| 13 | Branch protection | repository | administration | rulesets visible through the API | repository admin | **not enforced** (API: `main` and `staging` `protected: false`) | apply the rules on issue #14 (requires repository administration; no ruleset was changed in this work) |
| 14 | Compromised web origin | apps/web | threat model | — | — | accepted residual risk | malicious code served by the CF Meet origin can read whatever the page can; CSP / Trusted Types limit injection, not a hostile origin |

---

## K. 31-gate production acceptance matrix

Gate names and order as in `part-k-certification-evidence.md` §7.

| # | Gate | Previous | Final | Tests / evidence | Remaining dependency |
|---|---|---|---|---|---|
| 1 | Domain correctness | PASS | PASS | `conversations`, `messaging`, `engagement`, `integrations`, `compliance` | — |
| 2 | Migrations, constraints, DB role isolation | PASS | PASS | migration smoke up / reset / up incl. migrations 14–17; per-service roles; `migrations` | — |
| 3 | Tenant / IDOR isolation, every API family | PARTIAL | **PASS** | §C: 84 operations / 850 denials + 7 internal / 49 | — |
| 4 | Realtime delivery guarantees | PASS | PASS | `realtime`, `realtime_redis` (incl. Redis outage), §F | — |
| 5 | MANAGED attachments with real storage and scanner | PASS | PASS | `attachments` (real MinIO, real clamd), M11 journey | — |
| 6 | Encrypted-media processing | PARTIAL | PARTIAL | thumbnails + measured inspection for MANAGED (`attachments`, `media_inspect` unit tests); PRIVATE_E2EE never processed (gate, query, DB trigger); client-encrypted derivatives remain the client's | §J 9 (transcoding not built; derivative bytes not metered) |
| 7 | Meeting migration (real processes) | PASS (software) | PASS | UM e2e | — |
| 8 | Breakout / private Meeting chat migration | PARTIAL | **PASS** | §D, real-process e2e | — |
| 9 | External developer journey | PARTIAL | **PASS** | §G.1 | — |
| 10 | First-party integration | PASS | PASS | UM e2e, `integrations` | — |
| 11 | Privacy Relay revocation (real processes) | PASS | PASS | scenario 4 | — |
| 12 | SMS Bridge software (real processes, synthetic carrier) | PASS | PASS | scenario 5 + gateway-death journey | — |
| 13 | SMS public carrier edge | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | — | §J 1 |
| 14 | Webhook privacy (real processes) | PASS | PASS | scenario 9; M11 relay assertions | — |
| 15 | Concurrency | PARTIAL | **PASS** | legal-hold race (defect fixed), finalization races, §E | — |
| 16 | Capacity | PARTIAL | PARTIAL | §F (dev host, all workloads named in Part K now measured) | §J 6, 8 |
| 17 | Fault injection | PARTIAL | **PASS** | PostgreSQL loss, SMS Gateway death, Redis loss + previously certified cases (§E) | §J 12 (residual coverage) |
| 18 | Authentication / authorization security | PASS | PASS | §C, `foundation`, service-auth suites | — |
| 19 | MLS conformance (server, vectors) | PASS | PASS | RFC 9420 vectors, `mls_compatibility`; client core in Chromium | — |
| 20 | Real-device MLS runtime | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | Chromium runtime evidence (§G) — not a device | §J 4 |
| 21 | Independent cryptographic review | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | — | §J 2 |
| 22 | KT external witness | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | witness software tested (`transparency_witness`) | §J 3 |
| 23 | Push provider, media processing | BLOCKED_EXTERNAL / PARTIAL | BLOCKED_EXTERNAL | adapters tested against verifying fakes (`push_adapters`); media moved to gate 6 | §J 5 |
| 24 | Device recovery | PARTIAL | **PASS** (software) | `history_transfer` (server), crypto unit tests, Chromium journey | devices: gate 20 |
| 25 | Web XSS / CSP / Trusted Types | NOT_TESTED | **PARTIAL** | `apps/web` browser-security + CSP tests in Chromium; harness under strict CSP | §J 10, 11 |
| 26 | Dependency advisories (all lockfiles) | FAIL | **PASS** | §B: 0 vulnerabilities in every lockfile | — |
| 27 | Licence / SBOM / image / secret scans | PASS | PASS | licence gates (incl. the new crates), SBOM job, image security jobs (incl. secret scan of each image filesystem) | — |
| 28 | Compliance (retention, holds, exports, purge, healthcare) | PASS | PASS | `compliance` (incl. the hold race) | — |
| 29 | Production E2EE disabled, fail-closed | PASS | PASS | `operations`; history transfer refuses with `E2EE_DEVICES_DISABLED` in production | — |
| 30 | Hosted staging deployment | NOT_TESTED | NOT_TESTED | — | §J 6 |
| 31 | Owner production approval | BLOCKED_EXTERNAL | BLOCKED_EXTERNAL | — | §J 7 |

**Totals (31 gates):** PASS 21 · PARTIAL 3 · FAIL 0 · BLOCKED_EXTERNAL 6 · NOT_TESTED 1
(previously PASS 14 · PARTIAL 8 · FAIL 1 · BLOCKED_EXTERNAL 6 · NOT_TESTED 2).

| Declaration | State |
|---|---|
| SOFTWARE COMPLETE | NO — three PARTIAL items remain (§J 8–10); none is a known defect |
| INTEGRATION READY | YES (software): every cross-service edge passes with real processes and synthetic external parties |
| EXTERNAL EDGE CERTIFIED | NO: carrier, crypto review, devices, KT witnesses, push |
| PRODUCTION PASS | NO |

**Production readiness: NOT APPROVED** — gates 13, 20, 21, 22, 23 and 31 need external
parties or the owner, gate 30 needs an authorized hosted staging environment, and gates 6,
16 and 25 are PARTIAL.
