# CF Meet Part V — Parts H, I and J (§§97–145): implementation evidence

**Status:** SOFTWARE EVIDENCE on `staging`. Not acceptance, not a release authorization,
not a production pass. `main` is unchanged (`689728c`) and nothing was deployed.
**Scope:** Part H integration and migration, Part I APIs / events / persistence, Part J
security, reliability and operations — backend only (no client UI).
**Prior evidence:** [Part H scope register](part-h-backend-scope-and-carry-forward.md),
[Parts E–G](parts-e-g-evidence.md), [Workstreams A–D](workstreams-a-d-evidence.md),
[ADR 0053](../../decisions/0053-meeting-chat-migration-and-identity-bridge.md).

## 1. Commits (all on `origin/staging`, each verified against the remote after push)

| Commit | Content |
|---|---|
| `6993fc6` | H1: first-party delegation, Meeting conversation sync, Platform Access `principals/ensure` |
| `2a742bd` | H: Meeting chat migration (shadow → cutover → rollback → retire), CF Meet identity bridge, context authority |
| `e483f1e` | H §103: Privacy Relay pushes communication-context authority to Messaging |
| `a565092` | H §§106–112: SMS Bridge (Messaging conversation side + SMS Gateway transport side) |
| `0f9766a` | I §113 / §129: roles, bans, replies / threads, reactions, pins, unread, usage metering |
| `7871004` | I §§116–122: webhooks through Calling's existing platform webhook system; query-plan evidence |
| `ba3959f` | CI fixes (Privacy Relay smoke table count; SMS Gateway rustfmt) |
| `b1f8fd2` | J: production-E2EE refusal, kill switches, incidents, KT rollback detection, operator CLI, DR drill |
| `d65700a` | Deterministic reaction ordering (CI-found race) |

## 2. §§97–145 audit matrix

`DONE` = implemented and tested here. `PARTIAL` = implemented, with the named gap.
`EXTERNAL` = needs evidence that software alone cannot produce.

| § | Requirement | State | Evidence |
|---|---|---|---|
| 97 | Meeting chat migration plan; Meeting keeps admission / policy | DONE | ADR 0053; `services/meeting-service/src/services/unified_messaging/` |
| 98 | Idempotent MEETING conversation per conference; version-fenced membership | DONE | `sync_participant` / `sync_members`; Messaging `integrations` tests |
| 99 | Compatibility adapter; no new authoritative legacy writes after cutover | DONE | `/api/v2/messaging/…` bridge routes; `CHAT_MIGRATED` triggers; `unified_messaging_api` tests |
| 100 | Shadow → cutover → rollback gate with parity | DONE | `migration.rs` state machine; `unified_messaging_e2e` (real processes) |
| 101 | `LEGACY_MANAGED` provenance; never relabelled E2EE | DONE | migration import; Messaging `securityProvenance` |
| 102 | Product-neutral integrations | DONE | delegated surface `/internal/v1/delegated/**`; no product names in contracts |
| 103 | Privacy Relay authority sync | PARTIAL | both sides tested (signed scripted peers); no real-process PR + Messaging run |
| 104 | Identity bridge (CF Meet user / participant → principal) | DONE | `identity.rs`; `principals/ensure`; real-process E2E |
| 105 | External customer journey | DONE | Messaging tests with external (non-first-party) environments; `platform_access_e2e` |
| 106–112 | SMS Bridge: explicit downgrade, separate transport state, controls, synthetic certification | PARTIAL | software both sides; synthetic certification against scripted peers only |
| 113 | API inventory (members role / ban, replies / threads, reactions / pins, receipts / unread) | DONE | `tests/engagement.rs`; contracts http / common 1.9.0 |
| 114–115 | Versioning, errors, internal APIs authenticated per operation | DONE | contract versions in `packages/contracts/manifest.json`; service-auth REGISTRY |
| 116–117 | Realtime / domain events, outbox | DONE | events contract 1.6.0; reaction / pin events; outbox |
| 118 | Webhooks via the existing platform system (no second system), privacy | DONE | Messaging `webhook_relay`; Calling producer `cfmeet.messaging`, migration 000046 |
| 119–122 | Persistence, DB isolation with real roles, EXPLAIN evidence, benchmarks | DONE | `verify-service-isolation.sh` (real migrations); `tests/query_plans.rs`; `tests/load.rs` (ignored, measured) |
| 123–128 | Redis never authoritative; object storage | DONE (carried) | Part E / F evidence; relay and usage live in PostgreSQL only |
| 129 | Usage metering, no billing engine, no prices | DONE | `messaging_usage_records` (no cost column), `GET /v1/messaging/usage` (`pricing: NONE`) |
| 130–132 | Tenant isolation, data minimisation, rate limits | DONE (carried + new) | environment-scoped queries; usage / relay tests check cross-environment blindness |
| 133 | Kill switches; production E2EE disabled | DONE | `tests/operations.rs`; config refuses `MESSAGING_E2EE_DEVICES_ENABLED=true` in production |
| 134–135 | Reliability, jobs | DONE | leased / SKIP LOCKED workers, retries, dead letters → incidents |
| 136–138 | Backup / restore tested; DR incl. KT rollback detection; no invented RTO / RPO | DONE | `tests/disaster_recovery.rs`; durations printed, no RTO / RPO claimed |
| 139–141 | Observability (bounded labels), tracing, incidents | DONE | metric labels are `&'static str` only; `messaging_incidents` with fixed safe summaries |
| 142 | Secrets | DONE (carried) | key files only (inline keys refused); keyring never in backups (DR test) |
| 143 | Local dev without Docker | DONE | `services/messaging/README.md` "Running locally" |
| 144–145 | CI extended, never weakened; boundary guards | DONE | new gates in `messaging.yml`; operations registry; architecture guard unchanged |

## 3. API acceptance (new in Parts H–J)

| Method | Route | Service | Scope | Implementation | Tests | OpenAPI |
|---|---|---|---|---|---|---|
| PATCH | `/v1/conversations/{id}/members/{principalId}` | Messaging | `conversations.manage` | `membership::change_role` | `engagement` | `changeMemberRole` |
| GET / POST | `/v1/conversations/{id}/bans` | Messaging | `conversations.manage` | `membership::bans` / `ban` | `engagement` | `listBans` / `banMember` |
| DELETE | `/v1/conversations/{id}/bans/{principalId}` | Messaging | `conversations.manage` | `membership::unban` | `engagement` | `unbanMember` |
| POST | `/v1/conversations/{id}/messages` (`replyToMessageId`) | Messaging | `messages.send` | `engagement::set_reply` | `engagement` | `SendMessageRequest` |
| GET | `/v1/conversations/{id}/messages?threadRootId=` | Messaging | `messages.read` | `messages::list` | `engagement` | list parameters |
| GET | `/v1/conversations/{id}/messages/{mid}/reactions` | Messaging | `messages.read` | `engagement::reactions` | `engagement` | `listReactions` |
| PUT / DELETE | `/v1/conversations/{id}/messages/{mid}/reactions/{reaction}` | Messaging | `messages.send` | `engagement::react` | `engagement` | `addReaction` / `removeReaction` |
| GET | `/v1/conversations/{id}/pins` | Messaging | `messages.read` | `engagement::pins` | `engagement` | `listPins` |
| PUT / DELETE | `/v1/conversations/{id}/pins/{mid}` | Messaging | `messages.send` (OWNER / ADMIN) | `engagement::pin` | `engagement` | `pinMessage` / `unpinMessage` |
| GET | `/v1/conversations/{id}/unread` | Messaging | `messages.read` | `engagement::unread` | `engagement` | `getUnreadCount` |
| GET | `/v1/messaging/usage` | Messaging | `usage.read` | `usage::summary` | `engagement` | `getMessagingUsage` |
| GET | `/v1/messaging/sms-bridge/disclosure` | Messaging | `messages.read` | `sms_bridge::disclosure` | `sms_bridge` | yes |
| GET / POST | `/v1/conversations/{id}/sms-bridges` | Messaging | `messages.read` / `conversations.manage` | `sms_bridge::list` / `create` | `sms_bridge` | yes |
| POST | `/v1/conversations/{id}/sms-bridges/{bridgeId}/close` | Messaging | `conversations.manage` | `sms_bridge::close` | `sms_bridge` | yes |
| GET | `/v1/conversations/{id}/messages/{mid}/sms-transport` | Messaging | `messages.read` | `sms_bridge::message_transport` | `sms_bridge` | yes |
| POST | `/internal/v1/context-authorities` | Messaging | `messaging-context-authority.apply` (Privacy Relay) | `integrations::context_authority` | `integrations` | internal 1.x |
| POST | `/internal/v1/sms-bridge/inbound` | Messaging | `messaging-sms-bridge.deliver` (SMS Gateway) | `sms_bridge::accept_inbound` | `sms_bridge` | internal |
| POST / GET | `/internal/v1/sms-bridge/bindings`, `…/messages[/{id}]` | SMS Gateway | `sms-bridge.submit` (Messaging) | `sms::bridge` | `tests/bridge.rs` | `sms-gateway/internal` |
| POST | `/internal/v1/platform/events` (producer `cfmeet.messaging`) | Calling | `platform-event.publish` | `webhooks::ingest`, `catalog::project_messaging` | `webhooks::messaging_events_become_webhooks_without_content` | Calling http 1.11.0 |
| — | `/api/v2/messaging/…`, `/api/v2/admin/chat-migration` | Meeting | Meeting auth | `handlers/v2/unified_messaging.rs` | `unified_messaging_api`, `unified_messaging_e2e` | Meeting OpenAPI |

## 4. Database report

* **Isolation with real roles:** `infra/deployment/postgres/verify-service-isolation.sh`
  applied Messaging's real migrations (1–13) as the migrator role: 130 checks OK. The
  runtime role reads every table, has no DDL, and no other service's role can connect.
* **New Messaging tables:** reactions, pins, usage records (immutable, no cost column),
  webhook relay (trigger-fed in the event's transaction), kill switches, incidents,
  operator audit (append-only).
* **Query plans:** `tests/query_plans.rs` proves that each of 15 hot queries is served
  by an index. Sequential scans are priced out, and the indexes used are printed as
  `PLAN` lines. This proves that an index exists for each predicate. It does not certify
  plan choice at production volume.
* **Benchmarks:** `tests/load.rs` (ignored, run explicitly) is carried from Part E–G and
  was not re-run here.

## 5. Security report

* Production E2EE is refused by configuration. No server key escrow. No E2EE plaintext
  or reaction rows on the server (`E2EE_REACTIONS_ARE_MESSAGES`).
* SMS: never bridges an E2EE conversation; explicit, versioned disclosure; the number is
  sealed at the gateway and never returned or stored in Messaging; there are no prices.
* Webhooks: one webhook system (Calling's). Messaging relays ids-only envelopes; Calling
  projects them through an allow-list. Tests check that text, ciphertext, device and
  epoch data never cross.
* Backups: no message body of either profile appears in the archive, and the managed
  keyring is never in it. MANAGED search lexemes are present by design, so backups must
  be protected like MANAGED content.
* KT rollback: a database behind or forked from the operator checkpoint gives a CRITICAL
  incident, and log appends fail closed.
* Operator actions require a named operator and are audited append-only.

## 6. Mandatory scenarios

| # | Scenario | State | Evidence |
|---|---|---|---|
| 1 | Meeting migration (real processes) | PASS (software) | `unified_messaging_e2e` with the real Platform Access and Messaging binaries |
| 2 | Native identity | PASS (software) | same journey: identity bridge, logout revocations |
| 3 | External application | PASS (software) | external-environment journeys; `platform_access_e2e` |
| 4 | Privacy Relay revocation | PARTIAL | each side tested against a signed peer; no real-process chain |
| 5 | Managed SMS | PARTIAL | both sides tested with scripted peers; no carrier |
| 6 | E2EE downgrade protection | PASS (software) | plaintext into E2EE refused; E2EE never bridged; no server reactions |
| 7 | Failure recovery | PASS (software) | relay / outbox / SMS retries and dead letters; Meeting E2E recovery |
| 8 | Backup / restore | PASS (software) | `disaster_recovery` (backup 242 ms, restore 409 ms on the dev host; no RTO / RPO) |
| 9 | Webhook privacy | PARTIAL | Messaging relay and Calling projection / delivery each tested; no single real-process chain |

**SMS BRIDGE SOFTWARE: PARTIAL** (complete in software; the synthetic certification uses
scripted peers) · **PUBLIC CARRIER EDGE: PENDING EXTERNAL CERTIFICATION**.

## 7. CI

See the CI table in the final report of this assignment (run URLs per SHA). A queued run
is not counted as success.

## 8. Declarations

| Declaration | State |
|---|---|
| SOFTWARE COMPLETE | PARTIAL — scenarios 4, 5 and 9 lack a real-process cross-service run; breakout / private Meeting chat stay legacy |
| INTEGRATION READY | PARTIAL — contracts and both sides of every integration exist and are tested |
| EXTERNAL EDGE CERTIFIED | NO — no carrier, no external cryptographic review, no device runtime certification |
| PRODUCTION PASS | NO |

**Full Part V production readiness: NOT APPROVED.**
