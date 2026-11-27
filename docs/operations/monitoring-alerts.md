# Monitoring alerts as code (SEC-016)

Status: **rules and routing are validated in CI and DEPLOYED to production** by the
coordinated release (`release` and `deploy-monitoring` operations,
`infra/calling/production/monitoring_stack.py`, see
[Production monitoring stack](#production-monitoring-stack)). Alert delivery is driven by
the production environment secret `CFMEET_ALERT_WEBHOOK_URL` (and optionally
`CFMEET_ALERT_SLACK_WEBHOOK_URL`); while neither is set, alerts fire in Alertmanager but
are not delivered anywhere. Approving the `approval: required` thresholds stays an owner
action (see the end of this page). Public uptime is also checked every hour by
[scheduled synthetic checks](#scheduled-synthetic-checks).

## What exists

| File | Purpose |
|---|---|
| `infra/monitoring/prometheus/rules/meeting-service.rules.yml` | Alerts from [14-operational-runbook.md §84](../phase2-certification/14-operational-runbook.md#alerts-and-severity-84) |
| `infra/monitoring/prometheus/rules/messaging.rules.yml` | Alerts from [messaging-operations-runbook.md §8 / §9](../architecture/messaging-operations-runbook.md#9-incidents-and-signals-141) |
| `infra/monitoring/prometheus/rules/notifications.rules.yml` | Notification Service alerts ([runbook](notification-service-runbook.md)) |
| `infra/monitoring/prometheus/rules/platform-health.rules.yml` | Readiness-probe alerts for the services without metrics, and the Developer Platform worker heartbeat |
| `infra/monitoring/prometheus/rules/service-scrapes.rules.yml` | Scrape-availability alerts for the other services with a metrics listener |
| `infra/monitoring/prometheus/tests/*.test.yml` | `promtool test rules` unit tests. Each critical alert has a firing and a non-firing case. |
| `infra/monitoring/prometheus/prometheus.example.yml` | Hand-written example of the scrape configuration (the production one is rendered by `monitoring_stack.py`) |
| `infra/monitoring/alertmanager/alertmanager.yml` | Routes by severity; every integration reads its URL from a mounted secret file |
| `infra/monitoring/blackbox/blackbox.yml` | Blackbox exporter module for the readiness probes |
| `infra/monitoring/sql-exporter/*.collector.yml` | Read-only SQL collectors (Developer Platform worker heartbeat) |
| `infra/calling/production/monitoring_stack.py` (+ `test_monitoring_stack.py`) | Renders and deploys the production stack |
| `scripts/monitoring/check_alert_rules.py` (+ `test_check_alert_rules.py`) | Repository policy checker, standard library only |

CI runs the `monitoring alert rules` job in `.github/workflows/service-boundaries.yml` on
every push to `main` / `staging`, on every pull request and on `workflow_dispatch`. It runs:
the policy checker and its mutation tests; `promtool check rules`; `promtool test rules`;
`promtool check config` on the example config, which also loads every rule file;
`amtool check-config`; and the configuration `monitoring_stack.py` deploys, rendered with
every optional part (and the Alertmanager variant without receivers), through
`promtool check config`, `amtool check-config`, `blackbox_exporter --config.check` and
`sql_exporter -config.check`. Images are pinned by digest and are the ones production runs:
`prom/prometheus:v3.5.0`, `prom/alertmanager:v0.28.1`, `prom/blackbox-exporter:v0.27.0`,
`burningalchemist/sql_exporter:0.24.14`.

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
  listed in the matching `counters!` block. Prometheus's own `up`, the blackbox exporter's
  `probe_success` and the `metric_name` of a repository SQL collector are also allowed.
  Renaming a metric in code therefore fails CI until the rule is updated.
- Every critical alert is exercised by a promtool unit test.
- The Alertmanager file contains no URL and no literal key, token or password, including
  in comments. Only `<PLACEHOLDER>` values and `*_file` paths are allowed. The repository
  secret scan also passes on it.

## Production monitoring stack

`monitoring_stack.py` runs on the VPS after every successful `release` (a monitoring
failure is reported as `monitoringStack: NOT_UPDATED` and never fails the release) and as
the separate `deploy-monitoring` operation of `cfmeet-coordinated-release.yml`. The
workflow ships `infra/monitoring` next to the release module. All containers are on the
private network `cfmeet-core-private`, publish no port, run as uid 65534 with a read-only
root filesystem, no capabilities and a memory limit, and are recreated only when their
rendered configuration or image changes. A resource gate (1 GiB `MemAvailable` margin)
refuses to create them on a host without room.

| Container | What it does |
|---|---|
| `cfmeet-prometheus` | Scrapes only the listeners that exist in the running containers (see [Scrape targets](#scrape-targets)), loads every `rules/*.rules.yml`, sends to `cfmeet-alertmanager`. TSDB in the volume `cfmeet-prometheus-data`, 15 days / 2 GB. |
| `cfmeet-alertmanager` | `alertmanager.yml` with only the integrations whose secret is set (see [Alert delivery](#alert-delivery)). Joins the egress bridge `cfmeet-monitoring-egress` only when a receiver is configured. |
| `cfmeet-blackbox` | [Health probes](#health-probes). |
| `cfmeet-heartbeat-exporter` | [Developer Platform worker heartbeat](#developer-platform-worker-heartbeat). |

The run log records `monitoringTargets` (jobs, probed services, receivers) and
`monitoringStack` (container changes, readiness, and per-job target health read from
Prometheus' own target list). The Meeting Service token for `GET /internal/metrics` is
copied from the running container's `INTERNAL_API_TOKEN` into a file only Prometheus
mounts; without that variable the `meeting-service` job is not configured. The job
names stay `meeting-service`, `messaging`, `notifications`, `health-probes`,
`developer-platform-heartbeat` and the service names below, because the `up{job=...}`
alerts depend on them (a unit test checks that every job a rule names can be produced).

### Scrape targets

| Job | Target | Configured when |
|---|---|---|
| `meeting-service` | `cfmeet-production-backend:8080/internal/metrics` with `x-internal-token` from a file | the container sets `INTERNAL_API_TOKEN` |
| `messaging` | `cfmeet-messaging` `MESSAGING_METRICS_ADDRESS` (9197) | listener set, not loopback |
| `notifications` | `cfmeet-notifications:8102/metrics`, `cfmeet-notifications-2:8102/metrics` | the replicas run |
| `platform-access`, `calling`, `numbering`, `privacy-relay`, `sms-gateway`, `telephony` | the container's `*_METRICS_ADDRESS` listener | listener set, not loopback |
| `security-scan-gateway` | `cosmicforge-scan-gateway` `SCAN_GATEWAY_METRICS_ADDRESS` (9199) | listener set, not loopback |

API Gateway, Developer Platform and Organizations have no metrics route, and the
Communications metrics route requires a signed service token, so they are covered by
[health probes](#health-probes) instead; no endpoint is invented for them.
`*MetricsUnavailable` (warning, 5 min) fires when a configured listener stops answering.
Metric thresholds for these services belong in their runbooks first.

### Health probes

Job `health-probes`: the blackbox exporter GETs each running service's own health or
readiness route (`coordinated_release.SERVICES[...]['health']` on its container port,
credential-free, 200 expected). `ApiGatewayUnavailable`, `DeveloperPlatformUnavailable`,
`OrganizationsUnavailable` and `CommunicationsUnavailable` page (critical) after 3 minutes
of failure; `ServiceHealthProbeFailing` warns for any other service after 5 minutes;
`HealthProbesUnavailable` warns when the exporter itself does not answer.

Response: check the container (`docker ps`, `docker logs --tail 50 <container>`), its
`/ready` body (database, migrations), then the dependencies it names. A container that is
down after a release is restored with the `rollback` operation.

### Developer Platform worker heartbeat

The worker records one row per instance in `devplat_worker_heartbeats` on every pass
(default every 5 s). `cfmeet-heartbeat-exporter` reads it with the role `cfmeet_monitor`,
which can only connect to `cfmeet_developer_platform` and `SELECT` that one table
(`default_transaction_read_only = on`, 5 s statement timeout, 2 connections). Metrics:
`cfmeet_devplat_worker_heartbeat_age_seconds` (1e9 when no production row exists) and
`cfmeet_devplat_worker_instances_fresh`.

| Alert | Severity | Condition | Approval |
|---|---|---|---|
| DeveloperPlatformWorkerHeartbeatStale | critical | `max(cfmeet_devplat_worker_heartbeat_age_seconds) > 300` for 2 min (60 missed passes) | documented (this page) |
| DeveloperPlatformWorkerHeartbeatUnreadable | warning | the heartbeat metric is absent for 10 min | documented |

Response: `docker ps -a --filter name=cfmeet-developer-platform-worker`, its logs, and
its restart count. While it is down, invitation expiry, credential sweeps, reference
reconciliation and webhook delivery stop; the API keeps serving.

## Scheduled synthetic checks

`.github/workflows/cfmeet-production-synthetic-checks.yml` runs every hour (and on
dispatch) from a GitHub runner: the read-only public part of `certify`
(`infra/calling/production/public_certification.py`): `/health`, Swagger and OpenAPI
documents, CORS preflights from the web origins, TLS certificate lifetime, every public
GET route of the gateway manifest without credentials and every internal / operator /
callback GET route on the public host. Unauthenticated GET / OPTIONS / HEAD only; no
secret, no SSH, no account, no email, no scope grant, and it does not take the production
deployment lock. A failed check fails the run; the report is in the job summary and the
artifact. The Developer Platform worker heartbeat has no public endpoint and is covered by
the alert above, not by this workflow.

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

### organizations (no Prometheus exposition; checked by `verify` and `certify`)

The Organizations outbox holds two kinds of rows (`services/organizations/src/store.rs` `emit`):

* one `cfmeet.reconciliation` **record** per event: event history, never dispatched by design, so it stays `PENDING` with 0 attempts forever. It is **not** a backlog and never a delivery failure;
* one **consumer** row per subscribed destination (today `cfmeet.meeting-service`, 13 event types), delivered by the worker.

`coordinated_release.event_delivery()` / `classify_event_delivery()` (used by `verify`, `certify` and `activate-org-events`) report counts only, per destination, event type and state, and fail on genuine delivery defects only:

| Check | Fails when |
|---|---|
| consumer DEAD | any consumer row is `DEAD` |
| consumer stalled | a consumer row of a **configured** destination (`ORGANIZATIONS_EVENT_DESTINATIONS`) is `PENDING`/`INFLIGHT` more than 15 minutes after it was written |
| blocked behind DEAD | a consumer row waits behind a `DEAD` row of the same aggregate (ordered delivery) |
| record integrity | a consumer row has no reconciliation twin, or a reconciliation record was dispatched or modified |

Rows of a destination that is not configured are reported (`undeliverableUnconfigured`), not failed. The service's own `/ready` reports `outbox.pending` for consumer rows only, `outbox.delivery` (`enabled` / `disabled`) and the configured `outbox.destinations`; the admin API `GET /internal/v1/admin/event-delivery` reports per consumer destination and excludes the record.

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
security-scan-gateway export metrics too, but their runbooks define no metric thresholds.
Only their scrape availability is alerted ([Scrape targets](#scrape-targets)); their
owners should add conditions to a runbook first.

## Alert delivery

`alertmanager.yml` routes `critical` to `pager` (repeat 1 h), `warning` and anything
without a severity to `operations`, and `info` to `info-only` (no notification). A
critical alert inhibits the warning / info notifications of the same `alertname` +
`service`.

`pager` and `operations` each have two integrations, both reading their URL from a file:

| Production environment secret | Integration | File on the VPS |
|---|---|---|
| `CFMEET_ALERT_WEBHOOK_URL` | `webhook_configs` (Alertmanager webhook JSON) | `/opt/cfmeet-core/secrets/alertmanager-webhook-url` |
| `CFMEET_ALERT_SLACK_WEBHOOK_URL` | `slack_configs` (Slack-compatible incoming webhook) | `/opt/cfmeet-core/secrets/alertmanager-slack-webhook-url` |

On `release` and `deploy-monitoring` the workflow writes each set secret (https only)
to its root-only file and removes the file of an unset one; the step never fails the
operation and never prints a value. `monitoring_stack.py` removes the integration of a
missing file from the deployed copy, so Alertmanager always starts. No email integration
is configured. To start delivery, set a secret in the `production` environment and run
`deploy-monitoring`; to prove delivery use a non-production receiver first.

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

The production stack is deployed by the coordinated release
([Production monitoring stack](#production-monitoring-stack)).
