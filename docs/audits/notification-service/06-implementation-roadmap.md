# Document 6 — Implementation Roadmap (revision 2: unified service)

Each section can be assigned on its own **after owner approval**. Dependencies only point backwards. The rows in each section follow Part 15 of the brief (items 1–14). Where a section shares a row value with another, the row says so.

## Dependency graph

```mermaid
flowchart TD
  N0[N0 Repair existing paths]
  N1[N1 Contracts, permissions, notification-client]
  N2[N2 Service skeleton, schema, 2 replicas dark]
  N3[N3 Ingest, routing, preference engine, shadow]
  N4[N4 Push transport port]
  N5[N5 Email channel port]
  N6[N6 Owner read APIs X1-X5]
  N7[N7 Producer shadow publications]
  N8[N8 Inbox + user APIs]
  N9[N9 Push cutover + call fast lane]
  N10[N10 Preferences move]
  N11[N11 Standard + invitation email cutover]
  N12[N12 Critical lane cutover]
  N13[N13 Observability, alerting, operator channel]
  N14[N14 Canary governance]
  N15[N15 Retirement]
  N16[N16 Customer webhooks - only if D-W]

  N1 --> N2 --> N3
  N1 --> N6
  N3 --> N7
  N6 --> N7
  N3 --> N8
  N7 --> N8
  N4 --> N9
  N8 --> N9
  N8 --> N10
  N0 --> N11
  N5 --> N11
  N10 --> N11
  N13 --> N12
  N11 --> N12
  N3 --> N13
  N9 --> N14
  N12 --> N14
  N14 --> N15
  N15 --> N16
  N2 --> N5
```

**Can start immediately in parallel:** N0, N1, N4.
**Frontend and mobile integrate progressively:**

| Step | After | Work |
|---|---|---|
| FE1 | N1 | generated types |
| FE2 | N8 | inbox UI |
| FE3 | N10 | preference UI on U6/U7 |
| FE4 | N9 | service worker / mobile push SDK on U8/U9 |
| FE5 | N9 | CallKit / ConnectionService |

All UI work needs design approval (D-11).

---

## N0 — Repair the existing paths (owning services)

| # | Field | Content |
|---|---|---|
| 1 | Objective | Make today's notifications work and remove the security defects before anything moves. Users need working codes and notices now; the shadow comparison needs a correct baseline; several fixes (OTP hashing, lease renewal, suppression) are ported into the unified service |
| 2 | Reuse | All existing code and tooling (`activate-resend-email`, `email-acceptance`, release tests) |
| 3 | Services affected | Meeting, Messaging, Hub release tooling, nginx config, apps/web, apps/mobile |
| 4 | New components | none |
| 5 | API changes | none publicly. `verify-reset-token` returns a uniform answer (status semantics kept for the valid case) |
| 6 | Database changes | additive. Retention indexes / purge for `messaging_push_jobs`. No destructive change |
| 7 | Events and workers | email worker and push dispatcher lease renewal; meeting worker poison-recipient handling |
| 8 | Security | see the items below |
| 9 | Tests | one regression test per item (listed below) |
| 10 | Prerequisites | Phase 0 queries Q3, Q14 and Q15 to confirm S-1 and S-3 before changing them |
| 11 | Production verification | `email-acceptance confirm=otp` PASS; Q3 shows SENT notices for account holders; Q15 shows the internal routes are not reachable publicly |
| 12 | Rollback | each item is a separate release; standard `rollback` |
| 13 | Dependencies | none |
| 14 | Definition of done | every item below has merged, been released and been verified in production; Document 8 status → "repaired" |

The items:

| Item | Change | Evidence | Regression test |
|---|---|---|---|
| N0-1 | Grant `communications-preferences.read` to `cfmeet.meeting-service` in `communications_release.py` (the Hub bundle builder) and in the coordinated `GRANTS` | S-1 | release test asserts the grant is present; production `organization-acceptance`-style check sends a notice to a test account |
| N0-2 | **Owner** completes the Resend setup → `activate-resend-email` → `email-acceptance confirm=otp` (existing tooling; no code) | S-2 | the existing `test_email_operations.py` |
| N0-3 | `notifications::deliver`: `BadRequest` from `enqueue` marks that recipient `INVALID_ADDRESS` and continues | S-11 | poison address in `RECIPIENTS_SQL` → others still sent |
| N0-4 | Email worker and push dispatcher: per-item lease renewal, or batch ≤ lease / (timeout × 3) | S-8 | two workers + slow fake provider → 0 duplicates |
| N0-5 | Messaging: honour `notify=false` only for delegated/migration callers (`messaging-application.delegate` / import), never for client tokens | S-4 | client token `notify:false` → the push job is still created |
| N0-6 | nginx: `location ^~ /internal/ { return 404; }` on the public server block (verify first, Q15) | S-3 | public certify probe `/internal/operations/email/deliveries/<uuid>` → nginx 404 |
| N0-7 | `PRINCIPAL_ERASURE` deletes push registrations and jobs for the principal | S-7 | erasure test asserts 0 rows |
| N0-8 | OTP: store SHA-256(code + per-key salt), constant-time compare, key by hash of email | S-5 | existing OTP journey test + Redis key inspection |
| N0-9 | `verify-reset-token`: uniform response and timing | S-6 | enumeration test |
| N0-10 | Security notices (password changed, new device) enqueued in the same transaction (as step-up already is) | S-9 | failure injection → notice still queued |
| N0-11 | Web logout revokes the browser's Calling endpoint; mobile removes `voip` from `UIBackgroundModes` until CallKit exists | S-10, mobile risk | web unit test; app config review |
| N0-12 | Bounce/complaint suppression: refuse new non-security sends to a `recipient_hash` with COMPLAINED, or BOUNCED twice | S-12 | webhook → suppression → enqueue refused with `SUPPRESSED` |

---

## N1 — Contracts, permissions, `notification-client`

| # | Field | Content |
|---|---|---|
| 1 | Objective | Freeze every interface (U1–U9, I1–I10, X1–X5, 31 event types, envelope with `sealed`) |
| 2 | Reuse | contract validator, SDK generators, `cf-service-auth` registry |
| 3 | Services | `packages/contracts`, `packages/service-auth`, new `packages/notification-client` |
| 4 | New | notifications HTTP/internal/events contracts; internal additions on Account, Meeting, Messaging, Hub, Organizations; client crate (envelope builder, HPKE sealing, routing-fence helper) |
| 5 | API | contract files only |
| 6 | DB | none |
| 7 | Events/workers | schemas with positive and negative examples |
| 8 | Security | closed `grantable_to`; sensitive flags; HPKE key `kid` rotation in the contract |
| 9 | Tests | validator; conformance; forbidden-key and unsealed-secret examples refused |
| 10 | Prerequisites | owner approval; D-1, D-2 resolved (now: service registry, service fast lane) |
| 11 | Production verification | none |
| 12 | Rollback | revert |
| 13 | Dependencies | — |
| 14 | Done | validator green; FE1 types published |

## N2 — Service skeleton, full schema, 2 replicas (dark)

| # | Field | Content |
|---|---|---|
| 1 | Objective | A healthy, idle unified service in production |
| 2 | Reuse | Organizations/Developer Platform layout; Dockerfile; CI; `provision-service-database.sh`; coordinated release |
| 3 | Services | new service; release tooling |
| 4 | New | crate, migration crate (16 tables), `/health` and `/ready` (per-lane heartbeat), config, nonce table, retention worker |
| 5 | API | none exposed |
| 6 | DB | `cfmeet_notifications`, migration 000001 |
| 7 | Workers | none active |
| 8 | Security | hardened container; private TLS only; secrets as files |
| 9 | Tests | pending-migration refusal; prefix, guard and isolation checks |
| 10 | Prerequisites | N1 |
| 11 | Production verification | certify: 2 replicas healthy, 256 MiB each, gate passed |
| 12 | Rollback | remove the containers |
| 13 | Dependencies | N1 |
| 14 | Done | 24 h `/ready`; zero traffic |

## N3 — Ingest, routing fence, preference engine, shadow

| # | Field | Content |
|---|---|---|
| 1 | Objective | Accept all events, evaluate preferences and precedence, render, and record SHADOW |
| 2 | Reuse | Hub preference/policy logic (ported); Meeting templates (ported) |
| 3 | Services | the service |
| 4 | New | I1, I7, I8, W1, W4, W6, template registry (all categories), `notif_routing` |
| 5 | API | internal |
| 6 | DB | none new |
| 7 | Workers | W1, W4, W6 |
| 8 | Security | HPKE decrypt only in the renderer; sealed fields never logged |
| 9 | Tests | precedence cases 1–12; old-template parity (subject/body hash equal to the Meeting output for the same input); SHADOW makes no provider calls |
| 10 | Prerequisites | N2 |
| 11 | Production verification | a synthetic event shows in I7 |
| 12 | Rollback | revoke producer grants |
| 13 | Dependencies | N2 |
| 14 | Done | all 31 types pass tests |

## N4 — Push transport port

| # | Field | Content |
|---|---|---|
| 1 | Objective | Move APNs, FCM and Web Push adapters (plus APNS_VOIP) out of Messaging with their tests |
| 2 | Reuse | `push_adapters.rs` verbatim |
| 3 | Services | Messaging (path change only), the service |
| 4 | New | `packages/push-transport` (used by Messaging until 4.1, then by the service only) |
| 5–7 | API / DB / workers | none |
| 8 | Security | SSRF allowlist, metadata whitelist and RFC 8291 vectors move unchanged |
| 9 | Tests | existing tests pass unchanged |
| 10 | Prerequisites | none |
| 11 | Production verification | Messaging behaviour identical |
| 12 | Rollback | revert |
| 13 | Dependencies | — |
| 14 | Done | CI green, zero test edits |

## N5 — Email channel port

| # | Field | Content |
|---|---|---|
| 1 | Objective | The service owns an email ledger equivalent to today's, with the known defects fixed |
| 2 | Reuse | `email_delivery.rs` (ledger, worker, `ResendProvider`, `SmtpProvider`, `verify_svix`, `apply_provider_event`) and `tests/email_delivery.rs`, ported |
| 3 | Services | the service |
| 4 | New | `notif_email_messages`, provider events, suppressions; lanes; lease renewal; sealed content; keyed digests; mandatory expiry; provider breaker; public Resend callback route |
| 5 | API | `POST /api/v1/notifications/provider-events/resend`; I9 |
| 6 | DB | tables 10–12 |
| 7 | Workers | W2, W3 (email part) |
| 8 | Security | Svix verification; sealed bodies erased at the final state; never log addresses |
| 9 | Tests | all ported ledger tests (idempotency, retry, permanent failure, expiry, lease recovery, webhook authentication, monotonic status) plus: multi-replica no-duplicate with slow provider; suppression; CRITICAL not starved by a STANDARD backlog |
| 10 | Prerequisites | N2 |
| 11 | Production verification | `email-acceptance` retargeted to the service ledger with Resend (needs D-4 credentials) |
| 12 | Rollback | routing stays OFF; nothing depends on it yet |
| 13 | Dependencies | N2 |
| 14 | Done | acceptance email ACCEPTED and DELIVERED (webhook) from the service |

## N6 — Owner read APIs X1–X5

| # | Field | Content |
|---|---|---|
| 1 | Objective | The service can ask authorities instead of copying them |
| 2 | Reuse | Account profile/device queries; `RECIPIENTS_SQL` and `presented_title`; Messaging `push.rs` entitlement logic; Hub block/privacy checks; Organizations invitation store |
| 3 | Services | Meeting (Account + conferences), Messaging, Hub, Organizations |
| 4 | New | X1–X5 handlers + permissions |
| 5 | API | internal |
| 6 | DB | none |
| 7 | Workers | none |
| 8 | Security | grantable to `cfmeet.notifications` only; batch caps; X1 returns addresses only to this caller; X5 never returns the email |
| 9 | Tests | authorization denials; batch limits; X3 parity with today's `push.rs` decisions (same fixtures from `tests/notification_authorization.rs`) |
| 10 | Prerequisites | N1 |
| 11 | Production verification | certify auth probes (401 without a token) |
| 12 | Rollback | revoke grants |
| 13 | Dependencies | N1 |
| 14 | Done | all five live and unused |

## N7 — Producer shadow publications

| # | Field | Content |
|---|---|---|
| 1 | Objective | Every producer also publishes, in its business transaction, with no user effect |
| 2 | Reuse | each outbox |
| 3 | Services | Account/Meeting, Messaging, Calling, Hub, Organizations, Developer Platform |
| 4 | New | `account_notification_outbox` (Account, new table) + relay; Meeting `outbox_events` channel `NOTIFICATION`; Messaging publication; Calling destination + `CALL_PARTICIPANT_MISSED` (D-10); Hub destination CHECK; Organizations subscriber; Developer Platform publication; all using `notification-client` |
| 5 | API | none public |
| 6 | DB | additive (4 changes) |
| 7 | Workers | producer relays |
| 8 | Security | codes and tokens sealed before the outbox insert |
| 9 | Tests | one event per action in the same transaction; flag off stops it; existing suites unchanged |
| 10 | Prerequisites | N3, N6 |
| 11 | Production verification | Phase 2 exit criteria |
| 12 | Rollback | flags off |
| 13 | Dependencies | N3, N6 |
| 14 | Done | 14-day shadow within thresholds for every category |

## N8 — Inbox and user APIs U1–U5

| # | Field | Content |
|---|---|---|
| 1 | Objective | Users see an inbox for categories that have no channel today (Phase 3) |
| 2 | Reuse | the Meeting `/api/v2/communications` relay pattern, `VerifiedAccount` |
| 3 | Services affected | the service, Meeting (relay route + OpenAPI), web, mobile |
| 4 | New components | U1–U5; I2; inbox writer in W1; `notifications.inbox.changed` (internal) |
| 5 | API changes | U1–U5 (NEW); `meeting-service.json` gains the routes (mobile contract) |
| 6 | Database changes | none new (table 5 from N2) |
| 7 | Events and workers | W1 writes items; W5 retention |
| 8 | Security | own items only (404 for foreign IDs); closed `action.kind`; rate limits; authorization-matrix suite for U1–U5 |
| 9 | Tests | read/unread/dismiss sync across two sessions; grouping; silent items excluded from the count; ETag 304; pagination stability |
| 10 | Prerequisites | N3, N7 |
| 11 | Production verification | canary accounts receive a contact-request item end-to-end (real Hub action → item visible) |
| 12 | Rollback | relay route off; IN_APP routing OFF |
| 13 | Dependencies | N3, N7 |
| 14 | Definition of done | Phase 3 exit (Document 7 §5 A3); FE2 integrated behind a flag |

## N9 — Push cutover and call fast lane (Phase 4.1)

| # | Field | Content |
|---|---|---|
| 1 | Objective | All push (messages, calls, other categories, platform tenants) is delivered only by the service |
| 2 | Reuse | N4 adapters; Hub device index |
| 3 | Services | the service, Messaging (stops creating jobs; registration endpoints become facades), Hub (facade), Calling (`NotificationsPushTransport`) |
| 4 | New | U8, U9, I3, W3 push part, `notif_push_endpoints`, the Messaging `message.created` → service path with X3 |
| 5 | API | U8, U9; facades |
| 6 | DB | table 13 |
| 7 | Workers | W3 |
| 8 | Security | sealed handles; `UNIQUE handle_digest`; X3 recheck at dispatch; 5-key payload |
| 9 | Tests | revoked device gets nothing; org/MANAGED device can register; ringing latency unchanged with the service down (fault injection); platform-tenant `registerPush` facade parity |
| 10 | Prerequisites | N4, N8, D-3 (owner provisions credentials) |
| 11 | Production verification | real device per activated platform; backgrounded phone rings (with FE5) |
| 12 | Rollback | Document 5 §9, row 4.1 |
| 13 | Dependencies | N4, N8 |
| 14 | Done | 30 days with no rollback |

## N10 — Preferences move (Phase 4.2)

| # | Field | Content |
|---|---|---|
| 1 | Objective | The service becomes the single preference writer |
| 2 | Reuse | Hub change feed for catch-up |
| 3 | Services | the service, Hub (facade) |
| 4 | New | U6, U7; copy + catch-up job; 7-day change log for write-back |
| 5 | API | U6, U7; Hub endpoints become facades |
| 6 | DB | tables 14–16 |
| 7 | Workers | one-off copy |
| 8 | Security | own preferences only; version concurrency |
| 9 | Tests | copy parity; facade parity with old clients (7 booleans); freeze < 60 s; rollback write-back |
| 10 | Prerequisites | N8 |
| 11 | Production verification | `communications-verify` preference checks still pass through the facade |
| 12 | Rollback | Document 5 §9, row 4.2 |
| 13 | Dependencies | N8 |
| 14 | Done | counts and versions equal; 30 days stable |

## N11 — Standard and invitation email cutover (Phases 4.3 and 4.4)

| # | Field | Content |
|---|---|---|
| 1 | Objective | Meeting, developer and invitation emails are sent only by the service |
| 2 | Reuse | N5; X2 |
| 3 | Services | the service, Meeting, Developer Platform, ops script |
| 4 | New | scheduler for reminders (port of `enqueue_due_reminders` and the "still true" checks); Meeting producers routed by the fence |
| 5 | API | Resend webhook endpoint added (both endpoints active during the drain) |
| 6 | DB | — |
| 7 | Workers | W3, W4 |
| 8 | Security | tokens sealed; never in the inbox |
| 9 | Tests | fence boundary; drain; reminder after reschedule and after cancel; protected titles; `emailDelivery` semantics kept |
| 10 | Prerequisites | N0-1, N0-2, N0-3; N5 production acceptance; N10 |
| 11 | Production verification | acceptance meeting journey: one email per recipient, from the service ledger |
| 12 | Rollback | per-category fence back |
| 13 | Dependencies | N0, N5, N10 |
| 14 | Done | 30 days per category |

## N12 — Critical lane cutover (Phases 4.5 and 4.6)

| # | Field | Content |
|---|---|---|
| 1 | Objective | Security notices, then sign-in/verification/reset/step-up code emails, go through the service |
| 2 | Reuse | `account_notification_outbox` (N7); retained old path as break-glass |
| 3 | Services | Account (Meeting), the service |
| 4 | New | Account routing switch per category; `ACCOUNT_EMAIL_FALLBACK=local` |
| 5 | API | none |
| 6 | DB | — |
| 7 | Workers | W2 |
| 8 | Security | HPKE-sealed codes; erased at the final state; Account remains the verifier |
| 9 | Tests | OTP journey end-to-end through the service (port of `otp_journey_from_registration_to_session`); service-down → codes delayed, not lost; break-glass switch; the CRITICAL lane under a STANDARD flood |
| 10 | Prerequisites | N11 30 days stable; **N13 alerting deployed (D-9)**; CRITICAL shadow p95 ≤ Q17 + 5 s |
| 11 | Production verification | `email-acceptance confirm=otp` through the service at every canary step |
| 12 | Rollback | break-glass, then fence back |
| 13 | Dependencies | N11, N13 |
| 14 | Done | 60 days at 100 % with zero fallback use |

## N13 — Observability, alerting, operator channel

| # | Field | Content |
|---|---|---|
| 1 | Objective | Operators can see, diagnose and replay without database access, and **alerting is live before the critical lane moves** |
| 2 | Reuse | rule CI (`check_alert_rules.py`, promtool, amtool), certify framework, existing `meeting-service.rules.yml` / `messaging.rules.yml` |
| 3 | Services affected | the service, `infra/monitoring`, coordinated-release certify |
| 4 | New components | I4–I9; `notifications.rules.yml` (per-lane backlog, CRITICAL age > 60 s, failure rate, breaker, replica down); **Prometheus + Alertmanager deployed with real receivers (D-9)**; certify checks "routing agreement" and "lane backlog"; runbooks (outage → break-glass, replay, credential and HPKE key rotation); optional I10 operator channel (Phase 4.7, D-15) |
| 5 | API changes | internal I4–I10 |
| 6 | Database changes | none |
| 7 | Events and workers | — |
| 8 | Security | operator identity audit; PII-free responses; the service's own health alerts use a direct path (not the service) |
| 9 | Tests | rule unit tests; certify offline tests; a fired test alert reaches a person through the configured receiver |
| 10 | Prerequisites | N3; D-9 |
| 11 | Production verification | a synthetic alert is delivered; certify output includes routing and lane backlog |
| 12 | Rollback | receivers can be removed; rules are inert |
| 13 | Dependencies | N3 |
| 14 | Definition of done | runbooks reviewed; alert delivery proven in production; certify green |

## N14 — Canary governance

The canary percentages are acceptance accounts → 10 % → 50 % → 100 %. Gates are in Document 7 §5. Single routing is certified.

## N15 — Retirement

Follows Document 5 §10. Each retirement is a separate reviewed change, made only with the evidence listed there.

## N16 — Customer webhooks (only if D-W)

This needs a separate audit addendum. It is not before 90 days of stable user channels.

---

## Programme definition of done

1. Every category and channel in the unified service is at 100 % for at least 30 days. The CRITICAL lane is at 60 days, with zero break-glass use.
2. Old pipelines are retired per Document 5 §10.
3. All Document 7 §5 gates pass in production, including real-credential checks for every activated provider.
4. Document 8 S-1 … S-13 are closed.
5. No business action (sign-in, call, message, org change) has any synchronous dependency on the service. Fault-injection test R-12 passes in production.
