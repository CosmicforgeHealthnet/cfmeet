//! Scope allow / deny (Part IV §B4 / §B6.4): registry, application subset, environment
//! restrictions, first-party-only and privileged operator scopes (`numbers.admin`),
//! withdrawal cascading to credentials, authorized-principal restrictions and the
//! `authorization/evaluate` contract.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;

#[tokio::test]
async fn numbering_and_sms_scopes_are_grantable_and_effective() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let all = [
        "numbers.search",
        "numbers.read",
        "numbers.manage",
        "numbers.assign",
        "numbers.kyc.read",
        "numbers.kyc.manage",
        "numbers.portability",
        "sms.send",
        "sms.read",
    ];
    let c = h.consumer("acme", "EXTERNAL", &all).await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &all).await;
    let r = h.authenticate(&secret).await;
    let mut expected: Vec<&str> = all.to_vec();
    expected.sort();
    assert_eq!(r.body["authority"]["scopes"], json!(expected));
}

#[tokio::test]
async fn meeting_scopes_are_grantable_and_effective() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let all = ["meetings.read", "meetings.write"];
    let c = h.consumer("meet", "EXTERNAL", &all).await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &all).await;
    let r = h.authenticate(&secret).await;
    assert_eq!(r.body["authority"]["scopes"], json!(all));
    // Least privilege: a write-only credential holds exactly meetings.write.
    let (_, write_only) = h.issue(c.sa("SANDBOX"), &["meetings.write"]).await;
    let r = h.authenticate(&write_only).await;
    assert_eq!(r.body["authority"]["scopes"], json!(["meetings.write"]));
}

#[tokio::test]
async fn credentials_cannot_hold_scopes_the_application_does_not_allow() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["numbers.read"]).await;
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/service-accounts/{}/credentials",
                c.sa("SANDBOX")
            ),
            Some(json!({ "scopes": ["numbers.manage"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/service-accounts/{}/credentials",
                c.sa("SANDBOX")
            ),
            Some(json!({ "scopes": ["admin.everything"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.body);
}

#[tokio::test]
async fn environment_restricted_scopes_are_neither_granted_nor_honoured_elsewhere() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["numbers.read"]).await;
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/applications/{}/scopes",
            c.application_id
        ),
        Some(json!({ "scope": "numbers.manage", "environmentKinds": ["SANDBOX", "TEST"] })),
    )
    .await;
    // Issuance in PRODUCTION is refused.
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/service-accounts/{}/credentials",
                c.sa("PRODUCTION")
            ),
            Some(json!({ "scopes": ["numbers.manage"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Granted in SANDBOX; narrowing the restriction later withdraws it at once.
    let (_, secret) = h
        .issue(c.sa("SANDBOX"), &["numbers.manage", "numbers.read"])
        .await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["numbers.manage", "numbers.read"])
    );
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/applications/{}/scopes",
            c.application_id
        ),
        Some(json!({ "scope": "numbers.manage", "environmentKinds": ["TEST"] })),
    )
    .await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["numbers.read"])
    );
}

#[tokio::test]
async fn numbers_admin_is_privileged_operator_authority_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let first = h.consumer("cfops", "FIRST_PARTY", &[]).await;
    let external = h.consumer("acme", "EXTERNAL", &[]).await;
    // Never through the ordinary grant path.
    for c in [&first, &external] {
        let r = h
            .admin(
                Method::POST,
                &format!(
                    "/internal/v1/admin/applications/{}/scopes",
                    c.application_id
                ),
                Some(json!({ "scope": "numbers.admin" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    }
    // Never to an EXTERNAL tenant, even privileged.
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                external.application_id
            ),
            Some(json!({ "scope": "numbers.admin", "privileged": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // An explicit privileged grant to a FIRST_PARTY application works and is audited.
    let view = h
        .admin_ok(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                first.application_id
            ),
            Some(json!({ "scope": "numbers.admin", "privileged": true })),
        )
        .await;
    assert_eq!(view["grantBasis"], "PRIVILEGED_OPERATOR");
    let (_, secret) = h.issue(first.sa("PRODUCTION"), &["numbers.admin"]).await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["numbers.admin"])
    );
    let audited: i64 = sea_orm::ConnectionTrait::query_one(
        h.conn(),
        sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT count(*) FROM platform_access_audit WHERE action = 'PRIVILEGED_SCOPE_GRANTED'",
        ),
    )
    .await
    .unwrap()
    .unwrap()
    .try_get_by_index(0)
    .unwrap();
    assert_eq!(audited, 1);
    // The database itself refuses a STANDARD basis for numbers.admin.
    let err = sea_orm::ConnectionTrait::execute_unprepared(
        h.conn(),
        &format!(
            "UPDATE platform_application_scopes SET grant_basis = 'STANDARD' \
             WHERE application_id = '{}' AND scope = 'numbers.admin'",
            first.application_id
        ),
    )
    .await;
    assert!(err.is_err());
}

#[tokio::test]
async fn first_party_only_scopes_follow_classification() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let external = h.consumer("acme", "EXTERNAL", &[]).await;
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                external.application_id
            ),
            Some(json!({ "scope": "events.private_delivery" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let first = h
        .consumer("cf", "FIRST_PARTY", &["events.private_delivery"])
        .await;
    let (_, secret) = h
        .issue(first.sa("SANDBOX"), &["events.private_delivery"])
        .await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["events.private_delivery"])
    );
}

#[tokio::test]
async fn withdrawing_an_application_scope_withdraws_it_from_every_credential() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("acme", "EXTERNAL", &["calls.read", "sms.send"])
        .await;
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["calls.read", "sms.send"]).await;
    h.admin_ok(
        Method::DELETE,
        &format!(
            "/internal/v1/admin/applications/{}/scopes/sms.send",
            c.application_id
        ),
        None,
    )
    .await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["calls.read"])
    );
}

#[tokio::test]
async fn evaluate_rechecks_scopes_status_and_principal_restrictions_now() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h
        .consumer("acme", "EXTERNAL", &["numbers.read", "numbers.assign"])
        .await;
    let (view, secret) = h.issue(c.sa("SANDBOX"), &["numbers.read"]).await;
    let credential_id = uuid(&view["id"]);
    let eval = |scopes: serde_json::Value, principal: Option<uuid::Uuid>| {
        let mut body = json!({ "credentialId": credential_id, "requiredScopes": scopes });
        if let Some(p) = principal {
            body["principalId"] = json!(p);
        }
        body
    };
    let ctx = c.delegated("SANDBOX");
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(eval(json!(["numbers.read"]), None)),
            Some(ctx.clone()),
        )
        .await;
    assert_eq!(r.body["allowed"], true, "{}", r.body);
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(eval(json!(["numbers.assign"]), None)),
            Some(ctx.clone()),
        )
        .await;
    assert_eq!(r.body["allowed"], false);
    assert_eq!(r.body["reason"], "MISSING_SCOPES");
    assert_eq!(r.body["missingScopes"], json!(["numbers.assign"]));
    // Authorized-principal restriction.
    let env = c.env("SANDBOX");
    let p1 = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{env}/principals"),
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "user-1" })),
        )
        .await;
    let p2 = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{env}/principals"),
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "user-2" })),
        )
        .await;
    let (p1, p2) = (uuid(&p1["principal"]["id"]), uuid(&p2["principal"]["id"]));
    h.admin_ok(
        Method::PUT,
        &format!("/internal/v1/admin/credentials/{credential_id}/principal-restriction"),
        Some(json!({ "principalIds": [p1] })),
    )
    .await;
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["principalRestriction"],
        json!([p1])
    );
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(eval(json!(["numbers.read"]), Some(p1))),
            Some(ctx.clone()),
        )
        .await;
    assert_eq!(r.body["allowed"], true);
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(eval(json!(["numbers.read"]), Some(p2))),
            Some(ctx.clone()),
        )
        .await;
    assert_eq!(r.body["reason"], "PRINCIPAL_NOT_AUTHORIZED");
    // Revocation is seen immediately.
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/credentials/{credential_id}/revoke"),
        None,
    )
    .await;
    let r = h
        .as_calling(
            Method::POST,
            "/internal/v1/authorization/evaluate",
            Some(eval(json!(["numbers.read"]), Some(p1))),
            Some(ctx),
        )
        .await;
    assert_eq!(r.body["reason"], "CREDENTIAL_NOT_AUTHENTICATING");
    assert_eq!(r.body["effectiveState"], "REVOKED");
}

#[tokio::test]
async fn principal_restrictions_never_cross_environments() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["numbers.read"]).await;
    let (view, _) = h.issue(c.sa("SANDBOX"), &["numbers.read"]).await;
    let other_env = c.env("PRODUCTION");
    let p = h
        .admin_ok(
            Method::POST,
            &format!("/internal/v1/admin/environments/{other_env}/principals"),
            Some(json!({ "kind": "NATIVE_CF_USER", "externalReference": "prod-user" })),
        )
        .await;
    let r = h
        .admin(
            Method::PUT,
            &format!(
                "/internal/v1/admin/credentials/{}/principal-restriction",
                view["id"].as_str().unwrap()
            ),
            Some(json!({ "principalIds": [p["principal"]["id"]] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.body);
}
