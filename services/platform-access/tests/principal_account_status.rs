//! Blueprint v1.0 Section 14 (ACC-010) — `POST /internal/v1/principals/account-status`:
//! the first-party account authority (the Meeting Service) mirrors an account's current
//! status onto its NATIVE_CF_USER principal. Same caller and scope rule as `ensure`; it
//! never creates a principal and never revives a tombstoned one.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

async fn mirror(h: &Harness, c: &Consumer, reference: &str, status: &str) -> Reply {
    h.call(
        &h.peers.meeting,
        Method::POST,
        "/internal/v1/principals/account-status",
        Some(json!({ "externalReference": reference, "status": status })),
        Some(c.delegated("TEST")),
        None,
    )
    .await
}

#[tokio::test]
async fn account_status_is_mirrored_idempotently_and_reversibly() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let cfmeet = h
        .consumer("cfmeet", "FIRST_PARTY", &["messages.send"])
        .await;
    let reference = "cfmeet-user:6d1f0a";
    // Nothing to mirror before the account ever used communications.
    let missing = mirror(&h, &cfmeet, reference, "SUSPENDED").await;
    assert_eq!(missing.code(), "NOT_FOUND", "{}", missing.body);
    assert_eq!(h.scalar_principals(reference).await, 0, "never created");
    let ensured = h
        .call(
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/principals/ensure",
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": reference })),
            Some(cfmeet.delegated("TEST")),
            None,
        )
        .await;
    assert_eq!(ensured.status, StatusCode::OK, "{}", ensured.body);
    let id = ensured.body["principal"]["id"].clone();

    let suspended = mirror(&h, &cfmeet, reference, "SUSPENDED").await;
    assert_eq!(suspended.status, StatusCode::OK, "{}", suspended.body);
    assert_eq!(suspended.body["changed"], true);
    assert_eq!(suspended.body["principal"]["id"], id);
    assert_eq!(suspended.body["principal"]["status"], "SUSPENDED");
    let again = mirror(&h, &cfmeet, reference, "SUSPENDED").await;
    assert_eq!(again.body["changed"], false);
    // A suspended principal cannot be ensured; mirroring reactivation restores it.
    let refused = h
        .call(
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/principals/ensure",
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": reference })),
            Some(cfmeet.delegated("TEST")),
            None,
        )
        .await;
    assert_eq!(refused.code(), "INVALID_STATE", "{}", refused.body);
    for status in ["DEACTIVATED", "ACTIVE"] {
        let r = mirror(&h, &cfmeet, reference, status).await;
        assert_eq!(r.body["principal"]["status"], status, "{}", r.body);
    }

    // Unknown statuses and non-account references are refused.
    for (r, s) in [
        (reference, "TOMBSTONED"),
        (reference, "active"),
        ("tel:+15550100", "ACTIVE"),
    ] {
        let reply = mirror(&h, &cfmeet, r, s).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{r} {s}: {}",
            reply.body
        );
    }
    // Only the account authority, only for a FIRST_PARTY scope.
    let external = h.consumer("acme", "EXTERNAL", &["messages.send"]).await;
    let r = mirror(&h, &external, reference, "SUSPENDED").await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);
    for signer in [&h.peers.messaging, &h.peers.calling, &h.peers.untrusted] {
        let r = h
            .call(
                signer,
                Method::POST,
                "/internal/v1/principals/account-status",
                Some(json!({ "externalReference": reference, "status": "SUSPENDED" })),
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
    // A tombstoned principal stays DEACTIVATED for ever.
    let id = id.as_str().unwrap();
    let r = h
        .call(
            &h.peers.meeting,
            Method::POST,
            &format!("/internal/v1/principals/{id}/tombstone"),
            Some(json!({ "operator": "retention-officer" })),
            None,
            None,
        )
        .await;
    if r.status == StatusCode::OK {
        let revived = mirror(&h, &cfmeet, reference, "ACTIVE").await;
        assert!(
            matches!(revived.code(), "INVALID_STATE" | "NOT_FOUND"),
            "{}",
            revived.body
        );
    }
}
