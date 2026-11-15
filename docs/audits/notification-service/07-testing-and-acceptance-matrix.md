# Document 7 — Testing and Acceptance Matrix (revision 2: unified service)

*"NS" means the unified notification service. Old pipelines are called "legacy".*

**Test levels used below:**

| Level | Meaning |
|---|---|
| **U** | Unit test |
| **I** | Integration test against real PostgreSQL. This is the existing practice: every service's `tests/` runs against `postgres:16` in CI |
| **C** | Cross-service certification. These run real processes, like `certification/` and `communications-e2e.yml` |
| **S** | Staging or production shadow |
| **P** | Production acceptance through a coordinated-release operation, using dedicated acceptance accounts, as `communications-verify` and `organization-acceptance` do today |

**Production acceptance is never claimed from U or I alone (Part 2 rule 13).**

**Count summary** (canonical; also in README §Canonical register):

| Group | IDs | Count |
|---|---|---|
| Functional | F-01 … F-23 | 23 |
| Reliability | R-01 … R-23 | 23 |
| Security | ST-01 … ST-20 | 20 |
| Migration and fence | M-01 … M-21 | 21 |
| **Test cases total** | | **87** |
| Production acceptance measures | G-01 … G-28 | 28 |
| Delivery evidence gates | E1 … E4 (Document 4 §14.3) | 4 |

Security *tests* use `ST-nn`. Security *issues* in Document 8 use `S-n`.

Each test row also states which adapter it needs:
- **T** = an internal test adapter, such as `LOCAL_TEST` push, `RecordingProvider` email, or fake providers.
- **R** = real provider credentials.

---

## 1. Functional tests

| ID | Scenario | Levels | Adapter | Pass criteria |
|---|---|---|---|---|
| F-01 | Each of the 31 inbound event types (30 core + 1 optional) and the `notifications.cancel` control event produces the expected notification, channels and template | U, I | T | golden output per type; forbidden keys refused |
| F-02 | Web-only account (no mobile, no push endpoint) receives in-app and email, and no push is attempted | I, C | T | deliveries contain no PUSH row |
| F-03 | Multiple linked browsers: read on browser A updates the unread count on browser B | I, C | T | U2 on B reflects the change within the poll interval; `version` increments |
| F-04 | One-primary-mobile replacement: the old phone's push endpoint is disabled, the new phone gets subsequent wake-ups, and the old phone gets none | C | T | follows ADR 0060. Account → Hub event → `notif_push_endpoints.enabled=false` in the same transaction as receipt |
| F-05 | Organization role change, suspension and removal: in-app notice to the member; pending ORG_* deliveries cancelled on removal | I, C | T | `cancelKey` applied; no notice after removal |
| F-06 | Meeting reschedule, cancel and reminder via NS: one email per recipient, time shown in the recipient's time zone, protected titles shown as "CF Meet meeting" | I, C | T | matches today's `tests/profile_notifications.rs` assertions, re-run through NS |
| F-07 | Reminder cancelled by meeting cancellation before it fires | I | T | `notifications.cancel` → delivery CANCELLED, inbox item dismissed |
| F-08 | Missed call → in-app (+ push if an endpoint exists), coalesced per caller within 10 min | I, C | T | one group item with `groupCount` = n |
| F-09 | Voicemail on an EXTENSION/QUEUE mailbox → notice to the principals Calling resolves | C | T | recipients equal Calling's resolution |
| F-10 | Message wake-up (PRIVATE_E2EE and MANAGED) via the NS dispatch keeps the 5-key payload | I | T | adapter whitelist test passes; DB CHECK unchanged |
| F-11 | Preferences: category off → no push or email, in-app silent; channel off; quiet hours defer push; contact mute and conversation mute | U, I | T | Document 4 §7.2 cases 1–12, one test each |
| F-12 | Security category ignores mute, quiet hours and category-off | U, I | T | always delivered |
| F-13 | Old client PUTs the 7 legacy preference booleans without the new fields | I | T | new fields unchanged (no reset) |
| F-14 | Inbox pagination stable under concurrent inserts; ETag 304 on unchanged unread | I | T | no item skipped or duplicated across pages |
| F-15 | Bulk read `upTo` cursor, bulk dismiss by category | I | T | counts correct; idempotent replay |
| F-16 | Mobile route contract includes U1–U9 | U | — | `apps/mobile/tests/routes.test.ts` passes |
| F-17 | Token-bearing invitation emails are sent by the service from a sealed token; the inbox copy and logs never contain the token | I | T | the token is present only in the sealed body until the final state; grep of logs, inbox and admin APIs returns nothing |
| F-18 | Ported email ledger parity: every test in `services/meeting-service/tests/email_delivery.rs` passes against the service ledger (idempotency, temporary vs permanent failure, expiry, lease recovery, Svix authentication, monotonic status) | I | T | all pass unchanged in intent |
| F-19 | Template parity: for the same input, the service renders the same subject and body (hash) as today's Meeting templates (`templates/email/*`) for every email category | U | T | equal hashes (or reviewed, intended diffs) |
| F-20 | OTP journey through the service: register → Account outbox row (sealed code) → service ledger → provider receives the 6-digit code → content erased → wrong/right/reused code → sign-in | I, C | T (R in P) | identical outcome to `otp_journey_from_registration_to_session` |
| F-21 | Preference facades: old clients using Hub `notification-preferences` (7 booleans), conversation and contact mute endpoints read and write through the service with identical responses | I, C | T | contract tests on the Hub 1.3.0 schema pass |
| F-22 | Reminder scheduling in the service: schedule, reschedule (old reminder cancelled, new created), cancel, recurring occurrences, protected titles via X2 | I | T | matches `s11_reminders_once_per_start_*` behaviour |
| F-23 | Platform-tenant push via the Messaging `registerPush` facade | I | T | registration lands in `notif_push_endpoints` with the tenant scope; wake-up is metadata-only |

## 2. Reliability tests

| ID | Scenario | Levels | Pass criteria |
|---|---|---|---|
| R-01 | Duplicate events: same `eventId`, same digest | I | `DUPLICATE`, no new rows |
| R-02 | Same `eventId`, different body | I | `REJECTED IDEMPOTENCY_CONFLICT` |
| R-03 | Retried jobs: the producer outbox retries after NS 503 | C | single notification |
| R-04 | Offline device: push provider 410 / unregistered | I | endpoint disabled (`INVALID_HANDLE`) only if the handle is unchanged (as Messaging does today) |
| R-05 | Provider unavailable: 20 consecutive 5xx | I | breaker opens, deliveries DEFERRED, half-open after 60 s, recovery delivers before `expiresAt`, expired items EXPIRED |
| R-06 | NS restart mid-batch | I | leases expire; re-claim; email produces no duplicate ledger row (same key) |
| R-07 | Worker crash after transport call, before commit | I (fault injection) | email exact-once at the ledger; push duplicate wake-up ≤ 1 with the same collapse ID |
| R-08 | Queue backlog: 50k PENDING events | S | ingest returns 503 + `Retry-After` above the threshold; producers keep events; drain completes; no loss |
| R-09 | Concurrent workers: 3 NS instances, slow provider (timeout = 10 s) | I | 0 duplicate sends (the lease-renewal fix) |
| R-10 | Migration during live traffic: authority switch at fence T with steady event flow | S | jobs before T legacy, at or after T NS; ledger rows = unique (job, recipient) |
| R-11 | Rollback with in-flight events | S | Document 5 §9 row 4.3: new messages route back to the old path; the service drains its own rows; no message in both ledgers; no loss |
| R-12 | **NS completely down** | C | sign-in, OTP, call ringing/answer, message send/receive and webhooks all pass (C1–C6 journeys) |
| R-13 | Hub down | C | X4 restriction checks fail → social-category deliveries retried (not failed) for ≥ 30 min; preference evaluation continues (preferences are local after Phase 4.2); security notices unaffected |
| R-14 | Owner read API down (X1 Account contacts or X2 conference recipients returns 503) | I | affected deliveries retried with backoff until `expiresAt`; other categories and in-app unaffected; CRITICAL events carrying their own recipient are unaffected |
| R-15 | Delayed and reordered events: reminder arrives after cancel | I | reminder dropped (subject version) |
| R-16 | Calling push fast path with NS down or slow | C | `calling_call_ring_seconds` p95 unchanged versus baseline; call established via WS |
| R-17 | CRITICAL lane under a STANDARD flood (50k STANDARD pending) | I, S | CRITICAL p95 handoff unchanged; STANDARD backpressure does not refuse CRITICAL ingest |
| R-18 | Both replicas down for 10 min **with fallback OFF** (test configuration only) | C | codes are delayed, not lost (Account outbox); delivered after recovery if not expired; expired codes are not sent |
| R-19 | FORCED fallback | C, P | `critical_fallback = FORCED` set by one operation **without a container restart**; new code emails go through the old ledger within the 5 s routing cache; no message in both ledgers; switching back restores `NEW` routing for new decisions only |
| R-20 | One replica killed mid-batch | I | the other replica completes; no duplicate (lease renewal + key) |
| R-21 | **AUTO failover, both service replicas stopped** | C, P (maintenance window) | a verification code requested during the outage is delivered through the old path within 60 s + delivery time; it is the **same** code (`fallback_sealed`); when the replicas return, the service's committed copy gets `REVOKED` from X6 and is never sent |
| R-22 | **Service database unavailable** (`cfmeet-core-db` stopped in staging; the Meeting DB in its separate container stays up) | C, S | sign-in and code verification work; AUTO failover delivers codes via the old path; on recovery no duplicate (lease audit) |
| R-23 | Fallback readiness broken (legacy worker credentials removed in staging) | C | certify fails "fallback readiness"; the activation operation for any CRITICAL routing change refuses |

## 3. Security tests

| ID | Scenario | Levels | Pass criteria |
|---|---|---|---|
| ST-01 | Forged event: wrong key, wrong audience, expired token, replayed `jti` | I | 401 `INTERNAL_AUTHENTICATION_FAILED`, no rows |
| ST-02 | Producer identity spoof: Calling token claiming `producer: cfmeet.organizations` | I | REJECTED |
| ST-03 | Event type outside the producer allowlist | I | REJECTED `UNKNOWN_EVENT_TYPE` |
| ST-04 | Cross-tenant / cross-account: authorization-matrix suite for U1–U9 and I1–I12 and X1–X7 (foreign IDs, other account's items, preferences or devices, revoked session, platform-tenant scope) | I | 404 / 401; table fingerprints unchanged (Messaging matrix pattern) |
| ST-05 | Revoked device delivery: revoke → a queued push is not sent | I, C | DISABLED |
| ST-06 | E2EE payload confidentiality: inspect every PUSH payload for PRIVATE_E2EE | I | only `{v,type,conversationId,eventSequence,sequence}` (messages) or the call schema; Web Push ciphertext decrypts to metadata only (RFC 8291 vector) |
| ST-07 | Token disclosure: APIs, admin endpoints and logs never return or print push handles, invitation tokens or OTPs | U, I | grep-based log test; response schema has no handle field |
| ST-08 | Preference bypass: an event claiming `priority: HIGH` cannot skip mute unless the category is security | U | suppressed |
| ST-09 | Stale org membership: `membership.removed` then a pending ORG_* delivery | I | CANCELLED |
| ST-10 | Replay and idempotency attacks on U3–U5 (same key, different body) | I | `IDEMPOTENCY_CONFLICT` |
| ST-11 | Dangerous links: no URL is returned by U1; `action.kind` is a closed enum | U | schema validation |
| ST-12 | Shared handle: the same Web Push subscription registered by two accounts | I | second registration refused (`UNIQUE handle_digest`) |
| ST-13 | Erasure: account erased → all NS rows for `recipient_ref` gone within 24 h | I | 0 rows |
| ST-14 | CRITICAL priority and `sealed` payloads accepted only from the producers and event types allowed for them (Account for `account.*`; Meeting for invitation types) | I | REJECTED otherwise |
| ST-15 | Forbidden data keys (`email`, `token`, `content`, …) in an event | U | REJECTED |
| ST-16 | Public exposure: no `/internal/*` NS or Meeting route reachable from the internet | P (certify probe) | nginx 404 |
| ST-17 | Sealed-content handling: codes, tokens and addresses are decryptable only by the renderer; absent from logs, inbox, push payloads and admin APIs; email bodies erased at the final state; expired CRITICAL items cannot be replayed (I6) | U, I | grep and schema checks pass; replay refused |
| ST-18 | Owner APIs X1–X7 refuse every caller except `cfmeet.notifications`, and X5 never returns an email address | I | 403; schema check |
| ST-19 | Fallback key handling: `fallback_sealed` is decryptable only with Account's local key; never logged; startup self-test fails closed if the key is missing | U, I | grep and schema checks; self-test |
| ST-20 | Lease forgery and bypass: X6/X7 refuse every caller except `cfmeet.notifications`; the service cannot send a CRITICAL email without a granted lease (`notif_critical_send_without_lease_total` stays 0 under fault injection); a `REVOKED` lease aborts the send | I | 403; metric 0; no provider call |

## 4. Migration-specific tests

| ID | Scenario | Pass criteria |
|---|---|---|
| M-01 | Shadow mode makes no transport call (spy transport) | 0 calls |
| M-02 | Shadow comparator classifies matched, missing, extra and mismatch correctly (synthetic) | exact counts |
| M-03 | Routing fence: messages created before the fence exist only in the old ledger, after it only in the service ledger; no message in both (SQL join on idempotency key across both databases) | 0 overlaps |
| M-04 | Fence boundary at millisecond granularity (job created at T−1 ms vs T) | the correct single owner |
| M-05 | Routing mismatch detection: service ACTIVE while the producer is still routing to the old path (mis-set) | the producer decision still sends one message to one system; certify reports FAIL "routing disagreement" |
| M-06 | Push registry copy path (Q6 > 0, or Web Push activated in Messaging first): copy, facade switch with freeze, decisions per `event_id`, verify counts and that sealed handles round-trip | equal counts; no plaintext handle in the NS DB |
| M-07 | Producer flag off mid-stream | no further events; already-queued rows handled |
| M-08 | Drain completion: old ledger non-final rows reach 0; Resend events for old message IDs still applied by the old endpoint during the drain | 0 stuck rows |
| M-09 | Preference copy + catch-up + freeze: counts and versions equal; writes during the freeze are retried successfully; rollback write-back restores Hub values | equal; no lost write |
| M-10 | Sticky decision: the same idempotency key routed twice (business retry, duplicate request), including across a routing-mode change | one decision, one row in one system |
| M-11 | Guard trigger: old-path insert without an `OLD` decision is refused when the category is not `LEGACY_ONLY`, and allowed in `LEGACY_ONLY` | `ROUTING_DECISION_REQUIRED`; no row |
| M-12 | Mixed producer versions: one pre-fence and one fence-capable replica running | the activation operation refuses (certify: `routingProtocol` missing); if forced in a test, the trigger rejects the pre-fence writes; 0 duplicates |
| M-13 | Rollback floor: deploying a pre-fence producer image while any category is not `LEGACY_ONLY` | the release tooling refuses |
| M-14 | Crash matrix (kill points: before commit; after commit, before relay; after the service commit, before ACK; after provider accept, before the service commit) | exactly one system holds the message; the outcome per kill point matches Document 4 §9.7 |
| M-15 | Unilateral reroute of an unacked message: `dryRun` probe `UNKNOWN` → reroute; probe `KNOWN` → refuse | correct per case; F2 holds |
| M-16 | Two-phase reroute (I11): `CANCELLED` items rerouted, `IN_FLIGHT_OR_SENT` items not; invitations reported `emailDelivery: FAILED` | correct per item |
| M-17 | CRITICAL lease protocol: never-leased → automatic reroute, exact; live lease → blocked; completed → terminal; leased-uncompleted → FORCED only, recorded `REROUTED_POSSIBLE_DUPLICATE` | as specified in Document 4 §9.8 |
| M-18 | Reminder time partition: starts `< B` reminded only by Meeting, `>= B` only by the service, with ±5 min clock skew injected | exactly one reminder per start |
| M-19 | Audit detection: an injected violation (the same key in both ledgers) is detected within one cycle and the category is frozen (`FENCED_OLD`) | detected; frozen; alert delivered |
| M-20 | Service mixed versions: protocol N and N−1 accepted; unsupported protocol → 503 `PROTOCOL_UNSUPPORTED` and the producer retries | no loss; no duplicate |
| M-21 | Mode disagreement: producer `FENCED_NEW`, service `OFF` | event `HELD`, never sent, alert; resumes when fixed |

## 5. Production acceptance (baseline-derived)

The targets are formulas over the **Phase 0 baseline** (Document 5 §2). **Every `B(·)` is BLOCKED until the matching query (Q1–Q18) is MEASURED.** All 18 are OUTSTANDING as of 2026-11-15. Email baselines are taken over the 14 days after gate E1. No absolute performance guarantee is invented. Notation:
- `B(x)` = the baseline value of metric x over the 14 days before the stage;
- `σ` = the baseline daily standard deviation.

Every measure has an ID (G-nn) so it can be counted and referenced. "BLOCKED" means the baseline query is OUTSTANDING.

| ID | Gate (phase) | Measure | Target | Baseline dependency |
|---|---|---|---|---|
| G-01 | A1 dark (P1) | `/ready` uptime 24 h; MemAvailable after deploy | 100 %; ≥ 1,024 MiB above gate (resource gate) | Q16 |
| G-02 | A2 shadow exit (P2) | Event capture `ingested/emitted` | ≥ 99.9 % per producer | — |
| G-03 | A2 | Recipient + policy mismatch versus legacy | ≤ 0.1 %, every case explained | — |
| G-04 | A2 | p95 ingest-to-processed lag | ≤ max(60 s, 2 × `B(producer outbox p95 lag)`) | Q10–Q12 |
| G-05 | A2 | Producer outbox DEAD for the service destination | 0 | — |
| G-06 | A3 in-app (P3) | U1/U2 error rate | ≤ 0.5 % 5xx | — |
| G-07 | A3 | U2 p95 latency | ≤ `B(Hub /bootstrap p95)` (a comparable relay read) | Hub metric (not a Q; measure during P2) |
| G-08 | A3 | Wrong-recipient reports | 0 | — |
| G-09 | A3 | Notification loss: events accepted versus items created (in-app categories) | ≥ 99.9 % (remainder explained by policy suppression) | — |
| G-10 | A4.1 push | Invalid-handle rate | ≤ `B` once established (first activation: track only) | Q6/Q7 |
| G-11 | A4.1 | Payload validation failures | 0 | — |
| G-12 | A4.1 | Push to a revoked device | 0 | — |
| G-13 | A4.1 call push | `calling_call_ring_seconds` p95 / answer rate | unchanged within `B` ± 2σ | Calling metric over the 14 days before 4.1 |
| G-14 | A4.1 call push | Ringing works with the service stopped (R-12/R-16, maintenance window) | PASS | — |
| G-15 | A4.2 preferences | Copy parity; facade error rate | 100 % equal; ≤ baseline Hub error rate | Q8/Q9 |
| G-16 | A4.3 standard email | **E1 and E2 passed** (Document 4 §14.3) | required before the first category moves | — |
| G-17 | A4.3 | Messages per (notice, recipient) across both ledgers; audit violations (Document 4 §9.10) | exactly 1; 0 violations | — |
| G-18 | A4.3 | Email failure rate (FAILED+EXPIRED / created) | ≤ `B` + 2σ | Q1/Q2 after E1 — **BLOCKED** |
| G-19 | A4.3 | Delivery latency, notice created → provider ACCEPTED, p95 | ≤ `B` + 30 s | Q17 after E1 — **BLOCKED** |
| G-20 | A4.5 security notices | **E3 passed** | required | Q1/Q17 after E1 — **BLOCKED** |
| G-21 | A4.6 critical lane | **E4 passed**; R-19, R-21, R-22 passed in production within 7 days | required | — |
| G-22 | A4.6 | p95 code email handoff (outbox commit → provider ACCEPTED) | ≤ `B(Q17, AUTH_*)` + 5 s | Q17 after E1 — **BLOCKED** |
| G-23 | A4.6 | Code email failure rate; loss | ≤ `B`; 0 lost (every AUTH_* decision reaches a terminal state) | Q1/Q2 after E1 — **BLOCKED** |
| G-24 | A4.6 | `email-acceptance confirm=otp` through the service, with delivery evidence | PASS at every canary step | — |
| G-25 | A4.6 | CRITICAL sends without a lease; possible-duplicate reroutes | 0; each one recorded and explained | — |
| G-26 | A4.6 (operational acceptance) | AUTO or FORCED fallback use | 0 during the 60-day window | — |
| G-27 | Rollback drill (each stage) | Time from trigger to the old path for new decisions | ≤ 1 coordinated-release operation, no restart; no duplicate; no loss (audit) | — |
| G-28 | User-visible consistency | Read on one device reflected on another | within the poll interval (30 s) | — |

## 6. Test adapters versus real credentials

| Check | Internal test adapter sufficient | Needs real credentials (owner-provisioned) |
|---|---|---|
| Event ingest, policy, inbox, dedupe, fencing, rollback | ✔ | |
| Email orchestration up to the ledger | ✔ (`RecordingProvider`) | |
| Email actually delivered (gates E1–E4) | | ✔ real delivery evidence (Document 4 §14.3): Resend with webhook secret (`email.delivered`), **or** SMTP `250` plus receipt in an owner-controlled acceptance mailbox. Provider activation is the owner's E-track, independent of service development |
| Old-path fallback drill (`confirm=fallback`) | | ✔ the Meeting Service's own provider credentials (kept mounted until Phase 8) |
| Push payload shape, whitelist, RFC 8291 encryption, SSRF guard | ✔ (fakes, existing `tests/push_adapters.rs`) | |
| Web Push to a real browser | | ✔ VAPID key (generated on the VPS by `activate-web-push`) plus a real browser subscription (Chromium in CI can subscribe through FCM's endpoint, so this is verifiable in the coordinated-release journey) |
| FCM to a real Android device | | ✔ FCM service account |
| APNs (alert) / APNs VoIP to a real iPhone | | ✔ Apple Developer Program `.p8` key, topic, a physical device |
| Calling background ring (CallKit/ConnectionService) | | ✔ as above, plus the mobile build |
| SMS | n/a (not in scope; disabled) | — |

**Certification boundary:** until each row needing real credentials passes in production, its capability stays `IMPLEMENTED_PENDING_CONFIGURATION`. This matches the convention in `docs/operations/*`.
