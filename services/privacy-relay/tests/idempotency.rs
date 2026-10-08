//! §29: command idempotency for every mutation.

mod common;

use axum::http::StatusCode;
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::{json, Value};

/// (operation, path, request, different request), applied to a fresh context each.
async fn operations(
    h: &Harness,
    c: &Consumer,
) -> Vec<(&'static str, String, Value, Value, &'static str)> {
    let [a, b, cc] = principals(3).try_into().unwrap();
    let mut out = Vec::new();
    let create = context_request(&[a, b], "BIDIRECTIONAL", &[]);
    let mut other = create.clone();
    other["externalContextReference"] = json!("another-reference");
    out.push((
        "create context",
        "/internal/v1/contexts".to_string(),
        create,
        other,
        "communication_contexts",
    ));
    for (name, action, before) in [
        ("update context", "update", None),
        ("suspend", "suspend", None),
        ("resume", "resume", Some("suspend")),
        ("complete", "complete", None),
        ("revoke", "revoke", None),
        ("create session", "sessions", None),
    ] {
        let ctx = h.bidirectional(c, &[a, b]).await;
        let id = ctx["contextId"].as_str().unwrap().to_string();
        let mut version = 1;
        if let Some(pre) = before {
            h.lifecycle(c, &id, pre, 1).await;
            version = 2;
        }
        let (request, different) = match action {
            "update" => (
                json!({ "expectedVersion": version, "participants": [{ "principalId": a }, { "principalId": cc }] }),
                json!({ "expectedVersion": version, "participants": [{ "principalId": a }, { "principalId": b }, { "principalId": cc }] }),
            ),
            "sessions" => (
                json!({ "relayStrategy": "APP_ONLY" }),
                json!({ "relayStrategy": "APP_ONLY", "expiresAt": iso(at(30)) }),
            ),
            _ => (
                json!({ "expectedVersion": version }),
                json!({ "expectedVersion": version + 7 }),
            ),
        };
        let table = if action == "sessions" {
            "privacy_relay_sessions"
        } else {
            "relay_audit_events"
        };
        out.push((
            name,
            format!("/internal/v1/contexts/{id}/{action}"),
            request,
            different,
            table,
        ));
    }
    out
}

#[tokio::test]
async fn same_key_same_request_replays_and_different_request_conflicts() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    for (name, path, request, different, _) in operations(&h, &c).await {
        let key = format!("key-{}", uuid::Uuid::new_v4());
        let first = h.keyed(&path, &c, &key, request.clone()).await;
        assert!(first.status.is_success(), "{name}: {}", first.text);
        let again = h.keyed(&path, &c, &key, request.clone()).await;
        assert_eq!(again.status, first.status, "{name}");
        assert_eq!(again.json, first.json, "{name}: identical replay");
        assert_eq!(again.headers["idempotent-replayed"], "true", "{name}");
        let conflict = h.keyed(&path, &c, &key, different).await;
        assert_eq!(
            (conflict.status, conflict.code()),
            (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT"),
            "{name}"
        );
    }
}

#[tokio::test]
async fn concurrent_duplicates_have_one_domain_effect() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    for round in 0..3 {
        for (name, path, request, _, table) in operations(&h, &c).await {
            let before = h.count(&format!("SELECT count(*) FROM {table}")).await;
            let key = format!("race-{round}-{}", uuid::Uuid::new_v4());
            let responses = futures::future::join_all(
                (0..6).map(|_| h.keyed(&path, &c, &key, request.clone())),
            )
            .await;
            // The enforcement status is live state (a duplicate may observe PENDING while the
            // first execution is still delivering); everything else is identical.
            let bodies: Vec<Value> = responses
                .iter()
                .map(|r| {
                    let mut v = r.json.clone();
                    if let Some(e) = v.get_mut("enforcement") {
                        e["status"] = Value::Null;
                    }
                    v
                })
                .collect();
            assert!(
                responses.iter().all(|r| r.status.is_success()),
                "{name}: {:?}",
                responses.iter().map(|r| &r.text).collect::<Vec<_>>()
            );
            assert!(
                bodies.windows(2).all(|w| w[0] == w[1]),
                "{name}: every duplicate sees the same result"
            );
            assert_eq!(
                responses
                    .iter()
                    .filter(|r| !r.headers.contains_key("idempotent-replayed"))
                    .count(),
                1,
                "{name}"
            );
            let after = h.count(&format!("SELECT count(*) FROM {table}")).await;
            let expected_delta = match name {
                "create context" | "create session" => 1,
                // One authority event (+ ENFORCEMENT_REQUESTED / session cascade events).
                _ => after - before,
            };
            assert_eq!(after - before, expected_delta, "{name}");
            assert!(after > before, "{name}: one effect happened");
            assert_eq!(
                h.count(&format!(
                    "SELECT count(*) FROM privacy_relay_commands WHERE idempotency_key = '{key}'"
                ))
                .await,
                1,
                "{name}"
            );
        }
    }
    // One CONTEXT_REVOKED / CONTEXT_SUSPENDED / CONTEXT_COMPLETED per raced command.
    for event in [
        "CONTEXT_REVOKED",
        "CONTEXT_SUSPENDED",
        "CONTEXT_COMPLETED",
        "CONTEXT_RESUMED",
        "CONTEXT_PARTICIPANTS_CHANGED",
    ] {
        assert_eq!(
            h.count(&format!(
                "SELECT count(*) FROM relay_audit_events WHERE event_type = '{event}'"
            ))
            .await,
            if event == "CONTEXT_SUSPENDED" { 6 } else { 3 },
            "{event}"
        );
    }
}

#[tokio::test]
async fn keys_are_namespaced_by_scope_and_operation() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c1, c2) = (
        Consumer::new(Kind::Production),
        Consumer::new(Kind::Production),
    );
    let [a, b] = principals(2).try_into().unwrap();
    let key = "shared-key";
    let r1 = h
        .keyed(
            "/internal/v1/contexts",
            &c1,
            key,
            context_request(&[a, b], "BIDIRECTIONAL", &[]),
        )
        .await;
    let r2 = h
        .keyed(
            "/internal/v1/contexts",
            &c2,
            key,
            context_request(&[a, b], "BIDIRECTIONAL", &[]),
        )
        .await;
    assert_eq!(
        (r1.status, r2.status),
        (StatusCode::CREATED, StatusCode::CREATED)
    );
    assert_ne!(
        r1.json["context"]["contextId"],
        r2.json["context"]["contextId"]
    );
    let id = r1.json["context"]["contextId"].as_str().unwrap();
    let s = h
        .keyed(
            &format!("/internal/v1/contexts/{id}/suspend"),
            &c1,
            key,
            json!({ "expectedVersion": 1 }),
        )
        .await;
    assert_eq!(
        s.status,
        StatusCode::OK,
        "same key, other operation: {}",
        s.text
    );
    let missing = h
        .post(
            "/internal/v1/contexts",
            &c1,
            json!({ "request": context_request(&[a, b], "BIDIRECTIONAL", &[]) }),
        )
        .await;
    assert_eq!(
        missing.code(),
        "VALIDATION_ERROR",
        "the idempotency key is required"
    );
    let bad = h
        .keyed(
            "/internal/v1/contexts",
            &c1,
            "has space",
            context_request(&[a, b], "BIDIRECTIONAL", &[]),
        )
        .await;
    assert_eq!(bad.code(), "VALIDATION_ERROR");
}
