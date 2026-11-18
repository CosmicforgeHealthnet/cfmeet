//! N2: schema, readiness, service authentication and dark-mode safety.

mod common;

use axum::http::Method;
use common::*;
use serde_json::json;

#[tokio::test]
async fn the_migration_creates_18_prefixed_tables_with_every_route_off() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let tables = h
        .count("SELECT count(*)::bigint FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE 'notif\\_%'")
        .await;
    // The 17 service tables (N2), the N10 preference-copy job row and (ADR 0063, additive)
    // the partner push provider vault.
    assert_eq!(tables, 19);
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_preference_copy WHERE mode = 'OFF'")
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM information_schema.tables WHERE table_schema = 'public' AND table_name NOT LIKE 'notif\\_%' AND table_name <> 'notifications_seaql_migrations'").await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_routing WHERE mode <> 'OFF'")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_routing").await,
        notifications::catalogue::routing_rows().len() as i64
    );
    // Append-only attempts.
    h.exec("INSERT INTO notif_notifications (id, recipient_ref, category, route_category, lane, dedupe_key, source_producer, source_event_id, template_key, params, policy_snapshot, state) VALUES ('00000000-0000-4000-8000-000000000001', 'cfmeet-user:00000000-0000-4000-8000-000000000002', 'X', 'X', 'STANDARD', 'k', 'cfmeet.organizations', 'e', 't', '{}', '{}', 'ROUTED')").await.unwrap();
    h.exec("INSERT INTO notif_deliveries (id, notification_id, channel, lane, target_ref, status, routing_version) VALUES ('00000000-0000-4000-8000-000000000003', '00000000-0000-4000-8000-000000000001', 'PUSH', 'STANDARD', 'x', 'PENDING', 1)").await.unwrap();
    h.exec("INSERT INTO notif_delivery_attempts (delivery_id, attempt, started_at, outcome) VALUES ('00000000-0000-4000-8000-000000000003', 1, now(), 'OK')").await.unwrap();
    assert!(h
        .exec("UPDATE notif_delivery_attempts SET outcome = 'RETRY'")
        .await
        .is_err());
    // Recipient refs are opaque references only.
    assert!(h.exec("INSERT INTO notif_notifications (id, recipient_ref, category, route_category, lane, dedupe_key, source_producer, source_event_id, template_key, params, policy_snapshot, state) VALUES ('00000000-0000-4000-8000-000000000009', 'alice@example.com', 'X', 'X', 'STANDARD', 'k', 'cfmeet.organizations', 'e', 't', '{}', '{}', 'ROUTED')").await.is_err());
}

#[tokio::test]
async fn the_service_refuses_to_start_with_pending_migrations() {
    let Some(db) = TestDb::empty().await else {
        return;
    };
    let err = notifications::connect(&db.url, 2)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("pending"), "{err}");
}

#[tokio::test]
async fn ready_reports_migrations_protocol_and_dark_providers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h.raw(Method::GET, "/ready", Default::default(), None).await;
    let v = r.expect(200);
    assert_eq!(v["migrations"], "current");
    assert_eq!(v["routingProtocol"], json!([1]));
    assert_eq!(v["routing"]["nonOffRows"], 0);
    let health = h
        .raw(Method::GET, "/health", Default::default(), None)
        .await;
    assert_eq!(health.json["service"], "notifications");
    let m = h
        .raw(Method::GET, "/metrics", Default::default(), None)
        .await;
    assert_eq!(m.status.as_u16(), 200);
}

#[tokio::test]
async fn internal_routes_require_a_trusted_single_use_token_and_the_permission() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ev = envelope(
        cf_notification_client::Producer::Organizations,
        "organizations.invitation.accepted",
        &[alice()],
        &[
            (
                "invitationId",
                json!("5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ),
            (
                "organizationId",
                json!("5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ),
        ],
        None,
    );
    // No token.
    h.raw(
        Method::POST,
        "/internal/v1/notification-events",
        Default::default(),
        Some(json!({"events": [ev.clone()]})),
    )
    .await
    .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // Unknown key claiming to be the Meeting Service.
    let rogue = h.peers.rogue_meeting.clone();
    h.ingest(&rogue, vec![ev.clone()])
        .await
        .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // Trusted caller without the permission.
    let op = h.peers.operator.clone();
    h.ingest(&op, vec![ev.clone()])
        .await
        .expect_error(403, "FORBIDDEN");
    // Replay of one token.
    let signer = h.peers.organizations.clone();
    let signed = signer
        .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
            cf_service_auth::ServiceId::notifications(),
        ))
        .unwrap();
    let mut headers = axum::http::HeaderMap::new();
    signed.apply_to(&mut headers);
    h.raw(
        Method::POST,
        "/internal/v1/notification-events",
        headers.clone(),
        Some(json!({"events": [ev.clone()]})),
    )
    .await
    .expect(200);
    h.raw(
        Method::POST,
        "/internal/v1/notification-events",
        headers,
        Some(json!({"events": [ev]})),
    )
    .await
    .expect_error(401, "INTERNAL_AUTHENTICATION_FAILED");
    // Admin routes: the producer may not read them.
    let org = h.peers.organizations.clone();
    h.signed(&org, Method::GET, "/internal/v1/admin/routing", None, &[])
        .await
        .expect_error(403, "FORBIDDEN");
    let op = h.peers.operator.clone();
    let r = h
        .signed(&op, Method::GET, "/internal/v1/admin/routing", None, &[])
        .await;
    assert_eq!(
        r.expect(200)["routing"].as_array().unwrap().len(),
        notifications::catalogue::routing_rows().len()
    );
}

#[tokio::test]
async fn dark_mode_holds_events_and_calls_no_provider() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let ev = envelope(
        cf_notification_client::Producer::Organizations,
        "organizations.membership.removed",
        &[alice()],
        &[
            (
                "membershipId",
                json!("5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ),
            (
                "organizationId",
                json!("5e2a1b3c-4d5e-4f60-8a71-b2c3d4e5f607"),
            ),
        ],
        None,
    );
    let org = h.peers.organizations.clone();
    let r = h.ingest(&org, vec![ev]).await;
    assert_eq!(r.expect(200)["results"][0]["outcome"], "HELD");
    h.process_all().await;
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_notifications")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*)::bigint FROM notif_event_inbox WHERE state = 'HELD'")
            .await,
        1
    );
    assert!(h.provider.sent().is_empty());
    assert!(h.push.sent.lock().unwrap().is_empty());
    let op = h.peers.operator.clone();
    let dl = h
        .signed(
            &op,
            Method::GET,
            "/internal/v1/admin/dead-letters?kind=HELD_EVENT",
            None,
            &[],
        )
        .await;
    assert_eq!(dl.expect(200)["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn running_workers_keep_ready_green_beyond_their_first_minute() {
    // Regression: an hourly "retention" heartbeat made /ready 503 sixty seconds after start
    // (production certify 43828926604). Every heartbeat key must belong to a loop that
    // beats at least every few seconds.
    let Some(h) = Harness::new().await else {
        return;
    };
    let mut state = h.state.clone();
    state.workers_enabled = true;
    let workers = notifications::workers::spawn(&state);
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let keys: Vec<&str> = state.heartbeats.snapshot().keys().copied().collect();
    workers.shutdown();
    assert!(
        keys.contains(&"ingest") && keys.contains(&"scheduler"),
        "{keys:?}"
    );
    for k in &keys {
        assert!(
            [
                "ingest",
                "scheduler",
                "email-critical",
                "email-standard",
                "push"
            ]
            .contains(k),
            "heartbeat {k} is not a fast loop"
        );
    }
}
