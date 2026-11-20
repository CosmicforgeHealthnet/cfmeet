//! The admin bridge's operator commands (ADR 0068 A4): `POST /internal/v1/operator/*` need
//! `notifications.administer` (bridge only), `X-CF-Operator`, an `Idempotency-Key` UUID and,
//! for R3 commands, the second person; each applied command is recorded in
//! `notif_admin_operations` in the same transaction and read back from
//! `GET /internal/v1/operator/operations/{idempotencyKey}`.

mod common;

use axum::http::Method;
use cf_notification_client::Producer;
use chrono::Utc;
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const U: &str = "5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn retry_path(delivery: &str) -> String {
    format!("/internal/v1/operator/deliveries/{delivery}/retry")
}

fn remove_path(digest: &str) -> String {
    format!("/internal/v1/operator/email-suppressions/{digest}/remove")
}

fn routing_path(category: &str, channel: &str) -> String {
    format!("/internal/v1/operator/routing/{category}/{channel}/mode")
}

fn reroute_path(decision: &str) -> String {
    format!("/internal/v1/operator/reroute/{decision}/cancel")
}

fn operation_path(key: &str) -> String {
    format!("/internal/v1/operator/operations/{key}")
}

fn key() -> String {
    Uuid::new_v4().to_string()
}

/// A notification with one delivery in `status` on `channel` / `lane`; returns the delivery id.
async fn delivery(h: &Harness, channel: &str, lane: &str, status: &str) -> String {
    let (n, d) = (Uuid::new_v4(), Uuid::new_v4());
    h.exec(&format!(
        "INSERT INTO notif_notifications (id, recipient_ref, category, route_category, lane, dedupe_key, source_producer, source_event_id, template_key, params, policy_snapshot, state)
         VALUES ('{n}', 'cfmeet-user:00000000-0000-4000-8000-000000000002', 'MISSED_CALLS', 'MISSED_CALLS', '{lane}', 'k-{n}', 'cfmeet.calling', 'e-{n}', 't', '{{}}', '{{}}', 'ROUTED')"
    ))
    .await
    .unwrap();
    h.exec(&format!(
        "INSERT INTO notif_deliveries (id, notification_id, channel, lane, target_ref, status, attempts, last_error_class, routing_version, completed_at)
         VALUES ('{d}', '{n}', '{channel}', '{lane}', 'target-{d}', '{status}', 5, 'PROVIDER_UNAVAILABLE', 1, now())"
    ))
    .await
    .unwrap();
    d.to_string()
}

async fn delivery_status(h: &Harness, d: &str) -> String {
    h.text(&format!(
        "SELECT status FROM notif_deliveries WHERE id = '{d}'"
    ))
    .await
    .unwrap()
}

fn approval() -> [(&'static str, &'static str); 3] {
    [
        ("x-cf-approver", "hq:staff-0002"),
        ("x-cf-approval-ref", "apr-0001"),
        ("x-cf-intent-digest", DIGEST),
    ]
}

fn headers(key: &str, approved: bool) -> Vec<(&str, &str)> {
    let mut v = vec![OPERATOR, ("idempotency-key", key)];
    if approved {
        v.extend(approval());
    }
    v
}

async fn operation(h: &Harness, key: &str) -> Value {
    let bridge = h.peers.bridge.clone();
    h.signed(
        &bridge,
        Method::GET,
        &operation_path(key),
        None,
        &[OPERATOR],
    )
    .await
    .expect(200)
    .clone()
}

#[tokio::test]
async fn the_bridge_retries_a_failed_push_delivery_once_per_key() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bridge = h.peers.bridge.clone();
    let d = delivery(&h, "PUSH", "STANDARD", "FAILED").await;

    // The Idempotency-Key is required and must be a UUID.
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&d),
        Some(json!({})),
        &[OPERATOR],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&d),
        Some(json!({})),
        &[OPERATOR, ("idempotency-key", "retry-0001")],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    // Unknown body fields are refused.
    let k = key();
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&d),
        Some(json!({"expectedVersion": 1})),
        &headers(&k, false),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    assert_eq!(delivery_status(&h, &d).await, "FAILED");

    let r = h
        .signed(
            &bridge,
            Method::POST,
            &retry_path(&d),
            Some(json!({})),
            &headers(&k, false),
        )
        .await;
    let v = r.expect(200).clone();
    assert_eq!(v["status"], "RETRIED");
    assert_eq!(v["version"], Value::Null);
    assert_eq!(v["resource"]["deliveryId"], json!(d));
    assert_eq!(v["resource"]["deliveryStatus"], "PENDING");
    assert_eq!(v["resource"]["attempts"], 0);
    assert_eq!(delivery_status(&h, &d).await, "PENDING");
    assert!(!v.to_string().contains("target-"), "no push target");

    // A PENDING delivery is not retried again under a new key.
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&d),
        None,
        &headers(&key(), false),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");

    // The same key again answers without applying again.
    h.exec(&format!(
        "UPDATE notif_deliveries SET status = 'FAILED' WHERE id = '{d}'"
    ))
    .await
    .unwrap();
    let again = h
        .signed(
            &bridge,
            Method::POST,
            &retry_path(&d),
            Some(json!({})),
            &headers(&k, false),
        )
        .await;
    assert_eq!(again.expect(200)["status"], "RETRIED");
    assert_eq!(
        delivery_status(&h, &d).await,
        "FAILED",
        "never applied twice"
    );
    // The same key for another subject is a conflict.
    let other = delivery(&h, "PUSH", "STANDARD", "FAILED").await;
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&other),
        Some(json!({})),
        &headers(&k, false),
    )
    .await
    .expect_error(409, "IDEMPOTENCY_CONFLICT");
    assert_eq!(delivery_status(&h, &other).await, "FAILED");

    // The operation record answers what the command answered.
    let op = operation(&h, &k).await;
    assert_eq!(
        op,
        json!({"idempotency_key": k, "outcome": "APPLIED", "source_status": "RETRIED", "source_version": null})
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_admin_operations WHERE idempotency_key = '{k}' AND command = 'delivery.retry'
               AND subject_kind = 'DELIVERY' AND subject_id = '{d}' AND operator_ref = 'hq:staff-0001' AND approver_ref IS NULL"
        ))
        .await,
        1
    );
    // The ledger is append-only.
    assert!(h.exec("DELETE FROM notif_admin_operations").await.is_err());
}

#[tokio::test]
async fn deliveries_that_cannot_be_retried_are_refused_and_unchanged() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bridge = h.peers.bridge.clone();
    let critical = delivery(&h, "PUSH", "CRITICAL", "FAILED").await;
    let email = delivery(&h, "EMAIL", "STANDARD", "FAILED").await;
    for d in [&critical, &email] {
        h.signed(
            &bridge,
            Method::POST,
            &retry_path(d),
            Some(json!({})),
            &headers(&key(), false),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
        assert_eq!(delivery_status(&h, d).await, "FAILED");
    }
    h.signed(
        &bridge,
        Method::POST,
        &retry_path(&Uuid::new_v4().to_string()),
        Some(json!({})),
        &headers(&key(), false),
    )
    .await
    .expect_error(404, "NOT_FOUND");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_admin_operations")
            .await,
        0
    );
}

#[tokio::test]
async fn commands_are_the_bridge_s_alone() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let d = delivery(&h, "PUSH", "STANDARD", "FAILED").await;
    let k = key();
    let paths = [
        retry_path(&d),
        remove_path(DIGEST),
        routing_path("CONTACT_REQUESTS", "IN_APP"),
        reroute_path(U),
    ];
    let op = h.peers.operator.clone();
    let bridge = h.peers.bridge.clone();
    for path in &paths {
        // The operator console (replay / reroute holder) has no notifications.administer.
        h.signed(
            &op,
            Method::POST,
            path,
            Some(json!({})),
            &[("idempotency-key", &k)],
        )
        .await
        .expect_error(403, "FORBIDDEN");
        // Nor may it assert an HQ operator.
        h.signed(&op, Method::POST, path, Some(json!({})), &headers(&k, true))
            .await
            .expect_error(403, "FORBIDDEN");
        // The bridge without X-CF-Operator.
        h.signed(
            &bridge,
            Method::POST,
            path,
            Some(json!({})),
            &[("idempotency-key", &k)],
        )
        .await
        .expect_error(403, "FORBIDDEN");
    }
    assert_eq!(delivery_status(&h, &d).await, "FAILED");
    // The operation record is 404 for anyone but the bridge, and for an unknown key.
    h.signed(&op, Method::GET, &operation_path(&k), None, &[])
        .await
        .expect_error(404, "NOT_FOUND");
    h.signed(&bridge, Method::GET, &operation_path(&k), None, &[])
        .await
        .expect_error(404, "NOT_FOUND");
    h.signed(&bridge, Method::GET, &operation_path(&k), None, &[OPERATOR])
        .await
        .expect_error(404, "NOT_FOUND");
    h.signed(
        &bridge,
        Method::GET,
        &operation_path("not-a-key"),
        None,
        &[OPERATOR],
    )
    .await
    .expect_error(404, "NOT_FOUND");
}

#[tokio::test]
async fn the_bridge_removes_a_suppression_at_its_version() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bridge = h.peers.bridge.clone();
    let digest = h.state.keys.email_digest(BOB_EMAIL);
    h.exec(&format!(
        "INSERT INTO notif_email_suppressions (recipient_digest, reason) VALUES ('{digest}', 'COMPLAINED')"
    ))
    .await
    .unwrap();
    let read = h
        .signed(
            &bridge,
            Method::GET,
            &format!("/internal/v1/operator/email-suppressions?digest={digest}"),
            None,
            &[OPERATOR],
        )
        .await;
    assert_eq!(read.expect(200)["items"][0]["version"], 1);
    // Any update bumps the version (a provider event between HQ's read and its removal).
    h.exec(&format!(
        "UPDATE notif_email_suppressions SET updated_at = now() WHERE recipient_digest = '{digest}'"
    ))
    .await
    .unwrap();
    let k = key();
    h.signed(
        &bridge,
        Method::POST,
        &remove_path(&digest),
        Some(json!({"expectedVersion": 1})),
        &headers(&k, false),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");
    for bad in [
        json!({}),
        json!({"expectedVersion": 0}),
        json!({"expectedVersion": 2, "reason": "x"}),
    ] {
        h.signed(
            &bridge,
            Method::POST,
            &remove_path(&digest),
            Some(bad),
            &headers(&k, false),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    }
    h.signed(
        &bridge,
        Method::POST,
        &remove_path(&digest),
        Some(json!({"expectedVersion": 2})),
        &[OPERATOR],
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    h.signed(
        &bridge,
        Method::POST,
        &remove_path("xyz"),
        Some(json!({"expectedVersion": 2})),
        &headers(&k, false),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_suppressions")
            .await,
        1
    );

    let r = h
        .signed(
            &bridge,
            Method::POST,
            &remove_path(&digest),
            Some(json!({"expectedVersion": 2})),
            &headers(&k, false),
        )
        .await;
    let v = r.expect(200).clone();
    assert_eq!(
        v,
        json!({"status": "REMOVED", "version": null, "resource": {"digest": digest, "suppressed": false}})
    );
    assert!(!v.to_string().to_lowercase().contains("bob@"));
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_email_suppressions")
            .await,
        0
    );
    // Replay answers again; an unknown digest under a new key is 404.
    let again = h
        .signed(
            &bridge,
            Method::POST,
            &remove_path(&digest),
            Some(json!({"expectedVersion": 2})),
            &headers(&k, false),
        )
        .await;
    assert_eq!(again.expect(200)["status"], "REMOVED");
    h.signed(
        &bridge,
        Method::POST,
        &remove_path(DIGEST),
        Some(json!({"expectedVersion": 1})),
        &headers(&key(), false),
    )
    .await
    .expect_error(404, "NOT_FOUND");
    assert_eq!(operation(&h, &k).await["source_status"], "REMOVED");
}

#[tokio::test]
async fn the_bridge_changes_one_routing_row_with_a_second_person() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let bridge = h.peers.bridge.clone();
    let path = routing_path("CONTACT_REQUESTS", "IN_APP");
    // The routing read (I8) gives the version.
    let routing = h
        .signed(
            &bridge,
            Method::GET,
            "/internal/v1/admin/routing",
            None,
            &[OPERATOR],
        )
        .await;
    let rows = routing.expect(200)["routing"].as_array().unwrap().clone();
    let row = rows
        .iter()
        .find(|r| r["category"] == "CONTACT_REQUESTS" && r["channel"] == "IN_APP")
        .unwrap();
    assert_eq!(row["version"], 1);

    let k = key();
    let body = json!({"mode": "SHADOW", "expectedVersion": 1});
    // R3: without the approval nothing is done.
    h.signed(
        &bridge,
        Method::POST,
        &path,
        Some(body.clone()),
        &headers(&k, false),
    )
    .await
    .expect_error(403, "FORBIDDEN");
    h.signed(
        &bridge,
        Method::POST,
        &path,
        Some(json!({"mode": "SHADOW", "expectedVersion": 7})),
        &headers(&k, true),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");
    for bad in [
        json!({"mode": "ON", "expectedVersion": 1}),
        json!({"mode": "SHADOW"}),
        json!({"mode": "SHADOW", "expectedVersion": 1, "canaryPercent": 5}),
    ] {
        h.signed(&bridge, Method::POST, &path, Some(bad), &headers(&k, true))
            .await
            .expect_error(400, "VALIDATION_ERROR");
    }
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_routing WHERE mode <> 'OFF' OR version <> 1")
            .await,
        0
    );

    let r = h
        .signed(
            &bridge,
            Method::POST,
            &path,
            Some(body.clone()),
            &headers(&k, true),
        )
        .await;
    let v = r.expect(200).clone();
    assert_eq!(v["status"], "SHADOW");
    assert_eq!(v["version"], "2");
    assert_eq!(v["resource"]["mode"], "SHADOW");
    assert_eq!(v["resource"]["version"], 2);
    assert_eq!(
        h.text("SELECT changed_by_run FROM notif_routing WHERE category = 'CONTACT_REQUESTS' AND channel = 'IN_APP'")
            .await
            .unwrap(),
        format!("admin-bridge:{k}")
    );
    // Replay answers the recorded change without a second bump.
    let again = h
        .signed(&bridge, Method::POST, &path, Some(body), &headers(&k, true))
        .await;
    assert_eq!(again.expect(200)["version"], "2");
    assert_eq!(
        h.count("SELECT version::bigint FROM notif_routing WHERE category = 'CONTACT_REQUESTS' AND channel = 'IN_APP'").await,
        2
    );
    // The row already in that mode.
    h.signed(
        &bridge,
        Method::POST,
        &path,
        Some(json!({"mode": "SHADOW", "expectedVersion": 2})),
        &headers(&key(), true),
    )
    .await
    .expect_error(400, "VALIDATION_ERROR");
    // The release gates: CRITICAL and MEETING_NOTIFICATION EMAIL never become ACTIVE here.
    for (category, channel) in [
        ("AUTH_VERIFICATION", "EMAIL"),
        ("MEETING_NOTIFICATION", "EMAIL"),
    ] {
        h.signed(
            &bridge,
            Method::POST,
            &routing_path(category, channel),
            Some(json!({"mode": "ACTIVE", "expectedVersion": 1})),
            &headers(&key(), true),
        )
        .await
        .expect_error(400, "VALIDATION_ERROR");
    }
    // Unknown rows and channels.
    for p in [
        routing_path("CONTACT_REQUESTS", "EMAIL"),
        routing_path("CONTACT_REQUESTS", "SMS"),
    ] {
        h.signed(
            &bridge,
            Method::POST,
            &p,
            Some(json!({"mode": "OFF", "expectedVersion": 1})),
            &headers(&key(), true),
        )
        .await
        .expect_error(404, "NOT_FOUND");
    }
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_routing WHERE mode <> 'OFF'")
            .await,
        1
    );

    let op = operation(&h, &k).await;
    assert_eq!(op["source_status"], "SHADOW");
    assert_eq!(op["source_version"], "2");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*)::bigint FROM notif_admin_operations WHERE idempotency_key = '{k}' AND subject_kind = 'ROUTING'
               AND subject_id = 'CONTACT_REQUESTS/IN_APP' AND approver_ref = 'hq:staff-0002' AND approval_ref = 'apr-0001'"
        ))
        .await,
        1
    );
    // The ACTIVE move of a gated-in row is served (both providers configured in the harness).
    let active = h
        .signed(
            &bridge,
            Method::POST,
            &routing_path("DEVELOPER_PLATFORM", "IN_APP"),
            Some(json!({"mode": "ACTIVE", "expectedVersion": 1})),
            &headers(&key(), true),
        )
        .await;
    assert_eq!(active.expect(200)["status"], "ACTIVE");
}

#[tokio::test]
async fn the_bridge_cancels_a_never_dispatched_decision_for_reroute() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.set_category("DEVELOPER_PLATFORM", "ACTIVE").await;
    let dev = h.peers.developer.clone();
    let data = [
        ("applicationId", json!(U)),
        ("credentialId", json!(U)),
        ("expiresAt", json!("2026-11-20T09:00:00Z")),
    ];
    let a = envelope(
        Producer::DeveloperPlatform,
        "developer-platform.credential.expired",
        &[bob()],
        &data,
        None,
    );
    let b = envelope(
        Producer::DeveloperPlatform,
        "developer-platform.credential.expiring-soon",
        &[bob()],
        &data,
        None,
    );
    let da = a["route"]["decisionId"].as_str().unwrap().to_string();
    let db_ = b["route"]["decisionId"].as_str().unwrap().to_string();
    h.ingest(&dev, vec![a]).await.expect(200);
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    h.ingest(&dev, vec![b]).await.expect(200);
    let bridge = h.peers.bridge.clone();

    let k = key();
    h.signed(
        &bridge,
        Method::POST,
        &reroute_path(&db_),
        None,
        &headers(&k, false),
    )
    .await
    .expect_error(403, "FORBIDDEN");
    let r = h
        .signed(
            &bridge,
            Method::POST,
            &reroute_path(&db_),
            None,
            &headers(&k, true),
        )
        .await;
    let v = r.expect(200).clone();
    assert_eq!(v["status"], "CANCELLED");
    assert_eq!(v["version"], Value::Null);
    assert_eq!(v["resource"]["decisionId"], json!(db_));
    assert_eq!(v["resource"]["eventState"], "REJECTED");
    assert_eq!(v["resource"]["rejectReason"], "CANCELLED_FOR_REROUTE");
    assert!(!v.to_string().contains(BOB_EMAIL));
    // Replay; the in-flight decision is refused; an unknown one is 404.
    let again = h
        .signed(
            &bridge,
            Method::POST,
            &reroute_path(&db_),
            Some(json!({})),
            &headers(&k, true),
        )
        .await;
    assert_eq!(again.expect(200)["status"], "CANCELLED");
    h.signed(
        &bridge,
        Method::POST,
        &reroute_path(&da),
        None,
        &headers(&key(), true),
    )
    .await
    .expect_error(409, "VERSION_CONFLICT");
    h.signed(
        &bridge,
        Method::POST,
        &reroute_path(&Uuid::new_v4().to_string()),
        None,
        &headers(&key(), true),
    )
    .await
    .expect_error(404, "NOT_FOUND");
    h.process_all().await;
    h.email_worker("STANDARD").tick(Utc::now()).await.unwrap();
    assert_eq!(
        h.provider.sent().len(),
        1,
        "the cancelled decision is never sent"
    );
    let op = operation(&h, &k).await;
    assert_eq!(op["source_status"], "CANCELLED");
    assert_eq!(op["source_version"], Value::Null);
}
