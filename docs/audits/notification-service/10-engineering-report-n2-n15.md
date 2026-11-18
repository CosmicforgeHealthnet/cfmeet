# Document 10 — Engineering Report: N2–N15 and the Resend E-track

**Status (2026-11-17):** every Notification Service section N2–N15 is engineered, merged and tested (PRs #95, #96, #99), released (coordinated release 44150771996 on `506bff4`, then `21c90c4`, §4.4) and activated in production. The service delivers in production: organization updates in the inbox; message wake-ups by Web Push; organization invitations, meeting invitations, meeting notifications and the device security emails by email through its own Resend configuration. The verdict is in §8. APNs and FCM are not configured: their credentials do not exist (§5).

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
| **E-track E1** (Resend on the Meeting path) | `ENGINEERING_COMPLETE` (PR #88) | **`PRODUCTION_DEPLOYED_AND_VERIFIED`**: `activate-resend-email` 43963096642, `email-acceptance otp` 43968733391; after the passwordless sign-in (PR #97) again 44145668186 (OTP journey 10/10) | E1 complete (Resend ACCEPTED plus signed DELIVERED) | §3 |
| **N2** service foundation | `ENGINEERING_COMPLETE` | **`PRODUCTION_DEPLOYED_AND_VERIFIED`**: both replicas released by 44150771996, certify 44170126155 `CERTIFIED_READ_ONLY` | — | §2.1, §4 |
| **N3** ingest, routing, preferences, shadow | `ENGINEERING_COMPLETE` | **live**: I1 accepts events from all five producers (§4.4) | — | §2.2 |
| **N5** email channel | `ENGINEERING_COMPLETE` | **live**: `activate-notifications-email` 44172839707, again with egress 44217118248 (provider `resend`, both replicas reach Resend, signed webhook); real email DELIVERED (§4.4) | — | §2.3, §4.4 |
| **N6** owner APIs X1–X7 | `ENGINEERING_COMPLETE` (X6/X7 on Account: PR #96) | **`PRODUCTION_DEPLOYED_AND_VERIFIED`**: owners 43922950325; X6/X7 `activate-critical-lane` 44174010149, public denial of the lease routes proven | — | §2.4, §2.10 |
| **N7** producer publications + routing fence | `ENGINEERING_COMPLETE`: Messaging, Organizations, Hub, Developer Platform, Meeting (sealed, fenced), Calling | **live**: publications activated for all six producers; fences `FENCED_NEW` for `MESSAGE_WAKEUPS`, `ORGANIZATION_INVITATION`, `MEETING_INVITATION`, `MEETING_NOTIFICATION`, `SECURITY_DEVICE_LINKED`, `SECURITY_DEVICE_STEP_UP` | — | §2.9, §2.10, §4.4 |
| **N8** inbox | `ENGINEERING_COMPLETE` (backend; the web/mobile inbox UI is client work, FE2) | **live**: `ORGANIZATION_UPDATES` and `MEETING_NOTIFICATION` IN_APP `ACTIVE`; relay U1–U9 live | — | §4.4 |
| **N9** push + call fast lane | `ENGINEERING_COMPLETE` (Web Push, registration mirror, I3; APNs/FCM adapters need credentials) | **live**: `activate-notifications-push` 44176266362, `activate-messaging-push-mirror` 44177553529, `activate-calling-push` 44177899990; `MESSAGE_WAKEUPS` PUSH `ACTIVE` + `FENCED_NEW` | — | §2.10, §4.4 |
| **N10** preferences authority | `ENGINEERING_COMPLETE` | **live, authority `SERVICE`**: `activate-preference-copy` 44178337597, `activate-preference-authority` 44178664093 (account 7/7, contact 70/70, conversation 0/0, 0 different) | — | §4.4 |
| **N11** standard / invitation email | `ENGINEERING_COMPLETE` | **live**: Meeting `ORGANIZATION_INVITATION`, `MEETING_INVITATION`, `MEETING_NOTIFICATION` `FENCED_NEW` with EMAIL `ACTIVE` | — | §4.4 |
| **N12** critical lane | `ENGINEERING_COMPLETE` (X6/X7, PW1 relay, AUTO/FORCED failover, fallback key; passwordless codes in the catalogue: PR #99) | **live**: `AUTH_VERIFICATION`, `SECURITY_DEVICE_STEP_UP`, `SECURITY_DEVICE_LINKED` `FENCED_NEW`; real sign-in codes sent under X6/X7 and DELIVERED; fallback drill 44175510575 DELIVERED; one incident (no egress) handled by rollback (§4.4) | — | §2.10, §4.4 |
| **N13** monitoring | `ENGINEERING_COMPLETE` (rules, promtool tests, runbook, metrics) | rules not deployed: Prometheus/Alertmanager with receivers is owner decision D-9 | — | §2.6 |
| **N14** canary governance | `ENGINEERING_COMPLETE` | used for every move in §4.4 (one row per run, journaled, reversible) | — | §2.7 |
| **N15** retirement | not started by design: the legacy paths stay as the failover and rollback target | — | — | §6 |
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

### 2.9 N7b — Messaging shadow publication (`MESSAGE_WAKEUPS`)

**Fan-out.** `fanout_jobs` reads the `MESSAGE_WAKEUPS` mode first.
- In `LEGACY_ONLY` (or with no fence history) it runs exactly the pre-fence insert, with no decision.
- Otherwise one transaction records the sticky decision (key `evt_<hex>`) and writes the push jobs (`OLD`). In `SHADOW` it also writes one shadow publication. A `NEW` decision writes only the publication.

**Outbox.** Fence history migration 2 (`m20261205_000002_notification_publications_v1`) adds `messaging_notification_publications`, one row per decision. A CHECK makes the envelope ids-only: the four catalogue data keys, no sealed part, at most 8 KiB. The same migration adds `RECIPIENT_LIMIT` to the decision `reason` CHECK.

**Relay to I1.** `POST /internal/v1/notification-events`, permission `notifications-event.publish`.
- The envelope is built once with `cf-notification-client`. Recipients are the X3 account refs.
- The envelope is re-posted byte for byte. `ACCEPTED`, `DUPLICATE` and `HELD` acknowledge the row. Refusals dead-letter it. 5xx, 401, 403, 408, 429 and transport errors retry with backoff until expiry.
- The relay runs only with `CF_NOTIFICATIONS_BASE_URL`.

**Category names.** The routing key is `MESSAGE_WAKEUPS`, which is the service's own route category for `messaging.message.created` (`catalogue::route_category`). The envelope category `MESSAGES` is the catalogue placeholder that resolves to `DIRECT_MESSAGES` or `GROUP_MESSAGES`. No change was needed.

**Large groups.** A group may have up to 10,000 members, but one envelope is limited to 4,096 bytes, which is about 50 account refs. So only an event with at most `NEW_ROUTE_MAX_CANDIDATES` = 40 push candidates may be routed `NEW`. A unit test builds the worst-case envelope for that many refs. A larger audience is decided `OLD` with reason `RECIPIENT_LIMIT` even inside the canary, so the legacy push still reaches every member. A shadow copy that does not fit is `SKIPPED`. Splitting one event across several envelopes is a contract question for the owner (D-x) before `FENCED_NEW`.

**Operations.**
- `activate-messaging-publications`: probes I1 for 401, requires the Messaging `routingProtocol` and the publication outbox, grants `notifications-event.publish` to `cfmeet.messaging` on both replicas, and sets `CF_NOTIFICATIONS_BASE_URL` on Messaging. I1 must answer 401 again after the change.
- `activate-notification-fence` (`confirm` `<service>-<category>-<mode>`): moves one producer row between `LEGACY_ONLY` and `SHADOW` only. It is refused for any row in a `FENCED_*` mode. SHADOW is allowed only for `FENCE_ROUTABLE` categories (today only `messaging`/`MESSAGE_WAKEUPS`), because Meeting's producers do not record decisions yet and its guard triggers would refuse legacy emails. It also needs `routingProtocol` and `activate-messaging-publications`. Each change is journaled with its previous mode, and `rollback` restores it.

**Tests:**
- Messaging `tests/notification_shadow.rs`: 8 tests, including a fake I1 that verifies the real token.
- Messaging `tests/notification_fence.rs`: 6 tests, now including `RECIPIENT_LIMIT`.
- Envelope capacity unit test.
- `test_notification_operations.py`: `MessagingPublications` and `ProducerFence`.

### 2.7 N14 tooling

`activate-notifications-owners` and `activate-notifications-routing` (one row per run, gated, journaled, reversible) in the coordinated release.

---

### 2.10 Waves 2 and 3: the remaining producers, N9, N10, N12 (PRs #95, #96, #99)

**Producers (N7, N11).** Each one has an ids-only transactional outbox and a relay to I1 that runs only with `CF_NOTIFICATIONS_BASE_URL`, and each has its own activation.

| Producer | Outbox | Events | Activation |
|---|---|---|---|
| Organizations | `organization_notification_publications` | invitation accepted, role changed, membership changes | `activate-organizations-publications` |
| Communications Hub | `communication_notification_publications` (history 5) | contact requests, group invitations | `activate-communications-publications` |
| Developer Platform | `devplat_notification_publications` | operator and production-access notices | `activate-developer-publications` |
| Meeting | `meeting_notification_publications` | `MEETING_INVITATION`, `MEETING_NOTIFICATION` (reminders included), `ORGANIZATION_INVITATION`, and the CRITICAL account categories | `activate-meeting-publications` |
| Calling | `calling_notification_publications` (own history) | missed call, voicemail ready, recording ready; first-party accounts only | `activate-calling-publications` |

**Meeting specifics.**
- Every Meeting category is fenced (sticky decisions, guard triggers).
- Invitation tokens and addresses are HPKE-sealed to the service. The X25519 key pair is generated once on the VPS by `activate-meeting-publications`, with kid `notif-hpke-YYYY-MM-N`.
- What the catalogue cannot carry stays OLD: `RECIPIENT_LIMIT` for an audience that does not fit one envelope, `UNPUBLISHABLE` otherwise.

**Resend webhook authority.** The account keeps one webhook, on Meeting. Meeting forwards the events of messages it did not send, byte for byte, to the service callback, so there are no conflicting webhook authorities.

**N9 push.**
- **Registration mirror.** Messaging copies and mirrors its Web Push registrations of first-party accounts to the service (I13 `notifications-push-registration.sync`). Handles are sealed and never logged.
- **Sending.** The service sends Web Push with Messaging's existing VAPID key, through a service-owned 0400 copy, so every existing browser subscription stays valid.
- **Incoming calls.** Calling's incoming-call alerts use the fast lane I3 (`CALLING_PUSH_TRANSPORT=notifications`): first-party recipients only, fire-and-forget, one alert per (call, target).
- **Client.** The web service worker handles the service's payloads.

**N10 preferences.**
- The service pulls the Hub's preferences: snapshot, then catch-up from the change feed, then verification. Verification compares every account, conversation and contact row.
- The Hub switch moves `HUB` → `FREEZE` → `SERVICE` only when every kind verifies exactly; otherwise it returns to `HUB`.

**N12 CRITICAL lane.**
- **Leases.** The Meeting Service serves X6 (lease) and X7 (completion). The service sends a CRITICAL message only under a lease, so a code is sent once and never zero times; a 24-round race test covers this.
- **Failover.** The PW1 relay fails over automatically (AUTO, about 60 s without acknowledgement and never after a lease). FORCED always uses the legacy path.
- **Fallback.** The sealed fallback is the exact legacy email, sealed with the Account fallback key (XChaCha20, Meeting only, generated once).
- **Catalogue (PR #99, event contract 1.2.0).** It adds `account.sign-in-code.requested`, `account.email-change-code.requested` and `account.step-up-code.requested`. The sign-in email's magic link is rebuilt from the sealed token and `NOTIFICATIONS_APP_URL`. `activate-critical-lane` sets that URL to Meeting's `APP_URL` on both replicas, and the fence refuses `AUTH_VERIFICATION` `FENCED_NEW` while they differ.
- **Password categories.** After PR #97 `AUTH_PASSWORD_RESET` and `SECURITY_PASSWORD_CHANGED` have no producer: password sign-in was removed.

**Release tooling.**
- New operations: `activate-{communications,developer,meeting,calling}-publications`, `activate-notifications-email`, `activate-critical-lane`, `activate-notifications-push`, `activate-messaging-push-mirror`, `activate-calling-push`, `activate-preference-copy`, `activate-preference-authority`, and `notifications-acceptance`.
- `email-acceptance confirm=fallback` runs the fallback drill. `email-acceptance confirm=otp` follows the sign-in code to wherever the fence sent it.
- `activate-notification-fence` covers `SHADOW` / `FENCED_OLD` / `FENCED_NEW`. `FENCED_NEW` needs `SHADOW` or `FENCED_OLD` first and the service's routing rows `ACTIVE`; rollback restores the previous mode.
- 452 offline tests.

## 3. Resend E-track (gate E1)

| Check | Result (2026-11-16) |
|---|---|
| DKIM `resend._domainkey.cfmeet.com` | published (TXT `p=MIGfMA0…`) |
| Return path `send.cfmeet.com` | CNAME `send.forge.rmta.net` → MX `10 feedback.forge.rmta.net`, SPF `v=spf1 ip4:52.3.252.119 ip4:44.222.39.36 ip4:199.249.231.0/24 ~all` |
| DMARC `_dmarc.cfmeet.com` | **absent** (recommended `v=DMARC1; p=none;`, not required for sending) |
| Resend verification status | not readable without the API key; to be confirmed in the Resend dashboard |
| Credentials | **VPS only** (owner decision): `/opt/cfmeet-core/secrets/resend-api-key` and `resend-webhook-secret`, verified present, root-owned, mode 0400 and of the expected form (values never printed or transferred). No GitHub secrets. `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT` is not set, so the default `delivered@resend.dev` is used |
| `activate-resend-email` (earlier) | run [43807428863](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43807428863) on `4f9e4f2`: stopped at the then-GitHub credential check; no VPS change |
| `activate-resend-email` | run [43963096642](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43963096642) on `463f6b0` (PR #88), 22:06 UTC.<br>• Activation base equivalent to the deployed `44a2e9e` (62 inputs).<br>• Credential check PASS for both files.<br>• Meeting Service recreated with `EMAIL_PROVIDER=resend`, sender `CF Meet <no-reply@cfmeet.com>`, webhook signing configured.<br>• Acceptance email (delivery `bb73a1c1…`) ACCEPTED 22:06:22, signed DELIVERED 22:06:24.<br>• Regression PASS. |
| `email-acceptance confirm=otp` | run [43968733391](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43968733391) on `13aed26`, 22:46 UTC.<br>• Acceptance email ACCEPTED 22:46:31, signed DELIVERED 22:46:33.<br>• OTP journey PASS on all 10 checks: register, verification email recorded, verification email accepted by Resend, code issued, invalid code refused, valid code verifies, used code refused, sign-in after verification, authenticated session, cleanup deactivated. |
| E1 | **COMPLETE.** Real provider delivery proven by Resend's signed `email.delivered` webhook events, and the OTP journey passes end to end. Limitation: both proofs used Resend's delivery test inbox (`delivered@resend.dev`). A human-inbox check needs `CFMEET_EMAIL_ACCEPTANCE_RECIPIENT` set to a controlled address, then a re-run of `email-acceptance` |

**Owner decision (2026-11-16): Resend activation is suspended.**

- Credentials will **not** be stored in GitHub. When the work resumes, the owner will place them directly on the VPS as protected secret files.
- `activate-resend-email` and `email-acceptance` are not run until then.
- **Resumed (owner, 2026-11-16) with VPS-only credentials.** `activate-resend-email` and `email-acceptance` no longer read GitHub secrets. They verify over SSH that `/opt/cfmeet-core/secrets/resend-api-key` (required) and `resend-webhook-secret` (optional) exist, are root-owned with no group or other access, and hold values of the expected form. The values are never printed or transferred. The E1 result is recorded below when the runs complete.

With E1 recorded, the N14 gate no longer refuses email `ACTIVE` for lack of E1. No email category has been moved to the Notification Service. The Meeting Service email path (now through Resend) remains the only sender. Any move still requires the routing-fence, shadow and acceptance gates of Document 6.

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

| | |
|---|---|
| Readiness fix in production | PR #80 merged as `d553e53`. My release run 94 (43856717241) stopped in its gate because `main` advanced. The fix went live with release [43870645555](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43870645555) on `c0714ba` (12:53–13:08 UTC, which contains #80). `notifications` and `notifications-2` have run `c0714ba` since then. |
| Relay release | run [43914242156](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43914242156) `release` on `3c44089` (PR #82), 17:34 UTC. **Replaced:** `meeting-service`, `calling`, `api-gateway` (base `b8fefc2`). **Kept:** 12 services, `notifications` and `notifications-2` included. Run 100 (43902376581), dispatched by another session on the same SHA, ran the read-only `diagnose` operation; it did not deploy. |
| Resource gate | MemAvailable 5,008 MiB; required 832 MiB; remaining 4,176 MiB; shortfall 0 |
| Pre-release backup | 17 databases, `/opt/cfmeet-core/backups/43914242156-1/manifest.json`; restore-tested organizations, platform-access, meeting-service (PASS) |
| Verification | 33/33 checks PASS; violations: none; regression (Meetings health, Messaging/Calling/Platform TLS readiness, LiveKit keys) PASS |
| Certification | run [43919862343](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43919862343) `certify` on `3c44089`: **`CERTIFIED_READ_ONLY`** (17:41 UTC). This replaces NOT_CERTIFIED 43828926604. |
| Owner APIs | run [43922950325](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43922950325) `activate-notifications-owners` on `a1069d5`, 17:55 UTC.<br>• `main` had advanced by #85, which is release tooling only. The activation base found the commit equivalent to the deployed `3c44089`: 62 image build inputs checked, none changed.<br>• **Public-host denial proven** (404 + `X-CFMeet-Ingress: denied`) for `/internal/v1/accounts/notification-contacts/resolve` and `/internal/v1/conferences/{id}/notification-recipients/resolve`.<br>• Trust bundles updated: Messaging, Communications, Organizations, Meeting. Meeting gained `NOTIFICATION_OWNER_API_ENABLED=true`. Both replicas gained `NOTIFICATIONS_AUTHORITIES`, the owner base URLs and the first-party scope.<br>• Every owner route answers 401 unsigned afterwards; regression PASS.<br>Run 103 (43922930842) was refused by the gate, before any change, because of a short SHA. |
| Relay | run [43926737306](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43926737306) `activate-notifications-relay` on `a1069d5`, 18:26 UTC.<br>• Activation base equivalent to `3c44089` (62 inputs).<br>• Both replicas trust the Meeting Service for `notifications-user.forward` (I2 only); the Meeting Service has `CF_NOTIFICATIONS_BASE_URL`, the private TLS route.<br>• I2 answered 401 unsigned and the relay 401 without a session, before and after.<br>• Regression PASS.<br>U1–U9 now serve the signed-in user's own inbox, preferences and push endpoints. The inbox is empty because every routing row is `OFF`. |

**Production status:**
- **N2: `PRODUCTION_DEPLOYED_AND_VERIFIED` (dark).** It runs the readiness fix since 13:08 UTC and was certified at 17:41. The 24-hour zero-traffic observation (Document 6 N2 row 14) runs from 13:08 UTC.
- **N6: `PRODUCTION_DEPLOYED_AND_VERIFIED`.** X1–X5 are granted and reachable only from `cfmeet.notifications`, and are publicly denied.
- **N8 service side and relay: `PRODUCTION_DEPLOYED_AND_VERIFIED`.** U1–U9 are live through the Meeting relay. The web and mobile inbox UI is not built.
- **Delivery:** nothing is delivered. Every `notif_routing` row is `OFF`, there is no email or push provider, and Resend stays suspended.

### 4.3 N7b release, certification, Messaging publications and SHADOW

| Step | Evidence |
|---|---|
| Release | run [43987779716](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43987779716) `release` on `be00f8b` (PR #92), finished 01:28 UTC. The gate waited for green main CI (00:58). Verification 35/35 PASS, violations none. `routing_protocol meeting-service` and `routing_protocol messaging` PASS; Messaging fence history 2 (publication outbox) applied. Regression PASS. |
| Certification | run [43997503927](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43997503927) `certify` on `be00f8b`: **`CERTIFIED_READ_ONLY`** (01:46 UTC). |
| Publications | run [43999106430](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/43999106430) `activate-messaging-publications` on `be00f8b`, 01:56 UTC.<br>• Activation base: head is the released SHA.<br>• Before the change: I1 answered 401 unsigned; Messaging reported `routingProtocol`; `messaging_notification_publications` exists; Messaging has a signing identity.<br>• Trust bundles of `notifications` and `notifications-2` changed (`notifications-event.publish` for `cfmeet.messaging`); Messaging gained `CF_NOTIFICATIONS_BASE_URL`, the private TLS route.<br>• After the change: I1 still 401 unsigned, `routingProtocol` still reported; regression PASS. |
| SHADOW | run [44000239278](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/44000239278) `activate-notification-fence` `messaging-message-wakeups-shadow` on `be00f8b`, 02:05 UTC: `messaging`/`MESSAGE_WAKEUPS` **`LEGACY_ONLY` → `SHADOW`**, routing version 2, journaled (rollback restores `LEGACY_ONLY`). |

**Production status:**
- **N7b: deployed, `MESSAGE_WAKEUPS` in `SHADOW`.** Every message wake-up records an `OLD` decision and writes its legacy push jobs exactly as before. An audience of at most 40 candidates also enqueues one shadow publication, which the relay posts to I1 with `shadow: true`.
- **Not yet evidenced:** acknowledged shadow publications in production (rows `ACKED` with outcome `HELD`/`ACCEPTED`), and the shadow comparison of Document 6 N7. This needs a read-only check after real traffic: `diagnose` now emits `notificationDiagnostics` (counts only) for exactly this. Until then N7b is not `PRODUCTION_DEPLOYED_AND_VERIFIED`.
- **Delivery:** unchanged. Legacy Messaging push delivers every wake-up. The Notification Service delivers nothing (every `notif_routing` row `OFF`, shadow events are never delivered). No category is `FENCED_*`.

### 4.4 Waves 2–3 in production (2026-11-17 / 18)

**Releases and certification**

| Step | Evidence |
|---|---|
| Release `506bff4` (PR #96, waves 2–3 + PR #97) | run [44150771996](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/44150771996): verification 35/35 PASS, regression PASS |
| Certify `d5eb1ea` (tooling only on top) | run [44170126155](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/44170126155): `CERTIFIED_READ_ONLY` |
| Release `21c90c4` (PR #99, passwordless catalogue) | run [44191575507](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/44191575507): success |
| Certify `e6cd140` (tooling only on top) | run [44199057880](https://github.com/CosmicforgeHealthnet/cfmeet/actions/runs/44199057880): `CERTIFIED_READ_ONLY` |

Every activation below ran on a certified head and proved that no image build input had changed since the release (`activationBase`, 62 inputs).

**Activations**

| Operation | Run | Result |
|---|---|---|
| `activate-communications-publications` | 44171231077 | Hub → I1 grant, both replicas |
| `activate-developer-publications` | 44171595691 | Developer Platform (+ worker) → I1 |
| `activate-calling-publications` | 44172022550 | Calling → I1 |
| `activate-meeting-publications` | 44172332777 | HPKE key `notif-hpke-2026-11-1` generated once, both replicas; Meeting → I1 |
| `activate-notifications-email` | 44172839707 | provider `resend` on both replicas, signed webhook, service-owned 0400 copies of the VPS files |
| `activate-critical-lane` | 44174010149, again 44201470852 | X6/X7 grant, lease routes publicly denied, fallback key mounted (`criticalFallbackKey: true`); the second run gave both replicas Meeting's `APP_URL` |
| `email-acceptance confirm=fallback` | 44175510575 | fallback drill sealed with the Account fallback key, sent through the legacy ledger: Resend ACCEPTED, then a signed DELIVERED 2 s later |
| `activate-notifications-push` | 44176266362 | live Web Push with Messaging's VAPID key, both replicas |
| `activate-messaging-push-mirror` | 44177553529 | I13 grant; Messaging mirrors registrations |
| `activate-calling-push` | 44177899990 | `CALLING_PUSH_TRANSPORT=notifications` (before: no call push at all) |
| `activate-preference-copy` | 44178337597 | copy running |
| `activate-preference-authority` | 44178664093 | **authority `SERVICE`**: account 7/7, contact 70/70, conversation 0/0 equal, 0 different |

**Real production delivery**

- **In-app (`ORGANIZATION_UPDATES`, routing `ACTIVE` in 44173220256).** `notifications-acceptance` 44200593365 ran through the public API, with real Organizations events and the real Meeting relay. Six checks passed:
  - the inviter is told the invitation was accepted;
  - a role change reaches the member exactly once;
  - the unread count rises by one;
  - another account sees nothing and cannot mark it read;
  - read and dismiss are idempotent.

  The seventh check expected a category switched off to record no item at all. The service implements Document 4 §7.2 rule 7: the item is recorded silently. The check was corrected in PR #102, and the re-run 44218552131 passed **7/7**, preference included. `diagnose` counted 7 IN_APP deliveries `SENT` and 7 inbox items.
- **Email through the service: delivered (after the incident below).** `diagnose` 44221066781 (02:05 UTC), by Notification Service email ledger:
  - 7 `AUTH_VERIFICATION` sign-in codes went through the CRITICAL lane, each under one X6 lease and completed with X7 (Meeting `criticalOutbox`: 7 `ACKED`, completed, `ROUTED`), with no failover and no duplicate. One was the acceptance run 44218158768: every check PASS, decided NEW, Resend ACCEPTED, then a signed DELIVERED. Six were real users signing in between 01:49 and 02:01.
  - Resend statuses: 1 `DELIVERED`, 1 `ACCEPTED`, 5 `DELAYED` (Resend's "delivery delayed" from the receiving server; the legacy path shows the same status at times).
  - 16 signed Resend webhook events reached the service's callback through Meeting's byte-for-byte forwarding, so the single-webhook design works.
- **Push through the service: not yet.** Production has 0 enabled Web Push registrations (`diagnose`), so there is nothing to deliver to. APNs and FCM are not configured (§5).

**Incident: the replicas had no route to Resend (2026-11-17, 23:48–23:58 UTC).**

The fence moves before the incident:
- `MESSAGE_WAKEUPS`, `ORGANIZATION_INVITATION`, `MEETING_INVITATION`, `MEETING_NOTIFICATION`, `SECURITY_DEVICE_LINKED` and `SECURITY_DEVICE_STEP_UP` were moved to `FENCED_NEW` on `d5eb1ea` (runs 44179499440 … 44184530567).
- `AUTH_VERIFICATION` was moved on `e6cd140` (44202419165).

What happened:
- The OTP acceptance 44202682130 still signed in, because it reads the code directly. But the service's email for the `NEW` decision was never accepted by Resend.
- `diagnose` 44210281972 showed:
  - Meeting granted X6 leases (3 per message);
  - the service tried 8 times per message and failed with provider class `NETWORK`;
  - both CRITICAL messages expired.

  As designed, a leased decision is never failed over automatically, so neither message was sent twice; neither was sent at all.

Cause: the replicas were attached only to `cfmeet-core-private`, which is `--internal`. `activate-notifications-email` and `activate-notifications-push` gave them no route out.

Response, all through journaled fence runs:
- `AUTH_VERIFICATION`, `SECURITY_DEVICE_STEP_UP` and `SECURITY_DEVICE_LINKED` back to `LEGACY_ONLY` (44203245321, 44203766252, and the device-linked run);
- the three Meeting email categories and `MESSAGE_WAKEUPS` back to `SHADOW` (44210580209, 44210836107, 44211121354, 44211365483).

Meeting's own Resend path delivered throughout. One acceptance-run sign-in code and one acceptance step-up code were lost; both went to the acceptance test inbox.

Fix: PR #103. Both replicas join a dedicated non-internal bridge, `cfmeet-notifications-egress` (inter-container traffic disabled). The email and push activations fail and restore unless every replica gets an HTTP answer from its providers.

**Re-promotion with egress (2026-11-18, on `3089351`).**
- `activate-notifications-email` 44217118248: both replicas get HTTP 200 from `api.resend.com`.
- `activate-notifications-push` 44217393745: both replicas get HTTP answers from FCM, Mozilla and Apple Web Push.
- Fence runs:
  - `AUTH_VERIFICATION` `LEGACY_ONLY` → `SHADOW` → `FENCED_NEW` (44217687361, 44217926339);
  - `SECURITY_DEVICE_STEP_UP` (44219091457, 44219274443) and `SECURITY_DEVICE_LINKED` (44219540724, 44219759322), the same way;
  - `MEETING_INVITATION`, `ORGANIZATION_INVITATION`, `MEETING_NOTIFICATION` `SHADOW` → `FENCED_NEW` (44219978159, 44220275628, 44220521719);
  - `MESSAGE_WAKEUPS` `SHADOW` → `FENCED_NEW` (44220764318).
- OTP acceptance 44218158768: every check PASS through the NEW path. The run was marked failed only because the informational `sign_in_route` was judged like a check; fixed in PR #104.

**Production state after the re-promotion**

| Category | Producer fence | Service routing `ACTIVE` | Delivered by |
|---|---|---|---|
| `AUTH_VERIFICATION` (sign-in, email-change codes) | `FENCED_NEW` | EMAIL | Notification Service, CRITICAL lane (X6/X7), fallback key on Meeting |
| `SECURITY_DEVICE_STEP_UP`, `SECURITY_DEVICE_LINKED` | `FENCED_NEW` | EMAIL | Notification Service, CRITICAL lane |
| `MEETING_INVITATION`, `ORGANIZATION_INVITATION` | `FENCED_NEW` | EMAIL | Notification Service |
| `MEETING_NOTIFICATION` | `FENCED_NEW` | EMAIL, IN_APP | Notification Service |
| `MESSAGE_WAKEUPS` | `FENCED_NEW` | PUSH | Notification Service (Web Push; audiences over 40 stay OLD) |
| `ORGANIZATION_UPDATES` | (Organizations publishes directly) | IN_APP | Notification Service |
| incoming calls | Calling I3 | — | Notification Service fast lane (first-party, Web Push) |
| `AUTH_PASSWORD_RESET`, `SECURITY_PASSWORD_CHANGED` | `LEGACY_ONLY` | — | no producer since PR #97 |
| preferences | Hub authority `SERVICE` | — | Notification Service (U6/U7) |

---

## 5. What is not done, and why

| Item | State | What it needs |
|---|---|---|
| APNs (iOS push) | adapter built, not configured | owner credentials that do not exist: an APNs auth key (`.p8`), its key id, the Apple team id and the app bundle topic |
| FCM (Android push) | adapter built, not configured | an FCM service-account JSON for the Firebase project of the Android app |
| Web inbox / preference UI (FE2, FE3), web call push in the browser (FE4, FE5) | client work, not backend | a web / mobile release; the backend APIs (U1–U9 relay) are live |
| N13 alert delivery to a person | rules built and tested | Prometheus / Alertmanager with receivers (owner decision D-9) |
| Non-critical account notices (`new-sign-in`, `email-in-use`, `email-changed`) | still sent by Meeting's legacy path | a fence category of their own, and a decision on the masked address and undo token they carry |
| Multi-envelope events | an audience over 40 (message wake-ups) or one envelope (Meeting) stays `OLD` (`RECIPIENT_LIMIT`), so the legacy path still reaches everyone | a contract decision on splitting one event across envelopes |
| Call push cancel | not built | a catalogue event for "call answered elsewhere / ended" |

## 6. Retirement and time-based acceptance

The legacy paths are not retired: they are the failover and rollback target of every fence. Examples are the Meeting email ledger and worker (the CRITICAL AUTO / FORCED fallback) and Messaging's push dispatcher (`FENCED_OLD` / `LEGACY_ONLY` rollback). `OPERATIONALLY_ACCEPTED` and `LEGACY_RETIRED` are time-based (Document 5 §10) and are not claimed for any category: the windows start at the `FENCED_NEW` times of §4.4.

## 7. Baselines Q1–Q18

Q11, Q14 and Q15 remain COMPLETED as recorded in Document 9 §7.8 (verified: the certify evidence of run 43792702821 and the activation runs 43788212474 / 43788404790 are unchanged). Q1–Q4 and Q17–Q18 need E1 (email baselines on a working transport). Q16 (7-day memory minimum) is still a single sample; the dark release adds 512 MiB of limits (§4).

## 8. Verdict

**`CFMEET_UNIFIED_NOTIFICATION_SERVICE_PRODUCTION_ACTIVE` — for in-app and email.** The basis:
- every section N2–N15 is engineered, merged and tested;
- the service is released and certified, and every activation of §4.4 is recorded;
- real production delivery has been accepted:
  - in-app: `notifications-acceptance` 7/7;
  - email: real sign-in codes through the CRITICAL lane, Resend DELIVERED with signed webhooks, and the fallback drill DELIVERED.

**Qualified, and not claimed beyond this:**
- **Web Push is live but has no delivery evidence.** Production has no browser push registration to deliver to.
- **APNs and FCM are not configured.** Their credentials do not exist (§5).
- **No delivery is evidenced yet for meeting invitations, organization invitations, meeting notifications or the device notices.** They run on the service, but no real one has occurred since the re-promotion.
- **No time-based acceptance window has passed (§6).** `OPERATIONALLY_ACCEPTED` and `LEGACY_RETIRED` are not claimed.

