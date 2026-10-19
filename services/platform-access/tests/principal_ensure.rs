//! Part V H §104 — `POST /internal/v1/principals/ensure`: the first-party login authority
//! (the Meeting Service) maps an account it authenticated to its communication principal,
//! idempotently, inside its signed delegated scope. Refused for EXTERNAL tenants, other
//! principal categories, other callers, suspended scopes and inactive principals.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn ensure(h: &Harness, c: &Consumer, kind: &str, reference: &str) -> Reply {
    h.call(
        &h.peers.meeting,
        Method::POST,
        "/internal/v1/principals/ensure",
        Some(json!({ "kind": kind, "externalReference": reference })),
        Some(c.delegated("TEST")),
        None,
    )
    .await
}

#[tokio::test]
async fn first_party_backend_ensures_principals_idempotently() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let cfmeet = h
        .consumer("cfmeet", "FIRST_PARTY", &["messages.send"])
        .await;
    let a = ensure(&h, &cfmeet, "NATIVE_CF_USER", "cfmeet-user:4b7e1c").await;
    assert_eq!(a.status, StatusCode::OK, "{}", a.body);
    assert_eq!(a.body["created"], true);
    assert_eq!(a.body["principal"]["kind"], "NATIVE_CF_USER");
    assert_eq!(
        a.body["principal"]["environmentId"],
        json!(cfmeet.env("TEST"))
    );
    let again = ensure(&h, &cfmeet, "NATIVE_CF_USER", "cfmeet-user:4b7e1c").await;
    assert_eq!(again.body["created"], false);
    assert_eq!(again.body["principal"]["id"], a.body["principal"]["id"]);
    // Guests of a meeting are temporary external participants.
    let g = ensure(
        &h,
        &cfmeet,
        "TEMPORARY_EXTERNAL_PARTICIPANT",
        "meeting-participant:9f2a",
    )
    .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.body);
    // Another environment of the same application gets its own principal.
    let other_env = h
        .call(
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/principals/ensure",
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "cfmeet-user:4b7e1c" })),
            Some(cfmeet.delegated("SANDBOX")),
            None,
        )
        .await;
    assert_ne!(other_env.body["principal"]["id"], a.body["principal"]["id"]);

    // Other categories, malformed references (phone numbers) and no delegated scope.
    for (kind, reference) in [
        ("SERVICE_ACCOUNT", "x"),
        ("TELEPHONE_ENDPOINT", "8b1c5f2a-2b8a-4f43-8a52-9a0a2c2f3c11"),
        ("EXTERNAL_APPLICATION_PRINCIPAL", "user-1"),
        ("NATIVE_CF_USER", "tel:+15550100"),
        ("NATIVE_CF_USER", "has space"),
    ] {
        let r = ensure(&h, &cfmeet, kind, reference).await;
        assert!(
            matches!(
                r.status,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY
            ),
            "{kind} {reference}: {}",
            r.body
        );
        assert_eq!(
            h.scalar_principals(reference).await,
            0,
            "nothing created for {reference}"
        );
    }
    let r = h
        .call(
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/principals/ensure",
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "cfmeet-user:1" })),
            None,
            None,
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_REQUIRED", "{}", r.body);

    // An EXTERNAL tenant's scope is refused: only first-party login authorities map accounts.
    let external = h.consumer("acme", "EXTERNAL", &["messages.send"]).await;
    let r = ensure(&h, &external, "NATIVE_CF_USER", "cfmeet-user:4b7e1c").await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);

    // Callers without the grant (Messaging, Calling, untrusted) are refused.
    for signer in [&h.peers.messaging, &h.peers.calling, &h.peers.untrusted] {
        let r = h
            .call(
                signer,
                Method::POST,
                "/internal/v1/principals/ensure",
                Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "cfmeet-user:2" })),
                Some(cfmeet.delegated("TEST")),
                None,
            )
            .await;
        assert!(
            matches!(r.status, StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED),
            "{}",
            r.body
        );
    }

    // A suspended principal is never reactivated by ensure.
    let id = a.body["principal"]["id"].as_str().unwrap();
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/principals/{id}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = ensure(&h, &cfmeet, "NATIVE_CF_USER", "cfmeet-user:4b7e1c").await;
    assert_eq!(r.code(), "INVALID_STATE", "{}", r.body);
    // A suspended environment refuses new principals.
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/status",
            cfmeet.env("TEST")
        ),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = ensure(&h, &cfmeet, "NATIVE_CF_USER", "cfmeet-user:new").await;
    assert_eq!(r.code(), "INVALID_STATE", "{}", r.body);
}
