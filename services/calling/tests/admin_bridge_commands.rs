//! ADR 0068 phase A4: the admin bridge's operator commands (`POST /internal/v1/operator/*`)
//! and its settlement read (`GET /internal/v1/operator/operations/{idempotencyKey}`).
//!
//! Only `cfmeet.admin-bridge` (calling.administer) with a named HQ operator may command;
//! the key is required, a stale `expectedVersion` changes nothing (409 VERSION_CONFLICT),
//! two-person commands need the approval headers, a repeated key replays without applying
//! again, the same key elsewhere is 409 IDEMPOTENCY_CONFLICT, and every applied command
//! is recorded with its audit row in the same transaction. The recording deletion is
//! certified in `enterprise_recording.rs` (it needs a recorded call).

mod common;

use axum::http::{Method, StatusCode};
use common::{
    app::AppWorld,
    bridge::{self, Attribution, APPROVER, OPERATOR},
    TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

fn tenant(w: &AppWorld) -> Uuid {
    w.w.consumer.tenant.id.as_uuid()
}

fn environment(w: &AppWorld) -> Uuid {
    w.w.consumer.env(w.w.kind).id.as_uuid()
}

fn assert_result(r: &TestResponse, status: &str, version: Option<&str>) {
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["cache-control"], "no-store");
    common::contracts::check_openapi_component("OperatorCommandResult", &r.json);
    assert_eq!(r.json["status"], status, "{}", r.text);
    assert_eq!(r.json["version"], json!(version), "{}", r.text);
    let text = r.text.to_lowercase();
    for banned in [
        "sip:",
        "tel:",
        "+234",
        "objectkey",
        "object_key",
        "secret",
        "url",
    ] {
        assert!(!text.contains(banned), "{banned} must never appear: {text}");
    }
}

fn code(r: &TestResponse) -> String {
    r.json["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The settlement record answers what the command answered.
async fn assert_record(w: &AppWorld, key: Uuid, answered: &TestResponse) {
    let r = bridge::operation(w.h(), key).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check_openapi_component("OperatorOperationRecord", &r.json);
    assert_eq!(r.json["idempotency_key"], json!(key.to_string()));
    assert_eq!(r.json["outcome"], "APPLIED");
    assert_eq!(r.json["source_status"], answered.json["status"]);
    assert_eq!(r.json["source_version"], answered.json["version"]);
}

async fn recorded(w: &AppWorld, key: Uuid) -> i64 {
    w.w.count(&format!(
        "SELECT count(*) FROM calling_admin_operations WHERE idempotency_key = '{key}'"
    ))
    .await
}

/// A connected call reported by its initiator; returns the report id.
async fn reported_call(w: &AppWorld) -> Uuid {
    let (call, _, _) = w.connected_call("VOICE").await;
    let r = w
        .post(
            &format!("/v1/calls/{call}/abuse-reports"),
            json!({ "principalId": w.w.initiator.id(), "category": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["abuseReportId"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn the_bridge_resolves_an_abuse_report_once_at_its_version() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let report = reported_call(&w).await;
    let t = tenant(&w);
    let path = format!("/internal/v1/operator/abuse-reports/{report}/resolve");
    let body = |v: i64| json!({ "tenantId": t, "expectedVersion": v, "status": "REVIEWED" });

    // The operator console holds calling-abuse-reports.review, not calling.administer.
    let console = h.peers.operator_console.clone();
    let r = bridge::signed(
        h,
        &console,
        Method::POST,
        &path,
        Some(body(1)),
        Attribution::None,
        Some(Uuid::new_v4()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // The bridge without a named operator, and without a key.
    let r = bridge::signed(
        h,
        &h.peers.admin_bridge.clone(),
        Method::POST,
        &path,
        Some(body(1)),
        Attribution::None,
        Some(Uuid::new_v4()),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = bridge::signed(
        h,
        &h.peers.admin_bridge.clone(),
        Method::POST,
        &path,
        Some(body(1)),
        Attribution::Operator,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // Unknown fields, another tenant, a stale version: nothing changes.
    let mut extra = body(1);
    extra["note"] = json!("free text");
    let r = bridge::command(h, &path, extra, Attribution::Operator, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": Uuid::new_v4(), "expectedVersion": 1, "status": "REVIEWED" }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let stale = Uuid::new_v4();
    let r = bridge::command(h, &path, body(7), Attribution::Operator, stale).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(code(&r), "VERSION_CONFLICT");
    assert_eq!(recorded(&w, stale).await, 0);
    assert_eq!(
        bridge::operation(h, stale).await.status,
        StatusCode::NOT_FOUND
    );

    // R2: the operator alone resolves it, at version 1.
    let key = Uuid::new_v4();
    let r = bridge::command(h, &path, body(1), Attribution::Operator, key).await;
    assert_result(&r, "REVIEWED", Some("2"));
    assert_eq!(
        r.json["resource"]["abuseReportId"],
        json!(report.to_string())
    );
    assert_record(&w, key, &r).await;
    // The same audited transition, attributed to the bridge, joined by the key.
    let audited =
        w.w.count(&format!(
            "SELECT count(*) FROM platform_operator_audit WHERE action = 'ABUSE_REPORT_STATUS_SET' \
             AND target_id = '{report}' AND actor = 'cfmeet.admin-bridge' \
             AND correlation_id = 'op_{}'",
            key.simple()
        ))
        .await;
    assert_eq!(audited, 1);
    let who =
        w.w.count(&format!(
            "SELECT count(*) FROM calling_admin_operations WHERE idempotency_key = '{key}' \
             AND operator_ref = '{OPERATOR}' AND approver_ref IS NULL \
             AND command = 'calling.abuse-report.resolve' AND subject_kind = 'ABUSE_REPORT' \
             AND tenant_id = '{t}' AND resulting_state = 'REVIEWED' AND resulting_version = 2"
        ))
        .await;
    assert_eq!(who, 1);

    // A replay answers the current report and applies nothing.
    let r = bridge::command(h, &path, body(1), Attribution::Operator, key).await;
    assert_result(&r, "REVIEWED", Some("2"));
    assert_eq!(
        w.w.count(&format!(
            "SELECT count(*) FROM platform_operator_audit WHERE target_id = '{report}'"
        ))
        .await,
        1
    );
    // The same key for another subject or command: 409, nothing done.
    let other = reported_call(&w).await;
    let r = bridge::command(
        h,
        &format!("/internal/v1/operator/abuse-reports/{other}/resolve"),
        body(1),
        Attribution::Operator,
        key,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(code(&r), "IDEMPOTENCY_CONFLICT");

    // The record is the bridge's alone.
    let r = bridge::signed(
        h,
        &console,
        Method::GET,
        &format!("/internal/v1/operator/operations/{key}"),
        None,
        Attribution::None,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = bridge::operation(h, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn two_person_commands_need_the_approval_and_record_both_people() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let t = tenant(&w);
    let r = w
        .post(
            "/v1/blocks",
            json!({ "principalId": w.w.initiator.id(), "blockedPrincipalId": w.w.recipient.id(),
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let block: Uuid = r.json["blockId"].as_str().unwrap().parse().unwrap();
    let path = format!("/internal/v1/operator/blocks/{block}/release");
    let body = json!({ "tenantId": t, "expectedVersion": 1 });

    let refused = Uuid::new_v4();
    let r = bridge::command(h, &path, body.clone(), Attribution::Operator, refused).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(recorded(&w, refused).await, 0);
    let still =
        w.w.count(&format!(
            "SELECT count(*) FROM calling_block_rules WHERE id = '{block}' AND status = 'ACTIVE'"
        ))
        .await;
    assert_eq!(still, 1);

    let key = Uuid::new_v4();
    let r = bridge::command(h, &path, body.clone(), Attribution::Approved, key).await;
    assert_result(&r, "RELEASED", Some("2"));
    assert!(r.json["resource"]["releasedAt"].is_string());
    assert_record(&w, key, &r).await;
    let both =
        w.w.count(&format!(
            "SELECT count(*) FROM calling_admin_operations WHERE idempotency_key = '{key}' \
             AND operator_ref = '{OPERATOR}' AND approver_ref = '{APPROVER}' \
             AND approval_ref = '{}'",
            bridge::APPROVAL_REF
        ))
        .await;
    assert_eq!(both, 1);
    // Released already: the old version is stale now.
    let r = bridge::command(h, &path, body, Attribution::Approved, Uuid::new_v4()).await;
    assert_eq!(code(&r), "VERSION_CONFLICT", "{}", r.text);
}

#[tokio::test]
async fn limits_change_at_their_version_and_keep_the_spend_ceilings() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let t = tenant(&w);
    let path = "/internal/v1/operator/limits";
    // Not configured yet: version 0.
    let key = Uuid::new_v4();
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t, "expectedVersion": 0, "requestsPerMinute": 600 }),
        Attribution::Approved,
        key,
    )
    .await;
    assert_result(&r, "CONFIGURED", Some("1"));
    assert_eq!(r.json["resource"]["requestsPerMinute"], 600);
    assert_record(&w, key, &r).await;
    let read = bridge::signed(
        h,
        &h.peers.admin_bridge.clone(),
        Method::GET,
        &format!("/internal/v1/operator/limits?tenantId={t}"),
        None,
        Attribution::Operator,
        None,
    )
    .await;
    assert_eq!(read.json["tenant"]["version"], 1, "{}", read.text);
    assert_eq!(read.json["tenant"]["requestsPerMinute"], 600);

    // A spend ceiling set by the emergency CLI path survives an R3 limits change.
    h.platform
        .operator_set_tenant_limits(
            &calling::platform::operator::OperatorContext::new("ops.cli").unwrap(),
            w.w.consumer.tenant.id,
            calling::platform::limits::LimitValues {
                requests_per_minute: Some(600),
                telecom_spend_daily_limit_minor: Some(5_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t, "expectedVersion": 1, "requestsPerMinute": 700 }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(code(&r), "VERSION_CONFLICT", "{}", r.text);
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t, "expectedVersion": 2, "requestsPerMinute": 700,
                "maxConcurrentCommunications": 20 }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "CONFIGURED", Some("3"));
    let kept =
        w.w.count(&format!(
            "SELECT count(*) FROM platform_tenant_limits WHERE tenant_id = '{t}' \
             AND telecom_spend_daily_limit_minor = 5000 AND requests_per_minute = 700 \
             AND max_concurrent_communications = 20 AND version = 3"
        ))
        .await;
    assert_eq!(kept, 1);

    // The application level; an application of another tenant is 404; R3 needs approval.
    let app = w.w.consumer.application.id.as_uuid();
    let body = json!({ "tenantId": t, "applicationId": app, "expectedVersion": 0,
                       "maxConcurrentCommunications": 5 });
    let r = bridge::command(h, path, body.clone(), Attribution::Operator, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": Uuid::new_v4(), "applicationId": app, "expectedVersion": 0 }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = bridge::command(h, path, body, Attribution::Approved, Uuid::new_v4()).await;
    assert_result(&r, "CONFIGURED", Some("1"));
    assert_eq!(r.json["resource"]["applicationId"], json!(app.to_string()));
    // Invalid limits are refused before anything changes.
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t, "expectedVersion": 3, "requestsPerMinute": 0 }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

#[tokio::test]
async fn routing_enterprise_and_spend_policies_change_at_their_revision() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let (t, env) = (tenant(&w), environment(&w));

    // Routing: the restrictive default document, from revision 0.
    let policy = serde_json::to_value(calling::routing::policy::RoutingPolicy::default()).unwrap();
    let path = format!("/internal/v1/operator/routing-policies/{env}");
    let key = Uuid::new_v4();
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 0, "policy": policy }),
        Attribution::Approved,
        key,
    )
    .await;
    assert_result(&r, "CONFIGURED", Some("1"));
    assert_record(&w, key, &r).await;
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 0, "policy": policy }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(code(&r), "VERSION_CONFLICT", "{}", r.text);
    let mut bad = policy.clone();
    bad["unknownField"] = json!(true);
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 1, "policy": bad }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = bridge::command(
        h,
        &format!("/internal/v1/operator/routing-policies/{}", Uuid::new_v4()),
        json!({ "tenantId": t, "expectedVersion": 0, "policy": policy }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // Enterprise features: the baseline is created at version 1, then updated.
    let path = format!("/internal/v1/operator/enterprise-policies/{env}/capabilities");
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 0, "capabilities": ["MANAGE_ROUTING"] }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "CONFIGURED", Some("1"));
    assert_eq!(
        r.json["resource"]["adminCapabilities"],
        json!(["MANAGE_ROUTING"])
    );
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 1,
                "capabilities": ["MANAGE_ROUTING", "MANAGE_VOICEMAIL"] }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "CONFIGURED", Some("2"));
    let r = bridge::command(
        h,
        &path,
        json!({ "tenantId": t, "expectedVersion": 2, "capabilities": ["MANAGE_EVERYTHING"] }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let audited = w
        .w
        .count(&format!(
            "SELECT count(*) FROM platform_operator_audit WHERE action = 'ENTERPRISE_CAPABILITIES_SET' \
             AND target_id = '{env}' AND actor = 'cfmeet.admin-bridge'"
        ))
        .await;
    assert_eq!(audited, 2);

    // Spend (R4): created disabled, then enabled at revision 1.
    let path = format!("/internal/v1/operator/spend-policies/{env}");
    let spend = |v: i64, enabled: bool| {
        json!({ "tenantId": t, "expectedVersion": v, "currency": "NGN",
                "legAllowanceMinor": 500, "maxLegSeconds": 3600, "dailyLimitMinor": 100_000,
                "monthlyLimitMinor": 1_000_000, "maxConcurrentLegs": 10,
                "maxOriginationsPerMinute": 30, "maxPerPartyPerHour": 10, "enabled": enabled })
    };
    let r = bridge::command(
        h,
        &path,
        spend(0, false),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = bridge::command(
        h,
        &path,
        spend(0, false),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "DISABLED", Some("1"));
    let r = bridge::command(
        h,
        &path,
        spend(1, true),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "ENABLED", Some("2"));
    assert_eq!(r.json["resource"]["currency"], "NGN");
    let mut lower = spend(2, true);
    lower["currency"] = json!("ngn");
    let r = bridge::command(h, &path, lower, Attribution::Approved, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
}

#[tokio::test]
async fn retention_holds_are_placed_under_the_key_and_released_for_the_tenant() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let (t, env) = (tenant(&w), environment(&w));
    let app = w.w.consumer.application.id.as_uuid();
    let place = |class: &str| {
        json!({ "tenantId": t, "applicationId": app, "environmentId": env,
                "dataClass": class, "reasonCode": "DISPUTE" })
    };
    let path = "/internal/v1/operator/retention-holds";
    let r = bridge::command(h, path, place("*"), Attribution::Operator, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = bridge::command(
        h,
        path,
        place("NOT_A_CLASS"),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    // An environment outside the tenant: 404, nothing placed.
    let mut foreign = place("*");
    foreign["tenantId"] = json!(Uuid::new_v4());
    let r = bridge::command(h, path, foreign, Attribution::Approved, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    let hold = Uuid::new_v4();
    let r = bridge::command(h, path, place("CALL_RECORD"), Attribution::Approved, hold).await;
    assert_result(&r, "ACTIVE", None);
    assert_eq!(r.json["resource"]["holdId"], json!(hold.to_string()));
    assert_record(&w, hold, &r).await;
    let r = bridge::command(h, path, place("CALL_RECORD"), Attribution::Approved, hold).await;
    assert_result(&r, "ACTIVE", None);
    assert_eq!(
        w.w.count("SELECT count(*) FROM calling_retention_holds")
            .await,
        1,
        "the replay placed nothing"
    );

    let release = format!("/internal/v1/operator/retention-holds/{hold}/release");
    let r = bridge::command(
        h,
        &release,
        json!({ "tenantId": t, "expectedVersion": 1 }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "unversioned: {}", r.text);
    let key = Uuid::new_v4();
    let r = bridge::command(
        h,
        &release,
        json!({ "tenantId": t }),
        Attribution::Approved,
        key,
    )
    .await;
    assert_result(&r, "RELEASED", None);
    assert_record(&w, key, &r).await;
    // Idempotent under a new key (the audit row says it was not active).
    let r = bridge::command(
        h,
        &release,
        json!({ "tenantId": t }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_result(&r, "RELEASED", None);

    // A platform-wide hold (CLI only) is not the tenant's.
    let platform = calling::retention::holds::operator_place(
        h.conn(),
        &calling::platform::operator::OperatorContext::new("ops.cli").unwrap(),
        calling::retention::holds::HoldScope {
            data_class: None,
            resource_id: None,
            owner: None,
        },
        calling::retention::holds::HoldReason::Other,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let r = bridge::command(
        h,
        &format!(
            "/internal/v1/operator/retention-holds/{}/release",
            platform.id
        ),
        json!({ "tenantId": t }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn incidents_are_recorded_resolved_for_the_tenant_only() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let t = tenant(&w);
    let scope = w.w.scope();
    let (scoped, platform) = (Uuid::new_v4(), Uuid::new_v4());
    common::exec(
        h.conn(),
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, \
               category, severity, status, resource_type, resource_id, occurrences, restricted_evidence, \
               first_seen_at, last_seen_at) VALUES \
             ('{scoped}', '{}', '{}', '{}', 'telephony_command_dead:telephony_command:{}', \
              'TELEPHONY_COMMAND_DEAD', 'HIGH', 'OPEN', 'TELEPHONY_COMMAND', '{}', 3, \
              '{{\"commandKind\": \"TERMINATE\"}}', now(), now()), \
             ('{platform}', NULL, NULL, NULL, 'worker_backlog:00000000000000000000000000000000:outbox', \
              'WORKER_BACKLOG', 'HIGH', 'OPEN', NULL, NULL, 1, '{{\"queue\": \"outbox\"}}', now(), now())",
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
            scoped.simple(),
            scoped,
        ),
    )
    .await
    .unwrap();
    // R2: the operator alone.
    let key = Uuid::new_v4();
    let r = bridge::command(
        h,
        &format!("/internal/v1/operator/incidents/{scoped}/resolve"),
        json!({ "tenantId": t }),
        Attribution::Operator,
        key,
    )
    .await;
    assert_result(&r, "RESOLVED", None);
    assert!(r.json["resource"].get("restrictedEvidence").is_none());
    assert!(
        !r.text.contains("TERMINATE"),
        "restricted evidence is never relayed"
    );
    assert_record(&w, key, &r).await;
    let r = bridge::command(
        h,
        &format!("/internal/v1/operator/incidents/{platform}/resolve"),
        json!({ "tenantId": t }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn the_tenants_webhook_secrets_are_resealed_without_key_material() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let t = tenant(&w);
    let path = "/internal/v1/operator/webhook-keys/reseal";
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": Uuid::new_v4() }),
        Attribution::Approved,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let key = Uuid::new_v4();
    let r = bridge::command(
        h,
        path,
        json!({ "tenantId": t }),
        Attribution::Approved,
        key,
    )
    .await;
    if h.state.webhooks.key_ring_configured() {
        assert_result(&r, "RESEALED", None);
        assert!(r.json["resource"]["keys"].is_array());
        assert_record(&w, key, &r).await;
        let audited = w
            .w
            .count(&format!(
                "SELECT count(*) FROM platform_operator_audit WHERE action = 'WEBHOOK_KEYS_RESEALED' \
                 AND tenant_id = '{t}' AND actor = 'cfmeet.admin-bridge'"
            ))
            .await;
        assert_eq!(audited, 1);
    } else {
        // No sealing key ring: a definite refusal, nothing recorded.
        assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
        assert_eq!(code(&r), "CONFLICT");
        assert_eq!(recorded(&w, key).await, 0);
    }
}

#[tokio::test]
async fn no_other_caller_reaches_a_command() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let t = tenant(&w);
    let paths: Vec<(String, Value)> = vec![
        (
            format!(
                "/internal/v1/operator/abuse-reports/{}/resolve",
                Uuid::new_v4()
            ),
            json!({ "tenantId": t, "expectedVersion": 1, "status": "REVIEWED" }),
        ),
        (
            format!("/internal/v1/operator/blocks/{}/release", Uuid::new_v4()),
            json!({ "tenantId": t, "expectedVersion": 1 }),
        ),
        (
            "/internal/v1/operator/limits".to_string(),
            json!({ "tenantId": t, "expectedVersion": 0 }),
        ),
        (
            format!("/internal/v1/operator/recordings/{}/delete", Uuid::new_v4()),
            json!({ "tenantId": t, "expectedVersion": 1 }),
        ),
        (
            format!("/internal/v1/operator/incidents/{}/resolve", Uuid::new_v4()),
            json!({ "tenantId": t }),
        ),
    ];
    let peers = h.peers.clone();
    for (path, body) in paths {
        for signer in [
            &peers.operator_console,
            &peers.telephony,
            &peers.meeting_service,
        ] {
            let r = bridge::signed(
                h,
                signer,
                Method::POST,
                &path,
                Some(body.clone()),
                Attribution::None,
                Some(Uuid::new_v4()),
            )
            .await;
            assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
        }
        // Operator attribution from anyone but the bridge is refused outright.
        let r = bridge::signed(
            h,
            &peers.operator_console,
            Method::POST,
            &path,
            Some(body.clone()),
            Attribution::Operator,
            Some(Uuid::new_v4()),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.text);
    }
    assert_eq!(
        w.w.count("SELECT count(*) FROM calling_admin_operations")
            .await,
        0
    );
}
