# Monitoring alerts as code (SEC-016)

Status: **rules and routing exist in the repository and are validated in CI. Nothing is
deployed, and paging is NOT CONFIGURED.** Running Prometheus and Alertmanager, choosing
a receiver and approving thresholds are owner actions (see the end of this page).

## What exists

| File | Purpose |
|---|---|
| `infra/monitoring/prometheus/rules/meeting-service.rules.yml` | Alerts from [14-operational-runbook.md §84](../phase2-certification/14-operational-runbook.md#alerts-and-severity-84) |
| `infra/monitoring/prometheus/rules/messaging.rules.yml` | Alerts from [messaging-operations-runbook.md §8 / §9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |
| `infra/monitoring/prometheus/tests/*.test.yml` | `promtool test rules` unit tests. Each critical alert has a firing and a non-firing case. |
| `infra/monitoring/prometheus/prometheus.example.yml` | **Example only.** Shows how the rule files load and how the endpoints are scraped. |
| `infra/monitoring/alertmanager/alertmanager.yml` | Routes by severity to receivers that are deliberately empty |
| `scripts/monitoring/check_alert_rules.py` (+ `test_check_alert_rules.py`) | Repository policy checker, standard library only |

CI runs the `monitoring alert rules` job in `.github/workflows/service-boundaries.yml` on
every push to `main` / `staging`, on every pull request and on `workflow_dispatch`. It runs:
the policy checker and its mutation tests; `promtool check rules`; `promtool test rules`;
`promtool check config` on the example config, which also loads every rule file; and
`amtool check-config`. Images are pinned by digest: `prom/prometheus:v3.5.0`,
`prom/alertmanager:v0.28.1`.

The policy checker enforces the following:

- Every alert has a `severity` label (`critical`, `warning` or `info`), an `approval`
  label (`documented` or `required`) and a `service` label, plus `summary` and
  `description` annotations.
- Every `approval: required` alert has a `threshold_status` annotation that starts with
  `APPROVAL REQUIRED`.
- Every `runbook_url` is a repository path that exists, and its anchor matches a heading.
- Every metric name in an `expr` is exported by `services/*/src/**/*.rs`. Accepted forms:
  the literal name; a histogram base name (for `_bucket` / `_sum` / `_count` series); or a
  synthesized `cfmeet_rtc|cfmeet_collab|cfmeet_ops_<field>_total` counter whose field is
  listed in the matching `counters!` block. Prometheus's own `up` is also allowed. Renaming
  a metric in code therefore fails CI until the rule is updated.
- Every critical alert is exercised by a promtool unit test.
- The Alertmanager file contains no URL and no literal key, token or password, including
  in comments. Only `<PLACEHOLDER>` values and `*_file` paths are allowed. The repository
  secret scan also passes on it.

## Loading it

1. Run Prometheus 3.x. The example uses `http_headers`, which needs 3.x. Copy
   `prometheus.example.yml` and replace the `.invalid` hostnames with private network
   names. Keep `rule_files: [rules/*.rules.yml]` relative to the config.
2. meeting-service `GET /internal/metrics` answers only when the
   `x-internal-token: $INTERNAL_API_TOKEN` header is present. Without it the endpoint
   answers 404, which Prometheus reports as `up == 0`. A bearer token does not work.
   Prometheus sends the header with `http_headers` → `files:`. Mount that file from the
   secret store; never write the token into the config. Keep the route private.
3. The messaging metrics are on the separate `MESSAGING_METRICS_ADDRESS` listener (port
   9197 in production config). That listener has no authentication, so it must never be
   exposed publicly.
4. The job names must stay `meeting-service` and `messaging`, because the `up{job=...}`
   alerts depend on them.
5. Point `alerting.alertmanagers` at an Alertmanager that runs
   `infra/monitoring/alertmanager/alertmanager.yml`. Run
   `promtool check config` / `amtool check-config` before loading.

## Alert table

Approval: **documented** means the runbook (or code) states the threshold and the rule
uses it as stated. **required** means the runbook says APPROVAL REQUIRED or gives no
value. In that case the expression holds a starting point, not an approved SLA.

### meeting-service (`/internal/metrics`)

| Alert | Severity | Condition (expression summary) | Threshold | Approval | Runbook |
|---|---|---|---|---|---|
| MeetingServiceUnavailable | critical | `up{job="meeting-service"} == 0` | 2 min | documented (stands in for "Database unavailable") | [JOIN FAILURES](../phase2-certification/14-operational-runbook.md#join-failures) |
| MeetingJoinSuccessRateLow | critical | `meeting_join_success_total` / (success + `meeting_join_failure_total{outcome=~"provider_failure\|timeout\|client_failure"}`); denials and waiting excluded | < 90 % for 5 min | **required** (runbook starting point) | [JOIN FAILURES](../phase2-certification/14-operational-runbook.md#join-failures) |
| MeetingRtcProviderUnavailableForJoins | critical | share of `meeting_join_failure_total{outcome="provider_failure"}` among non-denied outcomes | > 50 % for 5 min | **required** (no runbook value) | [LIVEKIT OUTAGE](../phase2-certification/14-operational-runbook.md#livekit-outage) |
| MeetingConferenceTerminationFailing | critical | `rate(cfmeet_rtc_room_terminate_failures_total[5m]) > 0` (stands in for the stuck ENDING count) | sustained 10 min | **required** (count not approved) | [STUCK ENDING CONFERENCE](../phase2-certification/14-operational-runbook.md#stuck-ending-conference) |
| MeetingOutboxDeadLettersIncreasing | critical | `rate(cfmeet_rtc_outbox_dead_lettered_total[5m]) > 0` | sustained 10 min | documented | [OUTBOX BACKLOG](../phase2-certification/14-operational-runbook.md#outbox-backlog) |
| MeetingOutboxRetriesWithoutProgress | critical | `cfmeet_rtc_outbox_retried_total` rising while `cfmeet_rtc_outbox_published_total` is flat (stands in for "PENDING growing") | 10 min | documented | [OUTBOX BACKLOG](../phase2-certification/14-operational-runbook.md#outbox-backlog) |
| MeetingRecordingFailureRatioHigh | critical (only where recording is business-critical) | `increase(cfmeet_ops_recording_failed_total[1h])` / `increase(cfmeet_ops_recording_requested_total[1h])` | > 25 % for 15 min | **required** (no runbook value) | [RECORDING FAILURE](../phase2-certification/14-operational-runbook.md#recording-failure) |
| MeetingJoinLatencyP95High | warning | p95 of `meeting_join_latency_seconds_bucket` | > 3 s for 10 min | **required** (no runbook value) | [JOIN FAILURES](../phase2-certification/14-operational-runbook.md#join-failures) |
| MeetingRtcDriftEscalated | warning | `increase(cfmeet_rtc_drift_escalated_total[1h]) > 0` | any in 1 h | documented | [ORPHAN RTC ROOM](../phase2-certification/14-operational-runbook.md#orphan-rtc-room) |
| MeetingShadowMismatches | warning | `increase(cfmeet_ops_shadow_mismatches_total[1h]) > 0` | any in 1 h | **required** | [MIGRATION MISMATCH](../phase2-certification/14-operational-runbook.md#migration-mismatch) |
| MeetingRealtimeDegraded | warning | `cfmeet_rtc_ws_resync_required_total` or `cfmeet_collab_reactions_dropped_total` rising (Redis fanout) | sustained 15 min | **required** (duration documented; spike size not) | [REDIS OUTAGE](../phase2-certification/14-operational-runbook.md#redis-outage) |
| MeetingArtifactFinalizationStuck | warning | `rate(cfmeet_ops_artifact_finalize_retries_total[5m]) > 0` (stands in for "stuck > 30 min") | sustained 30 min | documented | [ARTIFACT STUCK PROCESSING](../phase2-certification/14-operational-runbook.md#artifact-stuck-processing) |
| MeetingRtcDriftRepaired | info | `increase(cfmeet_rtc_drift_repaired_total[1h]) > 0` | informational | documented | [§84](../phase2-certification/14-operational-runbook.md#alerts-and-severity-84) |
| MeetingLegacyWritesRejected | info | `increase(cfmeet_ops_legacy_writes_rejected_total[1h]) > 0` | informational | documented | [MIGRATION MISMATCH](../phase2-certification/14-operational-runbook.md#migration-mismatch) |

### messaging (`MESSAGING_METRICS_ADDRESS` `/metrics`)

| Alert | Severity | Condition | Threshold | Approval | Runbook |
|---|---|---|---|---|---|
| MessagingOutboxDeadLetters | warning | `max(messaging_outbox_events{status="DEAD_LETTER"}) > 0` | any, 5 min | documented (matches the OUTBOX_DEAD_LETTER incident) | [§8](../architecture/messaging-operations-runbook.md#8-dead-letters-136) |
| MessagingObjectDeletionDeadLetters | warning | `max(messaging_object_deletions{status="DEAD"}) > 0` | any, 5 min | documented (matches ATTACHMENT_DELETE_DEAD_LETTER) | [§8](../architecture/messaging-operations-runbook.md#8-dead-letters-136) |
| MessagingRetentionBacklog | warning | `max(messaging_purge_lag_seconds) > 3600` | 1 h (`RETENTION_BACKLOG_THRESHOLD`) | documented (in code) | [§9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |
| MessagingOutboxLagHigh | warning | `max(messaging_outbox_oldest_pending_seconds) > 600` | 10 min | **required** | [§9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |
| MessagingWebhookRelayLagHigh | warning | `max(messaging_webhook_relay_oldest_pending_seconds) > 600` | 10 min | **required** | [§9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |
| MessagingMetricsUnavailable | warning | `up{job="messaging"} == 0` | 5 min | **required** | [§9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |

### Runbook conditions that cannot be expressed today

| Condition | Why | What covers it now |
|---|---|---|
| Database unavailable (`/health` failing or 503 sustained) | `/health` returns a static 200 and does not check PostgreSQL. No metric counts 503 responses or pool acquire timeouts. | `MeetingServiceUnavailable` covers a process that is down or unreachable only. A database outage alone needs a new metric (for example a pool / DB-ping gauge) or a black-box probe of a DB-backed route. |
| Stuck ENDING count (ENDING older than 10 min) | SQL-only (`conference_sessions`) | `MeetingConferenceTerminationFailing` stands in for it; the runbook SQL gives the count |
| Outbox PENDING older than 5 min | The backlog is only in the JSON `GET /internal/rtc/metrics` (`outboxBacklog`), not in the Prometheus exposition | `MeetingOutboxRetriesWithoutProgress`. A fully stopped relay (no retries and no publishes) is not detectable until a backlog gauge is exported. |
| Artefacts stuck STOPPING / PROCESSING > 30 min | Artefact status age is SQL-only | `MeetingArtifactFinalizationStuck` stands in for it |
| Worker lease takeovers / worker failures (info) | Log- and SQL-based only (`worker_leases`); no metric | Not alerted |
| Active / unhealthy conferences | JSON-only (`/internal/operations/conferences/health`) | Not alerted; `conference_health_evaluations_total{state}` counts evaluations, not current conferences |
| Messaging KT rollback, scanner unavailable, abuse wave, storage drift | Raised as messaging incidents; no dedicated metric | `messaging-admin incidents list` |

Calling, numbering, telephony, privacy-relay, sms-gateway, platform-access and
security-scan-gateway export metrics too, but their runbooks define no alert conditions.
No rules exist for them yet. Their owners should add conditions to a runbook first.

## Paging status: NOT CONFIGURED

`alertmanager.yml` routes `critical` → `unconfigured-pager`, `warning` →
`unconfigured-ticket` and `info` → `unconfigured-info`. None of these receivers has an
integration, so Alertmanager accepts and drops every alert. A critical alert inhibits the
warning / info notifications of the same `alertname` + `service`.

To enable paging, the owner must:

1. Choose the product, for example a paging service, a ticket webhook or email.
2. Store its credential in the secret store and mount it as a file into the Alertmanager
   container.
3. Uncomment the matching template under the receiver and set the `*_file` field to the
   mounted path. Never put a URL, key or token in the YAML: the policy checker rejects
   it. If the integration has no `*_file` option, keep that receiver in the deployment's
   secret configuration, outside this repository.
4. Prove delivery with a synthetic alert sent to a non-production receiver
   (`amtool alert add` against a staging Alertmanager). Do not test against real on-call
   staff or customer channels.

## Owner steps to approve thresholds

For every `approval: required` alert (see the tables above):

1. Decide the value, or keep the starting point. For the critical ones, also decide
   whether they page. For recording, decide whether it is business-critical.
2. Edit the expression and `for:` in the rule file. Change the label to
   `approval: documented` and remove the `threshold_status` annotation.
3. Record the approved value in the runbook (§84 for meeting-service).
4. Update the expected annotations in `infra/monitoring/prometheus/tests/*.test.yml`.
   promtool compares them exactly. Adjust the firing / non-firing series to the new value.
5. Push. The `monitoring alert rules` CI job must pass before the change counts.

Deploying Prometheus / Alertmanager is outside the repository's scope. It is a separate
infrastructure decision; this repository has no running monitoring stack.
