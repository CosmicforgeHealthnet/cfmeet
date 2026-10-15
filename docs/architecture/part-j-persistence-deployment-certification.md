# Part J — Persistence, repository and deployment (§§89–98): certification

Status: **CLOSED — PASS** (software). Decisions: [ADR 0031](../decisions/0031-reliability-operations-recovery-and-deployment-separation.md);
ownership rules: [`service-data-ownership.md`](service-data-ownership.md). Final gate
results: [`part-k-production-acceptance.md`](part-k-production-acceptance.md) §1.

Legend: **E** existing (re-verified), **H** hardened, **N** new.

## §89 Repository structure — PASS (E/H)

| Owner | Path | Holds |
|---|---|---|
| Calling | `services/calling` | trust foundation, Call domain, API, realtime signaling, call-specific RTC adapter, routing / policy integration, enterprise calling, webhooks, usage / limits, Call records |
| Privacy Relay | `services/privacy-relay` | Communication Contexts, relay sessions, Contact Vault, privacy authorization, relay-number assignment policy |
| Telephony | `services/telephony` | Telephony core, SIP / PSTN legs, provider adapters, callbacks, routing execution, reconciliation |
| Shared | `packages/{contracts,service-auth,webhooks,object-storage}` | contracts / OpenAPI / event schemas; signing and transport primitives only — no business authority, no state machine |
| SIP / media edge | `infra/telephony` | Asterisk configuration, edge operations, certification harness |
| CI | `.github/workflows` | path-aware service, certification, supply-chain and boundary workflows |

`services/meeting-service` contains no Calling / Privacy Relay / Telephony logic (no call
legs, relay, Contact Vault, SIP or E.164 handling). `scripts/service-boundaries`
enforces the boundaries on every push: cross-service manifests / includes / paths /
database URLs / tables / helpers, Contact Vault confinement, plaintext phone fields,
and (N) `FOREIGN_SECRET_REFERENCE` — no service's production code or deployment config
names another service's secrets. The legacy phase-style documentation path of §89 is
deliberately **not** created; documentation lives under `docs/architecture` and
`docs/decisions`.

## §90 Calling schema family — PASS (E)

All ten logical tables exist under exactly these names: `calls`, `call_participants`,
`call_sessions` (+ `call_session_intervals`), `call_routes`, `call_legs`,
`call_attempts`, `call_authorizations`, `call_events`, `call_quality_records`,
`call_usage_records` (Calling 000005–000008), with the supporting `call_endpoints`,
`call_invitations`, `call_answer_claims`, `call_media_sessions`,
`call_routing_policies`, `call_telephony_commands`, `call_telephony_events`,
`call_public_bridges` and `calling_outbox_events`. Semantics proven by
`call_state_machine::*`, `call_records::*` (legs hold only an opaque execution binding;
attempts ordered and linked, unknown is not failure; quality bounded and append-only;
usage idempotent, finalization set once), `call_evidence::*` (events append-only with a
unique per-Call sequence and per-version state change). `calls.correlation_id` (N) links a
Call to its originating request.

## §91 Privacy Relay / Contact Vault schema family — PASS (E)

`communication_contexts`, `communication_context_participants`,
`privacy_relay_sessions`, `protected_contacts`, `contact_access_audit`,
`relay_number_inventory`, `relay_number_assignments`, `relay_audit_events` all exist
(plus channels / directions, fingerprints, grants, verification events, decisions,
commands, outbox, enforcement jobs, retention, and (N) `privacy_relay_incidents`,
`privacy_relay_operator_audit`). Constraints: active-participant uniqueness
(`uq_context_participants_active_*`), version columns, `encryption_key_version` on every
encrypted row, verification state, append-only triggers on every audit table, one active
holder per number (`uq_relay_assignments_holder` while ACTIVE / COOLDOWN), cooldown checks,
opaque `provider_binding_ref` that may not contain a number.

## §92 Telephony schema family — PASS (E)

`telecom_providers`, `telecom_routes`, `provider_call_bindings`, `provider_events`,
`telephony_commands`, `telephony_incidents`, `sip_peer_config_refs` all exist (plus legs,
bridges, captures, usage, outbox, reconciliation runs, number bindings, destination
classes, retention, operator audit). Provider and trunk secrets are never stored:
`credential_ref` / `secret_ref` are regex-checked references to deployment-managed files.
The only short-lived secret in the database is a single-use Contact Resolution Grant
secret of a not-yet-executed command (≤ 120 s, nulled at the transmit fence).

## §93 Constraints and indexing — PASS (E/H)

* Ownership indexes lead with tenant / application / environment on tenant-owned query
  paths; plan assertions run after `ANALYZE` (no statistics-timing flakiness), e.g.
  `enterprise_load::extension_calls_and_history_throughput`.
* Active uniqueness: single winning answer (`uq_call_answer_claims_participant`,
  `call_answer_group_claims`), active relay mapping, provider-event dedupe
  `UNIQUE (provider_id, provider_event_id)` (provider-qualified), Telephony usage one per
  leg.
* (H) Calling usage: `uq_call_usage_records_one_final_fact` — one finalized time fact per
  (Call, usage type, subject); `call_records::a_different_idempotency_key_can_never_bill_the_same_subject_twice`.
* State constraints: DB CHECKs + transition triggers agree with the Rust graphs —
  `call_state_machine::database_graph_equals_the_rust_graph_for_every_pair`, (N)
  `the_leg_transition_table_is_exhaustive_in_rust_and_in_the_database`, Telephony (N)
  `every_leg_state_pair_agrees_between_the_rust_graph_and_the_database`.
* Cleanup / reconciliation indexes: partial indexes for retention, setup-deadline sweep
  (N), pending commands / outbox; worker passes are bounded `LIMIT … FOR UPDATE SKIP LOCKED`.
* (H) Telephony: capacity reservation uses `FOR NO KEY UPDATE`, removing a deadlock with
  foreign-key lock checks found under SIP load
  (`failover::capacity_reservation_is_not_blocked_by_a_legs_foreign_key_locks`).

## §94 Migration strategy — PASS (H)

* Service-owned histories: `calling_seaql_migrations` (42 registered; number 000039 is unused), `privacy_relay_seaql_migrations`
  (14), `telephony_seaql_migrations` (15). Every migration is additive (schema / index /
  constraint before any flag) and has a working DOWN.
* Fresh up / status / reset / re-up: each service's CI migration smoke + `tests/migrations.rs`.
* (N) Upgrade from the previous head with data: each service's `tests/migration_upgrade.rs`
  migrates to N-1 (computed from the migrator), seeds rows, upgrades to head, asserts
  data intact and the schema equal to a clean install, rolls back one step and re-applies.
* (N) The real migrations apply as the least-privilege migrator role (§98).
* No cross-service platform-identity extraction was needed; none was undertaken.

## §95 Feature flags and rollback — PASS (H)

| Capability | Switch (server-side, environment) | Owner |
|---|---|---|
| Calling master (new Calls) | `CALLING_NEW_CALLS_ENABLED` (N, default on) | Calling |
| App calling | `CALLING_APP_CALLING_ENABLED` (N, default on) | Calling |
| Public outbound / inbound | `CALLING_PUBLIC_OUTBOUND_ENABLED`, `CALLING_PUBLIC_INBOUND_ENABLED`; Telephony `PUBLIC_OUTBOUND_CALLING_ENABLED`, `PUBLIC_INBOUND_CALLING_ENABLED` | Calling / Telephony |
| Callback relay | `CALLING_CALLBACK_RELAY_ENABLED`; Telephony `CALLBACK_RELAY_ENABLED` | Calling / Telephony |
| Enterprise / voicemail / recording | `ENTERPRISE_CALLING_ENABLED`, `VOICEMAIL_ENABLED`, `CALL_RECORDING_ENABLED`; Telephony `ENTERPRISE_SIP_ENABLED` | Calling / Telephony |
| Privacy Relay / Contact Vault / relay numbers | `PRIVACY_RELAY_ENABLED`, `CONTACT_VAULT_ENABLED`, `RELAY_NUMBER_POOL_ENABLED` | Privacy Relay |
| Provider / route circuit | `telephony-admin` provider / route enable-disable and circuits (operator-audited, `operator_admin::provider_and_route_disable_each_write_exactly_one_audit_row`) | Telephony |

Clients only observe capabilities. Each switch refuses only new work of its capability;
ending, cleanup, reconciliation and artifact deletion never consult one. Evidence:
`feature_rollback::*` (master, app, public outbound, public inbound, callback relay,
enterprise — live state present, intact, ends cleanly, unrelated app calling continues),
`enterprise_voicemail::kill_switch_blocks_new_voicemail_only`,
`enterprise_recording::kill_switch_never_blocks_stop_finalize_or_delete`,
`contact_vault::the_kill_switch_and_missing_configuration_fail_closed`. All switches are
documented in each service README / `.env.example`.

## §96 Local development policy — PASS (E/H)

Every backend service and its tests run against a directly reachable PostgreSQL
(`make calling-test`, `privacy-relay-test`, `telephony-test`; `TEST_DATABASE_URL`). Docker
is optional: dependency convenience, image builds, CI service containers and
certification. The root README previously listed Docker as a prerequisite and called
Privacy Relay / Telephony "future"; both contradictions are fixed.

## §97 CI and build gates — PASS (H)

| Workflow | Gates |
|---|---|
| Calling service | fmt, check, clippy `-D warnings`, architecture guards, all PostgreSQL tests (contract parity, tenant / security, DR required), migration smoke, real migrations through least-privilege roles, startup refusals, build, Docker image |
| Privacy Relay service | same, plus crypto / contact / IDOR / expiry / revocation / concurrency / retention suites and the Calling e2e |
| Telephony service | same, plus provider contract / events / paid safety / reconciliation / failure injection and the three-service e2e |
| Contracts and service auth, Webhooks, Object storage | schema validation, examples / counterexamples, validator mutation tests, package tests |
| Calling RTC / Telephony SIP / Enterprise calling certification | real LiveKit / Asterisk + SIPp / Asterisk + SIPp + S3; `*_CERT_REQUIRED=1` turns a missing dependency into a failure and a SKIPPED marker fails the job; SIP signalling load (N) |
| Dependency and image security (N) | RustSec per lockfile; image root-filesystem secret scan; no pending Debian security update |
| Service boundaries (H) | no path filter: guard + mutation tests, operations registry (N), secret scan, per-service database isolation (N) |

Path awareness (H): consumers now trigger on every shared change they can be affected by —
`packages/contracts/**` (incl. validator, OpenAPI, manifest schema), `service-auth`,
`webhooks` and `object-storage` for every service that links them (and the RTC /
enterprise certifications), `services/privacy-relay/**` and `services/calling/**` for the
Telephony e2e, `infra/deployment/**` for the services, and the boundary / secret-scan
workflow on every push.

## §98 Deployment separation — PASS (H)

* Three images, three databases, three credential sets; each service scales, restarts
  and fails independently (`infra/deployment/README.md`).
* Blast radius: Telephony provider / ARI secrets only in Telephony, Contact Vault keyrings
  only in Privacy Relay, LiveKit / webhook / pepper / signing secrets only in Calling —
  enforced by `FOREIGN_SECRET_REFERENCE`.
* (N) Database roles: `infra/deployment/postgres/provision-service-database.sh` creates a
  migrator owner and a DML-only runtime role per database with CONNECT revoked from
  PUBLIC; `verify-service-isolation.sh` proves no DDL / TRUNCATE / temp tables for the
  runtime role and no connection from any role to another service's database (CI, with the
  real migrations of each service).
* Public SIP, carrier signalling and provider callbacks terminate only in Telephony and
  `infra/telephony`; the Meeting Service is not a telecom ingress proxy.
* Images: non-root, filtered build contexts, Debian security updates applied at build
  time (N), image secret scan (N).
* Health: `GET /health` (database) per service; services refuse to start with pending
  migrations; release order and rollback documented. Root `docker-compose.yml` remains a
  Meeting-Service convenience (documented), not the deployment of the Part III services.
