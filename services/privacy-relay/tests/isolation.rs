//! Tenant / application / environment isolation: a foreign id is indistinguishable from
//! a nonexistent one, and scope comes only from the signed delegated context.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::ApplicationEnvironmentKind as Kind;
use common::*;
use serde_json::json;

#[tokio::test]
async fn foreign_contexts_and_sessions_look_nonexistent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let owner = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&owner, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();
    let sid = h.app_session(&owner, &id).await["relaySessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let missing = uuid::Uuid::new_v4().to_string();
    let other_tenant = Consumer::new(Kind::Production);
    let other_app = Consumer {
        application_id: uuid::Uuid::new_v4(),
        ..owner.clone()
    };
    let other_env = owner.sibling_environment();
    for (who, foreign) in [
        ("tenant", &other_tenant),
        ("application", &other_app),
        ("environment", &other_env),
    ] {
        for (method, template, body) in [
            (Method::GET, "/internal/v1/contexts/{c}", None),
            (Method::GET, "/internal/v1/contexts/{c}/evidence", None),
            (
                Method::POST,
                "/internal/v1/contexts/{c}/update",
                Some(
                    json!({ "idempotencyKey": "k1", "request": { "expectedVersion": 1, "expiresAt": iso(at(30)) } }),
                ),
            ),
            (
                Method::POST,
                "/internal/v1/contexts/{c}/revoke",
                Some(json!({ "idempotencyKey": "k2", "request": { "expectedVersion": 1 } })),
            ),
            (
                Method::POST,
                "/internal/v1/contexts/{c}/complete",
                Some(json!({ "idempotencyKey": "k3", "request": { "expectedVersion": 1 } })),
            ),
            (
                Method::POST,
                "/internal/v1/contexts/{c}/suspend",
                Some(json!({ "idempotencyKey": "k4", "request": { "expectedVersion": 1 } })),
            ),
            (
                Method::POST,
                "/internal/v1/contexts/{c}/sessions",
                Some(json!({ "idempotencyKey": "k5", "request": { "relayStrategy": "APP_ONLY" } })),
            ),
            (Method::GET, "/internal/v1/sessions/{s}", None),
            (
                Method::POST,
                "/internal/v1/sessions/{s}/revoke",
                Some(json!({ "idempotencyKey": "k6", "request": { "expectedVersion": 1 } })),
            ),
        ] {
            let real = template.replace("{c}", &id).replace("{s}", &sid);
            let fake = template.replace("{c}", &missing).replace("{s}", &missing);
            let foreign_response = h.call(method.clone(), &real, foreign, body.clone()).await;
            let missing_response = h.call(method.clone(), &fake, foreign, body.clone()).await;
            assert_eq!(
                foreign_response.status,
                StatusCode::NOT_FOUND,
                "{who} {template}"
            );
            assert_eq!(
                foreign_response.normalized_error(),
                missing_response.normalized_error(),
                "{who} {template}"
            );
        }
        // Authorization against a foreign session is the same 404.
        let r = h.authorize(foreign, &sid, a, &[b]).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::NOT_FOUND, "SESSION_NOT_FOUND"),
            "{who}"
        );
        // Lists never include foreign rows.
        let list = h
            .post("/internal/v1/contexts/query", foreign, json!({}))
            .await;
        assert_eq!(list.json["items"], json!([]), "{who}");
        let list = h
            .post("/internal/v1/sessions/query", foreign, json!({}))
            .await;
        assert_eq!(list.json["items"], json!([]), "{who}");
    }
    // The owner's state is untouched.
    let mine = h.get(&format!("/internal/v1/contexts/{id}"), &owner).await;
    assert_eq!(
        (mine.json["version"].as_i64(), mine.json["status"].as_str()),
        (Some(1), Some("ACTIVE"))
    );
    assert_eq!(h.reason(&owner, &sid, a, &[b]).await, "AUTHORIZED");
}

#[tokio::test]
async fn lists_are_keyset_paginated_and_filtered() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let mut ids = Vec::new();
    for i in 0..5 {
        h.clock.set(at(i));
        ids.push(
            h.bidirectional(&c, &[a, b]).await["contextId"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    }
    let first = h
        .post("/internal/v1/contexts/query", &c, json!({ "limit": 2 }))
        .await;
    assert_eq!(first.json["items"].as_array().unwrap().len(), 2);
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = h
        .post(
            "/internal/v1/contexts/query",
            &c,
            json!({ "limit": 2, "cursor": cursor }),
        )
        .await;
    let third = h
        .post(
            "/internal/v1/contexts/query",
            &c,
            json!({ "limit": 2, "cursor": second.json["nextCursor"] }),
        )
        .await;
    assert!(third.json["nextCursor"].is_null());
    let seen: Vec<String> = [&first, &second, &third]
        .iter()
        .flat_map(|r| {
            r.json["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["contextId"].as_str().unwrap().to_string())
        })
        .collect();
    let mut expected = ids.clone();
    expected.reverse();
    assert_eq!(seen, expected, "newest first, no gaps, no duplicates");
    h.lifecycle(&c, &ids[0], "revoke", 1).await;
    let revoked = h
        .post(
            "/internal/v1/contexts/query",
            &c,
            json!({ "status": "REVOKED" }),
        )
        .await;
    assert_eq!(revoked.json["items"].as_array().unwrap().len(), 1);
    for bad in [
        json!({ "limit": 0 }),
        json!({ "limit": 1000 }),
        json!({ "cursor": "zz" }),
        json!({ "offset": 10 }),
        json!({ "tenantId": c.tenant_id }),
    ] {
        assert_eq!(
            h.post("/internal/v1/contexts/query", &c, bad.clone())
                .await
                .code(),
            "VALIDATION_ERROR",
            "{bad}"
        );
    }
}
