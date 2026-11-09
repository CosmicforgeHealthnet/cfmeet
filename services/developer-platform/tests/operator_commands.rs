//! INV-002: operator commands / reads and the service-account read that the
//! operation-coverage baseline still listed as untested, through the real router.
//!
//! * `GET /internal/v1/operator/organizations/{organizationId}`
//! * `POST /internal/v1/operator/commercial-plans/{planId}/retire`
//! * `DELETE /internal/v1/operator/organizations/{organizationId}/entitlement-overrides/{overrideId}`
//! * `POST /internal/v1/operator/usage/streams/{producer}/{stream}/reset`
//! * `GET /v1/environments/{environmentId}/service-accounts/{serviceAccountId}`
//!
//! Each: no token (401), a trusted peer without the permission / a member without the
//! role (403), the missing named operator (400 OPERATOR_REQUIRED), malformed input (400),
//! missing resources (404 / 409) and the success path with its durable effect.

mod common;

use axum::http::{HeaderMap, Method, StatusCode};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// Organization -> project -> SANDBOX environment of `owner`.
async fn sandbox(c: &Control, owner: &str) -> (Uuid, Uuid, Uuid) {
    let o = c
        .req(
            As::User(owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let o = id(&o.json);
    let p = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let p = id(&p.json);
    let e = env(c, owner, p, "SANDBOX").await;
    (o, p, e)
}

async fn env(c: &Control, owner: &str, project: Uuid, kind: &str) -> Uuid {
    let e = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": kind })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    id(&e.json)
}

/// Signs in `email` and makes them a member of `org` with `role`.
async fn join(c: &Control, owner: &str, org: Uuid, email: &str, role: &str) -> String {
    let (token, _) = c.sessions.sign_in(email);
    let inv = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{org}/invitations"),
            Some(json!({ "email": email, "role": role })),
        )
        .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.json);
    let acc = c
        .req(
            As::User(&token),
            Method::POST,
            "/v1/invitations/accept",
            Some(json!({ "token": inv.json["token"] })),
        )
        .await;
    assert_eq!(acc.status, StatusCode::OK, "{}", acc.json);
    token
}

/// The operator console's signature WITHOUT a named operator.
async fn anonymous_operator(c: &Control, method: Method, path: &str) -> Response {
    let signed = c
        .operator
        .sign(&cf_service_auth::OutboundServiceRequest::new_operation(
            cf_service_auth::ServiceId::developer_platform(),
        ))
        .unwrap();
    let mut headers = HeaderMap::new();
    signed.apply_to(&mut headers);
    c.raw(method, path.to_string(), headers, None).await
}

/// 401 without a token, 403 for the gateway (no `developer-platform.administer`) and,
/// when `named` is set, OPERATOR_REQUIRED for an anonymous operator.
async fn assert_operator_only(c: &Control, method: Method, path: &str, named: bool) {
    let r = c
        .raw(method.clone(), path.to_string(), HeaderMap::new(), None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{path}: {}", r.json);
    assert_eq!(r.json["error"]["code"], "INTERNAL_AUTHENTICATION_FAILED");
    let gateway = c.peers.gateway.clone();
    let r = c.internal(&gateway, method.clone(), path, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.json);
    assert_eq!(r.json["error"]["code"], "INTERNAL_PERMISSION_DENIED");
    if named {
        let r = anonymous_operator(c, method, path).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{path}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "OPERATOR_REQUIRED");
    }
}

async fn operator_audit(c: &Control, action: &str, target: &str) -> i64 {
    c.count(&format!(
        "SELECT count(*) FROM devplat_operator_audit WHERE action = '{action}' AND target_id = '{target}'"
    ))
    .await
}

#[tokio::test]
async fn the_operator_reads_an_organization_overview() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, _, _) = sandbox(&c, &owner).await;
    let path = format!("/internal/v1/operator/organizations/{org}");
    assert_operator_only(&c, Method::GET, &path, false).await;

    let r = c
        .operator(
            Method::GET,
            "/internal/v1/operator/organizations/not-a-uuid",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .operator(
            Method::GET,
            &format!("/internal/v1/operator/organizations/{}", Uuid::new_v4()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "NOT_FOUND");

    let r = c.operator(Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(id(&r.json), org);
    assert_eq!(r.json["activeMembers"], 1, "{}", r.json);
    assert_eq!(r.json["projects"], 1, "{}", r.json);
    // The overview follows membership.
    join(&c, &owner, org, "dev@acme.example", "DEVELOPER").await;
    let r = c.operator(Method::GET, &path, None).await;
    assert_eq!(r.json["activeMembers"], 2, "{}", r.json);
}

#[tokio::test]
async fn the_operator_retires_a_commercial_plan_once() {
    let Some(c) = Control::new().await else {
        return;
    };
    let plan = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(json!({ "code": "growth", "displayName": "Growth", "entitlementProfileRef": "p.growth",
                         "entitlements": { "apiRequestsPerMinute": 100, "apiRequestsPerDay": 5000, "smsPerDay": 10 } })),
        )
        .await;
    assert_eq!(plan.status, StatusCode::CREATED, "{}", plan.json);
    assert_eq!(plan.json["status"], "ACTIVE");
    let plan_id = id(&plan.json);
    let path = format!("/internal/v1/operator/commercial-plans/{plan_id}/retire");
    assert_operator_only(&c, Method::POST, &path, true).await;
    assert_eq!(
        operator_audit(&c, "commercial_plan.retired", &plan_id.to_string()).await,
        0
    );

    let r = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans/growth/retire",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .operator(
            Method::POST,
            &format!(
                "/internal/v1/operator/commercial-plans/{}/retire",
                Uuid::new_v4()
            ),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");

    let r = c.operator(Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(id(&r.json), plan_id);
    assert_eq!(r.json["status"], "RETIRED");
    assert!(r.json["effectiveUntil"].is_string(), "{}", r.json);
    assert_eq!(
        operator_audit(&c, "commercial_plan.retired", &plan_id.to_string()).await,
        1
    );
    // Retiring twice is refused and audited once.
    let r = c.operator(Method::POST, &path, None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "INVALID_STATE");
    assert_eq!(
        operator_audit(&c, "commercial_plan.retired", &plan_id.to_string()).await,
        1
    );
}

#[tokio::test]
async fn the_operator_revokes_an_entitlement_override_of_its_organization_only() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let (other_owner, _) = c.sessions.sign_in("owner@globex.example");
    let (other_org, _, _) = sandbox(&c, &other_owner).await;
    let gateway = c.peers.gateway.clone();
    let entitlements = format!("/internal/v1/entitlements/environments/{env}?kind=SANDBOX");
    let before = c.internal(&gateway, Method::GET, &entitlements, None).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.json);
    let ov = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{org}/entitlement-overrides"),
            Some(json!({ "projectId": project, "entitlements": { "apiRequestsPerMinute": 7 }, "reasonCode": "COMMERCIAL_AGREEMENT" })),
        )
        .await;
    assert_eq!(ov.status, StatusCode::CREATED, "{}", ov.json);
    let override_id = id(&ov.json);
    let overridden = c.internal(&gateway, Method::GET, &entitlements, None).await;
    assert_eq!(
        overridden.json["apiRequestsPerMinute"], 7,
        "{}",
        overridden.json
    );

    let path =
        format!("/internal/v1/operator/organizations/{org}/entitlement-overrides/{override_id}");
    assert_operator_only(&c, Method::DELETE, &path, true).await;
    let r = c
        .operator(
            Method::DELETE,
            &format!("/internal/v1/operator/organizations/{org}/entitlement-overrides/latest"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    for wrong in [
        format!(
            "/internal/v1/operator/organizations/{org}/entitlement-overrides/{}",
            Uuid::new_v4()
        ),
        // Another organization's path cannot revoke this override.
        format!(
            "/internal/v1/operator/organizations/{other_org}/entitlement-overrides/{override_id}"
        ),
    ] {
        let r = c.operator(Method::DELETE, &wrong, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{wrong}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "NOT_FOUND");
    }
    let still = c.internal(&gateway, Method::GET, &entitlements, None).await;
    assert_eq!(
        still.json["apiRequestsPerMinute"], 7,
        "refusals change nothing"
    );

    let r = c.operator(Method::DELETE, &path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json, json!({ "id": override_id, "revoked": true }));
    let after = c.internal(&gateway, Method::GET, &entitlements, None).await;
    assert_eq!(
        after.json["apiRequestsPerMinute"], before.json["apiRequestsPerMinute"],
        "{}",
        after.json
    );
    assert_ne!(
        after.json["limitingLevel"]["apiRequestsPerMinute"],
        "OPERATOR_OVERRIDE"
    );
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_entitlement_overrides WHERE id = '{override_id}' \
               AND revoked_at IS NOT NULL AND revoked_by = 'operator:ops.tester'"
        ))
        .await,
        1
    );
    // Revoked once: a replay finds nothing to revoke.
    let r = c.operator(Method::DELETE, &path, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

#[tokio::test]
async fn the_operator_resets_a_usage_stream_watermark() {
    let Some(c) = Control::new().await else {
        return;
    };
    c.db.conn
        .execute_unprepared(
            "INSERT INTO devplat_usage_streams (producer, stream, position_at, position_key, facts_total, updated_at) \
             VALUES ('cfmeet.sms-gateway', 'test.events', now(), 'k9', 42, now())",
        )
        .await
        .unwrap();
    let path = "/internal/v1/operator/usage/streams/cfmeet.sms-gateway/test.events/reset";
    assert_operator_only(&c, Method::POST, path, true).await;

    for bad in [
        "/internal/v1/operator/usage/streams/cfmeet.sms-gateway/Test.Events/reset",
        "/internal/v1/operator/usage/streams/cfmeet.sms-gateway/9events/reset",
    ] {
        let r = c.operator(Method::POST, bad, None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    for missing in [
        "/internal/v1/operator/usage/streams/cfmeet.sms-gateway/other.events/reset",
        "/internal/v1/operator/usage/streams/cfmeet.calling/test.events/reset",
    ] {
        let r = c.operator(Method::POST, missing, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{missing}: {}", r.json);
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_usage_streams WHERE position_key = 'k9'")
            .await,
        1,
        "refusals change nothing"
    );

    let r = c.operator(Method::POST, path, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        r.json,
        json!({ "producer": "cfmeet.sms-gateway", "stream": "test.events", "position": null })
    );
    assert_eq!(
        c.count(
            "SELECT count(*) FROM devplat_usage_streams WHERE producer = 'cfmeet.sms-gateway' \
               AND stream = 'test.events' AND position_at IS NULL AND position_key IS NULL \
               AND facts_total = 42"
        )
        .await,
        1,
        "the watermark is cleared, the totals are kept"
    );
    assert_eq!(
        operator_audit(&c, "usage.stream_reset", "cfmeet.sms-gateway/test.events").await,
        1
    );
}

#[tokio::test]
async fn a_service_account_is_read_by_members_who_may_see_it() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, sandbox_env) = sandbox(&c, &owner).await;
    let test_env = env(&c, &owner, project, "TEST").await;
    let sa = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{sandbox_env}/service-accounts"),
            Some(json!({ "name": "worker" })),
        )
        .await;
    assert_eq!(sa.status, StatusCode::CREATED, "{}", sa.json);
    let sa = id(&sa.json);
    let path = format!("/v1/environments/{sandbox_env}/service-accounts/{sa}");

    // No gateway token; a gateway request without a session or credential.
    let r = c
        .raw(Method::GET, path.clone(), HeaderMap::new(), None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    let r = c.req(As::Nobody, Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.json);
    assert_eq!(r.json["error"]["code"], "AUTHENTICATION_REQUIRED");
    // ANALYST / BILLING members may not read service accounts; outsiders see nothing.
    for (email, role) in [
        ("analyst@acme.example", "ANALYST"),
        ("billing@acme.example", "BILLING"),
    ] {
        let token = join(&c, &owner, org, email, role).await;
        let r = c.req(As::User(&token), Method::GET, &path, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{role}: {}", r.json);
        assert_eq!(r.json["error"]["code"], "PERMISSION_DENIED");
    }
    let (outsider, _) = c.sessions.sign_in("someone@globex.example");
    let r = c.req(As::User(&outsider), Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);

    // Malformed id, unknown account, unknown environment, the account under another
    // environment of the same project.
    for (wrong, status) in [
        (
            format!("/v1/environments/{sandbox_env}/service-accounts/worker"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!(
                "/v1/environments/{sandbox_env}/service-accounts/{}",
                Uuid::new_v4()
            ),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/v1/environments/{}/service-accounts/{sa}", Uuid::new_v4()),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("/v1/environments/{test_env}/service-accounts/{sa}"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let r = c.req(As::User(&owner), Method::GET, &wrong, None).await;
        assert_eq!(r.status, status, "{wrong}: {}", r.json);
    }

    // OWNER and DEVELOPER read it.
    let developer = join(&c, &owner, org, "dev@acme.example", "DEVELOPER").await;
    for who in [&owner, &developer] {
        let r = c.req(As::User(who), Method::GET, &path, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(id(&r.json), sa);
        assert_eq!(r.json["organizationId"], org.to_string());
        assert_eq!(r.json["projectId"], project.to_string());
        assert_eq!(r.json["environmentId"], sandbox_env.to_string());
        assert_eq!(r.json["name"], "worker");
        assert_eq!(r.json["status"], "ACTIVE");
    }
    // The read follows the account's state.
    let s = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{path}/suspend"),
            None,
        )
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.json);
    let r = c.req(As::User(&developer), Method::GET, &path, None).await;
    assert_eq!(r.json["status"], "SUSPENDED", "{}", r.json);
}
