# Document 10 — Engineering Report: N2–N15 and the Resend E-track

**Status (2026-11-16):** `CFMEET_NOTIFICATION_SERVICE_N2_DEPLOYED_DARK` — see §1 for the per-section status. The final engineering goal `CFMEET_UNIFIED_NOTIFICATION_SERVICE_ENGINEERING_COMPLETE` is **not** claimed: producer publications (N7), the client integrations (FE2–FE5) and the cutovers (N9 Messaging facades, N10 preference switch, N11, N12) are not built, and gate E1 waits on the owner's Resend credentials.

Statuses are kept separate, as the mission requires:

| Status | Meaning |
|---|---|
| `ENGINEERING_COMPLETE` | code merged, tests green |
| `PRODUCTION_DEPLOYED_AND_VERIFIED` | released by the coordinated release and verified / certified in production |
| `OPERATIONALLY_ACCEPTED` | the 30-day (60-day CRITICAL) observation window has passed with its gates |
| `LEGACY_RETIRED` | the old pipeline was removed with the evidence of Document 5 §10 |

---

## 1. Section status

| Section | Engineering | Production | Accepted | Evidence |
|---|---|---|---|---|
| **E-track E1** (Resend on the existing Meeting path) | built (N0-era tooling) | **SUSPENDED by owner decision (2026-11-16)**: no activation, no acceptance run; run [43807428863](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43807428863) had stopped at the credential check, nothing changed | — | §3 |
| **N2** service foundation | `ENGINEERING_COMPLETE` (PR #75, readiness fix PR #80) | deployed dark (release 43819010350, verification PASS); certify 43828926604 **NOT_CERTIFIED** (readiness), fix in PR #80, re-release and re-certify in §4.2 | 24 h dark observation restarts at the certified release | §2.1, §4 |
| **N3** ingest, routing, preferences engine, shadow | `ENGINEERING_COMPLETE` (service side, PR #75) | deployed dark with N2 (43819010350) | n/a (no traffic until N7) | §2.2 |
| **N5** email channel | `ENGINEERING_COMPLETE` (PR #75) | deployed dark, provider `none` | E2 needs E1 | §2.3 |
| **N6** owner APIs X1–X5 | `ENGINEERING_COMPLETE` (PR #76); Meeting U1–U9 relay (owner-approved) in the relay PR, §2.4 | released with the readiness fix (§4.2); owner activation recorded in §4.2 | n/a | §2.4, §4.2 |
| **N7** producer publications + routing fence | **N7a fence foundation built** (Meeting + Messaging tables, inert guard triggers, decision module, `routingProtocol`, rollback floor); publications not built | — | — | §2.8, §5 |
| **N8** inbox | service side + U1–U5 built (PR #75); Meeting relay §2.4; web/mobile UI not built | — | — | §2.5 |
| **N9** push + call fast lane | service side built (U8/U9, dispatcher, I3); Messaging facades, Calling transport, service worker not built | — | — | §2.5, §5 |
| **N10** preferences authority | U6/U7, change log, Hub copy function built; copy job, facade switch not built | — | — | §2.5, §5 |
| **N11** standard / invitation email cutover | service rendering and sender profiles built; producer routing not built | — | gate E1/E2 | §5 |
| **N12** critical lane | X6/X7 client and lease-guarded CRITICAL worker built; Account X6/X7 endpoints, PW1 relay, AUTO/FORCED fallback not built | — | E3/E4 | §5 |
| **N13** monitoring | rules + promtool tests + runbook + metrics built; Prometheus/Alertmanager deployment with receivers is owner decision D-9 | — | — | §2.6 |
| **N14** canary governance | gated routing operation built (`activate-notifications-routing`) | — | — | §2.7 |
| **N15** retirement | not applicable: no category has moved; every legacy pipeline stays operational | — | — | §6 |

---

## 2. What was built

### 2.1 N2 — `services/notifications`

| Item | Result |
|---|---|
| Crate | `services/notifications` (`notifications`, `notifications-migration`); identity `cfmeet.notifications`; depends only on shared packages (`service-auth`, `notification-client` with `open`, `push-transport`, `webhooks`, `communications-policy`, `migration-guard`) |
| Migration | `m20261116_000001_notifications_foundation`: the 17 `notif_` tables of Document 4 §6; history `notifications_seaql_migrations`; every routing row seeded `OFF`; downgrade refuses while data exists |
| Deviations from the illustrative DDL | sealed columns are `TEXT` `secret_box` seals (`v2.<kid>.…`); `notif_event_inbox.shadow` + state `HELD`; email body stored as one sealed JSON document; `route_decision_id` and `sender_profile` on the ledger; `notif_preferences.version` starts at 0 (absent = defaults) |
| Probes | `/health`; `/ready` (migrations current, worker heartbeats ≤ 60 s, `routingProtocol: [1]`, provider kinds, owners, non-OFF route count, HELD count); `/metrics` |
| Configuration | file-only secrets (inline secrets refused), data master key (sealing + keyed digests, rotation with a previous key), optional HPKE opening key, email provider selection identical to Meeting under `NOTIFICATIONS_*`, sender profiles, push providers, owner APIs |
| Deployment | coordinated release entries `notifications` and `notifications-2` (one image, identity, database; 256 MiB each; private TLS route 8456 on the primary); provision creates the database, roles, signing key, empty trust bundle and data key; safe defaults keep providers `none` |

### 2.2 N3 — ingest, routing, preferences engine, shadow

| Item | Result |
|---|---|
| I1 | strict parse + `cf-notification-client::validate` with the authenticated issuer; per-event `ACCEPTED` / `DUPLICATE` / `REJECTED` (reason) / `HELD`; digest mismatch and decision reuse refused; `dryRun` probe (`DUPLICATE`/`UNKNOWN`); `PROTOCOL_UNSUPPORTED` 503; STANDARD backpressure (10,000 pending or 5 min), CRITICAL never refused; rejected bodies are not stored |
| Contract amendment | `cfmeet.notifications.internal-api` 1.1.0: I1 `shadow` flag — producer shadow copies are recorded and never delivered whatever the service mode |
| W1 | CRITICAL claimed first; owners asked at send time; per-recipient transaction keyed by `UNIQUE (recipient_ref, dedupe_key)`; SHADOW deliveries + observations with the render digest; control event `notifications.cancel`; sealed part removed from the stored envelope when processed |
| Precedence | rules 1–12 of Document 4 §7.2 as a pure function (unit cases 1–12); D-7 organization-policy slot empty |
| Catalogue | all 31 inbound types routed (routing category, preference category, channels, template, action, legacy email category) and the control event |
| Templates | Meeting templates ported verbatim; subject/body parity tested (verification, reminder) |
| Reminders | before boundary B the Meeting scan triggers reminders (`conference.scheduled` with a `reminder:` dedupe key); otherwise `conference.*` arm / cancel `notif_schedules` for starts ≥ B (W4 fires them through X2) |

### 2.3 N5 — email channel

Port of the Meeting ledger: `EmailProvider` with Resend / SMTP / recording / none, identical error classification and Svix verification, monotonic provider statuses. Fixed: UUIDv5 message ids (stable provider `Idempotency-Key`), sealed address / subject / body (body erased at final state), keyed recipient digests, mandatory expiry, CRITICAL and STANDARD lane workers (CRITICAL batch 4, never starved), per-row lease renewal, provider breaker (5 failures → 60 s), suppression table (complaint or two bounces; never the CRITICAL lane), sender profiles (`organization-invitation` is configuration, owner decision pending). CRITICAL messages with a route decision are sent only under an Account X6 lease, inside a 20 s deadline, and completed with X7. The public callback `POST /api/v1/notifications/provider-events/resend` answers 404 until a secret is configured.

### 2.4 N6 — owner APIs

Merged in PR [#76](https://github.com/CosmicforgeHealthnet/cfmeet/pull/76) (`main` `6850660`). Each owner answers from its own data, behind a service token signed by `notifications` and carrying that owner's permission. None of them is reachable from the public gateway.

| API | Owner | Implementation | Tests |
|---|---|---|---|
| X1 `resolveAccountContacts` | Meeting Service | `services/meeting-service/src/services/notification_owner.rs` | `tests/notification_owner.rs` |
| X2 `resolveConferenceRecipients` | Meeting Service | same module; uses the same recipient rules as the existing reminder worker (`notifications.rs` refactor), so the two paths cannot diverge | same |
| X3 `authorizeWakeups` | Messaging | `src/notifications.rs`, `realtime/push.rs authorize_wakeups`; metadata only, no message content | `tests/notification_wakeups.rs` |
| X4 `checkCommunicationRestrictions` | Communications | `src/restrictions.rs`; calls must carry the first-party delegated scope (`CFMEET_COMMUNICATION_*`); group wake-ups use action `OTHER` | `tests/notification_restrictions.rs` |
| X5 `resolveInvitationInvitee` | Organizations | `src/http/invitations.rs resolve_invitee` | `tests/notification_owner.rs` |

The Meeting owner routes stay off until `NOTIFICATION_OWNER_API_ENABLED=true`. They are switched on, with the other owners' grants, only by the coordinated-release operation `activate-notifications-owners`. That operation probes every owner afterwards and expects a 401 without a token.

**Meeting U1–U9 relay (owner-approved 2026-11-16).** The inbox, preferences and push-endpoint facade for the CF Meet apps: `GET /api/v2/notifications`, `GET …/unread-count`, `PATCH …/{notificationId}`, `POST …/read`, `POST …/dismiss`, `GET`/`PUT …/preferences`, `PUT`/`DELETE …/devices/{accountDeviceId}/push` (`services/meeting-service/src/handlers/v2/notifications.rs`, `src/services/notification_relay.rs`).

- The Account session is verified first: registered account, revocation checked, 401 otherwise.
- The request is then forwarded to I2 with the server-resolved account (`cfmeet-user:<id>`). The client never names the account.
- Malformed input is refused with 400 before anything is forwarded.
- Answers that are off-contract or fail service authentication become 503.
- Without `CF_NOTIFICATIONS_BASE_URL` every route answers 503 after the session check.

Gateway classification: the nine operations are `EXCLUDED` / `FIRST_PARTY_APP_NOTIFICATIONS` in `infra/api-gateway/public-routes.json`. They are served on the Meeting Service ingress to the apps' own signed-in users and are never routed as a customer API. No Notification Service route is public: it has no published port, and its private TLS route is on the private network only.

Production switch: the new operation `activate-notifications-relay` grants `notifications-user.forward` to the Meeting Service on both replicas and sets `CF_NOTIFICATIONS_BASE_URL`. It probes for 401 before and after.

Public-host hardening: `activate-notifications-owners` now applies and proves public-host denials of the Meeting owner routes X1/X2 (404 with `X-CFMeet-Ingress: denied`) before it mounts them.

Tests: `tests/notification_relay.rs` (4), `test_notification_operations.py` (relay and denial). Open contract question: X3 answers `409` above 1,000 recipients; the batch limit needs a contract decision before N9.

### 2.5 N8 / N9 / N10 service side

Inbox U1–U5 (own items only, 404 for foreign ids, cursor pagination, grouping, silent items not counted, versions, ETag on U2), preferences U6/U7 (contract document, optimistic version, change log, Hub copy function), push U8/U9 (ADR 0060 device ownership through X1, sealed handles never echoed, keyed digest uniqueness), push dispatcher (metadata-only payloads inside the transport whitelist, invalid handles disable the endpoint), I3 call fast lane (≤ 1 s overall, `NOT_CONFIGURED` until `CALL_INVITATIONS/PUSH` is ACTIVE), admin APIs I4–I12.

### 2.6 N13

`infra/monitoring/prometheus/rules/notifications.rules.yml` (8 alerts) with promtool tests; `docs/operations/notification-service-runbook.md`. `NotificationsUnavailable` uses only the scrape (`up`), so it fires when the service itself is down. Not yet evidenced: a fired alert reaching a person — Prometheus / Alertmanager are not deployed with receivers (D-9, owner).

### 2.8 N7a — routing fence v1 foundation (Document 4 §9)

**Meeting database.** Migration binary `notification-fence-migration` with its own history table `notification_fence_migrations`. Previous images neither know nor check it, so they still boot. It creates:
- `notification_routing_state`, seeded `LEGACY_ONLY` for `AUTH_VERIFICATION`, `AUTH_PASSWORD_RESET`, `SECURITY_DEVICE_STEP_UP`, `SECURITY_PASSWORD_CHANGED`, `SECURITY_DEVICE_LINKED`, `MEETING_INVITATION`, `MEETING_NOTIFICATION` and `ORGANIZATION_INVITATION`;
- `notification_route_decisions`, with an F3 guard trigger (SQLSTATE `CFN02`): only the audited NEW→OLD reroutes, acknowledgement and completion set once, leases one at a time, a completed decision is terminal;
- `account_notification_outbox`;
- `BEFORE INSERT` guard triggers on `email_deliveries` and `meeting_notifications` (`ROUTING_DECISION_REQUIRED`, SQLSTATE `CFN01`). They are inert in `LEGACY_ONLY`.

Category mapping:
- From `email_deliveries.category`. `NOTIFICATION` counts as `SECURITY_DEVICE_LINKED` only for `device-linked:` keys.
- From `meeting_notifications.kind`: the three conference kinds map to `MEETING_NOTIFICATION`.

The module is `src/services/notification_fence.rs`:
- `route_message`: sticky decisions under `FOR UPDATE`; the canary is SHA-256 mod 100, pinned to `cf-notification-client`; CRITICAL categories go OLD under `FORCED`.
- `reroute_unacked` and the CRITICAL lease helpers (X6 / X7 rules of §9.8).

`GET /health` reports `routingProtocol: 1` once the history is applied.

**Messaging database.** Migration binary `messaging-notification-fence-migration`, history `messaging_notification_fence_seaql_migrations`. The tables are `messaging_notification_routing_state` (seeded with `MESSAGE_WAKEUPS`, `LEGACY_ONLY`) and `messaging_notification_route_decisions`. The repository requires every Messaging table to start with `messaging_`, so these deviate from the unprefixed names of §9.2. There is a guard trigger on `messaging_push_jobs` (key `evt_<event id hex>`, which is the outbox event id) and a `route_message` module. `/ready` reports `routingProtocol: 1`, and the `cfmeet.messaging.common` contract goes to 1.26.0.

**Release tooling (§9.6):**
- Both fence migrations run with every release. Their histories are optional histories in the backup catalog.
- `verify` checks `routing_protocol` for both producers.
- The **rollback floor** (M-13): `deploy` and `rollback` refuse a producer image without the fence binary while any of that producer's categories is outside `LEGACY_ONLY`.
- `require_fence_protocol` (M-12) is the precondition that future fence-routing operations must call.

**Not built yet:**
- producer publications (SHADOW events);
- the Meeting `outbox_events` channel `NOTIFICATION`;
- deterministic keys for invitations;
- the PW1 relay and the decision purge;
- the hourly cross-ledger audit;
- the fence-routing activation operation;
- the M-14 crash matrix and M-16 two-phase reroute (I11).

**Tests:**
- Meeting `tests/notification_fence.rs`: 11 tests, plus 5 unit tests.
- Messaging `tests/notification_fence.rs`: 6 tests.
- Fence helpers in `test_notification_operations.py`.
- Every existing suite runs with the guard triggers installed.

### 2.7 N14 tooling

`activate-notifications-owners` and `activate-notifications-routing` (one row per run, gated, journaled, reversible) in the coordinated release.

---

## 3. Resend E-track (gate E1)

| Check | Result (2026-11-16) |
|---|---|
| DKIM `resend._domainkey.cfmeet.com` | published (TXT `p=MIGfMA0…`) |
| Return path `send.cfmeet.com` | CNAME `send.forge.rmta.net` → MX `10 feedback.forge.rmta.net`, SPF `v=spf1 ip4:52.3.252.119 ip4:44.222.39.36 ip4:199.249.231.0/24 ~all` |
| DMARC `_dmarc.cfmeet.com` | **absent** (recommended `v=DMARC1; p=none;`, not required for sending) |
| Resend verification status | not readable without the API key; to be confirmed in the Resend dashboard |
| Production secrets | `CFMEET_RESEND_API_KEY`, `CFMEET_RESEND_WEBHOOK_SECRET` and the variable `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT` are **not set** in the GitHub `production` environment |
| `activate-resend-email` | run [43807428863](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43807428863) on `4f9e4f2`: gate PASS, activation base PASS (no image input changed since `163be40`), stopped at the credential check; no VPS change |
| E1 | **not completed** (`PENDING_OWNER_EMAIL_PROVIDER_ACTIVATION`) |

**Owner decision (2026-11-16): Resend activation is suspended.**

- Credentials will **not** be stored in GitHub. When the work resumes, the owner will place them directly on the VPS as protected secret files.
- `activate-resend-email` and `email-acceptance` are not run until then.
- Engineering consequence when resuming: `activate-resend-email` currently reads the key and webhook secret from GitHub `production` secrets. It must first be changed to require pre-placed root-owned 0400 files on the VPS instead, and refuse to proceed otherwise. That change is not made yet; it waits for the resume decision.

Until E1, every email `ACTIVE` routing change stays refused by the N14 gate. The existing Meeting email path stays the only sender.

---

## 4. Production evidence

### 4.1 N2 dark release

| | |
|---|---|
| Merge | PR [#75](https://github.com/CosmicforgeHealthnet/cfmeet/pull/75) → `main` `5c9fb38f0ab8f3f9fd6dc83b0213788a54f685ea` |
| Release | run [43819010350](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43819010350) `release` on `5c9fb38`, success (07:43 UTC) |
| Scope | replaced: `notifications`, `notifications-2` (new), `calling`, `meeting-service` (build inputs changed since `163be40`); 11 services kept on `163be40` |
| Resource gate | MemAvailable 5,068 MiB; required 1,344 MiB (2 × 256 MiB + transient); remaining 3,724 MiB; shortfall 0 |
| Pre-release backup | 16 databases, `/opt/cfmeet-core/backups/43819010350-1/manifest.json`; restore-tested organizations, platform-access, meeting-service (PASS) |
| Provisioning | database `cfmeet_notifications` + roles, signing key, empty trust bundle, data key; private TLS route 8456 installed |
| Migration | `notifications-migration up`: `notifications_seaql_migrations` applied 1 (`m20261116_000001_notifications_foundation`) |
| Verification | 33 checks PASS (running ×15 incl. `notifications` and `notifications-2`, migrations ×11 incl. `notifications`, `provider_features_off`, internal-not-public, CORS, swagger, Organizations event delivery); violations: none; regression (Meetings health, Messaging/Calling/Platform TLS readiness, LiveKit keys) PASS |
| Dark state | every `notif_routing` row `OFF`, `NOTIFICATIONS_EMAIL_PROVIDER=none`, `NOTIFICATIONS_PUSH_PROVIDER=none`, no owner API, no producer grant: nothing can be delivered |
| Certification | run [43828926604](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43828926604) `certify` (read-only): **NOT_CERTIFIED**. The only failing check was `notifications` readiness. |

**Certification failure and fix.** `/ready` treats any worker heartbeat older than 60 s as stale. The worker loop wrote a `retention` heartbeat only when hourly retention ran, so readiness went red one minute after start and stayed red until the next hour. This was a fault in the readiness logic, not in delivery: every route was `OFF` and nothing was sent. Fix in PR [#80](https://github.com/CosmicforgeHealthnet/cfmeet/pull/80): retention runs inside the scheduler loop, whose continuous heartbeat already proves the loop is alive, and has no heartbeat of its own. The regression test `running_workers_keep_ready_green_beyond_their_first_minute` asserts that every heartbeat key belongs to a worker that beats continuously. A release prepared on `6850660` before the fix (run 43829150172) was cancelled in its gate, before deploy.

**N2 production status: deployed dark, release verification PASS, not yet certified.** It does not claim `PRODUCTION_DEPLOYED_AND_VERIFIED` until the post-fix release in §4.2 certifies. The 24-hour zero-traffic observation (Document 6 N2 row 14) restarts at that release.

### 4.2 Readiness-fix release, certification and owner activation

*(recorded when the runs complete)*

---

## 5. Remaining engineering (not built)

| Section | What remains |
|---|---|
| N6 | X6 / X7 on Account (Meeting) with the decision tables of N7 |
| N7 | (N7a foundation built, §2.8) PW1 relay; publications from Meeting, Messaging, Calling (destination + `CALL_PARTICIPANT_MISSED`, D-10), Hub (destination `NOTIFICATIONS`), Organizations (subscriber), Developer Platform; fence tests M-10 … M-17 |
| N8 | web / mobile inbox (FE2) on U1–U5 through the Meeting relay; the contact-request canary needs the Hub publication (N7) |
| N9 | Messaging registration facades and the copy of existing Web Push registrations (D-1b copy path), Calling `NotificationsPushTransport` on I3, the browser service worker for the service's VAPID key, real-device acceptance per platform |
| N10 | Hub copy + catch-up job, facade switch of the Hub preference endpoints, FE3 |
| N11 | Meeting / Developer Platform producer routing through the fence, reminder takeover operation, canary |
| N12 | Account X6 / X7, PW1 AUTO failover, FORCED mode, fallback drill (`email-acceptance confirm=fallback`), D-9 alerting |

---

## 6. Retirement and time-based acceptance

No category has moved to the service, so no 30-day, 60-day or 90-day window has started and every legacy pipeline (Meeting email ledger and worker, Meeting notice worker and reminder scan, Messaging push dispatcher, Hub preferences) remains the operating path. `LEGACY_RETIRED` is not applicable to any item.

## 7. Baselines Q1–Q18

Q11, Q14 and Q15 remain COMPLETED as recorded in Document 9 §7.8 (verified: the certify evidence of run 43792702821 and the activation runs 43788212474 / 43788404790 are unchanged). Q1–Q4 and Q17–Q18 need E1 (email baselines on a working transport). Q16 (7-day memory minimum) is still a single sample; the dark release adds 512 MiB of limits (§4).
