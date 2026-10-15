# Part I — Reliability, observability and operations (§§81–88): certification

Status: **CLOSED — PASS** (software). Decisions: [ADR 0031](../decisions/0031-reliability-operations-recovery-and-deployment-separation.md).
Final tree and gate results: §9 and the Part K report
([`part-k-production-acceptance.md`](part-k-production-acceptance.md)).

Method: every requirement was mapped to existing evidence first (Parts B–H already
provided transactional outboxes, leased SKIP LOCKED workers, Telephony reconciliation,
incidents and bounded metrics); only the gaps below were implemented. Reliability stays
inside the service that owns the state — no new service, no shared worker or incident
service.

Legend: **E** existing (re-verified), **H** hardened, **N** new.

## §81 PostgreSQL authority and transactional outbox — PASS

| Requirement | | Evidence |
|---|---|---|
| Call events + outbox in one transaction | E | deferred trigger `trg_call_events_require_outbox`; `call_evidence::state_changes_commit_with_their_event_and_outbox_row_or_not_at_all`, `a_failed_creation_persists_nothing` |
| Calling → Telephony commands in the business transaction | E | `enqueue_in(txn, …)` in outbound / bridge / failover / enterprise SIP / voicemail / recording |
| Release of paid legs (TERMINATE) commits with the terminal transition | N | deferred trigger `trg_calls_release_public_legs` (Calling 000043); `durable_workers::the_terminate_intent_commits_with_the_terminal_transition_and_survives_a_crash` |
| Privacy Relay state + audit + outbox / enforcement together | E | `audit::every_authority_mutation_writes_state_audit_and_outbox_together`, `a_failure_before_the_outbox_rolls_the_whole_command_back` |
| Telephony leg + command + outbox together; rollback leaves nothing | E/N | `telephony_core::a_rolled_back_transaction_leaves_no_leg_command_or_outbox_intent` |
| Crash-safe claims (lease + claim token), expired lease recovers | H | Calling `durable_workers::{a_claim_whose_worker_died_is_reclaimed_after_its_lease_and_sent_once, a_stale_worker_cannot_complete_or_reschedule_after_losing_its_lease, an_outbox_claim_lost_to_a_crash_is_reclaimed_after_its_lease_and_published_once}`; Privacy Relay `outbox_publication::a_crashed_publishers_lease_expires_and_another_instance_publishes_exactly_once`, `enforcement::a_crashed_workers_lease_expires_and_another_instance_applies_exactly_once`; Telephony `outbox_delivery::an_expired_outbox_lease_is_reclaimed_and_delivered_exactly_once`, `unknown_outcome::worker_crash_after_the_provider_accepted_goes_to_reconciliation` |
| Two workers never execute the same paid action | E/N | `durable_workers::two_concurrent_dispatch_workers_send_each_command_exactly_once`; Telephony `duplicate_commands_have_one_provider_effect`; `disaster_recovery::a_stopped_worker_backlog_drains_exactly_once_in_per_leg_order` |
| Retry classes: transient / permanent / unknown, capped backoff, no blind paid retry | H | Privacy Relay `outbox_publication::http_failures_are_classified_permanent_or_transient`; Telephony `outbox_delivery::{a_permanent_refusal_is_dead_lettered_once_and_never_retried, transient_failures_back_off_with_a_cap_and_dead_letter_only_when_exhausted}`; `unknown_outcome::timeout_after_send_is_unknown_and_never_blindly_retried` |
| Dead letter stops retrying and is visible (incident) | H/N | Calling `a_command_at_its_attempt_cap_goes_dead_stops_retrying_and_raises_one_incident`, `an_outbox_claim_crashing_at_its_attempt_cap_is_dead_lettered_not_reclaimed_forever`; Privacy Relay `enforcement_is_dead_lettered_after_its_attempt_cap_stops_retrying_and_is_visible`, `a_permanent_refusal_is_dead_lettered_at_once_and_raises_one_incident`; Telephony `a_command_at_its_attempt_cap_is_dead_lettered_and_a_transmitted_one_is_reconciled`, `a_control_command_that_can_never_run_stops_at_its_cap_and_a_terminate_is_never_abandoned` |
| Replay idempotent (events, commands, provider events, webhooks, usage, artifacts, retention) | E | `idempotency::concurrent_duplicates_have_one_domain_effect`; `provider_events_are_deduplicated_by_provider_and_event_id`; `webhooks::concurrent_workers_and_crash_recovery_deliver_once`; `call_records::usage_records_are_idempotent_and_finalization_is_set_once`; `enterprise_voicemail::concurrent_finalization_stores_once`; `retention::two_concurrent_workers_act_on_each_row_exactly_once` |
| A revocation racing the originate never sends a paid command (defect found and fixed) | N | `cross_service_races::{context_revoked_after_the_grant_was_issued_issues_no_paid_command, context_revoked_while_the_originate_is_in_flight_ends_in_the_controlled_outcome}` (12 rounds each) |

## §82 Worker responsibilities — PASS

The registry `scripts/service-boundaries/operations_registry.json` lists every durable
responsibility with exactly one owning service, its entry point and its proving tests;
`check_operations_registry.py` (CI, `make service-boundaries`) fails on a missing required
responsibility, a renamed entry point, a deleted test or an unknown owner (6 mutation
tests).

| Responsibility | Owner | | Entry point | Key evidence |
|---|---|---|---|---|
| Call timeout (stale pre-connect Calls, offers, media admission) | Calling | N | `expire_stalled_setups` (+ offer expiry) | `durable_workers::a_call_stuck_before_connect_times_out_once_and_its_paid_leg_is_released` (two sweepers), `the_setup_deadline_never_cuts_a_legitimate_queue_wait` |
| Relay expiry / grace / release / cooldown | Privacy Relay | E | `expire_relay_assignments`, `complete_relay_cooldowns`, `expire_sessions` | `concurrent_workers_release_and_cool_down_each_assignment_exactly_once`, `a_delayed_callback_never_reaches_the_next_session` |
| Telephony command worker | Telephony | H | `run_commands_once` (transmit fence re-checks Part H controls) | `paid_safety::a_change_between_route_choice_and_the_transmit_fence_stops_the_request` |
| Telephony reconciler | Telephony | E | `reconcile_once` | `reconciliation::*` (stuck ringing, connected-but-ended, active-without-binding, orphan bridge, unknown termination, event gaps, missing usage, two reconcilers) |
| Usage finalizer (KNOWN / ESTIMATED / UNKNOWN, no duplicate fact) | Telephony / Calling | E/H | `finalize_usage_once`, `finalize_usage` | `missing_usage_finalization_is_completed_exactly_once`; `a_different_idempotency_key_can_never_bill_the_same_subject_twice` (unique index `uq_call_usage_records_one_final_fact`) |
| Voicemail / recording artifact worker | Calling | E | `voicemail_tick`, `recording_tick`, `capture_tick`, `orphan_tick` | `concurrent_finalization_stores_once`, `storage_failure_is_recoverable_and_captures_can_fail`, `orphaned_capture_objects_are_never_left_behind` |
| Webhook delivery worker | Calling | E | `deliver_due_once` | signed payload / delivery id / timestamp, bounded retry, dead letter, replay, SSRF: `webhooks::*` |

## §83 Redis and realtime degradation — PASS (E)

There is no Redis in Calling, Privacy Relay or Telephony. Realtime fan-out is PostgreSQL
`LISTEN / NOTIFY` published after commit and holds no authority; every rate limit and
quota is enforced in PostgreSQL. Evidence: `call_history::a_missed_call_survives_push_and_realtime_outage`,
`signaling::a_disconnected_endpoint_recovers_from_the_api`, `signals_fan_out_across_instances`,
`degradation_matrix::realtime_fanout_down_loses_signals_but_never_calling`. Clients recover
the authoritative Call state and version from the API on reconnect; client replay is never
trusted.

## §84 Observability — PASS

| Area | | Families (bounded enum labels only) |
|---|---|---|
| Calling lifecycle | N | `calling_call_requests_total`, `calling_call_request_refusals_total{reason}`, `calling_call_setup_seconds`, `calling_call_ring_seconds`, `calling_call_connected_seconds`, `calling_call_outcomes_total{outcome}` |
| RTC | N | `calling_media_admissions_total{source,result,reason}`, `calling_media_reconnects_total`; end-of-call quality summary from real evidence (admitted sessions, reconnects, connected participants) — asserted against real LiveKit in the RTC certification. Transport RTT / jitter / loss are not available server-side and are not invented. |
| Telephony | E/N | originations, leg transitions, setup latency, bridges, circuits, reconciliation, provider health; new `telephony_provider_responses_total{operation,class}`, `telephony_sip_responses_total{class}` (RFC 3398 classes, never cause text), `telephony_inbound_offers_total{result}` |
| Privacy Relay | N | separate metrics listener (`PRIVACY_RELAY_METRICS_ADDRESS`): contexts, sessions, relay assignments / releases / cooldowns, authorization decisions incl. expired / blocked, resolution failures |
| Fraud / spend | E | spend blocks, velocity, route denials, destination-class refusals, circuit openings, abuse signals |
| Workers | N | queue depth and oldest-pending age for every outbox / command queue, retries, dead letters, worker lag |

Label safety is enforced per service by source and rendered-registry guards
(Calling `metric_safety::*`, Privacy Relay `metric_safety::*`, Telephony
`paid_safety::metrics_are_bounded_and_never_carry_identifiers`). Per-tenant quality is a
query over durable records, never a metric label.

## §85 Structured logging and tracing — PASS

The request's correlation id is stored on the Call (`calls.correlation_id`) and on every
durable command / outbox row and continued on every hop; TERMINATE / BRIDGE / ANSWER /
PROMPT, Telephony → Calling events, keypad and capture deliveries and Privacy Relay
publications no longer mint new ids (H). Worker spans carry `call_id` / leg id +
correlation. Evidence: Calling `trace_correlation::one_public_call_is_linked_end_to_end_by_call_id_and_request_correlation`
(request → Call → route → leg → command → Telephony event → termination → usage, logs
included, no telephone number); Privacy Relay `correlation_propagation::*`; Telephony
`outbox_delivery::delivered_events_continue_the_originating_commands_correlation`. Part H
log-leak guards unchanged and green.

## §86 Operational incidents — PASS

Per-service incident tables share one shape (dedupe key unique while OPEN, occurrences,
first / last seen, severity, safe summary; Privacy Relay's summary is DB-checked to refuse
digits / `+` / `@` / `sip:`). Lifecycle OPEN → RESOLVED with an audited operator resolve in
each admin CLI (`platform_admin incidents resolve`, `privacy-relay-admin incidents resolve`,
`telephony-admin incidents resolve`). Category coverage of §34:

| §34 symptom | Category |
|---|---|
| Carrier / provider outage | `PROVIDER_CIRCUIT_OPENED`, `UNKNOWN_*_OUTCOME`, route-denial bursts |
| SIP edge degradation / authentication attack | `SIP_EDGE_UNAVAILABLE` (N), `SIP_PEER_REJECTED`, `SIP_INGRESS_RATE_LIMITED`, `CALLBACK_*` |
| Orphan / stuck Call or bridge | `ORPHAN_BRIDGE_LEG`, `RINGING_TIMEOUT`, `STUCK_QUEUE_ENTRY`, `PROVIDER_ACTIVE_WITHOUT_BINDING` |
| Repeated unknown provider outcome | `UNKNOWN_ORIGINATION / TERMINATION / CONTROL_OUTCOME` |
| Failure spike by route / provider | `REPEATED_ROUTE_DENIALS`, circuit incidents |
| Contact Vault / key management | `CONTACT_KEY_UNAVAILABLE` (N), `RELAY_MAPPING_AMBIGUOUS` (N), `PRIVACY_RELAY_UNAVAILABLE` (N) |
| Fraud / spend threshold | `ABUSE_SIGNAL`, `REPEATED_BLOCKED_ATTEMPTS`, `ABUSE_REPORTS_BURST`, `CREDENTIAL_GUESSING` (N) |
| Outbox / worker lag | `WORKER_BACKLOG` (N), `OUTBOX_DEAD_LETTER` (N), `TELEPHONY_COMMAND_DEAD` (N), `ENFORCEMENT_DEAD_LETTER` (N), `CAPTURE_DELIVERY_DEAD` |
| RTC / media degradation | media-provider failures surface as Call failures + `calling_media_admissions_total{result}`; reconciliation repairs drift (`app_media::reconciliation_repairs_drift_without_creating_authority`) |

Evidence: `admin_cli::incidents_are_listed_and_resolved_by_an_audited_operator`,
Privacy Relay `incidents::*`, Telephony `operator_admin::an_operator_resolves_an_open_incident_with_exactly_one_audit_row`.

## §87 Graceful degradation — PASS

`tests/degradation_matrix.rs` (Calling, N), one test per dependency, each proving the
dependent capability fails closed while app-to-app and enterprise internal calling keep
working: carrier / route down, SIP edge / Telephony unavailable, RTC provider failing,
Privacy Relay unavailable, Contact Vault unavailable, spend authority unprovable (DB fault
during reservation), realtime fan-out down. Existing: `public_failover::public_route_trouble_never_disables_app_calls`,
`enterprise_operations::telephony_down_degrades_only_sip_paths`; Telephony
`sip_edge_outage::*` (N, deterministic edge-down), `telephony_core::privacy_relay_down_fails_closed_before_any_destination_exists`.
Public → app fallback never reveals a number (Part D/E routing unchanged).

## §88 Backup, recovery and business continuity — PASS

* Tooling (N): `infra/deployment/backup/backup.sh` / `restore.sh` — one service database
  per dump (refuses another service's database), checksum, empty-target-only restore in
  one transaction. Keyrings and secrets are never in a database backup.
* Exercises (N), each service's `tests/disaster_recovery.rs`, run in CI with
  `DISASTER_RECOVERY_CERT_REQUIRED=1`:
  * **Calling** — restored DB: migrator reports nothing pending; leased TERMINATE, outbox
    events and webhook deliveries drain exactly once (no new originate, paid leg or usage
    fact); the connected Call reconciles; a restored voicemail still needs scope and is
    404 to another tenant; a deleted one stays deleted; the dump holds no number or
    secret. Worker backlog (20 Calls, Telephony down) drains across two instances with
    per-Call ordering intact.
  * **Privacy Relay** — revoked / expired mappings stay unusable (delayed callback
    refused), cooldown preserved, outbox / enforcement drain once on two instances; the
    dump contains no plaintext number or key material; restoring without the keyring
    version refuses start / fails closed with `CONTACT_KEY_UNAVAILABLE`, never plaintext.
  * **Telephony** — PENDING commands execute exactly once; a command past the transmit
    fence is never re-sent (reconciliation); no unauthorized active leg; no duplicate
    usage; the dump holds no provider secret. Backlog of 32 commands on 8 legs drains
    exactly once in per-leg order on two workers.
* Provider outage playbook: disable route / provider (`telephony-admin`, audited) → app
  calling continues (`degradation_matrix`, `public_failover`) → unknown legs reconciled
  (`reconciliation::*`) → re-enable after health validation (provider health refresh,
  `SIP_EDGE_UNAVAILABLE` resolves on healthy). Documented in `infra/deployment/README.md`.
* Environment-level items not certifiable here: object-storage bucket versioning /
  lifecycle and cross-region replication, PITR / WAL archiving of the managed PostgreSQL
  instances, secret-store backup of keyrings — operator responsibilities listed in
  `security-deployment-checklist.md` and `infra/deployment/README.md`.

## Residual notes (not failures)

* Dead-lettered work is final; re-drive is the Calling outbox replay command or an
  operator action from the incident.
* A Calling webhook delivery that reaches its attempt cap while leased stays visible in the
  `calling_webhook_deliveries_pending` gauge (customer-endpoint failures are the
  customer's to fix and already end in FAILED with replay); it has no dedicated incident.
  `WORKER_BACKLOG` samples the outbox and the Telephony command queue.

## 9. Results

See [`part-k-production-acceptance.md`](part-k-production-acceptance.md) §1 for the final
full-suite counts and CI runs on the final SHA.
