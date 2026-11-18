//! ADR 0062 (CF Meet Platform "Build"): partner users, user tokens, allowed websites and
//! partner meeting principals — through the real router, real service tokens and a real
//! PostgreSQL database.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::*;
use platform_access::{
    config::platform_build::{EndUserCaps, PlatformBuildConfig},
    domain::EnvironmentKind,
    identity::user_tokens::{Grants, UserTokenClaims, UserTokenKeys},
};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const PARTNER_SCOPES: &[&str] = &[
    "principals.read",
    "principals.write",
    "user_tokens.issue",
    "messages.read",
    "messages.send",
    "calls.write",
    "meetings.write",
];

fn build(keys: Arc<UserTokenKeys>) -> PlatformBuildConfig {
    PlatformBuildConfig {
        end_users_enabled: true,
        user_tokens_enabled: true,
        environment_origins_enabled: true,
        meeting_platform_enabled: true,
        end_user_caps: EndUserCaps::default(),
        user_token_keys: Some(keys),
    }
}

async fn harness() -> Option<(Harness, Arc<UserTokenKeys>)> {
    let keys = Arc::new(UserTokenKeys::generate());
    let h = Harness::with_build(build(keys.clone())).await?;
    Some((h, keys))
}

/// A Developer Console call forwarding `credential`.
async fn dp(
    h: &Harness,
    method: Method,
    path: &str,
    body: Option<Value>,
    credential: Uuid,
) -> Reply {
    h.call_with_headers(
        &h.peers.developer_platform,
        method,
        path,
        body,
        None,
        None,
        &[("cf-forwarded-credential-id", credential.to_string())],
    )
    .await
}

/// A Developer Console delegation call (human actor).
async fn console(h: &Harness, method: Method, path: &str, body: Option<Value>) -> Reply {
    h.call_with_headers(
        &h.peers.developer_platform,
        method,
        path,
        body,
        None,
        None,
        &[("x-cf-delegated-actor", format!("u-{}", "a".repeat(32)))],
    )
    .await
}

async fn verify(h: &Harness, token: &str) -> Reply {
    h.call(
        &h.peers.messaging,
        Method::POST,
        "/internal/v1/user-tokens/verify",
        Some(json!({ "token": token })),
        None,
        None,
    )
    .await
}

/// An EXTERNAL consumer with every partner scope and a SANDBOX credential holding `scopes`.
async fn partner(h: &Harness, slug: &str, scopes: &[&str]) -> (Consumer, Uuid) {
    let c = h.consumer(slug, "EXTERNAL", PARTNER_SCOPES).await;
    let (credential, _) = h.issue(c.sa("SANDBOX"), scopes).await;
    (c, uuid(&credential["id"]))
}

async fn create(h: &Harness, credential: Uuid, external_id: &str) -> Value {
    let r = dp(
        h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": external_id })),
        credential,
    )
    .await;
    assert!(r.status.is_success(), "{} {}", r.status, r.body);
    r.body["user"].clone()
}

async fn mint(h: &Harness, credential: Uuid, user: &str, grants: Value) -> Reply {
    dp(
        h,
        Method::POST,
        &format!("/internal/v1/end-users/{user}/tokens"),
        Some(json!({ "grants": grants })),
        credential,
    )
    .await
}

async fn count(h: &Harness, sql: &str) -> i64 {
    h.conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

// ---------------------------------------------------------------------------
// Flags
// ---------------------------------------------------------------------------

#[tokio::test]
async fn with_the_flags_off_every_new_route_answers_like_an_unknown_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let env = c.env("SANDBOX");
    let id = Uuid::new_v4();
    let unknown = dp(
        &h,
        Method::GET,
        "/internal/v1/no-such-route",
        None,
        credential,
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    for (signer, method, path, body) in [
        (
            &h.peers.developer_platform,
            Method::POST,
            "/internal/v1/end-users".to_string(),
            Some(json!({ "externalId": "u1" })),
        ),
        (
            &h.peers.developer_platform,
            Method::GET,
            "/internal/v1/end-users".to_string(),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::GET,
            format!("/internal/v1/end-users/{id}"),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::PATCH,
            format!("/internal/v1/end-users/{id}"),
            Some(json!({})),
        ),
        (
            &h.peers.developer_platform,
            Method::DELETE,
            format!("/internal/v1/end-users/{id}"),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::POST,
            format!("/internal/v1/end-users/{id}/revoke-sessions"),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::POST,
            format!("/internal/v1/end-users/{id}/tokens"),
            Some(json!({})),
        ),
        (
            &h.peers.developer_platform,
            Method::POST,
            "/internal/v1/end-users/tokens".to_string(),
            Some(json!({ "externalId": "u1" })),
        ),
        (
            &h.peers.developer_platform,
            Method::GET,
            format!("/internal/v1/delegation/environments/{env}/end-users"),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::GET,
            format!("/internal/v1/delegation/environments/{env}/allowed-origins"),
            None,
        ),
        (
            &h.peers.developer_platform,
            Method::PUT,
            format!("/internal/v1/delegation/environments/{env}/allowed-origins"),
            Some(json!({ "origins": [] })),
        ),
        (
            &h.peers.messaging,
            Method::POST,
            "/internal/v1/user-tokens/verify".to_string(),
            Some(json!({ "token": "cfut_sbx_a.b.c" })),
        ),
        (
            &h.peers.gateway,
            Method::GET,
            "/internal/v1/user-tokens/keys".to_string(),
            None,
        ),
        (
            &h.peers.gateway,
            Method::GET,
            "/internal/v1/environment-origins".to_string(),
            None,
        ),
        (
            &h.peers.gateway,
            Method::GET,
            format!("/internal/v1/environments/{env}/origins"),
            None,
        ),
        (
            &h.peers.meeting,
            Method::POST,
            "/internal/v1/principals/ensure-meeting-platform".to_string(),
            Some(
                json!({ "environmentId": env, "kind": "BOT_OR_SERVICE", "externalReference": "cfmeet:meeting-system" }),
            ),
        ),
    ] {
        let r = h
            .call_with_headers(
                signer,
                method.clone(),
                &path,
                body,
                None,
                None,
                &[
                    ("cf-forwarded-credential-id", credential.to_string()),
                    ("x-cf-delegated-actor", format!("u-{}", "a".repeat(32))),
                ],
            )
            .await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.body
        );
        assert_eq!(r.code(), "NOT_FOUND", "{method} {path}");
    }
    // Nothing was written.
    assert_eq!(
        count(&h, "SELECT count(*) FROM platform_end_user_profiles").await,
        0
    );
    assert_eq!(
        count(&h, "SELECT count(*) FROM platform_principals").await,
        0
    );
}

// ---------------------------------------------------------------------------
// Partner Users API
// ---------------------------------------------------------------------------

#[tokio::test]
async fn partner_users_are_idempotent_validated_updated_and_tombstoned() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let created = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "rider-42", "displayName": "Ada", "imageUrl": "https://cdn.example.com/a.png" })),
        credential,
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let user = &created.body["user"];
    let id = user["id"].as_str().unwrap().to_string();
    assert_eq!(user["externalId"], "rider-42");
    assert_eq!(user["displayName"], "Ada");
    assert_eq!(user["imageUrl"], "https://cdn.example.com/a.png");
    assert_eq!(user["status"], "ACTIVE");
    // Idempotent on externalId.
    let again = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "rider-42" })),
        credential,
    )
    .await;
    assert_eq!(again.status, StatusCode::OK);
    assert_eq!(again.body["user"]["id"], id.as_str());
    assert_eq!(again.body["user"]["displayName"], "Ada");
    // The principal is an EXTERNAL_APPLICATION_PRINCIPAL of the credential's environment.
    assert_eq!(
        count(&h, &format!(
            "SELECT count(*) FROM platform_principals WHERE id = '{id}' AND kind = 'EXTERNAL_APPLICATION_PRINCIPAL' AND environment_id = '{}'",
            c.env("SANDBOX")
        ))
        .await,
        1
    );

    // Validation.
    for (body, what) in [
        (json!({ "externalId": "+2348012345678" }), "phone number"),
        (json!({ "externalId": "tel:0801234" }), "tel uri"),
        (json!({ "externalId": "sip:alice@example.org" }), "sip uri"),
        (json!({ "externalId": "has space" }), "space"),
        (json!({ "externalId": "x".repeat(129) }), "too long"),
        (json!({ "externalId": "" }), "empty"),
        (
            json!({ "externalId": "ok-1", "displayName": "x".repeat(65) }),
            "long name",
        ),
        (
            json!({ "externalId": "ok-1", "displayName": "a\u{0007}" }),
            "control",
        ),
        (
            json!({ "externalId": "ok-1", "imageUrl": "http://cdn.example.com/a.png" }),
            "http image",
        ),
        (
            json!({ "externalId": "ok-1", "unknown": true }),
            "unknown field",
        ),
    ] {
        let r = dp(
            &h,
            Method::POST,
            "/internal/v1/end-users",
            Some(body),
            credential,
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.body);
        assert_eq!(r.code(), "VALIDATION_ERROR", "{what}");
    }

    // Reads.
    let got = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/end-users/{id}"),
        None,
        credential,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body["user"]["externalId"], "rider-42");
    create(&h, credential, "rider-43").await;
    create(&h, credential, "rider-44").await;
    let page = dp(
        &h,
        Method::GET,
        "/internal/v1/end-users?limit=2",
        None,
        credential,
    )
    .await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert_eq!(page.body["items"].as_array().unwrap().len(), 2);
    let cursor = page.body["nextCursor"].as_str().unwrap().to_string();
    let next = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/end-users?limit=2&cursor={cursor}"),
        None,
        credential,
    )
    .await;
    assert_eq!(next.body["items"].as_array().unwrap().len(), 1);
    assert!(next.body["nextCursor"].is_null());
    let by_external = dp(
        &h,
        Method::GET,
        "/internal/v1/end-users?externalId=rider-43",
        None,
        credential,
    )
    .await;
    assert_eq!(by_external.body["items"][0]["externalId"], "rider-43");
    assert_eq!(by_external.body["items"].as_array().unwrap().len(), 1);

    // PATCH: profile and status.
    let patched = dp(
        &h,
        Method::PATCH,
        &format!("/internal/v1/end-users/{id}"),
        Some(json!({ "displayName": "Ada L.", "imageUrl": null, "status": "SUSPENDED" })),
        credential,
    )
    .await;
    assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
    assert_eq!(patched.body["user"]["displayName"], "Ada L.");
    assert!(patched.body["user"]["imageUrl"].is_null());
    assert_eq!(patched.body["user"]["status"], "SUSPENDED");
    let deactivate = dp(
        &h,
        Method::PATCH,
        &format!("/internal/v1/end-users/{id}"),
        Some(json!({ "status": "DEACTIVATED" })),
        credential,
    )
    .await;
    assert_eq!(deactivate.code(), "VALIDATION_ERROR");

    // The console read (human members) sees the same users.
    let listed = console(
        &h,
        Method::GET,
        &format!(
            "/internal/v1/delegation/environments/{}/end-users",
            c.env("SANDBOX")
        ),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    assert_eq!(listed.body["items"].as_array().unwrap().len(), 3);
    let unknown_env = console(
        &h,
        Method::GET,
        &format!(
            "/internal/v1/delegation/environments/{}/end-users",
            Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert_eq!(unknown_env.status, StatusCode::NOT_FOUND);

    // DELETE: tombstone (profile erased, id kept), then NOT_FOUND.
    let deleted = dp(
        &h,
        Method::DELETE,
        &format!("/internal/v1/end-users/{id}"),
        None,
        credential,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.body);
    let gone = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/end-users/{id}"),
        None,
        credential,
    )
    .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(
        count(
            &h,
            &format!(
            "SELECT count(*) FROM platform_principals WHERE id = '{id}' AND status = 'DEACTIVATED' \
               AND tombstoned_at IS NOT NULL AND external_reference = 'tombstoned:{id}'"
        )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &h,
            &format!(
                "SELECT count(*) FROM platform_end_user_profiles WHERE principal_id = '{id}' \
               AND display_name IS NULL AND image_url IS NULL"
            )
        )
        .await,
        1
    );
    // The externalId is free again: a new user (new id).
    let reborn = create(&h, credential, "rider-42").await;
    assert_ne!(reborn["id"], id.as_str());

    // Every mutation was audited under the credential's service account.
    let actor = format!("devplat.s-{}", c.sa("SANDBOX").simple());
    for (action, n) in [
        ("PRINCIPAL_REGISTERED", 4),
        ("PRINCIPAL_STATUS_SET", 1),
        ("PRINCIPAL_TOMBSTONED", 1),
    ] {
        assert_eq!(
            count(&h, &format!(
                "SELECT count(*) FROM platform_access_audit WHERE actor = '{actor}' AND action = '{action}'"
            ))
            .await,
            n,
            "{action}"
        );
    }
}

#[tokio::test]
async fn partner_users_never_leave_their_credentials_environment() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let user = create(&h, credential, "rider-1").await;
    let id = user["id"].as_str().unwrap();
    // Another environment of the same application, and another tenant.
    let (test_credential, _) = h.issue(c.sa("TEST"), PARTNER_SCOPES).await;
    let test_credential = uuid(&test_credential["id"]);
    let (_, other_credential) = partner(&h, "globex", PARTNER_SCOPES).await;
    for other in [test_credential, other_credential] {
        for (method, path, body) in [
            (Method::GET, format!("/internal/v1/end-users/{id}"), None),
            (
                Method::PATCH,
                format!("/internal/v1/end-users/{id}"),
                Some(json!({ "displayName": "x" })),
            ),
            (Method::DELETE, format!("/internal/v1/end-users/{id}"), None),
            (
                Method::POST,
                format!("/internal/v1/end-users/{id}/revoke-sessions"),
                None,
            ),
            (
                Method::POST,
                format!("/internal/v1/end-users/{id}/tokens"),
                Some(json!({ "grants": {} })),
            ),
        ] {
            let r = dp(&h, method.clone(), &path, body, other).await;
            assert_eq!(
                r.status,
                StatusCode::NOT_FOUND,
                "{method} {path}: {}",
                r.body
            );
        }
        let list = dp(
            &h,
            Method::GET,
            "/internal/v1/end-users?externalId=rider-1",
            None,
            other,
        )
        .await;
        assert!(list.body["items"].as_array().unwrap().is_empty());
    }
    // The same externalId elsewhere is another user.
    let elsewhere = create(&h, test_credential, "rider-1").await;
    assert_ne!(elsewhere["id"], user["id"]);

    // FIRST_PARTY credentials are refused.
    let fp = h
        .consumer("firstparty", "FIRST_PARTY", PARTNER_SCOPES)
        .await;
    let (fp_credential, _) = h.issue(fp.sa("SANDBOX"), PARTNER_SCOPES).await;
    let r = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "u" })),
        uuid(&fp_credential["id"]),
    )
    .await;
    assert_eq!(r.code(), "POLICY_VIOLATION", "{}", r.body);

    // Without the scope.
    let (read_only, _) = h.issue(c.sa("SANDBOX"), &["principals.read"]).await;
    let read_only = uuid(&read_only["id"]);
    let r = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "u" })),
        read_only,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "SCOPE_REQUIRED");
    assert_eq!(
        dp(
            &h,
            Method::GET,
            &format!("/internal/v1/end-users/{id}"),
            None,
            read_only
        )
        .await
        .status,
        StatusCode::OK
    );
    let (write_only, _) = h.issue(c.sa("SANDBOX"), &["principals.write"]).await;
    let r = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/end-users/{id}"),
        None,
        uuid(&write_only["id"]),
    )
    .await;
    assert_eq!(r.code(), "SCOPE_REQUIRED");

    // No forwarded credential / an unknown or revoked one.
    let r = h
        .call(
            &h.peers.developer_platform,
            Method::GET,
            "/internal/v1/end-users",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_REQUIRED");
    let r = dp(
        &h,
        Method::GET,
        "/internal/v1/end-users",
        None,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID");
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/credentials/{read_only}/revoke"),
        None,
    )
    .await;
    let r = dp(
        &h,
        Method::GET,
        &format!("/internal/v1/end-users/{id}"),
        None,
        read_only,
    )
    .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID");
    // A signed delegated context naming another credential.
    let r = h
        .call_with_headers(
            &h.peers.developer_platform,
            Method::GET,
            "/internal/v1/end-users",
            None,
            Some(
                c.delegated("SANDBOX")
                    .with_initiating_credential(Uuid::new_v4()),
            ),
            None,
            &[("cf-forwarded-credential-id", credential.to_string())],
        )
        .await;
    assert_eq!(r.code(), "DELEGATED_CONTEXT_INVALID");
    // A suspended environment.
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/status",
            c.env("SANDBOX")
        ),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let r = dp(&h, Method::GET, "/internal/v1/end-users", None, credential).await;
    assert_eq!(r.code(), "INVALID_STATE", "{}", r.body);

    // Only the Developer Console holds the permission.
    let r = h
        .call_with_headers(
            &h.peers.calling,
            Method::GET,
            "/internal/v1/end-users",
            None,
            None,
            None,
            &[("cf-forwarded-credential-id", test_credential.to_string())],
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
}

#[tokio::test]
async fn the_environment_cap_counts_active_users() {
    let keys = Arc::new(UserTokenKeys::generate());
    let mut cfg = build(keys);
    cfg.end_user_caps.sandbox = 2;
    let Some(h) = Harness::with_build(cfg).await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let a = create(&h, credential, "a").await;
    create(&h, credential, "b").await;
    let third = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "c" })),
        credential,
    )
    .await;
    assert_eq!(third.status, StatusCode::CONFLICT, "{}", third.body);
    assert_eq!(third.code(), "LIMIT_EXCEEDED");
    let upsert = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users/tokens",
        Some(json!({ "externalId": "c", "grants": {} })),
        credential,
    )
    .await;
    assert_eq!(upsert.code(), "LIMIT_EXCEEDED");
    // An existing user is still returned.
    let existing = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users",
        Some(json!({ "externalId": "a" })),
        credential,
    )
    .await;
    assert_eq!(existing.status, StatusCode::OK);
    // Suspending frees a slot; reactivating needs one.
    let a_id = a["id"].as_str().unwrap();
    let r = dp(
        &h,
        Method::PATCH,
        &format!("/internal/v1/end-users/{a_id}"),
        Some(json!({ "status": "SUSPENDED" })),
        credential,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    create(&h, credential, "c").await;
    let r = dp(
        &h,
        Method::PATCH,
        &format!("/internal/v1/end-users/{a_id}"),
        Some(json!({ "status": "ACTIVE" })),
        credential,
    )
    .await;
    assert_eq!(r.code(), "LIMIT_EXCEEDED");
    // Another environment has its own cap.
    let (test_credential, _) = h.issue(c.sa("TEST"), PARTNER_SCOPES).await;
    create(&h, uuid(&test_credential["id"]), "a").await;
}

// ---------------------------------------------------------------------------
// User tokens
// ---------------------------------------------------------------------------

#[tokio::test]
async fn minted_tokens_verify_with_effective_grants() {
    let Some((h, keys)) = harness().await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let user = create(&h, credential, "rider-1").await;
    let id = user["id"].as_str().unwrap();
    let conference = Uuid::new_v4();
    let grants = json!({
        "msg": ["read", "send"],
        "call": ["receive"],
        "mtg": [{ "conferenceId": conference, "role": "PARTICIPANT" }],
    });
    let minted = mint(&h, credential, id, grants.clone()).await;
    assert_eq!(minted.status, StatusCode::CREATED, "{}", minted.body);
    assert_eq!(minted.headers["cache-control"], "no-store");
    let token = minted.body["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("cfut_sbx_"), "{token}");
    assert!(token.len() <= 2048);

    for verifier in [&h.peers.messaging, &h.peers.gateway, &h.peers.meeting] {
        let v = h
            .call(
                verifier,
                Method::POST,
                "/internal/v1/user-tokens/verify",
                Some(json!({ "token": token })),
                None,
                None,
            )
            .await;
        assert_eq!(v.status, StatusCode::OK, "{}", v.body);
        assert_eq!(v.body["tokenId"], minted.body["tokenId"]);
        assert_eq!(v.body["tenantId"], c.tenant_id.to_string());
        assert_eq!(v.body["applicationId"], c.application_id.to_string());
        assert_eq!(v.body["environmentId"], c.env("SANDBOX").to_string());
        assert_eq!(v.body["environmentKind"], "SANDBOX");
        assert_eq!(v.body["credentialId"], credential.to_string());
        assert_eq!(v.body["serviceAccountId"], c.sa("SANDBOX").to_string());
        assert_eq!(v.body["user"]["id"], id);
        assert_eq!(v.body["user"]["externalId"], "rider-1");
        assert_eq!(v.body["user"]["status"], "ACTIVE");
        assert_eq!(v.body["grants"], grants);
        assert_eq!(v.body["expiresAt"], minted.body["expiresAt"]);
    }
    // Keys.
    let k = h
        .call(
            &h.peers.gateway,
            Method::GET,
            "/internal/v1/user-tokens/keys",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(k.status, StatusCode::OK);
    assert_eq!(k.body["keys"][0]["kid"], keys.kid());
    assert_eq!(k.body["keys"][0]["alg"], "EdDSA");
    assert_eq!(k.body["keys"][0]["crv"], "Ed25519");
    assert_eq!(k.body["keys"][0]["x"], keys.public_x());
    assert_eq!(k.body["keys"][0]["status"], "ACTIVE");
    // Only granted consumers verify.
    let denied = h
        .call(
            &h.peers.developer_platform,
            Method::POST,
            "/internal/v1/user-tokens/verify",
            Some(json!({ "token": token })),
            None,
            None,
        )
        .await;
    assert_eq!(denied.code(), "INTERNAL_PERMISSION_DENIED");

    // Effective grants shrink with the credential's CURRENT scopes.
    h.admin_ok(
        Method::DELETE,
        &format!(
            "/internal/v1/admin/applications/{}/scopes/messages.send",
            c.application_id
        ),
        None,
    )
    .await;
    h.admin_ok(
        Method::DELETE,
        &format!(
            "/internal/v1/admin/applications/{}/scopes/meetings.write",
            c.application_id
        ),
        None,
    )
    .await;
    let v = verify(&h, &token).await;
    assert_eq!(v.status, StatusCode::OK, "{}", v.body);
    assert_eq!(
        v.body["grants"],
        json!({ "msg": ["read"], "call": ["receive"], "mtg": [] })
    );
    // …and new mints need the scopes again.
    let r = mint(&h, credential, id, json!({ "msg": ["send"] })).await;
    assert_eq!(r.code(), "SCOPE_REQUIRED");
}

#[tokio::test]
async fn upsert_and_mint_creates_the_user_and_its_token() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let (_, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let r = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users/tokens",
        Some(json!({ "externalId": "rider-9", "displayName": "Bo", "grants": { "msg": ["read"] }, "ttlSeconds": 600 })),
        credential,
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["user"]["externalId"], "rider-9");
    assert_eq!(r.body["user"]["displayName"], "Bo");
    let v = verify(&h, r.body["token"].as_str().unwrap()).await;
    assert_eq!(v.status, StatusCode::OK, "{}", v.body);
    assert_eq!(v.body["user"]["displayName"], "Bo");
    let issued =
        chrono::DateTime::parse_from_rfc3339(v.body["issuedAt"].as_str().unwrap()).unwrap();
    let expires =
        chrono::DateTime::parse_from_rfc3339(v.body["expiresAt"].as_str().unwrap()).unwrap();
    assert_eq!((expires - issued).num_seconds(), 600);
    // Upsert: same user, profile updated.
    let again = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users/tokens",
        Some(json!({ "externalId": "rider-9", "displayName": "Bob" })),
        credential,
    )
    .await;
    assert_eq!(again.status, StatusCode::CREATED, "{}", again.body);
    assert_eq!(again.body["user"]["id"], r.body["user"]["id"]);
    assert_eq!(again.body["user"]["displayName"], "Bob");
    // Both scopes are needed.
    let (c2, only_issue) = partner(&h, "globex", &["user_tokens.issue", "messages.read"]).await;
    let _ = c2;
    let r = dp(
        &h,
        Method::POST,
        "/internal/v1/end-users/tokens",
        Some(json!({ "externalId": "rider-9" })),
        only_issue,
    )
    .await;
    assert_eq!(r.code(), "SCOPE_REQUIRED");
}

#[tokio::test]
async fn mint_requests_are_validated_scoped_and_rate_limited() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let (_, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let user = create(&h, credential, "rider-1").await;
    let id = user["id"].as_str().unwrap();
    for (body, code) in [
        (
            json!({ "grants": { "msg": ["write"] } }),
            "VALIDATION_ERROR",
        ),
        (
            json!({ "grants": { "call": ["place"] } }),
            "VALIDATION_ERROR",
        ),
        (
            json!({ "grants": { "mtg": [{ "conferenceId": Uuid::new_v4(), "role": "OWNER" }] } }),
            "VALIDATION_ERROR",
        ),
        (
            json!({ "grants": { "mtg": (0..11).map(|_| json!({ "conferenceId": Uuid::new_v4(), "role": "VIEWER" })).collect::<Vec<_>>() } }),
            "VALIDATION_ERROR",
        ),
        (
            json!({ "grants": {}, "ttlSeconds": 59 }),
            "VALIDATION_ERROR",
        ),
        (
            json!({ "grants": {}, "ttlSeconds": 86401 }),
            "VALIDATION_ERROR",
        ),
    ] {
        let r = dp(
            &h,
            Method::POST,
            &format!("/internal/v1/end-users/{id}/tokens"),
            Some(body.clone()),
            credential,
        )
        .await;
        assert_eq!(r.code(), code, "{body}: {}", r.body);
    }
    // Ten meeting grants (the maximum) still fit in 2048 bytes.
    let ten: Vec<Value> = (0..10)
        .map(|_| json!({ "conferenceId": Uuid::new_v4(), "role": "PARTICIPANT" }))
        .collect();
    let r = mint(
        &h,
        credential,
        id,
        json!({ "msg": ["read", "send"], "call": ["receive"], "mtg": ten }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert!(r.body["token"].as_str().unwrap().len() <= 2048);
    // Grant scopes: a credential with user_tokens.issue only.
    let (_, issue_only) = partner(&h, "globex", &["user_tokens.issue", "principals.write"]).await;
    let other = create(&h, issue_only, "u").await;
    let other_id = other["id"].as_str().unwrap();
    for grants in [
        json!({ "msg": ["read"] }),
        json!({ "msg": ["send"] }),
        json!({ "call": ["receive"] }),
        json!({ "mtg": [{ "conferenceId": Uuid::new_v4(), "role": "HOST" }] }),
    ] {
        let r = mint(&h, issue_only, other_id, grants.clone()).await;
        assert_eq!(r.code(), "SCOPE_REQUIRED", "{grants}");
    }
    assert_eq!(
        mint(&h, issue_only, other_id, json!({})).await.status,
        StatusCode::CREATED
    );
    // A suspended user gets no token.
    dp(
        &h,
        Method::PATCH,
        &format!("/internal/v1/end-users/{other_id}"),
        Some(json!({ "status": "SUSPENDED" })),
        issue_only,
    )
    .await;
    assert_eq!(
        mint(&h, issue_only, other_id, json!({})).await.code(),
        "INVALID_STATE"
    );

    // 120 mints per user per hour (one already happened above).
    for i in 1..120 {
        let r = mint(&h, credential, id, json!({})).await;
        assert_eq!(r.status, StatusCode::CREATED, "mint {i}: {}", r.body);
    }
    let limited = mint(&h, credential, id, json!({})).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.code(), "RATE_LIMITED");
    // The next hour starts a new window.
    h.clock.advance(chrono::Duration::hours(1));
    assert_eq!(
        mint(&h, credential, id, json!({})).await.status,
        StatusCode::CREATED
    );
}

#[tokio::test]
async fn tokens_die_with_their_user_credential_key_and_time() {
    let Some((h, keys)) = harness().await else {
        return;
    };
    let (c, credential) = partner(&h, "acme", PARTNER_SCOPES).await;
    let user = create(&h, credential, "rider-1").await;
    let id = user["id"].as_str().unwrap().to_string();
    let token = |r: Reply| r.body["token"].as_str().unwrap().to_string();
    let t1 = token(mint(&h, credential, &id, json!({ "msg": ["read"] })).await);
    assert_eq!(verify(&h, &t1).await.status, StatusCode::OK);

    // Tampered signature / claims, garbage, wrong prefix.
    let mut tampered = t1.clone().into_bytes();
    let last = tampered.len() - 2;
    tampered[last] = if tampered[last] == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).unwrap();
    for bad in [
        tampered.as_str(),
        "cfut_sbx_a.b.c",
        "garbage",
        &t1.replacen("cfut_sbx_", "cfut_live_", 1),
        &format!("{t1}x"),
    ] {
        let r = verify(&h, bad).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{bad}: {}", r.body);
        assert_eq!(r.code(), "USER_TOKEN_INVALID", "{bad}");
    }
    // Another deployment's key (unknown kid).
    let foreign = Harness::on_build(h.db.clone(), build(Arc::new(UserTokenKeys::generate())));
    assert_eq!(verify(&foreign, &t1).await.code(), "USER_TOKEN_INVALID");

    // Foreign-environment / forged chains, correctly signed.
    let (test_credential, _) = h.issue(c.sa("TEST"), PARTNER_SCOPES).await;
    let test_credential = uuid(&test_credential["id"]);
    let test_user = create(&h, test_credential, "rider-1").await;
    let now = h.clock_now().timestamp();
    let base = UserTokenClaims {
        principal_id: uuid(&user["id"]),
        tenant_id: c.tenant_id,
        application_id: c.application_id,
        environment_id: c.env("SANDBOX"),
        environment_kind: EnvironmentKind::Sandbox,
        service_account_id: c.sa("SANDBOX"),
        credential_id: credential,
        token_id: Uuid::new_v4(),
        issued_at: now,
        not_before: now,
        expires_at: now + 600,
        grants: Grants::default(),
    };
    assert_eq!(verify(&h, &keys.sign(&base)).await.status, StatusCode::OK);
    let forged = [
        // A user of another environment under this environment's credential.
        UserTokenClaims {
            principal_id: uuid(&test_user["id"]),
            ..base.clone()
        },
        // Another environment's credential claimed for this environment.
        UserTokenClaims {
            credential_id: test_credential,
            ..base.clone()
        },
        // Claimed environment differs from the credential's.
        UserTokenClaims {
            environment_id: c.env("TEST"),
            environment_kind: EnvironmentKind::Test,
            ..base.clone()
        },
        // Another service account / tenant.
        UserTokenClaims {
            service_account_id: c.sa("TEST"),
            ..base.clone()
        },
        UserTokenClaims {
            tenant_id: Uuid::new_v4(),
            ..base.clone()
        },
        // Unknown principal / credential.
        UserTokenClaims {
            principal_id: Uuid::new_v4(),
            ..base.clone()
        },
        UserTokenClaims {
            credential_id: Uuid::new_v4(),
            ..base.clone()
        },
    ];
    for claims in forged {
        let r = verify(&h, &keys.sign(&claims)).await;
        assert_eq!(r.code(), "USER_TOKEN_INVALID", "{claims:?}: {}", r.body);
    }

    // Expiry (60 s skew).
    h.clock.advance(chrono::Duration::seconds(3600 + 59));
    assert_eq!(verify(&h, &t1).await.status, StatusCode::OK);
    h.clock.advance(chrono::Duration::seconds(2));
    assert_eq!(verify(&h, &t1).await.code(), "USER_TOKEN_EXPIRED");

    // revoke-sessions: earlier tokens die, later ones live.
    let t2 = token(mint(&h, credential, &id, json!({})).await);
    assert_eq!(verify(&h, &t2).await.status, StatusCode::OK);
    let revoked = dp(
        &h,
        Method::POST,
        &format!("/internal/v1/end-users/{id}/revoke-sessions"),
        None,
        credential,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert!(revoked.body["tokensNotBefore"].is_string());
    assert_eq!(verify(&h, &t2).await.code(), "USER_TOKEN_REVOKED");
    let t3 = token(mint(&h, credential, &id, json!({})).await);
    assert_eq!(verify(&h, &t3).await.status, StatusCode::OK);

    // A suspended user: revoked; reactivated: valid again.
    for (status, expected) in [("SUSPENDED", "USER_TOKEN_REVOKED"), ("ACTIVE", "")] {
        dp(
            &h,
            Method::PATCH,
            &format!("/internal/v1/end-users/{id}"),
            Some(json!({ "status": status })),
            credential,
        )
        .await;
        let r = verify(&h, &t3).await;
        assert_eq!(r.code(), expected, "{status}: {}", r.body);
    }

    // A suspended environment.
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/status",
            c.env("SANDBOX")
        ),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(verify(&h, &t3).await.code(), "USER_TOKEN_REVOKED");
    h.admin_ok(
        Method::POST,
        &format!(
            "/internal/v1/admin/environments/{}/status",
            c.env("SANDBOX")
        ),
        Some(json!({ "status": "ACTIVE" })),
    )
    .await;
    assert_eq!(verify(&h, &t3).await.status, StatusCode::OK);

    // A retired signing key revokes; a previous key still verifies.
    let rotated = Harness::on_build(
        h.db.clone(),
        build(Arc::new(UserTokenKeys::new(
            ed25519_dalek_key(),
            vec![],
            vec![
                platform_access::identity::user_tokens::parse_public_key(&keys.public_x()).unwrap(),
            ],
        ))),
    );
    rotated.clock.set(h.clock_now());
    assert_eq!(verify(&rotated, &t3).await.code(), "USER_TOKEN_REVOKED");
    let keys_view = rotated
        .call(
            &rotated.peers.gateway,
            Method::GET,
            "/internal/v1/user-tokens/keys",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(keys_view.body["keys"][1]["kid"], keys.kid());
    assert_eq!(keys_view.body["keys"][1]["status"], "RETIRED");
    let overlapping = Harness::on_build(
        h.db.clone(),
        build(Arc::new(UserTokenKeys::new(
            ed25519_dalek_key(),
            vec![
                platform_access::identity::user_tokens::parse_public_key(&keys.public_x()).unwrap(),
            ],
            vec![],
        ))),
    );
    overlapping.clock.set(h.clock_now());
    assert_eq!(verify(&overlapping, &t3).await.status, StatusCode::OK);

    // A revoked minting credential.
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/credentials/{credential}/revoke"),
        None,
    )
    .await;
    assert_eq!(verify(&h, &t3).await.code(), "USER_TOKEN_REVOKED");

    // A deleted user (minted under another credential).
    let (other, _) = h.issue(c.sa("SANDBOX"), PARTNER_SCOPES).await;
    let other = uuid(&other["id"]);
    let t4 = token(mint(&h, other, &id, json!({})).await);
    assert_eq!(verify(&h, &t4).await.status, StatusCode::OK);
    dp(
        &h,
        Method::DELETE,
        &format!("/internal/v1/end-users/{id}"),
        None,
        other,
    )
    .await;
    assert_eq!(verify(&h, &t4).await.code(), "USER_TOKEN_REVOKED");
}

fn ed25519_dalek_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&rand::random::<[u8; 32]>())
}

trait ClockNow {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc>;
}

impl ClockNow for Harness {
    fn clock_now(&self) -> chrono::DateTime<chrono::Utc> {
        use platform_access::domain::Clock;
        self.clock.now()
    }
}

// ---------------------------------------------------------------------------
// Allowed websites
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allowed_origins_are_validated_managed_and_fed() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &[]).await;
    let sandbox = c.env("SANDBOX");
    let production = c.env("PRODUCTION");
    let path = |env: Uuid| format!("/internal/v1/delegation/environments/{env}/allowed-origins");
    let put = |env: Uuid, origins: Value| {
        let h = &h;
        let path = path(env);
        async move { console(h, Method::PUT, &path, Some(json!({ "origins": origins }))).await }
    };
    // Validation.
    for (env, origins, what) in [
        (
            production,
            json!(["http://localhost:3000"]),
            "loopback in PRODUCTION",
        ),
        (
            production,
            json!(["http://app.example.com"]),
            "http in PRODUCTION",
        ),
        (
            sandbox,
            json!(["http://app.example.com"]),
            "http non-loopback",
        ),
        (sandbox, json!(["https://*.example.com"]), "wildcard"),
        (sandbox, json!(["https://app.example.com/path"]), "path"),
        (sandbox, json!(["https://App.example.com"]), "uppercase"),
        (
            sandbox,
            json!(["https://app.example.com:443"]),
            "default port",
        ),
        (
            sandbox,
            json!((0..21)
                .map(|i| format!("https://a{i}.example.com"))
                .collect::<Vec<_>>()),
            "21 origins",
        ),
    ] {
        let r = put(env, origins).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{what}: {}", r.body);
        assert_eq!(r.code(), "VALIDATION_ERROR", "{what}");
    }
    // Valid lists.
    let ok = put(
        sandbox,
        json!([
            "https://app.example.com",
            "http://localhost:3000",
            "http://127.0.0.1:5173"
        ]),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["origins"].as_array().unwrap().len(), 3);
    let twenty: Vec<String> = (0..20)
        .map(|i| format!("https://a{i}.example.com"))
        .collect();
    assert_eq!(put(production, json!(twenty)).await.status, StatusCode::OK);
    // The actor is required for changes.
    let r = h
        .call(
            &h.peers.developer_platform,
            Method::PUT,
            &path(sandbox),
            Some(json!({ "origins": [] })),
            None,
            None,
        )
        .await;
    assert_eq!(r.code(), "OPERATOR_REQUIRED");
    // Reads: console and services.
    let got = console(&h, Method::GET, &path(sandbox), None).await;
    assert_eq!(
        got.body,
        json!({ "environmentId": sandbox, "origins": ["https://app.example.com", "http://localhost:3000", "http://127.0.0.1:5173"] })
    );
    for reader in [&h.peers.gateway, &h.peers.messaging] {
        let r = h
            .call(
                reader,
                Method::GET,
                &format!("/internal/v1/environments/{sandbox}/origins"),
                None,
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(r.body["origins"], got.body["origins"]);
    }
    let none = h
        .call(
            &h.peers.gateway,
            Method::GET,
            &format!("/internal/v1/environments/{}/origins", c.env("TEST")),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(none.body["origins"], json!([]));
    let unknown = h
        .call(
            &h.peers.gateway,
            Method::GET,
            &format!("/internal/v1/environments/{}/origins", Uuid::new_v4()),
            None,
            None,
            None,
        )
        .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    let denied = h
        .call(
            &h.peers.calling,
            Method::GET,
            "/internal/v1/environment-origins",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(denied.code(), "INTERNAL_PERMISSION_DENIED");

    // The feed: one item per change with the latest state.
    let feed = |after: i64| {
        let h = &h;
        async move {
            h.call(
                &h.peers.gateway,
                Method::GET,
                &format!("/internal/v1/environment-origins?after={after}"),
                None,
                None,
                None,
            )
            .await
        }
    };
    let page = feed(0).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let items = page.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["environmentId"], sandbox.to_string());
    assert_eq!(items[0]["environmentStatus"], "ACTIVE");
    assert_eq!(items[1]["environmentId"], production.to_string());
    let cursor = page.body["nextAfter"].as_i64().unwrap();
    assert_eq!(cursor, items[1]["seq"].as_i64().unwrap());
    // An unchanged list writes nothing.
    put(sandbox, got.body["origins"].clone()).await;
    assert!(feed(cursor).await.body["items"]
        .as_array()
        .unwrap()
        .is_empty());
    // A status change of the environment (or tenant) is fed.
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/environments/{sandbox}/status"),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let changed = feed(cursor).await;
    let items = changed.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", changed.body);
    assert_eq!(items[0]["environmentId"], sandbox.to_string());
    assert_eq!(items[0]["environmentStatus"], "SUSPENDED");
    let cursor = changed.body["nextAfter"].as_i64().unwrap();
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    let tenant_change = feed(cursor).await;
    assert_eq!(
        tenant_change.body["items"].as_array().unwrap().len(),
        2,
        "{}",
        tenant_change.body
    );
    // Clearing the list is a change too.
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/tenants/{}/status", c.tenant_id),
        Some(json!({ "status": "ACTIVE" })),
    )
    .await;
    let cleared = put(production, json!([])).await;
    assert_eq!(cleared.body["origins"], json!([]));
    let last = feed(tenant_change.body["nextAfter"].as_i64().unwrap()).await;
    let items = last.body["items"].as_array().unwrap();
    assert_eq!(items.last().unwrap()["origins"], json!([]));
    assert_eq!(items.last().unwrap()["environmentStatus"], "ACTIVE");
    // Bounded pages.
    let r = h
        .call(
            &h.peers.gateway,
            Method::GET,
            "/internal/v1/environment-origins?limit=501",
            None,
            None,
            None,
        )
        .await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
    // Audited.
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM platform_access_audit WHERE action = 'ENVIRONMENT_ORIGINS_SET'"
        )
        .await,
        3
    );
    // FIRST_PARTY tenants have no partner websites.
    let fp = h.consumer("firstparty", "FIRST_PARTY", &[]).await;
    let r = put(fp.env("SANDBOX"), json!(["https://app.example.com"])).await;
    assert_eq!(r.code(), "POLICY_VIOLATION");
}

// ---------------------------------------------------------------------------
// Partner meeting principals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn meeting_platform_principals_allow_exactly_two_combinations() {
    let Some((h, _)) = harness().await else {
        return;
    };
    let c = h.consumer("acme", "EXTERNAL", &[]).await;
    let env = c.env("PRODUCTION");
    let ensure = |environment: Uuid, kind: &str, reference: String| {
        let h = &h;
        let body =
            json!({ "environmentId": environment, "kind": kind, "externalReference": reference });
        async move {
            h.call(
                &h.peers.meeting,
                Method::POST,
                "/internal/v1/principals/ensure-meeting-platform",
                Some(body),
                None,
                None,
            )
            .await
        }
    };
    let system = ensure(env, "BOT_OR_SERVICE", "cfmeet:meeting-system".into()).await;
    assert_eq!(system.status, StatusCode::OK, "{}", system.body);
    assert_eq!(system.body["kind"], "BOT_OR_SERVICE");
    assert_eq!(system.body["tenantId"], c.tenant_id.to_string());
    assert_eq!(system.body["applicationId"], c.application_id.to_string());
    assert_eq!(system.body["environmentId"], env.to_string());
    let again = ensure(env, "BOT_OR_SERVICE", "cfmeet:meeting-system".into()).await;
    assert_eq!(again.body["principalId"], system.body["principalId"]);
    let participant = format!("meeting-participant:{}", Uuid::new_v4());
    let p = ensure(env, "TEMPORARY_EXTERNAL_PARTICIPANT", participant.clone()).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(p.body["kind"], "TEMPORARY_EXTERNAL_PARTICIPANT");
    assert_eq!(
        ensure(env, "TEMPORARY_EXTERNAL_PARTICIPANT", participant)
            .await
            .body["principalId"],
        p.body["principalId"]
    );
    // Every other combination.
    for (kind, reference) in [
        ("BOT_OR_SERVICE", "cfmeet:other".to_string()),
        (
            "BOT_OR_SERVICE",
            format!("meeting-participant:{}", Uuid::new_v4()),
        ),
        (
            "TEMPORARY_EXTERNAL_PARTICIPANT",
            "cfmeet:meeting-system".to_string(),
        ),
        (
            "TEMPORARY_EXTERNAL_PARTICIPANT",
            "meeting-participant:nope".to_string(),
        ),
        (
            "EXTERNAL_APPLICATION_PRINCIPAL",
            "cfmeet:meeting-system".to_string(),
        ),
        ("NATIVE_CF_USER", "user-1".to_string()),
    ] {
        let r = ensure(env, kind, reference.clone()).await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{kind} {reference}");
    }
    // Unknown environment, FIRST_PARTY tenant, suspended environment.
    assert_eq!(
        ensure(
            Uuid::new_v4(),
            "BOT_OR_SERVICE",
            "cfmeet:meeting-system".into()
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    let fp = h.consumer("firstparty", "FIRST_PARTY", &[]).await;
    assert_eq!(
        ensure(
            fp.env("PRODUCTION"),
            "BOT_OR_SERVICE",
            "cfmeet:meeting-system".into()
        )
        .await
        .code(),
        "POLICY_VIOLATION"
    );
    h.admin_ok(
        Method::POST,
        &format!("/internal/v1/admin/environments/{}/status", c.env("TEST")),
        Some(json!({ "status": "SUSPENDED" })),
    )
    .await;
    assert_eq!(
        ensure(
            c.env("TEST"),
            "BOT_OR_SERVICE",
            "cfmeet:meeting-system".into()
        )
        .await
        .code(),
        "INVALID_STATE"
    );
    // Only the Meeting Service holds the permission.
    let r = h
        .call(
            &h.peers.calling,
            Method::POST,
            "/internal/v1/principals/ensure-meeting-platform",
            Some(json!({ "environmentId": env, "kind": "BOT_OR_SERVICE", "externalReference": "cfmeet:meeting-system" })),
            None,
            None,
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
}
