//! Final completion (Section 6): production-access requests are decided only by named
//! reviewers (`DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS`), manually, never by the
//! requesting organization's own members.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

async fn requested(c: &Control) -> (Uuid, Uuid) {
    let (owner, owner_id) = c.sessions.sign_in("owner@reviewers.example");
    let o = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{}/projects", id(&o.json)),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let project = id(&p.json);
    let e = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": ["MEETINGS"], "useCaseSummary": "Appointments" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(
        r.json["state"], "PRODUCTION_REQUESTED",
        "manual approval: nothing automatic"
    );
    (id(&r.json), owner_id)
}

#[tokio::test]
async fn an_operator_who_is_not_a_named_reviewer_cannot_decide() {
    let Some(c) =
        Control::with_state(|s| s.with_production_reviewers(vec!["github-alice".into()])).await
    else {
        return;
    };
    let (rid, _) = requested(&c).await;
    let base = format!("/internal/v1/operator/production-access-requests/{rid}");
    for command in ["review", "approve", "reject"] {
        let r = c
            .operator_as(
                Method::POST,
                &format!("{base}/{command}"),
                None,
                Some(Uuid::new_v4()),
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{command}: {}",
            r.json
        );
        assert_eq!(r.json["error"]["code"], "POLICY_VIOLATION");
    }
    // Reads stay available to any administering operator.
    let read = c.operator(Method::GET, &base, None).await;
    assert_eq!(read.status, StatusCode::OK);
    assert_eq!(read.json["state"], "PRODUCTION_REQUESTED");
}

#[tokio::test]
async fn a_named_reviewer_decides_but_never_for_their_own_organization() {
    let Some(c) =
        Control::with_state(|s| s.with_production_reviewers(vec!["ops.tester".into()])).await
    else {
        return;
    };
    let (rid, owner_id) = requested(&c).await;
    let base = format!("/internal/v1/operator/production-access-requests/{rid}");
    let selfie = c
        .operator_as(
            Method::POST,
            &format!("{base}/approve"),
            None,
            Some(owner_id),
        )
        .await;
    assert_eq!(selfie.json["error"]["code"], "SELF_APPROVAL_FORBIDDEN");
    let ok = c
        .operator_as(
            Method::POST,
            &format!("{base}/approve"),
            None,
            Some(Uuid::new_v4()),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    assert_eq!(ok.json["state"], "APPROVED");
}
