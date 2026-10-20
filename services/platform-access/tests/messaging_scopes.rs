//! Part V Workstream B: the Messaging scopes and Messaging's narrow internal access.
//!
//! Scopes: registered, least privilege (none implies another), `messaging.admin`
//! privileged operator only, never granted implicitly to existing credentials, honoured
//! only within environment restrictions and while tenant / application / environment are
//! active. Messaging as an internal caller: authenticates credentials and resolves
//! principals of the signed delegated scope only, can never register principals or
//! administer identities, and forged tokens / contexts are refused.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::{ApplicationEnvironmentKind, DelegatedPlatformContext};
use common::*;
use serde_json::json;
use uuid::Uuid;

const STANDARD: &[&str] = &[
    "messages.send",
    "messages.read",
    "conversations.create",
    "conversations.manage",
    "attachments.write",
    "attachments.read",
    "moderation.manage",
    "messaging.export",
];

impl Harness {
    async fn as_messaging(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
        delegated: Option<DelegatedPlatformContext>,
    ) -> Reply {
        self.call(&self.peers.messaging, method, path, body, delegated, None)
            .await
    }

    async fn messaging_authenticate(&self, credential: &str) -> Reply {
        self.as_messaging(
            Method::POST,
            "/internal/v1/credentials/authenticate",
            Some(json!({ "credential": credential })),
            None,
        )
        .await
    }

    async fn principal(&self, environment: Uuid, kind: &str, reference: &str) -> Uuid {
        let p = self
            .admin_ok(
                Method::POST,
                &format!("/internal/v1/admin/environments/{environment}/principals"),
                Some(json!({ "kind": kind, "externalReference": reference })),
            )
            .await;
        uuid(&p["principal"]["id"])
    }
}

#[tokio::test]
async fn messaging_scopes_are_registered_and_listed_with_their_trust() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let r = h
        .as_messaging(Method::GET, "/internal/v1/scopes", None, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let scopes = r.body["scopes"].as_array().unwrap();
    for name in STANDARD {
        let s = scopes
            .iter()
            .find(|s| s["name"] == *name)
            .unwrap_or_else(|| panic!("{name}"));
        assert_eq!(s["trust"], "STANDARD", "{name}");
    }
    let admin = scopes
        .iter()
        .find(|s| s["name"] == "messaging.admin")
        .unwrap();
    assert_eq!(admin["trust"], "PRIVILEGED_OPERATOR");
}

#[tokio::test]
async fn each_messaging_scope_is_granted_explicitly_and_implies_no_other() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", STANDARD).await;
    for scope in STANDARD {
        let (_, secret) = h.issue(c.sa("SANDBOX"), &[scope]).await;
        let r = h.messaging_authenticate(&secret).await;
        assert_eq!(r.body["outcome"], "AUTHENTICATED", "{}", r.body);
        // messages.send never brings moderation, export, administration or management.
        assert_eq!(r.body["authority"]["scopes"], json!([scope]), "{scope}");
    }
    let (_, all) = h.issue(c.sa("PRODUCTION"), STANDARD).await;
    let mut expected = STANDARD.to_vec();
    expected.sort();
    assert_eq!(
        h.messaging_authenticate(&all).await.body["authority"]["scopes"],
        json!(expected)
    );
}

#[tokio::test]
async fn new_messaging_scopes_never_reach_existing_credentials_implicitly() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A Part IV consumer with a credential issued before any Messaging grant.
    let c = h
        .consumer("acme", "EXTERNAL", &["calls.read", "sms.send"])
        .await;
    let (view, secret) = h.issue(c.sa("SANDBOX"), &["calls.read", "sms.send"]).await;
    // The application is later allowed Messaging scopes.
    for scope in ["messages.send", "messages.read", "conversations.create"] {
        h.admin_ok(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                c.application_id
            ),
            Some(json!({ "scope": scope })),
        )
        .await;
    }
    // The existing credential is unchanged: no implicit grant, no reissue needed.
    let r = h.messaging_authenticate(&secret).await;
    assert_eq!(r.body["outcome"], "AUTHENTICATED");
    assert_eq!(
        r.body["authority"]["scopes"],
        json!(["calls.read", "sms.send"])
    );
    assert_eq!(r.body["authority"]["credential"]["id"], view["id"]);
    // Calling (an existing consumer) sees exactly the same authority.
    assert_eq!(
        h.authenticate(&secret).await.body["authority"]["scopes"],
        json!(["calls.read", "sms.send"])
    );
    // A credential needs the application's allowance: an unallowed Messaging scope is
    // refused at issuance.
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/service-accounts/{}/credentials",
                c.sa("SANDBOX")
            ),
            Some(json!({ "scopes": ["conversations.manage"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
}

#[tokio::test]
async fn messaging_admin_is_privileged_operator_authority_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let first = h.consumer("cfops", "FIRST_PARTY", &[]).await;
    let external = h.consumer("acme", "EXTERNAL", &[]).await;
    for c in [&first, &external] {
        let r = h
            .admin(
                Method::POST,
                &format!(
                    "/internal/v1/admin/applications/{}/scopes",
                    c.application_id
                ),
                Some(json!({ "scope": "messaging.admin" })),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    }
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                external.application_id
            ),
            Some(json!({ "scope": "messaging.admin", "privileged": true })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let view = h
        .admin_ok(
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                first.application_id
            ),
            Some(json!({ "scope": "messaging.admin", "privileged": true })),
        )
        .await;
    assert_eq!(view["grantBasis"], "PRIVILEGED_OPERATOR");
    let (_, secret) = h.issue(first.sa("PRODUCTION"), &["messaging.admin"]).await;
    assert_eq!(
        h.messaging_authenticate(&secret).await.body["authority"]["scopes"],
        json!(["messaging.admin"])
    );
}

#[tokio::test]
async fn environment_restrictions_suspension_and_revocation_apply_to_messaging_scopes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["messages.read"]).await;
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/applications/{}/scopes",
            c.application_id
        ),
        Some(json!({ "scope": "messages.send", "environmentKinds": ["SANDBOX"] })),
    )
    .await;
    // Cross-environment misuse: a PRODUCTION credential can never hold the sandbox-only
    // scope.
    let r = h
        .admin(
            Method::POST,
            &format!(
                "/internal/v1/admin/service-accounts/{}/credentials",
                c.sa("PRODUCTION")
            ),
            Some(json!({ "scopes": ["messages.send"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let (view, secret) = h
        .issue(c.sa("SANDBOX"), &["messages.send", "messages.read"])
        .await;
    assert_eq!(
        h.messaging_authenticate(&secret).await.body["authority"]["scopes"],
        json!(["messages.read", "messages.send"])
    );
    // A suspended tenant yields SAFETY_ONLY authority (Messaging refuses it).
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = h.messaging_authenticate(&secret).await;
    assert_eq!(r.body["authority"]["authorityMode"], "SAFETY_ONLY");
    assert_eq!(r.body["authority"]["suspendedLink"], "TENANT");
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        Some(json!({ "status": "ACTIVE" })),
    )
    .await;
    // Revocation is effective on the next authentication (no cache in Platform Access).
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/credentials/{}/revoke",
            uuid(&view["id"])
        ),
        None,
    )
    .await;
    let r = h.messaging_authenticate(&secret).await;
    assert_eq!(r.body["outcome"], "REJECTED", "{}", r.body);
}

#[tokio::test]
async fn messaging_resolves_principals_only_inside_the_signed_delegated_scope() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let a = h.consumer("acme", "EXTERNAL", &["messages.send"]).await;
    let b = h.consumer("globex", "EXTERNAL", &["messages.send"]).await;
    let alice = h
        .principal(a.env("SANDBOX"), "EXTERNAL_APPLICATION_PRINCIPAL", "alice")
        .await;
    let resolve = |id: Uuid| json!({ "references": [{ "principalId": id }] });
    let r = h
        .as_messaging(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve(alice)),
            Some(a.delegated("SANDBOX")),
        )
        .await;
    assert_eq!(r.body["results"][0]["outcome"], "RESOLVED", "{}", r.body);
    assert_eq!(
        r.body["results"][0]["principal"]["kind"],
        "EXTERNAL_APPLICATION_PRINCIPAL"
    );
    // Another tenant's scope, and another environment of the same application, cannot
    // see alice (reported like a missing principal).
    for ctx in [b.delegated("SANDBOX"), a.delegated("PRODUCTION")] {
        let r = h
            .as_messaging(
                Method::POST,
                "/internal/v1/principals/resolve",
                Some(resolve(alice)),
                Some(ctx),
            )
            .await;
        assert_eq!(r.body["results"][0]["outcome"], "NOT_FOUND", "{}", r.body);
    }
    // Cross-application / forged claims: tenant B with A's environment is refused.
    let forged = DelegatedPlatformContext::new(
        b.tenant_id,
        a.application_id,
        a.env("SANDBOX"),
        ApplicationEnvironmentKind::Sandbox,
        a.sa("SANDBOX"),
    );
    let r = h
        .as_messaging(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve(alice)),
            Some(forged),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID");
    // Without a delegated context there is no scope at all.
    let r = h
        .as_messaging(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve(alice)),
            None,
        )
        .await;
    assert!(r.status.is_client_error(), "{}", r.body);
    // A deactivated principal resolves INACTIVE (devices of it must not be used).
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/principals/{alice}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = h
        .as_messaging(
            Method::POST,
            "/internal/v1/principals/resolve",
            Some(resolve(alice)),
            Some(a.delegated("SANDBOX")),
        )
        .await;
    assert_eq!(r.body["results"][0]["outcome"], "INACTIVE", "{}", r.body);
}

#[tokio::test]
async fn principal_restrictions_are_reported_to_messaging() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["messages.send"]).await;
    let env = c.env("SANDBOX");
    let alice = h.principal(env, "NATIVE_CF_USER", "alice").await;
    let (view, secret) = h.issue(c.sa("SANDBOX"), &["messages.send"]).await;
    h.admin_ok(
        Method::PUT,
        &format!(
            "/internal/v1/admin/credentials/{}/principal-restriction",
            uuid(&view["id"])
        ),
        Some(json!({ "principalIds": [alice] })),
    )
    .await;
    let r = h.messaging_authenticate(&secret).await;
    assert_eq!(
        r.body["authority"]["principalRestriction"],
        json!([alice]),
        "{}",
        r.body
    );
}

#[tokio::test]
async fn messaging_holds_no_registration_administration_or_forged_identity() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &["messages.send"]).await;
    let pid = h
        .principal(c.env("SANDBOX"), "NATIVE_CF_USER", "alice")
        .await;
    // Principal registration and tombstoning are Calling's only.
    let r = h
        .as_messaging(
            Method::POST,
            "/internal/v1/principals/telephone-endpoints",
            Some(json!({ "contactReference": Uuid::new_v4() })),
            Some(c.delegated("SANDBOX")),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let r = h
        .as_messaging(
            Method::POST,
            &format!("/internal/v1/principals/{pid}/tombstone"),
            Some(json!({ "operator": "ops.mallory" })),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Identity administration (privilege escalation) is operator-console only, even with
    // an operator header.
    let r = h
        .call(
            &h.peers.messaging,
            Method::POST,
            &format!(
                "/internal/v1/admin/applications/{}/scopes",
                c.application_id
            ),
            Some(json!({ "scope": "messaging.admin", "privileged": true })),
            None,
            Some("ops.mallory"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.body);
    // A forged Messaging token (right name, unlisted key) is refused outright.
    let (_, secret) = h.issue(c.sa("SANDBOX"), &["messages.send"]).await;
    let r = h
        .call(
            &h.peers.rogue_messaging,
            Method::POST,
            "/internal/v1/credentials/authenticate",
            Some(json!({ "credential": secret })),
            None,
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.code(), "INTERNAL_AUTHENTICATION_FAILED");
}
