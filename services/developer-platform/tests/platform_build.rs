//! ADR 0062 / 0063 — the Developer Console part of the CF Meet Platform "Build": the
//! partner Users API, user tokens, the console user list and SANDBOX token generator,
//! allowed websites, push credentials, the optional creation caps and the MESSAGE_SENT
//! usage type. REAL router and service tokens; Platform Access and the Notification
//! Service's provider vault are in-memory fakes that follow the build contract.

mod common;

use std::sync::{Arc, Mutex as StdMutex};

use axum::http::{Method, StatusCode};
use cf_service_auth::CorrelationId;
use common::*;
use developer_platform::{
    config::{BuildFeatures, FeatureFlags},
    platform_access::EndUserCall,
    push_providers::{PushProviderPort, PushProviders, PushReply, PushScope, PushUnavailable},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const ERR: &str = "platform-developer/common/v1.schema.json#/$defs/Error";

/// A marker inside the uploaded secrets: it must never appear anywhere but the request.
const SECRET_MARKER: &str = "SUPERSECRETKEYMATERIAL";

fn p8() -> String {
    format!(
        "-----BEGIN PRIVATE KEY-----\nfixture-not-a-key-{SECRET_MARKER}\n-----END PRIVATE KEY-----"
    )
}

fn all_on() -> BuildFeatures {
    BuildFeatures {
        users_api: true,
        allowed_origins: true,
        test_tokens: true,
        push_credentials: true,
        ..Default::default()
    }
}

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

fn code(r: &Response) -> &str {
    r.json["error"]["code"].as_str().unwrap_or_default()
}

/// One vault call: (method, scope, provider, body).
type VaultCall = (String, PushScope, String, Option<Value>);

/// The Notification Service's N-P1 vault (in memory).
#[derive(Default)]
struct FakeVault {
    /// (method, scope, provider, body).
    calls: StdMutex<Vec<VaultCall>>,
    /// The next verify answer.
    verify: StdMutex<Option<Value>>,
}

#[async_trait::async_trait]
impl PushProviderPort for FakeVault {
    async fn put(
        &self,
        scope: &PushScope,
        provider: &str,
        body: &Value,
        _c: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.calls.lock().unwrap().push((
            "PUT".into(),
            *scope,
            provider.into(),
            Some(body.clone()),
        ));
        Ok(PushReply::Ok(json!({
            "provider": provider, "status": "UNVERIFIED", "fingerprint": body["fingerprint"],
            "updatedAt": chrono::Utc::now().to_rfc3339(),
        })))
    }

    async fn delete(
        &self,
        scope: &PushScope,
        provider: &str,
        _c: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.calls
            .lock()
            .unwrap()
            .push(("DELETE".into(), *scope, provider.into(), None));
        Ok(PushReply::Ok(Value::Null))
    }

    async fn verify(
        &self,
        scope: &PushScope,
        provider: &str,
        _c: &CorrelationId,
    ) -> Result<PushReply, PushUnavailable> {
        self.calls
            .lock()
            .unwrap()
            .push(("VERIFY".into(), *scope, provider.into(), None));
        Ok(PushReply::Ok(
            self.verify
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(json!({ "status": "VERIFIED" })),
        ))
    }
}

fn sealing_key() -> cf_notification_client::SealingKey {
    let (public, _) = cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
    cf_notification_client::SealingKey::parse(&public).unwrap()
}

async fn harness(build: BuildFeatures, vault: Option<Arc<FakeVault>>) -> Option<Control> {
    Control::with_state(move |s| {
        let s = s.with_build(build);
        match vault {
            Some(v) => s.with_push(PushProviders {
                port: v,
                sealing_key: sealing_key(),
            }),
            None => s,
        }
    })
    .await
}

/// Organization -> project -> environment of `kind`.
async fn workspace(c: &Control, owner: &str) -> (Uuid, Uuid, Uuid) {
    let o = c
        .req(
            As::User(owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Rider Co" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let o = id(&o.json);
    let p = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/organizations/{o}/projects"),
            Some(json!({ "displayName": "Rider app" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let p = id(&p.json);
    (o, p, environment(c, owner, p, "SANDBOX").await)
}

async fn environment(c: &Control, owner: &str, project: Uuid, kind: &str) -> Uuid {
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

async fn dump(c: &Control, table: &str) -> String {
    use sea_orm::ConnectionTrait;
    c.db.conn
        .query_one(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!("SELECT coalesce(json_agg(t)::text, '') AS v FROM {table} t"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "v")
        .unwrap()
}

#[tokio::test]
async fn every_build_route_is_absent_while_its_flag_is_off() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (_, _, env) = workspace(&c, &owner).await;
    let machine = c.pa.machine(
        env,
        &[
            "principals.read",
            "principals.write",
            "user_tokens.issue",
            "credentials.manage",
        ],
    );
    for (who, method, path, body) in [
        (
            As::Machine(&machine),
            Method::POST,
            "/v1/users".to_string(),
            Some(json!({ "externalId": "r-1" })),
        ),
        (
            As::Machine(&machine),
            Method::GET,
            "/v1/users".to_string(),
            None,
        ),
        (
            As::Machine(&machine),
            Method::POST,
            "/v1/users/tokens".to_string(),
            Some(json!({})),
        ),
        (
            As::Machine(&machine),
            Method::GET,
            format!("/v1/users/{}", Uuid::new_v4()),
            None,
        ),
        (
            As::User(&owner),
            Method::GET,
            format!("/v1/environments/{env}/users"),
            None,
        ),
        (
            As::User(&owner),
            Method::GET,
            format!("/v1/environments/{env}/allowed-origins"),
            None,
        ),
        (
            As::User(&owner),
            Method::PUT,
            format!("/v1/environments/{env}/allowed-origins"),
            Some(json!({ "origins": [] })),
        ),
        (
            As::User(&owner),
            Method::POST,
            format!("/v1/environments/{env}/test-user-tokens"),
            Some(json!({})),
        ),
        (
            As::User(&owner),
            Method::GET,
            format!("/v1/environments/{env}/push-credentials"),
            None,
        ),
        (
            As::User(&owner),
            Method::POST,
            format!(
                "/v1/environments/{env}/push-credentials/{}/verify",
                Uuid::new_v4()
            ),
            None,
        ),
    ] {
        let r = c.req(who, method.clone(), &path, body).await;
        assert_eq!(
            r.status,
            StatusCode::NOT_FOUND,
            "{method} {path}: {}",
            r.json
        );
        assert_eq!(code(&r), "NOT_FOUND");
        conforms(ERR, &r.json);
    }
    assert!(c.pa.end_user_calls.lock().unwrap().is_empty());
    assert!(c.pa.origins.lock().unwrap().is_empty());
    // Today's creation behaviour: no cap.
    for n in 0..4 {
        let o = c
            .req(
                As::User(&owner),
                Method::POST,
                "/v1/organizations",
                Some(json!({ "displayName": format!("Extra {n}") })),
            )
            .await;
        assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    }
}

#[tokio::test]
async fn the_users_api_delegates_on_behalf_of_the_forwarded_credential() {
    let Some(c) = harness(all_on(), None).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (_, project, env) = workspace(&c, &owner).await;
    let machine = c.pa.machine(
        env,
        &[
            "principals.read",
            "principals.write",
            "user_tokens.issue",
            "messages.send",
        ],
    );
    let credential = machine.initiating_credential_id().unwrap();
    let m = || As::Machine(&machine);
    // Create: 201 new, 200 existing (idempotent on externalId).
    let body = json!({ "externalId": "rider-42", "displayName": "Rider 42",
        "imageUrl": "https://cdn.rider.example/42.png" });
    let r = c
        .req(m(), Method::POST, "/v1/users", Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}EndUser"), &r.json);
    let user = id(&r.json);
    let again = c.req(m(), Method::POST, "/v1/users", Some(body)).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.json);
    assert_eq!(id(&again.json), user);
    // Every call carried the gateway-authenticated credential.
    assert!(c
        .pa
        .end_user_calls
        .lock()
        .unwrap()
        .iter()
        .all(|(cred, _)| *cred == credential));
    // Idempotency-Key on create: a replay is marked and does not call Platform Access.
    let calls = c.pa.end_user_calls.lock().unwrap().len();
    let k = [("idempotency-key", "create-rider-7")];
    let first = c
        .req_with(
            m(),
            Method::POST,
            "/v1/users",
            Some(json!({ "externalId": "rider-7" })),
            &k,
        )
        .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.json);
    let replay = c
        .req_with(
            m(),
            Method::POST,
            "/v1/users",
            Some(json!({ "externalId": "rider-7" })),
            &k,
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert_eq!(c.pa.end_user_calls.lock().unwrap().len(), calls + 1);
    let reused = c
        .req_with(
            m(),
            Method::POST,
            "/v1/users",
            Some(json!({ "externalId": "rider-8" })),
            &k,
        )
        .await;
    assert_eq!(reused.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(code(&reused), "IDEMPOTENCY_KEY_REUSED");
    // Reads.
    let list = c
        .req(
            m(),
            Method::GET,
            "/v1/users?externalId=rider-42&limit=10",
            None,
        )
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}EndUserList"), &list.json);
    assert_eq!(list.json["items"].as_array().unwrap().len(), 1);
    let got = c
        .req(m(), Method::GET, &format!("/v1/users/{user}"), None)
        .await;
    assert_eq!(got.json["externalId"], "rider-42", "{}", got.json);
    // Update, revoke sessions.
    let up = c
        .req(
            m(),
            Method::PATCH,
            &format!("/v1/users/{user}"),
            Some(json!({ "displayName": null, "status": "SUSPENDED" })),
        )
        .await;
    assert_eq!(up.status, StatusCode::OK, "{}", up.json);
    assert_eq!(up.json["status"], "SUSPENDED");
    assert!(up.json["displayName"].is_null());
    let bad = c
        .req(
            m(),
            Method::PATCH,
            &format!("/v1/users/{user}"),
            Some(json!({ "externalId": "x" })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let rv = c
        .req(
            m(),
            Method::POST,
            &format!("/v1/users/{user}/revoke-sessions"),
            None,
        )
        .await;
    assert_eq!(rv.status, StatusCode::OK, "{}", rv.json);
    conforms(&format!("{CTRL}RevokeSessionsResult"), &rv.json);
    // Tokens: returned once, never cached, never stored for a replay.
    let grants = json!({ "msg": ["read", "send"], "mtg": [{ "conferenceId": Uuid::new_v4(), "role": "PARTICIPANT" }] });
    let k = [("idempotency-key", "token-1")];
    let t = c
        .req_with(
            m(),
            Method::POST,
            &format!("/v1/users/{user}/tokens"),
            Some(json!({ "grants": grants, "ttlSeconds": 600 })),
            &k,
        )
        .await;
    assert_eq!(t.status, StatusCode::CREATED, "{}", t.json);
    conforms(&format!("{CTRL}UserToken"), &t.json);
    assert!(t.json["token"].as_str().unwrap().starts_with("cfut_sbx_"));
    assert_eq!(t.headers["cache-control"], "no-store");
    let token = t.json["token"].as_str().unwrap().to_string();
    let replay = c
        .req_with(
            m(),
            Method::POST,
            &format!("/v1/users/{user}/tokens"),
            Some(json!({ "grants": grants, "ttlSeconds": 600 })),
            &k,
        )
        .await;
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    assert!(replay.json.get("token").is_none(), "{}", replay.json);
    assert_eq!(replay.json["tokenRedacted"], true);
    conforms(&format!("{CTRL}UserToken"), &replay.json);
    assert!(!dump(&c, "devplat_idempotency_records")
        .await
        .contains(&token));
    assert!(!dump(&c, "devplat_idempotency_records")
        .await
        .contains("cfut_"));
    for bad in [
        json!({ "grants": { "msg": ["write"] } }),
        json!({ "grants": {}, "ttlSeconds": 30 }),
        json!({ "grants": { "mtg": [{ "conferenceId": "x", "role": "HOST" }] } }),
    ] {
        let r = c
            .req(
                m(),
                Method::POST,
                &format!("/v1/users/{user}/tokens"),
                Some(bad.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // Upsert + mint.
    let up = c
        .req(
            m(),
            Method::POST,
            "/v1/users/tokens",
            Some(json!({ "externalId": "rider-99", "grants": { "call": ["receive"] } })),
        )
        .await;
    assert_eq!(up.status, StatusCode::CREATED, "{}", up.json);
    conforms(&format!("{CTRL}UserWithToken"), &up.json);
    assert_eq!(up.json["user"]["externalId"], "rider-99");
    // Delete: 204, a tombstone.
    let del = c
        .req(m(), Method::DELETE, &format!("/v1/users/{user}"), None)
        .await;
    assert_eq!(del.status, StatusCode::NO_CONTENT, "{}", del.json);
    let gone = c
        .req(m(), Method::GET, &format!("/v1/users/{user}"), None)
        .await;
    assert_eq!(gone.json["status"], "DEACTIVATED");
    // Phone numbers and SIP addresses are never ids: refused before Platform Access.
    let calls = c.pa.end_user_calls.lock().unwrap().len();
    for ext in ["+15551234567", "sip:alice@example.com", "has space", ""] {
        let r = c
            .req(
                m(),
                Method::POST,
                "/v1/users",
                Some(json!({ "externalId": ext })),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{ext}");
    }
    assert_eq!(c.pa.end_user_calls.lock().unwrap().len(), calls);
    // Another environment's user is NOT_FOUND (Platform Access takes the environment from
    // the credential).
    let test_env = environment(&c, &owner, project, "TEST").await;
    let other = c.pa.machine(test_env, &["principals.read"]);
    let r = c
        .req(
            As::Machine(&other),
            Method::GET,
            &format!("/v1/users/{}", id(&again.json)),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

#[tokio::test]
async fn the_users_api_checks_scopes_callers_and_maps_platform_access_errors() {
    let Some(c) = harness(all_on(), None).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (_, _, env) = workspace(&c, &owner).await;
    // Scope refusals (checked here first: Platform Access is not called).
    let reader = c.pa.machine(env, &["principals.read"]);
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            "/v1/users",
            Some(json!({ "externalId": "r-1" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(code(&r), "INSUFFICIENT_SCOPE");
    let r = c
        .req(
            As::Machine(&reader),
            Method::POST,
            &format!("/v1/users/{}/tokens", Uuid::new_v4()),
            Some(json!({ "grants": {} })),
        )
        .await;
    assert_eq!(code(&r), "INSUFFICIENT_SCOPE");
    let minter = c.pa.machine(env, &["user_tokens.issue"]);
    let r = c
        .req(
            As::Machine(&minter),
            Method::POST,
            "/v1/users/tokens",
            Some(json!({ "externalId": "r-1", "grants": {} })),
        )
        .await;
    assert_eq!(
        code(&r),
        "INSUFFICIENT_SCOPE",
        "the upsert also needs principals.write"
    );
    let writer = c.pa.machine(env, &["principals.write"]);
    let r = c
        .req(As::Machine(&writer), Method::GET, "/v1/users", None)
        .await;
    assert_eq!(code(&r), "INSUFFICIENT_SCOPE");
    assert!(c.pa.end_user_calls.lock().unwrap().is_empty());
    // Human sessions never use the machine Users API.
    let r = c
        .req(As::User(&owner), Method::GET, "/v1/users", None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(code(&r), "PERMISSION_DENIED");
    let r = c.req(As::Nobody, Method::GET, "/v1/users", None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Platform Access refusals map to the Developer Platform envelope.
    let full = c.pa.machine(
        env,
        &["principals.read", "principals.write", "user_tokens.issue"],
    );
    for (status, pa_code, want_status, want_code) in [
        (
            409,
            "LIMIT_EXCEEDED",
            StatusCode::CONFLICT,
            "LIMIT_EXCEEDED",
        ),
        (
            400,
            "VALIDATION_ERROR",
            StatusCode::BAD_REQUEST,
            "VALIDATION_ERROR",
        ),
        (
            403,
            "SCOPE_REQUIRED",
            StatusCode::FORBIDDEN,
            "INSUFFICIENT_SCOPE",
        ),
        (
            422,
            "POLICY_VIOLATION",
            StatusCode::UNPROCESSABLE_ENTITY,
            "POLICY_VIOLATION",
        ),
        (
            429,
            "RATE_LIMITED",
            StatusCode::TOO_MANY_REQUESTS,
            "RATE_LIMITED",
        ),
        (404, "NOT_FOUND", StatusCode::NOT_FOUND, "NOT_FOUND"),
        (
            401,
            "DELEGATED_CONTEXT_REQUIRED",
            StatusCode::FORBIDDEN,
            "DELEGATED_CONTEXT_REQUIRED",
        ),
        (
            403,
            "DELEGATED_CONTEXT_INVALID",
            StatusCode::FORBIDDEN,
            "DELEGATED_CONTEXT_INVALID",
        ),
    ] {
        *c.pa.end_users_refusal.lock().unwrap() = Some((status, pa_code.into()));
        let r = c
            .req(
                As::Machine(&full),
                Method::POST,
                "/v1/users",
                Some(json!({ "externalId": "r-2" })),
            )
            .await;
        assert_eq!((r.status, code(&r)), (want_status, want_code), "{pa_code}");
        conforms(ERR, &r.json);
    }
    *c.pa.end_users_refusal.lock().unwrap() = None;
    c.pa.down.store(true, std::sync::atomic::Ordering::SeqCst);
    let r = c
        .req(As::Machine(&full), Method::GET, "/v1/users", None)
        .await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    c.pa.down.store(false, std::sync::atomic::Ordering::SeqCst);
    // A revoked credential no longer authenticates.
    c.pa.revoke(&full);
    let r = c
        .req(As::Machine(&full), Method::GET, "/v1/users", None)
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Only well-formed calls reached Platform Access.
    assert!(c
        .pa
        .end_user_calls
        .lock()
        .unwrap()
        .iter()
        .all(|(_, call)| matches!(call, EndUserCall::Create { .. })));
}

#[tokio::test]
async fn console_users_websites_and_test_tokens_follow_the_role_matrix() {
    let Some(c) = harness(all_on(), None).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (org, project, env) = workspace(&c, &owner).await;
    let admin = join(&c, &owner, org, "admin@rider.example", "ADMIN").await;
    let dev = join(&c, &owner, org, "dev@rider.example", "DEVELOPER").await;
    let analyst = join(&c, &owner, org, "analyst@rider.example", "ANALYST").await;
    let billing = join(&c, &owner, org, "billing@rider.example", "BILLING").await;
    // The SANDBOX token generator: OWNER / ADMIN / DEVELOPER.
    let body = json!({ "externalId": "tester-1", "displayName": "Tester",
        "grants": { "msg": ["read", "send"] }, "ttlSeconds": 3600 });
    let path = format!("/v1/environments/{env}/test-user-tokens");
    let t = c
        .req(As::User(&dev), Method::POST, &path, Some(body.clone()))
        .await;
    assert_eq!(t.status, StatusCode::CREATED, "{}", t.json);
    conforms(&format!("{CTRL}UserWithToken"), &t.json);
    assert_eq!(t.headers["cache-control"], "no-store");
    for who in [&analyst, &billing] {
        let r = c
            .req(As::User(who), Method::POST, &path, Some(body.clone()))
            .await;
        assert_eq!(
            (r.status, code(&r)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED")
        );
    }
    let machine =
        c.pa.machine(env, &["user_tokens.issue", "principals.write"]);
    let r = c
        .req(
            As::Machine(&machine),
            Method::POST,
            &path,
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let test_env = environment(&c, &owner, project, "TEST").await;
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{test_env}/test-user-tokens"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(
        (r.status, code(&r)),
        (StatusCode::UNPROCESSABLE_ENTITY, "POLICY_VIOLATION")
    );
    assert!(!dump(&c, "devplat_activity_events").await.contains("cfut_"));
    // The console user list: OWNER / ADMIN / DEVELOPER.
    let users = format!("/v1/environments/{env}/users");
    for who in [&owner, &admin, &dev] {
        let r = c.req(As::User(who), Method::GET, &users, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        conforms(&format!("{CTRL}EndUserList"), &r.json);
        assert_eq!(r.json["items"][0]["externalId"], "tester-1");
    }
    for who in [&analyst, &billing] {
        let r = c.req(As::User(who), Method::GET, &users, None).await;
        assert_eq!(
            (r.status, code(&r)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED")
        );
    }
    let r = c
        .req(As::Machine(&machine), Method::GET, &users, None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Allowed websites: every member reads, OWNER / ADMIN replace.
    let origins = format!("/v1/environments/{env}/allowed-origins");
    for who in [&owner, &admin, &dev, &analyst, &billing] {
        let r = c.req(As::User(who), Method::GET, &origins, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        conforms(&format!("{CTRL}AllowedOrigins"), &r.json);
    }
    let list = json!({ "origins": ["https://app.rider.example", "http://localhost:3000"] });
    for who in [&dev, &analyst, &billing] {
        let r = c
            .req(As::User(who), Method::PUT, &origins, Some(list.clone()))
            .await;
        assert_eq!(
            (r.status, code(&r)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED")
        );
    }
    let r = c
        .req(As::User(&admin), Method::PUT, &origins, Some(list.clone()))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AllowedOrigins"), &r.json);
    assert_eq!(r.json["origins"], list["origins"]);
    assert_eq!(c.pa.origins.lock().unwrap()[&env].len(), 2);
    for bad in [
        json!({ "origins": ["https://*.rider.example"] }),
        json!({ "origins": ["https://app.rider.example/path"] }),
        json!({ "origins": ["http://app.rider.example"] }),
        json!({ "origins": (0..21).map(|i| format!("https://a{i}.example")).collect::<Vec<_>>() }),
    ] {
        let r = c
            .req(As::User(&owner), Method::PUT, &origins, Some(bad.clone()))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    // localhost is for SANDBOX / TEST only (the PRODUCTION rule is unit-tested).
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/environments/{test_env}/allowed-origins"),
            Some(json!({ "origins": ["http://127.0.0.1:5173"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    // Machines: `credentials.manage` replaces, `projects.read` reads.
    let reader = c.pa.machine(env, &["projects.read"]);
    let r = c
        .req(As::Machine(&reader), Method::GET, &origins, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = c
        .req(
            As::Machine(&reader),
            Method::PUT,
            &origins,
            Some(list.clone()),
        )
        .await;
    assert_eq!(code(&r), "INSUFFICIENT_SCOPE");
    let manager = c.pa.machine(env, &["credentials.manage"]);
    let r = c
        .req(
            As::Machine(&manager),
            Method::PUT,
            &origins,
            Some(json!({ "origins": [] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(c.pa.origins.lock().unwrap()[&env].is_empty());
    // A non-member never learns the environment exists.
    let (stranger, _) = c.sessions.sign_in("stranger@other.example");
    let r = c
        .req(As::User(&stranger), Method::GET, &origins, None)
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let acts = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{org}/activity?limit=100"),
            None,
        )
        .await;
    let actions: Vec<&str> = acts.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["action"].as_str())
        .collect();
    assert!(actions.contains(&"allowed_origins.updated"), "{actions:?}");
    assert!(actions.contains(&"user_token.test_issued"), "{actions:?}");
}

#[tokio::test]
async fn push_credentials_are_sealed_forwarded_and_never_persisted() {
    let vault = Arc::new(FakeVault::default());
    let Some(c) = harness(all_on(), Some(vault.clone())).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (org, project, env) = workspace(&c, &owner).await;
    let admin = join(&c, &owner, org, "admin@rider.example", "ADMIN").await;
    let dev = join(&c, &owner, org, "dev@rider.example", "DEVELOPER").await;
    let analyst = join(&c, &owner, org, "analyst@rider.example", "ANALYST").await;
    let path = format!("/v1/environments/{env}/push-credentials");
    let apns = |host: &str| {
        json!({ "provider": "APNS", "teamId": "ABCDE12345", "keyId": "KEY1234567",
            "bundleId": "com.rider.app", "voip": true, "apnsHost": host, "p8": p8() })
    };
    // OWNER / ADMIN only; never machines.
    for who in [&dev, &analyst] {
        let r = c
            .req(As::User(who), Method::POST, &path, Some(apns("SANDBOX")))
            .await;
        assert_eq!(
            (r.status, code(&r)),
            (StatusCode::FORBIDDEN, "PERMISSION_DENIED")
        );
        let r = c.req(As::User(who), Method::GET, &path, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN);
    }
    let machine =
        c.pa.machine(env, &["credentials.manage", "projects.manage"]);
    let r = c.req(As::Machine(&machine), Method::GET, &path, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // SANDBOX / TEST may only use the APNs sandbox host.
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &path,
            Some(apns("PRODUCTION")),
        )
        .await;
    assert_eq!(
        (r.status, code(&r)),
        (StatusCode::UNPROCESSABLE_ENTITY, "POLICY_VIOLATION")
    );
    assert!(
        vault.calls.lock().unwrap().is_empty(),
        "refused before sealing"
    );
    // Upload: sealed, forwarded, metadata only.
    let k = [("idempotency-key", "apns-voip-1")];
    let r = c
        .req_with(
            As::User(&admin),
            Method::POST,
            &path,
            Some(apns("SANDBOX")),
            &k,
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}PushCredential"), &r.json);
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(
        (r.json["provider"].as_str(), r.json["voip"].as_bool()),
        (Some("APNS"), Some(true))
    );
    assert_eq!(r.json["status"], "UNVERIFIED");
    assert_eq!(
        r.json["fingerprint"],
        hex::encode(Sha256::digest(p8().as_bytes()))
    );
    assert!(!r.json.to_string().contains(SECRET_MARKER));
    let first_ref = r.json["credentialRef"].as_str().unwrap().to_string();
    let replay = c
        .req_with(
            As::User(&admin),
            Method::POST,
            &path,
            Some(apns("SANDBOX")),
            &k,
        )
        .await;
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    {
        let calls = vault.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "a replay is not forwarded again");
        let (method, scope, provider, body) = &calls[0];
        assert_eq!((method.as_str(), provider.as_str()), ("PUT", "APNS_VOIP"));
        assert_eq!(scope.environment_id, env);
        assert_eq!(scope.application_id, project);
        assert_eq!(scope.tenant_id, org);
        let body = body.as_ref().unwrap();
        assert!(
            !body.to_string().contains(SECRET_MARKER),
            "only the sealed secret leaves"
        );
        assert!(!body.to_string().contains("BEGIN PRIVATE KEY"));
        let sealed: Value = serde_json::from_str(body["sealed"].as_str().unwrap()).unwrap();
        assert_eq!(sealed["v"], 1);
        assert_eq!(sealed["parts"][0]["kid"], "notif-hpke-2026-11");
        assert_eq!(
            body["metadata"],
            json!({ "teamId": "ABCDE12345", "keyId": "KEY1234567",
            "bundleId": "com.rider.app", "apnsHost": "SANDBOX" })
        );
        assert_eq!(body["fingerprint"], r.json["fingerprint"]);
    }
    // FCM.
    let sa = json!({ "type": "service_account", "project_id": "rider-app-1",
        "private_key": format!("-----BEGIN PRIVATE KEY-----\n{}{SECRET_MARKER}\n-----END PRIVATE KEY-----\n", "A".repeat(1600)),
        "client_email": "push@rider-app-1.iam.gserviceaccount.com" })
    .to_string();
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &path,
            Some(
                json!({ "provider": "FCM", "projectId": "rider-app-1", "serviceAccountJson": sa }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["provider"], "FCM");
    let fcm_ref = r.json["credentialRef"].as_str().unwrap().to_string();
    {
        let calls = vault.calls.lock().unwrap();
        let body = calls[1].3.as_ref().unwrap();
        let sealed: Value = serde_json::from_str(body["sealed"].as_str().unwrap()).unwrap();
        assert!(
            sealed["parts"].as_array().unwrap().len() > 1,
            "a long secret is split"
        );
        assert!(!body.to_string().contains(SECRET_MARKER));
    }
    // Re-uploading a provider replaces it.
    let r = c
        .req(As::User(&owner), Method::POST, &path, Some(apns("SANDBOX")))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let apns_ref = r.json["credentialRef"].as_str().unwrap().to_string();
    assert_ne!(apns_ref, first_ref);
    let list = c.req(As::User(&admin), Method::GET, &path, None).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}PushCredentialList"), &list.json);
    assert_eq!(list.json["items"].as_array().unwrap().len(), 2);
    // Verify: the outcome is recorded (bounded error codes only).
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{path}/{apns_ref}/verify"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}PushCredential"), &r.json);
    assert_eq!(r.json["status"], "VERIFIED");
    assert!(r.json["lastVerifiedAt"].is_string());
    *vault.verify.lock().unwrap() =
        Some(json!({ "status": "FAILED", "lastError": "FCM_UNAUTHORIZED" }));
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("{path}/{fcm_ref}/verify"),
            None,
        )
        .await;
    assert_eq!(
        (r.json["status"].as_str(), r.json["lastError"].as_str()),
        (Some("FAILED"), Some("FCM_UNAUTHORIZED"))
    );
    // The secret is nowhere: not in metadata, idempotency records or activity.
    for table in [
        "devplat_push_providers",
        "devplat_idempotency_records",
        "devplat_activity_events",
    ] {
        let all = dump(&c, table).await;
        assert!(!all.contains(SECRET_MARKER), "{table}");
        assert!(!all.contains("BEGIN PRIVATE KEY"), "{table}");
    }
    // Another environment's reference is NOT_FOUND; delete removes it at the vault too.
    let other = environment(&c, &owner, project, "TEST").await;
    let r = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("/v1/environments/{other}/push-credentials/{apns_ref}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("{path}/{apns_ref}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{}", r.json);
    assert!(vault
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|(m, _, p, _)| m == "DELETE" && p == "APNS_VOIP"));
    let list = c.req(As::User(&owner), Method::GET, &path, None).await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_push_providers").await,
        1
    );
}

#[tokio::test]
async fn push_credentials_fail_closed_without_the_vault_and_need_push_in_production() {
    let Some(c) = harness(all_on(), None).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (_, project, env) = workspace(&c, &owner).await;
    let body = json!({ "provider": "APNS", "teamId": "ABCDE12345", "keyId": "KEY1234567",
        "bundleId": "com.rider.app", "voip": false, "apnsHost": "SANDBOX", "p8": p8() });
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{env}/push-credentials"),
            Some(body.clone()),
        )
        .await;
    assert_eq!(
        (r.status, code(&r)),
        (StatusCode::SERVICE_UNAVAILABLE, "CAPABILITY_UNAVAILABLE")
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_push_providers").await,
        0
    );

    // PRODUCTION: only with the PUSH capability approved and LIVE.
    let vault = Arc::new(FakeVault::default());
    let Some(c) = harness(all_on(), Some(vault.clone())).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (org_a, project_a, _) = workspace(&c, &owner).await;
    let live = |caps: &'static [&'static str], project: Uuid| {
        let c = &c;
        let owner = owner.clone();
        async move {
            let req = c
                .req(
                    As::User(&owner),
                    Method::POST,
                    &format!("/v1/projects/{project}/production-access-requests"),
                    Some(json!({ "capabilities": caps, "useCaseSummary": "Rider calls" })),
                )
                .await;
            assert_eq!(req.status, StatusCode::CREATED, "{}", req.json);
            let rid = id(&req.json);
            let ok = c
                .operator_as(
                    Method::POST,
                    &format!("/internal/v1/operator/production-access-requests/{rid}/approve"),
                    None,
                    Some(Uuid::new_v4()),
                )
                .await;
            assert_eq!(ok.json["state"], "APPROVED", "{}", ok.json);
            let act = c
                .req(
                    As::User(&owner),
                    Method::POST,
                    &format!("/v1/production-access-requests/{rid}/activate"),
                    None,
                )
                .await;
            assert_eq!(act.json["state"], "LIVE", "{}", act.json);
            Uuid::parse_str(act.json["productionEnvironmentId"].as_str().unwrap()).unwrap()
        }
    };
    let prod_a = live(&["MEETINGS"], project_a).await;
    let mut prod_body = body.clone();
    prod_body["apnsHost"] = json!("PRODUCTION");
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{prod_a}/push-credentials"),
            Some(prod_body.clone()),
        )
        .await;
    assert_eq!(
        (r.status, code(&r)),
        (StatusCode::FORBIDDEN, "PRODUCTION_ACCESS_REQUIRED")
    );
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{org_a}/projects"),
            Some(json!({ "displayName": "Second app" })),
        )
        .await;
    let project_b = id(&p.json);
    environment(&c, &owner, project_b, "SANDBOX").await;
    let prod_b = live(&["PUSH", "END_USERS"], project_b).await;
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{prod_b}/push-credentials"),
            Some(prod_body),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["apnsHost"], "PRODUCTION");
    // END_USERS unlocked the Users API scopes for PRODUCTION.
    let grants = c.pa.app_scopes.lock().unwrap().clone();
    for s in ["principals.read", "principals.write", "user_tokens.issue"] {
        assert!(
            grants
                .get(&(project_b, s.to_string()))
                .is_some_and(|k| k.contains(&"PRODUCTION".to_string())),
            "{s}"
        );
    }
    let _ = project;
}

#[tokio::test]
async fn creation_caps_apply_only_when_enabled() {
    let caps = BuildFeatures {
        creation_caps: true,
        max_workspaces_per_owner: 2,
        max_projects_per_workspace: 2,
        ..Default::default()
    };
    let Some(c) = harness(caps, None).await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let create_org = |n: u32| {
        let c = &c;
        let owner = owner.clone();
        async move {
            c.req(
                As::User(&owner),
                Method::POST,
                "/v1/organizations",
                Some(json!({ "displayName": format!("Workspace {n}") })),
            )
            .await
        }
    };
    let first = create_org(1).await;
    assert_eq!(first.status, StatusCode::CREATED);
    let org = id(&first.json);
    assert_eq!(create_org(2).await.status, StatusCode::CREATED);
    let third = create_org(3).await;
    assert_eq!(
        (third.status, code(&third)),
        (StatusCode::CONFLICT, "LIMIT_EXCEEDED")
    );
    conforms(ERR, &third.json);
    // Another account has its own allowance; membership alone does not count.
    let (other, _) = c.sessions.sign_in("other@rider.example");
    let r = c
        .req(
            As::User(&other),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Other" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let other_org = id(&r.json);
    // Closing a workspace frees its place.
    let close = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{org}/close"),
            None,
        )
        .await;
    assert_eq!(close.status, StatusCode::OK, "{}", close.json);
    assert_eq!(create_org(4).await.status, StatusCode::CREATED);
    // Projects per workspace (ARCHIVED ones do not count).
    let ws = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "W" })),
        )
        .await;
    assert_eq!(ws.status, StatusCode::CONFLICT, "still two owned");
    let project = |n: u32| {
        let c = &c;
        let other = other.clone();
        async move {
            c.req(
                As::User(&other),
                Method::POST,
                &format!("/v1/organizations/{other_org}/projects"),
                Some(json!({ "displayName": format!("P{n}") })),
            )
            .await
        }
    };
    let p1 = project(1).await;
    assert_eq!(p1.status, StatusCode::CREATED);
    assert_eq!(project(2).await.status, StatusCode::CREATED);
    let p3 = project(3).await;
    assert_eq!(
        (p3.status, code(&p3)),
        (StatusCode::CONFLICT, "LIMIT_EXCEEDED")
    );
    let archived = c
        .req(
            As::User(&other),
            Method::POST,
            &format!("/v1/projects/{}/archive", id(&p1.json)),
            None,
        )
        .await;
    assert_eq!(archived.status, StatusCode::OK, "{}", archived.json);
    assert_eq!(project(4).await.status, StatusCode::CREATED);
}

#[tokio::test]
async fn message_sent_usage_is_accepted_from_messaging_only_and_shown_everywhere() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@rider.example");
    let (org, project, env) = workspace(&c, &owner).await;
    let fact = |source: &str| {
        cf_usage_facts::UsageFact {
            source_fact_id: format!("message:{source}"),
            usage_type: "MESSAGE_SENT".into(),
            unit: "OPERATION".into(),
            quantity: 1,
            tenant_id: org,
            application_id: project,
            environment_id: env,
            environment_kind: Some("SANDBOX".into()),
            resource_ref: format!("message:{source}"),
            occurred_at: chrono::Utc::now(),
            period_start: None,
            period_end: None,
            dimensions: [
                ("contextClass".to_string(), json!("DIRECT")),
                ("securityProfile".to_string(), json!("MANAGED")),
            ]
            .into(),
            digest: String::new(),
        }
        .normalized()
        .sealed()
    };
    let post = |signer: Arc<cf_service_auth::ServiceTokenSigner>,
                facts: Vec<cf_usage_facts::UsageFact>| {
        let c = &c;
        async move {
            c.internal(
                &signer,
                Method::POST,
                "/internal/v1/usage/facts",
                Some(json!({ "stream": "messaging.messages", "facts": facts,
                    "advanceTo": { "at": chrono::Utc::now(), "key": "k1" } })),
            )
            .await
        }
    };
    let a = Uuid::new_v4().to_string();
    let r = post(producers.calling.clone(), vec![fact(&a)]).await;
    assert_eq!(
        r.json["results"][0]["reasonCode"], "PRODUCER_NOT_ALLOWED",
        "{}",
        r.json
    );
    let r = post(
        producers.messaging.clone(),
        vec![fact(&a), fact(&Uuid::new_v4().to_string())],
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["recorded"], 2, "{}", r.json);
    conforms(&format!("{CTRL}IngestUsageFactsResponse"), &r.json);
    // Rollups, fact views and exports carry the new type.
    let s = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage"),
            None,
        )
        .await;
    assert_eq!(s.status, StatusCode::OK, "{}", s.json);
    conforms(&format!("{CTRL}UsageSummary"), &s.json);
    assert!(
        s.json["totals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["usageType"] == "MESSAGE_SENT" && t["quantity"] == 2),
        "{}",
        s.json
    );
    let f = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage/facts?usageType=MESSAGE_SENT"),
            None,
        )
        .await;
    assert_eq!(f.status, StatusCode::OK, "{}", f.json);
    conforms(&format!("{CTRL}UsageFactList"), &f.json);
    assert_eq!(f.json["items"].as_array().unwrap().len(), 2);
    for granularity in ["daily", "facts"] {
        let e = c
            .req(As::User(&owner), Method::GET,
                &format!("/v1/environments/{env}/usage/export?format=json&granularity={granularity}&usageType=MESSAGE_SENT"), None)
            .await;
        assert_eq!(e.status, StatusCode::OK, "{}", e.json);
        assert!(
            e.json["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|i| i["usageType"] == "MESSAGE_SENT"),
            "{}",
            e.json
        );
        assert!(
            !e.json["items"].as_array().unwrap().is_empty(),
            "{granularity}"
        );
    }
}
