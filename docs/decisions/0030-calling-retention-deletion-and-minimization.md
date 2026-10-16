# ADR 0030 — Calling retention, deletion and minimization

Status: Accepted

## Context

Part H §80 requires every service to own the retention of its data with one shared
vocabulary (ADR 0029 §10). Privacy Relay
([`services/privacy-relay/src/retention`](../../services/privacy-relay/src/retention/mod.rs):
contact-value erasure, verification events, grants, access audit, relay evidence, outbox,
number ledger) and Telephony
([`services/telephony/src/retention`](../../services/telephony/src/retention/mod.rs):
provider events, resolved incidents, holds, retention audit) implemented their part.
Calling kept everything indefinitely: Calls with display metadata, webhook history,
incidents, provider evidence and the operator audit had no bound, no hold, no deletion
audit, and principals had no minimization (only a status).

The constraints are the ones the earlier parts fixed: Calls, events, usage, spend and the
audits are append-only evidence guarded by triggers; call events are the outbox source
(`fk_calling_outbox_events_source … ON DELETE RESTRICT`) and the evidence of every state
transition; principals are referenced by every Call relation (`ON DELETE RESTRICT`).

## Decision

### 1. Data classes (one policy each — `retention::policies(&settings)`)

| Class | Action | Bound | Why |
|---|---|---|---|
| `CALL_RECORD` | **Minimize** display metadata of *terminal* Calls: `call_participants.display_name` → NULL (`display_minimized_at` set), `enterprise_call_contexts.entry_label` → the entry extension code | `CALLING_RETENTION_CALL_DISPLAY_DAYS` (365) after `ended_at` | the only free display text a Call carries; the Call id, state, outcome, timestamps, principal / ContactReference relations, policy and evidence links stay (history keeps working with opaque ids) |
| `CALL_EVENT` | **Retain** | — (`CALLING_RETENTION_CALL_EVENTS_DAYS` must stay unset; a value refuses startup) | outbox source (RESTRICT FK) and transition evidence; events carry no display text (only `display_name_set` booleans) |
| `CALL_USAGE` | **Retain** | — | immutable usage facts, never rewritten |
| `SPEND_EVIDENCE` | **Retain** | — | spend reservations / cost evidence |
| `WEBHOOK_DELIVERY_HISTORY` | **Delete** SUCCEEDED / FAILED / CANCELLED deliveries and their attempts | `CALLING_RETENTION_WEBHOOK_HISTORY_DAYS` (90) after `completed_at` | PENDING / DELIVERING / RETRY_SCHEDULED are never touched; an original with a replay waits for the replay (RESTRICT FK) |
| `WEBHOOK_EVENT_HISTORY` | **Delete** normalized events with no delivery left | same (after `created_at`) | an event with pending work keeps its delivery, hence itself |
| `CALLING_INCIDENT` | **Delete** RESOLVED incidents | `CALLING_RETENTION_INCIDENTS_DAYS` (365) after `resolved_at` | OPEN incidents are never deleted |
| `MEDIA_PROVIDER_EVIDENCE` | **Delete** `call_media_provider_events` and `call_telephony_events` of terminal (or unattributed) Calls | `CALLING_RETENTION_MEDIA_PROVIDER_EVENTS_DAYS` (30) after receipt | restricted provider evidence; no FK needs it; a live Call's events stay (ordering / dedupe) |
| `OPERATOR_AUDIT` | **Delete** | `CALLING_RETENTION_OPERATOR_AUDIT_DAYS` (2555) after `occurred_at` | its own bound, independent of the audited resource's lifetime |
| `ADMIN_AUDIT` (`enterprise_admin_audit`), `RECORDING_ACCESS_AUDIT` | **Retain** | — | append-only accountability that must outlive the artifacts it describes |
| `RECORDING_ARTIFACT` / `VOICEMAIL_ARTIFACT` | **Delete** (Part F per-policy workers, ADR 0023 / 0024) | the recording policy's / mailbox's `retentionDays` | unchanged workers; they now honour retention holds |
| `PRINCIPAL` | **Minimize** on operator tombstone (§4) | — | §80.42 |

All bounds are configuration with documented defaults (`.env.example`, 1–3650 days),
**platform conventions, never legal claims**; invalid values refuse startup.

### 2. Worker

`retention::worker::RetentionWorker::retention_tick(limit)`, scheduled on every instance
by `AppCallService::run_retention` (`CALLING_RETENTION_INTERVAL_SECS`, default 60):

* one transaction per class batch: `SET LOCAL calling.retention_worker = 'on'`, a
  `FOR UPDATE SKIP LOCKED` claim of at most `limit` (≤ 500) due rows no active hold
  covers, the delete / minimization (each statement re-checks its predicate), one
  retention-audit row, commit — never an unbounded DELETE;
* concurrent instances claim disjoint rows; a row is acted on and counted once;
* a database error rolls the batch back (nothing partial), is logged as a summary,
  counted (`calling_retention_actions_total{outcome="FAILED"}`), audited `FAILED` when the
  database still accepts it, and retried next tick; other classes continue.

### 3. Worker flag and guards

`calling_forbid_mutation_outside_retention()` accepts a DELETE only when
`current_setting('calling.retention_worker', true) = 'on'` (transaction-local, set by
the retention module alone; application code never sets it). It guards exactly the
purgeable tables: webhook events / deliveries / attempts, `calling_incidents`,
`call_media_provider_events`, `call_telephony_events`, `platform_operator_audit`. UPDATE
stays refused on them; TRUNCATE is refused on all of them. Calls, events, usage,
participants, the enterprise / recording audits and the retention audit keep their
unconditional guards — **the flag deletes none of them** (tested).

Minimization guards: `display_minimized_at` is set once, only under the flag, and
`display_name` must be NULL from then on (CHECK) — a minimized display can never
reappear; `entry_label` may change only to the extension code, under the flag, once.

### 4. Principal tombstone (§80.42)

`platform_admin principal tombstone --principal <id>` (operator-audited
`PRINCIPAL_TOMBSTONED`, plus a `PRINCIPAL / MINIMIZE` retention-audit row), one
transaction: status DEACTIVATED with `tombstoned_at` (CHECK: a tombstoned principal is
DEACTIVATED for ever; it can never resolve or be reactivated), `external_reference`
replaced by `tombstoned:<id>` (the id itself for TELEPHONE_ENDPOINT, whose references
are UUIDs) — the single change `calling_guard_principal_reference` allows on that
immutable column — and the display names of its participations erased. Nothing is
deleted, so every foreign key stays intact and history lists the Calls by opaque id.
The `tombstoned:` prefix is reserved (registration refuses it). SERVICE_ACCOUNT
principals follow their service account and are refused; a held principal is refused.

### 5. Holds

`calling_retention_holds`: data class or `*`, optional resource id, tenant +
application + environment (all three, CHECK), or — platform-wide only, `*` and no
resource — none; neutral reason codes `POLICY_REVIEW`, `DISPUTE`,
`SECURITY_INVESTIGATION`, `OTHER`; released (never deleted; immutable otherwise).
`calling_retention_held(class, resource, tenant, application, environment)` is the one
matching rule: a scope hold matches only when all three ids are equal, so a hold in one
tenant never protects another. The worker checks the row's own id and its parents
(delivery → event / subscription; incident / provider event → Call), and the recording /
voicemail retention passes check the artifact, its Call (and mailbox). `platform_admin
retention hold|release|list|policies`; hold / release are operator-audited
(`RETENTION_HOLD_PLACED` / `RETENTION_HOLD_RELEASED`, target `RETENTION_HOLD`). Due rows a
hold keeps are recorded as `SKIPPED_HOLD` whenever their count changes (not per tick).

### 6. Audit

`calling_retention_audit` (append-only: UPDATE / DELETE / TRUNCATE refused for everyone):
data class, action (DELETE / MINIMIZE), optional opaque resource ref, batch count,
policy label (`VAR=days` or a marker), actor (`SYSTEM` or the operator id), outcome
(`DELETED`, `MINIMIZED`, `SKIPPED_HOLD`, `FAILED`), time. Never the deleted or
minimized content. The operator audit vocabulary gains `RETENTION_HOLD_PLACED`,
`RETENTION_HOLD_RELEASED`, `PRINCIPAL_TOMBSTONED` and `WEBHOOK_KEYS_RESEALED` (the
`webhooks reseal` command now writes one row with counts and batch size only).

## Consequences

* Minimization vs deletion: Calling deletes only records nothing else needs (terminal
  webhook history, resolved incidents, provider evidence of finished Calls, aged operator
  audit); it minimizes what other evidence references (Calls, principals) and retains the
  evidence chain (events, usage, spend, enterprise and recording access audits).
* Migration `m20261015_000042_calling_retention` (DOWN restores the guards and the 000038
  vocabulary `NOT VALID`; minimized values stay minimized after a rollback).
* Tests: `services/calling/tests/retention.rs`, `enterprise_recording::retention_honours_holds_and_survives_a_storage_outage`,
  `admin_cli::retention_holds_tombstones_and_reseals_are_operator_audited`,
  `webhooks::operator_reseal_writes_one_audit_row_with_counts_only`.
