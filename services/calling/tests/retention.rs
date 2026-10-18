//! Part H §80 (ADR 0030): Calling retention, deletion and minimization.
//!
//! * terminal Calls lose their display metadata after `CALL_RECORD`'s bound — never
//!   before, never a live Call — while the Call, its events, usage, spend, outbox and
//!   relations stay, history still lists it, and the display can never reappear;
//! * webhook history: terminal deliveries (+ attempts) and delivery-less events go after
//!   their bound; PENDING / RETRY_SCHEDULED deliveries and their events stay;
//! * RESOLVED incidents, provider events of terminal Calls and operator audit rows go on
//!   their OWN bounds; OPEN incidents and live Calls' events stay;
//! * holds: a held resource / scope is skipped (and recorded as SKIPPED_HOLD); a hold in
//!   another scope protects nothing; placing / releasing is operator-audited;
//! * two concurrent workers act on each row exactly once (audit counts);
//! * a failing batch leaves nothing partial, is audited FAILED and is retried;
//! * the worker flag opens only the purgeable tables: plain DELETE / UPDATE / TRUNCATE on
//!   evidence stays refused, and Calls / events / usage stay undeletable even with it;
//! * principal tombstone: DEACTIVATED for ever, reference replaced, display erased, every
//!   foreign key intact, history still lists the Calls by opaque id.

mod common;

use common::NewPrincipal;

use std::sync::Arc;

use axum::http::StatusCode;
use calling::{
    platform::{operator::OperatorContext, principals::PrincipalReference, PlatformError},
    retention::{
        holds::{self, HoldOwner, HoldReason, HoldScope},
        tombstone,
        worker::{RetentionReport, RetentionWorker},
        DataClass, RetentionSettings,
    },
};
use chrono::{DateTime, Duration, Utc};
use common::{app::AppWorld, exec, scalar_i64};
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

fn worker(a: &AppWorld) -> RetentionWorker {
    RetentionWorker::new(
        a.h().conn().clone(),
        Arc::new(a.h().clock.clone()),
        RetentionSettings::default(),
    )
}

async fn tick(a: &AppWorld) -> RetentionReport {
    worker(a).retention_tick(500).await.unwrap()
}

/// A worker whose CALL_RECORD bound is one day (`CALLING_RETENTION_CALL_DISPLAY_DAYS=1`),
/// so a Call can age past it while the world's API credential is still valid.
async fn tick_one_day_display(a: &AppWorld) -> RetentionReport {
    let settings = RetentionSettings::from_lookup(&|k: &str| {
        (k == "CALLING_RETENTION_CALL_DISPLAY_DAYS").then(|| "1".to_string())
    })
    .unwrap();
    RetentionWorker::new(
        a.h().conn().clone(),
        Arc::new(a.h().clock.clone()),
        settings,
    )
    .retention_tick(500)
    .await
    .unwrap()
}

fn op() -> OperatorContext {
    OperatorContext::new("ops.retention").unwrap()
}

fn conn(a: &AppWorld) -> &DatabaseConnection {
    a.h().conn()
}

fn now(a: &AppWorld) -> DateTime<Utc> {
    use calling::domain::Clock;
    a.h().clock.now()
}

/// `now - days` as a SQL timestamptz literal.
fn ago(a: &AppWorld, days: i64) -> String {
    format!(
        "'{}'::timestamptz",
        (now(a) - Duration::days(days)).to_rfc3339()
    )
}

async fn count(a: &AppWorld, sql: &str) -> i64 {
    scalar_i64(conn(a), sql).await
}

/// Sum of audited rows for (class, outcome).
async fn audited(a: &AppWorld, class: &str, outcome: &str) -> i64 {
    count(
        a,
        &format!(
            "SELECT COALESCE(sum(batch_count), 0)::bigint FROM calling_retention_audit \
             WHERE data_class = '{class}' AND outcome = '{outcome}'"
        ),
    )
    .await
}

fn owner(a: &AppWorld) -> HoldOwner {
    let s = a.w.scope();
    HoldOwner {
        tenant_id: s.tenant_id().as_uuid(),
        application_id: s.application_id().as_uuid(),
        environment_id: s.environment_id().as_uuid(),
    }
}

/// Another tenant's scope in the same deployment.
async fn other_owner(a: &AppWorld) -> HoldOwner {
    let c = a
        .h()
        .consumer(
            "othertenant",
            calling::domain::TenantClassification::External,
        )
        .await;
    let e = c.env(calling::domain::EnvironmentKind::Production);
    HoldOwner {
        tenant_id: e.tenant_id.as_uuid(),
        application_id: e.application_id.as_uuid(),
        environment_id: e.id.as_uuid(),
    }
}

fn scope_values(o: &HoldOwner) -> String {
    format!(
        "'{}', '{}', '{}'",
        o.tenant_id, o.application_id, o.environment_id
    )
}

/// A VOICE call from the world's initiator (with a display name) to its recipient;
/// connected and ended when `end` is set, left ringing otherwise.
async fn named_call(a: &AppWorld, display: &str, end: bool) -> Uuid {
    let caller = a.device(&a.w.initiator, "caller-main").await;
    let callee = a.device(&a.w.recipient, "callee-main").await;
    let r = a
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": a.w.initiator.id(),
                "initiatorEndpointId": caller.endpoint,
                "targetPrincipalIds": [a.w.recipient.id()],
                "initiatorDisplayName": display,
            }),
            &[("idempotency-key", &Uuid::new_v4().to_string())],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    if end {
        let invitation = a.invitation_for(call, callee.endpoint).await.unwrap();
        let (_, v) = a.state(call).await;
        assert_eq!(
            a.accept(call, invitation, &callee, v).await.status,
            StatusCode::OK
        );
        let s1 = a.session_on(call, caller.endpoint).await.unwrap();
        let s2 = a.session_on(call, callee.endpoint).await.unwrap();
        a.connect(call, s1).await;
        a.connect(call, s2).await;
        a.h().clock.advance(Duration::seconds(20));
        let (_, v) = a.state(call).await;
        assert_eq!(a.command(call, "end", v).await.status, StatusCode::OK);
        assert_eq!(a.call_view(call).await["state"], "ENDED");
    }
    call
}

async fn display_names(a: &AppWorld, call: Uuid) -> Vec<Option<String>> {
    conn(a)
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT display_name FROM call_participants WHERE call_id = '{call}' ORDER BY role"
            ),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
        .collect()
}

/// Row counts of every evidence table retention must never shrink.
async fn evidence(a: &AppWorld) -> Vec<(&'static str, i64)> {
    let mut out = Vec::new();
    for table in [
        "calls",
        "call_events",
        "calling_outbox_events",
        "call_usage_records",
        "call_spend_reservations",
        "call_participants",
        "call_sessions",
        "call_invitations",
        "call_authorizations",
        "call_session_intervals",
        "call_quality_records",
        "enterprise_admin_audit",
        "call_recording_access_audit",
        "platform_principals",
    ] {
        out.push((
            table,
            count(a, &format!("SELECT count(*) FROM {table}")).await,
        ));
    }
    out
}

/// Usage facts of the Call (state, version, every usage row's immutable facts).
async fn call_facts(a: &AppWorld, call: Uuid) -> String {
    let rows = conn(a)
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT c.current_state || ':' || c.version || ':' || c.ended_at::text || ':' || \
                   (SELECT count(*) FROM call_events e WHERE e.call_id = c.id) || ':' || \
                   COALESCE((SELECT string_agg(u.usage_type || '=' || u.quantity::text, ',' ORDER BY u.id) \
                     FROM call_usage_records u WHERE u.call_id = c.id), '') \
                 FROM calls c WHERE c.id = '{call}'"
            ),
        ))
        .await
        .unwrap();
    rows[0].try_get_by_index::<String>(0).unwrap()
}

async fn app_history(a: &AppWorld, principal: Uuid) -> Value {
    let r = a.get(&format!("/v1/calls?principalId={principal}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check_openapi_component("CallHistoryList", &r.json);
    r.json
}

fn history_entry(list: &Value, call: Uuid) -> Value {
    list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .cloned()
        .unwrap_or_else(|| panic!("{call} missing from history: {list}"))
}

/// The coherence of a minimized terminal Call (H10): display gone; Call, events, usage,
/// spend, outbox and relations intact; history still answers.
#[tokio::test]
async fn terminal_calls_are_minimized_after_the_bound_and_their_evidence_is_kept() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let ended = named_call(&a, "Ada Lovelace", true).await;
    let live = named_call(&a, "Grace Hopper", false).await;
    let facts = call_facts(&a, ended).await;
    let before = evidence(&a).await;
    assert!(
        before
            .iter()
            .any(|(t, n)| *t == "call_usage_records" && *n > 0),
        "the ended call has usage facts: {before:?}"
    );
    assert!(display_names(&a, ended)
        .await
        .contains(&Some("Ada Lovelace".to_string())));

    // Not before the bound (here one day after the Call ended).
    a.h().clock.advance(Duration::hours(23));
    let r = tick_one_day_display(&a).await;
    assert_eq!(r.calls_minimized, 0, "{r:?}");
    assert!(display_names(&a, ended)
        .await
        .contains(&Some("Ada Lovelace".to_string())));

    a.h().clock.advance(Duration::hours(2));
    let r = tick_one_day_display(&a).await;
    assert_eq!(r.calls_minimized, 1, "{r:?}");
    assert!(r.failed.is_empty(), "{r:?}");
    assert!(
        display_names(&a, ended).await.iter().all(Option::is_none),
        "display metadata erased"
    );
    // A live (non-terminal) Call is never minimized, whatever its age.
    assert!(display_names(&a, live)
        .await
        .contains(&Some("Grace Hopper".to_string())));
    assert_eq!(a.call_view(live).await["state"], "RINGING");
    // Evidence: nothing deleted, the Call's state / version / events / usage unchanged.
    assert_eq!(evidence(&a).await, before);
    assert_eq!(call_facts(&a, ended).await, facts);
    assert_eq!(audited(&a, "CALL_RECORD", "MINIMIZED").await, 1);
    // The Call is still readable and history still lists it (opaque ids only).
    assert_eq!(a.call_view(ended).await["state"], "ENDED");
    let callee = a.w.recipient.id().as_uuid();
    let entry = history_entry(&app_history(&a, callee).await, ended);
    assert_eq!(entry["state"], "ENDED");
    assert_eq!(
        entry["counterparts"][0]["principalId"],
        a.w.initiator.id().to_string()
    );
    assert!(entry["counterparts"][0]["displayName"].is_null(), "{entry}");

    // Idempotent: a second tick finds nothing.
    let r = tick_one_day_display(&a).await;
    assert_eq!(r.calls_minimized, 0);
    assert_eq!(audited(&a, "CALL_RECORD", "MINIMIZED").await, 1);

    // No resurrection: neither a later request on the Call nor a direct write brings
    // the display back, and the marker cannot be cleared.
    let (_, v) = a.state(ended).await;
    let late = a.command(ended, "end", v).await;
    assert_ne!(
        late.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        late.text
    );
    assert!(exec(
        conn(&a),
        &format!("UPDATE call_participants SET display_name = 'Ada' WHERE call_id = '{ended}'"),
    )
    .await
    .is_err());
    assert!(exec(
        conn(&a),
        &format!(
            "UPDATE call_participants SET display_minimized_at = NULL WHERE call_id = '{ended}'"
        ),
    )
    .await
    .is_err());
    // Only the retention worker sets the marker.
    assert!(exec(
        conn(&a),
        &format!(
            "UPDATE call_participants SET display_name = NULL, display_minimized_at = now() \
             WHERE call_id = '{live}'"
        ),
    )
    .await
    .is_err());
    assert!(display_names(&a, ended).await.iter().all(Option::is_none));
    assert_eq!(evidence(&a).await, before);
}

/// Webhook fixtures: one ACTIVE subscription in the world's scope.
async fn subscription(a: &AppWorld) -> Uuid {
    let id = Uuid::new_v4();
    exec(
        conn(a),
        &format!(
            "INSERT INTO webhook_subscriptions (id, tenant_id, application_id, environment_id, endpoint_url, \
               status, event_types, secret_sealed, secret_version, version, created_at, updated_at) \
             VALUES ('{id}', {scope}, 'https://receiver.example/hook', 'ACTIVE', '{{call.ended}}', \
               'v1.c2VhbGVk', 1, 1, {t}, {t})",
            scope = scope_values(&owner(a)),
            t = ago(a, 0),
        ),
    )
    .await
    .unwrap();
    id
}

async fn webhook_event(a: &AppWorld, age_days: i64) -> Uuid {
    let id = Uuid::new_v4();
    let event_id = format!("whe_{}", Uuid::new_v4().simple());
    exec(
        conn(a),
        &format!(
            "INSERT INTO webhook_events (id, event_id, source_event_id, producer, event_type, tenant_id, \
               application_id, environment_id, occurred_at, body, created_at) \
             VALUES ('{id}', '{event_id}', '{source}', 'cfmeet.calling', 'call.ended', {scope}, {t}, \
               '{{\"id\": \"{event_id}\", \"type\": \"call.ended\"}}', {t})",
            source = Uuid::new_v4(),
            scope = scope_values(&owner(a)),
            t = ago(a, age_days),
        ),
    )
    .await
    .unwrap();
    id
}

async fn delivery(
    a: &AppWorld,
    subscription: Uuid,
    event: Uuid,
    status: &str,
    age_days: i64,
    replay_of: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    let terminal = matches!(status, "SUCCEEDED" | "FAILED" | "CANCELLED");
    let t = ago(a, age_days);
    exec(
        conn(a),
        &format!(
            "INSERT INTO webhook_deliveries (id, webhook_event_id, subscription_id, tenant_id, application_id, \
               environment_id, replay_of, status, attempt_count, next_attempt_at, created_at, updated_at, completed_at) \
             VALUES ('{id}', '{event}', '{subscription}', {scope}, {replay}, '{status}', 1, {t}, {t}, {t}, {done})",
            scope = scope_values(&owner(a)),
            replay = replay_of
                .map(|r| format!("'{r}'"))
                .unwrap_or_else(|| "NULL".into()),
            done = if terminal { t.clone() } else { "NULL".into() },
        ),
    )
    .await
    .unwrap();
    exec(
        conn(a),
        &format!(
            "INSERT INTO webhook_delivery_attempts (delivery_id, attempt_number, outcome, http_status, \
               started_at, finished_at) VALUES ('{id}', 1, '{outcome}', 500, {t}, {t})",
            outcome = if status == "SUCCEEDED" {
                "SUCCEEDED"
            } else {
                "RETRY"
            },
        ),
    )
    .await
    .unwrap();
    id
}

async fn exists(a: &AppWorld, table: &str, column: &str, id: Uuid) -> bool {
    count(
        a,
        &format!("SELECT count(*) FROM {table} WHERE {column} = '{id}'"),
    )
    .await
        > 0
}

#[tokio::test]
async fn delivered_webhook_history_is_purged_and_pending_work_is_never_touched() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let sub = subscription(&a).await;
    // E1: delivered; E2: one FAILED + one PENDING delivery; E3: RETRY_SCHEDULED;
    // E4: CANCELLED; E5: no delivery; E6: a delivery and its replay; E7: recent.
    let e1 = webhook_event(&a, 100).await;
    let d1 = delivery(&a, sub, e1, "SUCCEEDED", 100, None).await;
    let e2 = webhook_event(&a, 100).await;
    let d2_failed = delivery(&a, sub, e2, "FAILED", 100, None).await;
    let sub2 = subscription(&a).await;
    let d2_pending = delivery(&a, sub2, e2, "PENDING", 100, None).await;
    let e3 = webhook_event(&a, 100).await;
    let d3 = delivery(&a, sub, e3, "RETRY_SCHEDULED", 100, None).await;
    let e4 = webhook_event(&a, 100).await;
    let d4 = delivery(&a, sub, e4, "CANCELLED", 100, None).await;
    let e5 = webhook_event(&a, 100).await;
    let e6 = webhook_event(&a, 100).await;
    let d6 = delivery(&a, sub, e6, "SUCCEEDED", 100, None).await;
    let r6 = delivery(&a, sub, e6, "SUCCEEDED", 95, Some(d6)).await;
    let e7 = webhook_event(&a, 10).await;
    let d7 = delivery(&a, sub, e7, "SUCCEEDED", 10, None).await;

    // Plain deletes are refused on the history tables.
    for sql in [
        format!("DELETE FROM webhook_delivery_attempts WHERE delivery_id = '{d1}'"),
        format!("DELETE FROM webhook_deliveries WHERE id = '{d7}'"),
        format!("DELETE FROM webhook_events WHERE id = '{e5}'"),
        "TRUNCATE webhook_events CASCADE".to_string(),
        "TRUNCATE webhook_delivery_attempts".to_string(),
    ] {
        assert!(exec(conn(&a), &sql).await.is_err(), "{sql}");
    }

    let r = tick(&a).await;
    assert!(r.failed.is_empty(), "{r:?}");
    // Terminal deliveries past the bound (the replay first; its original next tick).
    assert_eq!(r.webhook_deliveries_deleted, 4, "{r:?}");
    for d in [d1, d2_failed, d4, r6] {
        assert!(!exists(&a, "webhook_deliveries", "id", d).await);
        assert!(!exists(&a, "webhook_delivery_attempts", "delivery_id", d).await);
    }
    for d in [d2_pending, d3, d6, d7] {
        assert!(exists(&a, "webhook_deliveries", "id", d).await, "{d}");
        assert!(exists(&a, "webhook_delivery_attempts", "delivery_id", d).await);
    }
    // Events without any delivery left; never one with pending work.
    assert_eq!(r.webhook_events_deleted, 3, "{r:?}");
    for e in [e1, e4, e5] {
        assert!(!exists(&a, "webhook_events", "id", e).await);
    }
    for e in [e2, e3, e6, e7] {
        assert!(exists(&a, "webhook_events", "id", e).await, "{e}");
    }
    let r = tick(&a).await;
    assert_eq!(
        (r.webhook_deliveries_deleted, r.webhook_events_deleted),
        (1, 1)
    );
    assert!(!exists(&a, "webhook_events", "id", e6).await);
    assert_eq!(audited(&a, "WEBHOOK_DELIVERY_HISTORY", "DELETED").await, 5);
    assert_eq!(audited(&a, "WEBHOOK_EVENT_HISTORY", "DELETED").await, 4);
    // Pending / retrying deliveries are kept however old they get.
    a.h().clock.advance(Duration::days(400));
    let r = tick(&a).await;
    assert_eq!(
        r.webhook_deliveries_deleted, 1,
        "only the recent delivered one: {r:?}"
    );
    for d in [d2_pending, d3] {
        assert!(exists(&a, "webhook_deliveries", "id", d).await);
    }
    for e in [e2, e3] {
        assert!(exists(&a, "webhook_events", "id", e).await);
    }
}

async fn incident(
    a: &AppWorld,
    o: &HoldOwner,
    status: &str,
    age_days: i64,
    call: Option<Uuid>,
) -> Uuid {
    let id = Uuid::new_v4();
    let t = ago(a, age_days);
    exec(
        conn(a),
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, category, \
               severity, status, call_id, occurrences, restricted_evidence, first_seen_at, last_seen_at, resolved_at) \
             VALUES ('{id}', {scope}, 'retention-test:{key}', 'REPEATED_ROUTE_DENIALS', 'LOW', '{status}', {call}, 1, \
               '{{}}', {t}, {t}, {resolved})",
            scope = scope_values(o),
            key = id.simple(),
            call = call.map(|c| format!("'{c}'")).unwrap_or_else(|| "NULL".into()),
            resolved = if status == "RESOLVED" { t.clone() } else { "NULL".into() },
        ),
    )
    .await
    .unwrap();
    id
}

async fn media_event(a: &AppWorld, call: Option<Uuid>, age_days: i64) -> Uuid {
    let id = Uuid::new_v4();
    let t = ago(a, age_days);
    let scope = match call {
        Some(c) => format!("'{c}', {}", scope_values(&owner(a))),
        None => "NULL, NULL, NULL, NULL".into(),
    };
    exec(
        conn(a),
        &format!(
            "INSERT INTO call_media_provider_events (id, provider_adapter, provider_event_id, event_kind, call_id, \
               tenant_id, application_id, environment_id, identity_status, outcome, occurred_at, received_at, processed_at) \
             VALUES ('{id}', 'fake', 'evt-{key}', 'OTHER', {scope}, 'NONE', 'NO_OP', {t}, {t}, {t})",
            key = id.simple(),
        ),
    )
    .await
    .unwrap();
    id
}

async fn operator_row(a: &AppWorld, age_days: i64) -> Uuid {
    let id = Uuid::new_v4();
    let o = owner(a);
    exec(
        conn(a),
        &format!(
            "INSERT INTO platform_operator_audit (id, actor, correlation_id, action, target_kind, target_id, \
               tenant_id, detail, occurred_at) VALUES ('{id}', 'ops.past', 'op_{key}', 'TENANT_STATUS_SET', \
               'TENANT', '{t}', '{t}', '{{}}', {at})",
            key = id.simple(),
            t = o.tenant_id,
            at = ago(a, age_days),
        ),
    )
    .await
    .unwrap();
    id
}

#[tokio::test]
async fn incidents_provider_events_and_operator_audit_follow_their_own_bounds() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let o = owner(&a);
    let ended = named_call(&a, "Ada", true).await;
    let live = named_call(&a, "Grace", false).await;
    let open_old = incident(&a, &o, "OPEN", 2000, None).await;
    let resolved_old = incident(&a, &o, "RESOLVED", 366, Some(ended)).await;
    let resolved_recent = incident(&a, &o, "RESOLVED", 300, None).await;
    let unknown_old = media_event(&a, None, 31).await;
    let ended_old = media_event(&a, Some(ended), 31).await;
    let live_old = media_event(&a, Some(live), 31).await;
    let ended_recent = media_event(&a, Some(ended), 29).await;
    let telephony_old = Uuid::new_v4();
    exec(
        conn(&a),
        &format!(
            "INSERT INTO call_telephony_events (event_id, call_id, event_type, disposition, occurred_at, received_at) \
             VALUES ('{telephony_old}', '{ended}', 'LEG_ANSWERED', 'APPLIED', {t}, {t})",
            t = ago(&a, 31)
        ),
    )
    .await
    .unwrap();
    let audit_old = operator_row(&a, 2556).await;
    let audit_recent = operator_row(&a, 2554).await;

    // Plain DELETE / UPDATE / TRUNCATE stay refused on every purgeable evidence table.
    for sql in [
        format!("DELETE FROM calling_incidents WHERE id = '{resolved_old}'"),
        format!("DELETE FROM call_media_provider_events WHERE id = '{unknown_old}'"),
        format!("DELETE FROM call_telephony_events WHERE event_id = '{telephony_old}'"),
        format!("DELETE FROM platform_operator_audit WHERE id = '{audit_old}'"),
        format!("UPDATE platform_operator_audit SET actor = 'x' WHERE id = '{audit_old}'"),
        "TRUNCATE calling_incidents".to_string(),
        "TRUNCATE call_media_provider_events".to_string(),
        "TRUNCATE platform_operator_audit".to_string(),
    ] {
        assert!(exec(conn(&a), &sql).await.is_err(), "{sql}");
    }

    let r = tick(&a).await;
    assert!(r.failed.is_empty(), "{r:?}");
    assert_eq!(r.incidents_deleted, 1);
    assert!(!exists(&a, "calling_incidents", "id", resolved_old).await);
    assert!(
        exists(&a, "calling_incidents", "id", open_old).await,
        "OPEN never"
    );
    assert!(exists(&a, "calling_incidents", "id", resolved_recent).await);
    assert_eq!(r.provider_events_deleted, 3, "{r:?}");
    assert!(!exists(&a, "call_media_provider_events", "id", unknown_old).await);
    assert!(!exists(&a, "call_media_provider_events", "id", ended_old).await);
    assert!(!exists(&a, "call_telephony_events", "event_id", telephony_old).await);
    assert!(
        exists(&a, "call_media_provider_events", "id", live_old).await,
        "a live Call's provider evidence stays"
    );
    assert!(exists(&a, "call_media_provider_events", "id", ended_recent).await);
    // The operator audit: its own bound only (the audited tenant still exists).
    assert_eq!(r.operator_audit_deleted, 1);
    assert!(!exists(&a, "platform_operator_audit", "id", audit_old).await);
    assert!(exists(&a, "platform_operator_audit", "id", audit_recent).await);
    assert_eq!(audited(&a, "CALLING_INCIDENT", "DELETED").await, 1);
    assert_eq!(audited(&a, "MEDIA_PROVIDER_EVIDENCE", "DELETED").await, 3);
    assert_eq!(audited(&a, "OPERATOR_AUDIT", "DELETED").await, 1);
    // The retention audit itself is append-only.
    for sql in [
        "UPDATE calling_retention_audit SET batch_count = 0",
        "DELETE FROM calling_retention_audit",
        "TRUNCATE calling_retention_audit",
    ] {
        assert!(exec(conn(&a), sql).await.is_err(), "{sql}");
    }

    // The worker flag opens the purgeable tables only: Calls, events, usage, spend,
    // participants and the enterprise / recording audits stay undeletable with it.
    for sql in [
        format!("DELETE FROM call_usage_records WHERE call_id = '{ended}'"),
        format!("DELETE FROM call_events WHERE call_id = '{ended}'"),
        format!("DELETE FROM call_participants WHERE call_id = '{ended}'"),
        format!("DELETE FROM calls WHERE id = '{ended}'"),
        "DELETE FROM calling_retention_audit".to_string(),
        "TRUNCATE call_media_provider_events".to_string(),
        format!("UPDATE platform_operator_audit SET actor = 'x' WHERE id = '{audit_recent}'"),
    ] {
        let txn = conn(&a).begin().await.unwrap();
        txn.execute_unprepared("SET LOCAL calling.retention_worker = 'on'")
            .await
            .unwrap();
        let result = txn.execute_unprepared(&sql).await;
        txn.rollback().await.unwrap();
        assert!(result.is_err(), "{sql} passed under the worker flag");
    }
    assert_eq!(
        count(
            &a,
            &format!("SELECT count(*) FROM calls WHERE id = '{ended}'")
        )
        .await,
        1
    );
}

#[tokio::test]
async fn holds_protect_their_own_scope_only_and_are_operator_audited() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let mine = owner(&a);
    let theirs = other_owner(&a).await;
    let held_call = named_call(&a, "Held Caller", true).await;
    let free_call = named_call(&a, "Free Caller", true).await;
    let my_incident = incident(&a, &mine, "RESOLVED", 400, None).await;
    let their_incident = incident(&a, &theirs, "RESOLVED", 400, None).await;
    let other_incident = incident(&a, &mine, "RESOLVED", 400, None).await;

    let operator = op();
    let t = now(&a);
    // A class hold in MY scope, a resource hold on one Call, and a hold naming my
    // Call's id in ANOTHER tenant's scope (protects nothing).
    let class_hold = holds::operator_place(
        conn(&a),
        &operator,
        HoldScope {
            data_class: Some(DataClass::CallingIncident),
            resource_id: Some(my_incident),
            owner: Some(mine),
        },
        HoldReason::Dispute,
        t,
    )
    .await
    .unwrap();
    holds::operator_place(
        conn(&a),
        &op(),
        HoldScope {
            data_class: Some(DataClass::CallingIncident),
            resource_id: Some(their_incident),
            owner: Some(mine),
        },
        HoldReason::Other,
        t,
    )
    .await
    .unwrap();
    let call_hold = holds::operator_place(
        conn(&a),
        &op(),
        HoldScope {
            data_class: None,
            resource_id: Some(held_call),
            owner: Some(mine),
        },
        HoldReason::SecurityInvestigation,
        t,
    )
    .await
    .unwrap();
    holds::operator_place(
        conn(&a),
        &op(),
        HoldScope {
            data_class: None,
            resource_id: Some(free_call),
            owner: Some(theirs),
        },
        HoldReason::PolicyReview,
        t,
    )
    .await
    .unwrap();
    // Operator audit: one row per placement, same correlation as the operator context.
    assert_eq!(
        count(
            &a,
            &format!(
                "SELECT count(*) FROM platform_operator_audit WHERE action = 'RETENTION_HOLD_PLACED' \
                   AND target_kind = 'RETENTION_HOLD' AND target_id = '{}' AND actor = 'ops.retention' \
                   AND correlation_id = '{}' AND tenant_id = '{}'",
                class_hold.id,
                operator.correlation_id(),
                mine.tenant_id
            )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM platform_operator_audit WHERE action = 'RETENTION_HOLD_PLACED'"
        )
        .await,
        4
    );

    a.h().clock.advance(Duration::days(366));
    let r = tick(&a).await;
    assert!(r.failed.is_empty(), "{r:?}");
    // Held: my incident, my held Call. Not held: the other tenant's incident (a hold in
    // my scope naming it protects nothing), my other incident, the free Call (a hold in
    // the other tenant's scope naming it protects nothing).
    assert!(exists(&a, "calling_incidents", "id", my_incident).await);
    assert!(!exists(&a, "calling_incidents", "id", their_incident).await);
    assert!(!exists(&a, "calling_incidents", "id", other_incident).await);
    assert_eq!(r.incidents_deleted, 2);
    assert_eq!(r.calls_minimized, 1);
    assert!(display_names(&a, held_call)
        .await
        .contains(&Some("Held Caller".to_string())));
    assert!(display_names(&a, free_call)
        .await
        .iter()
        .all(Option::is_none));
    // The skipped rows are evidence too — once, not once per tick. (The `*` hold on the
    // held Call also keeps that Call's provider evidence.)
    assert_eq!(audited(&a, "CALLING_INCIDENT", "SKIPPED_HOLD").await, 1);
    assert_eq!(audited(&a, "CALL_RECORD", "SKIPPED_HOLD").await, 1);
    let skipped_rows =
        "SELECT count(*) FROM calling_retention_audit WHERE outcome = 'SKIPPED_HOLD'";
    let rows = count(&a, skipped_rows).await;
    tick(&a).await;
    assert_eq!(
        count(&a, skipped_rows).await,
        rows,
        "one row per change, not per tick"
    );
    assert!(
        holds::is_held(conn(&a), DataClass::CallRecord, Some(held_call), Some(mine))
            .await
            .unwrap()
    );
    assert!(!holds::is_held(
        conn(&a),
        DataClass::CallRecord,
        Some(held_call),
        Some(theirs)
    )
    .await
    .unwrap());

    // Holds are immutable and never deleted; a partial scope is refused.
    for sql in [
        format!("UPDATE calling_retention_holds SET reason_code = 'OTHER' WHERE id = '{}'", class_hold.id),
        format!("DELETE FROM calling_retention_holds WHERE id = '{}'", class_hold.id),
        "TRUNCATE calling_retention_holds".to_string(),
        format!(
            "INSERT INTO calling_retention_holds (id, data_class, tenant_id, reason_code, created_by, created_at) \
             VALUES ('{}', '*', '{}', 'OTHER', 'ops', now())",
            Uuid::new_v4(),
            mine.tenant_id
        ),
        format!(
            "INSERT INTO calling_retention_holds (id, data_class, reason_code, created_by, created_at) \
             VALUES ('{}', 'CALL_RECORD', 'OTHER', 'ops', now())",
            Uuid::new_v4()
        ),
    ] {
        assert!(exec(conn(&a), &sql).await.is_err(), "{sql}");
    }
    assert!(matches!(
        holds::operator_place(
            conn(&a),
            &op(),
            HoldScope {
                data_class: Some(DataClass::CallRecord),
                resource_id: None,
                owner: None,
            },
            HoldReason::Other,
            t,
        )
        .await,
        Err(PlatformError::Validation(_))
    ));

    // Release (audited, idempotent): the next tick acts and records the return to zero.
    let released = holds::operator_release(conn(&a), &op(), class_hold.id, now(&a))
        .await
        .unwrap();
    assert!(released.released_at.is_some());
    let again = holds::operator_release(conn(&a), &op(), class_hold.id, now(&a))
        .await
        .unwrap();
    assert_eq!(again.released_at, released.released_at);
    holds::operator_release(conn(&a), &op(), call_hold.id, now(&a))
        .await
        .unwrap();
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM platform_operator_audit WHERE action = 'RETENTION_HOLD_RELEASED'"
        )
        .await,
        3
    );
    let r = tick(&a).await;
    assert_eq!((r.incidents_deleted, r.calls_minimized), (1, 1));
    assert!(!exists(&a, "calling_incidents", "id", my_incident).await);
    // The release is visible as a return to zero, per class that had held rows.
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM calling_retention_audit WHERE outcome = 'SKIPPED_HOLD' AND batch_count = 0"
        )
        .await,
        rows
    );
    assert_eq!(holds::list(conn(&a), true).await.unwrap().len(), 2);
    assert_eq!(holds::list(conn(&a), false).await.unwrap().len(), 4);

    // A platform-wide hold keeps everything.
    holds::operator_place(
        conn(&a),
        &op(),
        HoldScope {
            data_class: None,
            resource_id: None,
            owner: None,
        },
        HoldReason::PolicyReview,
        now(&a),
    )
    .await
    .unwrap();
    let late = incident(&a, &theirs, "RESOLVED", 400, None).await;
    let r = tick(&a).await;
    assert_eq!(r.total(), 0, "{r:?}");
    assert!(exists(&a, "calling_incidents", "id", late).await);
}

#[tokio::test]
async fn two_concurrent_workers_act_on_each_row_exactly_once() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let o = owner(&a);
    for _ in 0..60 {
        incident(&a, &o, "RESOLVED", 400, None).await;
        operator_row(&a, 2600).await;
    }
    let (w1, w2) = (worker(&a), worker(&a));
    let mut rounds = 0;
    loop {
        let (r1, r2) = tokio::join!(w1.retention_tick(7), w2.retention_tick(7));
        let (r1, r2) = (r1.unwrap(), r2.unwrap());
        assert!(r1.failed.is_empty() && r2.failed.is_empty());
        rounds += 1;
        if r1.total() + r2.total() == 0 {
            break;
        }
        assert!(rounds < 40, "workers never drained");
    }
    assert_eq!(count(&a, "SELECT count(*) FROM calling_incidents").await, 0);
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM platform_operator_audit WHERE actor = 'ops.past'"
        )
        .await,
        0
    );
    // Every row acted on exactly once: the audited counts are the row counts.
    assert_eq!(audited(&a, "CALLING_INCIDENT", "DELETED").await, 60);
    assert_eq!(audited(&a, "OPERATOR_AUDIT", "DELETED").await, 60);
    // Batches are bounded.
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM calling_retention_audit WHERE outcome = 'DELETED' AND batch_count > 7"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn a_failed_batch_leaves_nothing_partial_and_is_retried() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let sub = subscription(&a).await;
    let event = webhook_event(&a, 100).await;
    let d = delivery(&a, sub, event, "SUCCEEDED", 100, None).await;
    let resolved = incident(&a, &owner(&a), "RESOLVED", 400, None).await;
    // An injected database failure on the delivery delete (after the attempts delete in
    // the same transaction).
    exec(
        conn(&a),
        "CREATE FUNCTION test_refuse_delete() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'injected failure'; END; $$; \
         CREATE TRIGGER trg_test_refuse_delete BEFORE DELETE ON webhook_deliveries \
           FOR EACH ROW EXECUTE FUNCTION test_refuse_delete();",
    )
    .await
    .unwrap();
    let failed_before = calling::metrics::counter(
        "calling_retention_actions_total",
        &[
            ("data_class", "WEBHOOK_DELIVERY_HISTORY"),
            ("action", "DELETE"),
            ("outcome", "FAILED"),
        ],
    );
    let r = tick(&a).await;
    assert_eq!(r.failed, vec![DataClass::WebhookDeliveryHistory], "{r:?}");
    // Nothing partial: the attempts deleted before the failure are back (rolled back).
    assert!(exists(&a, "webhook_deliveries", "id", d).await);
    assert!(exists(&a, "webhook_delivery_attempts", "delivery_id", d).await);
    assert!(exists(&a, "webhook_events", "id", event).await);
    assert_eq!(audited(&a, "WEBHOOK_DELIVERY_HISTORY", "DELETED").await, 0);
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM calling_retention_audit WHERE data_class = 'WEBHOOK_DELIVERY_HISTORY' \
               AND outcome = 'FAILED'"
        )
        .await,
        1
    );
    assert!(
        calling::metrics::counter(
            "calling_retention_actions_total",
            &[
                ("data_class", "WEBHOOK_DELIVERY_HISTORY"),
                ("action", "DELETE"),
                ("outcome", "FAILED"),
            ],
        ) > failed_before
    );
    // The other classes were not stopped by the failure.
    assert_eq!(r.incidents_deleted, 1);
    assert!(!exists(&a, "calling_incidents", "id", resolved).await);

    // The dependency recovers: the next tick finishes the work, once.
    exec(
        conn(&a),
        "DROP TRIGGER trg_test_refuse_delete ON webhook_deliveries; DROP FUNCTION test_refuse_delete();",
    )
    .await
    .unwrap();
    let r = tick(&a).await;
    assert!(r.failed.is_empty(), "{r:?}");
    assert_eq!(
        (r.webhook_deliveries_deleted, r.webhook_events_deleted),
        (1, 1)
    );
    assert!(!exists(&a, "webhook_deliveries", "id", d).await);
    assert!(!exists(&a, "webhook_delivery_attempts", "delivery_id", d).await);
    assert_eq!(audited(&a, "WEBHOOK_DELIVERY_HISTORY", "DELETED").await, 1);
}

#[tokio::test]
async fn principal_tombstone_keeps_history_and_foreign_keys_and_never_resolves_again() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let call = named_call(&a, "Ada Lovelace", true).await;
    let principal = a.w.initiator.id().as_uuid();
    let before = evidence(&a).await;
    let operator = op();

    let view = tombstone::operator_tombstone(
        conn(&a),
        a.h().state.identity.authority().as_ref(),
        &operator,
        principal,
        now(&a),
    )
    .await
    .unwrap();
    assert!(!view.already_tombstoned);
    assert!(view.participations_minimized >= 1);
    assert_eq!(
        count(
            &a,
            &format!(
                "SELECT count(*) FROM platform_principals WHERE id = '{principal}' AND status = 'DEACTIVATED' \
                   AND tombstoned_at IS NOT NULL AND external_reference = 'tombstoned:{principal}'"
            )
        )
        .await,
        1
    );
    assert!(display_names(&a, call).await.iter().all(Option::is_none));
    // Nothing deleted; every reference to the principal still resolves to its row.
    assert_eq!(evidence(&a).await, before);
    for (table, column) in [
        ("calls", "initiator_principal_id"),
        ("call_participants", "principal_id"),
        ("call_invitations", "principal_id"),
    ] {
        assert_eq!(
            count(
                &a,
                &format!(
                    "SELECT count(*) FROM {table} t LEFT JOIN platform_principals p ON p.id = t.{column} \
                     WHERE p.id IS NULL"
                )
            )
            .await,
            0,
            "{table}.{column}"
        );
    }
    assert!(
        count(
            &a,
            &format!("SELECT count(*) FROM call_participants WHERE principal_id = '{principal}'")
        )
        .await
            >= 1
    );
    // History (the other party's) still lists the Call with the opaque id.
    let entry = history_entry(&app_history(&a, a.w.recipient.id().as_uuid()).await, call);
    assert_eq!(
        entry["counterparts"][0]["principalId"],
        principal.to_string()
    );
    assert!(entry["counterparts"][0]["displayName"].is_null());
    // Audited: operator audit and retention audit, no identifying detail.
    assert_eq!(
        count(
            &a,
            &format!(
                "SELECT count(*) FROM platform_operator_audit WHERE action = 'PRINCIPAL_TOMBSTONED' \
                   AND target_id = '{principal}' AND correlation_id = '{}' \
                   AND NOT (detail::text LIKE '%initiator-1%')",
                operator.correlation_id()
            )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &a,
            &format!(
                "SELECT count(*) FROM calling_retention_audit WHERE data_class = 'PRINCIPAL' AND action = 'MINIMIZE' \
                   AND outcome = 'MINIMIZED' AND resource_ref = '{principal}' AND actor = 'ops.retention'"
            )
        )
        .await,
        1
    );

    // It never resolves again: not by id, not by its former reference.
    let ctx = &a.w.ctx;
    assert!(a
        .h()
        .state
        .principals
        .resolve(ctx, &PrincipalReference::Id(a.w.initiator.id()))
        .await
        .is_err());
    assert!(a
        .h()
        .state
        .principals
        .resolve(
            ctx,
            &PrincipalReference::External {
                kind: calling::domain::PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "initiator-1".to_string(),
            },
        )
        .await
        .is_err());
    let caller = Uuid::new_v4();
    let r = a
        .post_with(
            "/v1/calls",
            json!({
                "kind": "VOICE",
                "initiatorPrincipalId": principal,
                "initiatorEndpointId": caller,
                "targetPrincipalIds": [a.w.recipient.id()],
            }),
            &[("idempotency-key", &Uuid::new_v4().to_string())],
        )
        .await;
    assert!(r.status.is_client_error(), "{} {}", r.status, r.text);
    // Never reactivated, never re-identified.
    assert!(matches!(
        a.h()
            .platform
            .set_principal_status(a.w.initiator.id(), calling::domain::PrincipalStatus::Active)
            .await,
        Err(PlatformError::InvalidState(_))
    ));
    for sql in [
        format!("UPDATE platform_principals SET status = 'ACTIVE' WHERE id = '{principal}'"),
        format!("UPDATE platform_principals SET external_reference = 'initiator-1' WHERE id = '{principal}'"),
        format!("UPDATE platform_principals SET tombstoned_at = NULL WHERE id = '{principal}'"),
        format!("DELETE FROM platform_principals WHERE id = '{principal}'"),
    ] {
        assert!(exec(conn(&a), &sql).await.is_err(), "{sql}");
    }
    let env = a.w.consumer.env(a.w.kind).id;
    assert!(matches!(
        a.h()
            .platform
            .register_principal(
                env,
                NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::ExternalApplicationPrincipal,
                    external_reference: format!("tombstoned:{}", Uuid::new_v4()),
                },
            )
            .await,
        Err(PlatformError::PolicyViolation(_))
    ));
    // Idempotent.
    let again = tombstone::operator_tombstone(
        conn(&a),
        a.h().state.identity.authority().as_ref(),
        &op(),
        principal,
        now(&a),
    )
    .await
    .unwrap();
    assert!(again.already_tombstoned);
    assert_eq!(
        count(
            &a,
            "SELECT count(*) FROM calling_retention_audit WHERE data_class = 'PRINCIPAL'"
        )
        .await,
        1
    );

    // A held principal is not tombstoned.
    let other = a.w.recipient.id().as_uuid();
    holds::operator_place(
        conn(&a),
        &op(),
        HoldScope {
            data_class: Some(DataClass::Principal),
            resource_id: Some(other),
            owner: Some(owner(&a)),
        },
        HoldReason::Dispute,
        now(&a),
    )
    .await
    .unwrap();
    assert!(matches!(
        tombstone::operator_tombstone(
            conn(&a),
            a.h().state.identity.authority().as_ref(),
            &op(),
            other,
            now(&a)
        )
        .await,
        Err(PlatformError::InvalidState(_))
    ));
    assert_eq!(
        count(
            &a,
            &format!("SELECT count(*) FROM platform_principals WHERE id = '{other}' AND tombstoned_at IS NULL")
        )
        .await,
        1
    );
}

/// The enterprise entry label of a terminal Call (a copy of the extension's display
/// label) is minimized to the extension code, once, by the worker only.
#[tokio::test]
async fn enterprise_entry_labels_are_minimized_to_the_extension_code() {
    let Some(e) = common::enterprise::EnterpriseWorld::new().await else {
        return;
    };
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    e.a.device(&bob, "bob-mobile").await;
    let r = e
        .call_extension("VOICE", &alice, &alice_phone, "1002", "retention-ext-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let label = |e: &common::enterprise::EnterpriseWorld| {
        let conn = e.h().conn().clone();
        async move {
            common::enterprise::strings(
                &conn,
                &format!(
                    "SELECT entry_label FROM enterprise_call_contexts WHERE call_id = '{call}'"
                ),
            )
            .await
        }
    };
    assert_eq!(label(&e).await, ["Extension 1002"]);
    // Only the worker may change it, and only to the code.
    assert!(exec(
        e.h().conn(),
        &format!(
            "UPDATE enterprise_call_contexts SET entry_label = '1002', version = version + 1 \
             WHERE call_id = '{call}'"
        ),
    )
    .await
    .is_err());
    // A live Call keeps it however old.
    e.h().clock.advance(Duration::minutes(30));
    assert_eq!(tick_one_day_display(&e.a).await.calls_minimized, 0);
    let (_, v) = e.a.state(call).await;
    let cancel = e.a.command(call, "cancel", v).await;
    assert_eq!(cancel.status, StatusCode::OK, "{}", cancel.text);
    e.h().clock.advance(Duration::hours(25));
    let r = tick_one_day_display(&e.a).await;
    assert_eq!(r.calls_minimized, 1, "{r:?}");
    assert_eq!(label(&e).await, ["1002"]);
    // Never back.
    assert!(exec(
        e.h().conn(),
        &format!(
            "UPDATE enterprise_call_contexts SET entry_label = 'Extension 1002', version = version + 1 \
             WHERE call_id = '{call}'"
        ),
    )
    .await
    .is_err());
    assert_eq!(tick_one_day_display(&e.a).await.calls_minimized, 0);
    assert_eq!(label(&e).await, ["1002"]);
}
