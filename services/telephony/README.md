# CF Meet Telephony service

`services/telephony/` is the Rust/Axum peer service that **executes** public-network
calls (Part E §39–§51). It owns Telephony legs (executions of Calling's canonical,
provider-independent CallLegs), durable telephony commands, provider bindings, provider
events, interconnect providers / routes / circuits / health / capacity, bridges,
provider-level usage, incidents and reconciliation, all in its own PostgreSQL database. It
shares no source with any other service and depends only on `packages/service-auth` and
the contracts in `packages/contracts/telephony/`.

It never decides whether a business user may call. Calling (route, spend, canonical Call)
and Privacy Relay (authority, protected contacts) decide, and every paid action carries
their proof. It never owns the logical Call, never receives a customer credential and
never touches another service's database. Raw telephone numbers exist only in memory
inside the execution boundary. Provider identifiers never leave it.

Decisions: [ADR 0013](../../docs/decisions/0013-routing-engine-and-route-plans.md) (Routing Engine, in Calling),
[ADR 0014](../../docs/decisions/0014-telephony-service-core.md) (core),
[ADR 0015](../../docs/decisions/0015-sip-media-edge-stack-and-network-topology.md) (SIP / media edge),
[ADR 0016](../../docs/decisions/0016-first-telecom-interconnect.md) (interconnect contract, first interconnect),
[ADR 0017](../../docs/decisions/0017-provider-failover.md) (failover, unknown outcomes).
Edge topology, segmentation, toll-fraud boundary and carrier peer configuration:
[`infra/telephony/README.md`](../../infra/telephony/README.md).

## Status

| Part E | Status |
|---|---|
| §39–§40 Telephony core: legs, durable commands with a transmit fence, three-valued outcomes, normalized events, durable outbox to Calling | **Implemented** |
| §41 SIP / media edge (Asterisk 20 + ARI): origination, inbound, ring, answer, RTP evidence, bridges, DTMF, hold / resume, redirect, hangup, unknown-peer refusal, edge-down safety | **Implemented; certified against SIPp simulators** (`tests/sip_certification.rs`) |
| §42–§43 Provider-neutral interconnect contract; SIP-trunk interconnect; deterministic test adapter | **Implemented**; both adapters run the same contract suite |
| §44–§47 App-to-phone, phone-to-app, phone-to-phone relay, callback relay (with Calling and Privacy Relay) | **Implemented and simulated end to end** |
| §48 Unknown provider outcome; §49 provider event normalization; §50 reconciliation; §51 failover | **Implemented** |
| Paid-action safety (spend reservations in Calling; duration limit, operator disable, circuits, abuse signals here) and bounded metrics | **Implemented** |
| Real lawful telecom interconnect certification (Nigeria first) | **BLOCKED.** It needs a contracted carrier / SIP trunk, a public number and staging credentials. `tests/real_interconnect_certification.rs` prints `NOT RUN` without them and never fakes success |

## Internal API

Service-authenticated (`X-CF-Service-Authorization`, ADR 0004), Calling only. Contract:
[`telephony/internal/v1.openapi.json`](../../packages/contracts/telephony/internal/v1.openapi.json),
bodies in [`telephony/commands`](../../packages/contracts/telephony/commands/v1.schema.json)
and [`telephony/routes`](../../packages/contracts/telephony/routes/v1.schema.json).

| Route | Permission (received by Telephony) |
|---|---|
| `POST /internal/v1/legs/originate` | `telephony-leg.originate` |
| `POST /internal/v1/legs/{id}/terminate\|hold\|resume` | `telephony-leg.terminate` / `telephony-leg.hold` / `telephony-leg.resume` |
| `GET /internal/v1/legs/{id}`, `GET /internal/v1/bridges/{id}`, `GET /internal/v1/routes/availability`, `GET /internal/v1/readiness` | `telephony-status.read` |
| `POST /internal/v1/bridges`, `DELETE /internal/v1/bridges/{id}` | `telephony-bridge.manage` |
| `POST /provider-callbacks/{provider}[/{token}]` | none; the provider adapter authenticates the callback before parsing (16 KiB limit, strict parser, dedupe) |
| `GET /health` | none |

Telephony → Calling is the durable outbox: normalized events
([`telephony/events`](../../packages/contracts/telephony/events/v1.schema.json)) are
delivered to Calling's `POST /internal/v1/telephony/events` in per-leg order, and inbound
offers ([`telephony/inbound`](../../packages/contracts/telephony/inbound/v1.schema.json))
go to `POST /internal/v1/telephony/inbound-calls`. Telephony → Privacy Relay: protected
contact resolution with a single-use grant, inbound relay resolution and inbound caller
protection.

Every outbox row carries the correlation id of the command whose flow produced it, and
event, keypad and capture deliveries continue it (worker log spans carry the leg id and
the correlation id). Delivery retries with capped backoff; an event Calling refuses
permanently (4xx other than an authentication / permission / rate-limit / conflict
refusal, or 404) or one that exhausted its attempts (360, about six hours at the 60 s cap)
is dead-lettered once and raises one `OUTBOX_DEAD_LETTER` incident per reason. Commands
stop at their `max_attempts`: an untransmitted command becomes `DEAD_LETTER` (an
ORIGINATE's leg fails as `NOT_SENT`), a transmitted one is `UNKNOWN_OUTCOME` and goes to
reconciliation, and a TERMINATE is never abandoned (its leg is held in `ENDING` and
reconciled). Concurrent workers keep each leg's commands in creation order.

## Configuration

See `src/config.rs` for the full table. Highlights:

* `DATABASE_URL`: Telephony's own database.
* Kill switches, all defaulting to `false`. Each one only stops NEW paid effects:
  * `TELEPHONY_ENABLED`
  * `PUBLIC_OUTBOUND_CALLING_ENABLED`
  * `PUBLIC_INBOUND_CALLING_ENABLED`
  * `CALLBACK_RELAY_ENABLED`
* `CF_RUNTIME_ENVIRONMENT`, `CF_SERVICE_AUTH_KEY_ID`, `CF_SERVICE_AUTH_PRIVATE_KEY_FILE`,
  `CF_SERVICE_TRUST_BUNDLE_FILE`: service identity. Keys and bundles come from files only.
* `CALLING_INTERNAL_BASE_URL`, `PRIVACY_RELAY_INTERNAL_BASE_URL`: peers. Without Privacy
  Relay, every protected origination fails closed before a destination exists. Without
  Calling, events stay durably in the outbox.
* The SIP / media edge: ARI URL and a credentials FILE (`infra/telephony/README.md`).
  Trunk usernames and passwords live in the edge's secret store. Database rows hold
  references only.
* `TELEPHONY_METRICS_ADDRESS` (optional): a separate listener for `GET /metrics`, with
  bounded counters and gauges whose labels are never identifiers: provider response
  classes (`telephony_provider_responses_total{operation,class}`) and SIP response
  classes (`telephony_sip_responses_total{class}`: 1XX / 2XX / 4XX / 5XX /
  NORMAL_CLEARING / UNSPECIFIED, never a cause text), inbound offers by result, the
  command queue (pending, retrying, in flight, unknown outcome, dead-lettered, oldest
  pending age), the outbox (pending, oldest pending age, dead-lettered), dependency
  failures and the SIP / media edge health.

Interconnects, routes, SIP peer references and relay-number bindings are managed with
`telephony-admin provider | route | sip-peer | number`. `telephony-admin incidents` lists
open incidents; `telephony-admin --operator <id> incidents resolve <incident-id>
[--note ...]` resolves one (audited in the same transaction). Incidents include
`PRIVACY_RELAY_UNAVAILABLE` (resolution could not reach Privacy Relay; resolves when it
answers again) and `SIP_EDGE_UNAVAILABLE` (the provider health refresh saw the edge
degraded / unavailable; resolves when healthy). The deterministic test adapter is refused
in `staging` and `production`.

## Development

```sh
make telephony-migrate      # apply migrations (services/telephony/.env)
make telephony              # run the service
make telephony-test         # all tests (TEST_DATABASE_URL; no Docker)
make telephony-lint         # fmt + clippy
make telephony-sip-cert     # real SIP edge certification (apt install asterisk sip-tester; SIPp needs CAP_NET_RAW)
make telephony-integration  # Calling <-> Telephony <-> Privacy Relay against real binaries
```

| Test | What it proves |
|---|---|
| `telephony_core` | Only Calling may command; authorization proof required; durable before any provider action; one effect per command; kill switches; Privacy Relay down fails closed (one `PRIVACY_RELAY_UNAVAILABLE` incident); caller identity only the relay identity bound to the chosen interconnect (`CALLER_IDENTITY_UNAVAILABLE` otherwise), its assignment reference kept on the leg; a rolled-back transaction leaves no leg / command / outbox intent; every leg state pair agrees between the Rust graph and the database trigger |
| `provider_contract` | Both interconnect adapters follow the neutral contract (capabilities, read-back, absence only with evidence, idempotent termination, hold / resume, callbacks authenticated before parsing, no numbers in diagnostics) |
| `unknown_outcome` | Timeout before send = safe retry; timeout / reset / malformed after send = UNKNOWN, never re-sent; crash after accept → reconciliation; late success binds; attempt caps: DEAD_LETTER before the fence, UNKNOWN after it, a TERMINATE never abandoned |
| `provider_events` | Callback authentication (valid / invalid / missing signature, previous-secret overlap), body limit, malformed provider ids and payloads, strict parser, dedupe, replay bursts, out-of-order and late events, quarantine of unknown objects, incident thresholds, no token / signature / secret / body in logs, retention, event contract. A DISABLED provider's callbacks are still authenticated and applied to its in-flight legs (disable stops new originations only) |
| `reconciliation` | Every drift case, unresolved terminations, provider inventory, orphan bridges, usage once, two reconcilers → one repair |
| `failover` | Definite → other interconnect; UNKNOWN → refused until reconciled; late primary success suppresses B; circuits, health, capacity; capacity reservation never blocked by a leg's foreign-key locks |
| `paid_safety` | Duration limit, operator disable, transmit-time re-check (route / provider disabled, circuit opened, destination class blocked between route choice and the fence → nothing sent), abuse signals, metrics label safety |
| `destination_classes` | Pre-toll classification: STANDARD proceeds; PREMIUM / HIGH_RISK refused; SATELLITE / SHARED_COST per the configured list; unclassified per `ALLOW` / `DENY`; longest prefix wins; other countries' rules never apply; the metric carries the class only |
| `sip_ingress` | Inbound dialogs: bound relay number accepted; unbound number / unknown peer → `SIP_PEER_REJECTED`; source outside `allowed_source_cidrs`; dialog bound; disabled route / provider; rate-limited burst → one `SIP_INGRESS_RATE_LIMITED` incident; no number in incident evidence |
| `outbox_delivery` | Permanent refusals and exhausted retries dead-lettered once (one incident per reason), never retried, never blocking the leg's later events; capped backoff; an expired outbox lease reclaimed and delivered exactly once; delivered events continue the originating command's correlation (also in worker log spans) |
| `operator_admin` | `telephony-admin` provider / route disable and incident resolve each write exactly one operator audit row; refusals write none |
| `sip_edge_outage` | SIP / media edge down (deterministic): public SIP originations fail definitely with nothing dialled and no usage, one `SIP_EDGE_UNAVAILABLE` incident, gauge and readiness; enterprise commands stop at their cap; a TERMINATE completes once the edge is back |
| `disaster_recovery` | Backup (`backup.sh --service telephony`) and restore into a fresh database; the restored workers run PENDING commands exactly once, never re-send a post-fence command, create no unauthorized leg or usage duplicate; no secret or destination number in the dump (SKIPPED without `pg_dump` unless `DISASTER_RECOVERY_CERT_REQUIRED=1`); a stopped-worker backlog drained by two workers exactly once in per-leg order |
| `migration_upgrade` | Previous head (all migrations but the newest) with data → head → one step down → head |
| `load` | Intake / execution, event ingestion and duplicate dedupe, reconciliation (and SIP signalling on the certification edge when installed) with `LOAD` lines: percentiles, error rate, DB connections, lock waits, worker lag |
| `http_contract` | The OpenAPI lists exactly the routes; real bodies validate |
| `sip_certification` | The real edge against SIPp (requires asterisk + sipp) |
| `real_interconnect_certification` | Real carrier: `NOT RUN` without protected credentials |
| `architecture`, `migrations` | No other service's source or database; provider / SIP specifics confined; no number logging; schema authority |
| `edge_config` | The committed edge configuration identifies endpoints by source IP only: no `username` identification, no user / anonymous identifier module, explicit module set, every endpoint has an `identify` match |

## Docker

```sh
docker build -f services/telephony/Dockerfile -t cfmeet-telephony .   # repository-root context
```

The image holds `telephony`, `telephony-migration` and `telephony-admin` only: no edge
configuration, no key, no trunk credential.
