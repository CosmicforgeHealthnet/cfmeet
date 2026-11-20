//! ADR 0068, phases A4–A7 — the admin bridge's COMMANDS and further reads served by the
//! Meeting Service (`/internal/v1/operator/*`, contract
//! `meeting-service/admin-operator/v1.openapi.json` 1.1.0).
//!
//! Every command: only `cfmeet.admin-bridge` with the route's permission and a named HQ
//! operator; `Idempotency-Key` required; the second person required (R3 / R4); the change
//! only at `expectedVersion`; recorded in `meeting_admin_operations` in the same
//! transaction; a replay answers from the record; the settlement read answers the same
//! status and version.

mod common;

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use backend::services::admin_operator as op;
use cf_service_auth::{
    KeyId, OutboundServiceRequest, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::*;
use sea_orm::{ConnectionTrait, Statement};
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const BASE: &str = "/internal/v1/operator";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn signer(id: ServiceId, kid: &str) -> (ServiceTokenSigner, ServiceSigningKey) {
    let key = ServiceSigningKey::generate(KeyId::parse(kid).unwrap());
    let public =
        ServiceSigningKey::from_pkcs8_pem(KeyId::parse(kid).unwrap(), &key.to_pkcs8_pem()).unwrap();
    (
        ServiceTokenSigner::new(
            id,
            RuntimeEnvironment::Test,
            key,
            TokenTtl::default(),
            Arc::new(SystemClock),
        ),
        public,
    )
}

fn verifier(issuers: &[(&str, &ServiceSigningKey, &[&str])]) -> Arc<ServiceTokenVerifier> {
    let trusted: Vec<Value> = issuers
        .iter()
        .map(|(id, k, perms)| {
            json!({ "issuer": id, "audience": "cfmeet.meeting-service", "runtime_environment": "test",
                    "keys": [k.public_jwk()], "permissions": perms })
        })
        .collect();
    let bundle = json!({ "bundle_version": 1, "receiver": "cfmeet.meeting-service",
                         "runtime_environment": "test", "trusted_issuers": trusted });
    let policy = TrustPolicy::from_json(
        &bundle.to_string(),
        &ServiceId::meeting_service(),
        RuntimeEnvironment::Test,
    )
    .unwrap();
    Arc::new(ServiceTokenVerifier::new(policy, Arc::new(SystemClock)))
}

struct Harness {
    app: TestApp,
    router: Router,
    bridge: ServiceTokenSigner,
    /// The bridge granted only `meeting.administer`.
    meetings_only: Router,
    /// The bridge granted only `account.administer`.
    accounts_only: Router,
    peer: ServiceTokenSigner,
}

impl Harness {
    async fn start() -> Option<Self> {
        let app = TestApp::new().await?;
        let (bridge, b_pub) = signer(ServiceId::admin_bridge(), "admin-bridge-test-1");
        let (peer, p_pub) = signer(ServiceId::organizations(), "organizations-test-1");
        let both: &[&str] = &["account.administer", "meeting.administer"];
        let router = op::router(
            app.state.clone(),
            verifier(&[
                ("cfmeet.admin-bridge", &b_pub, both),
                (
                    "cfmeet.organizations",
                    &p_pub,
                    &["meeting-organization-events.apply"],
                ),
            ]),
        );
        let meetings_only = op::router(
            app.state.clone(),
            verifier(&[("cfmeet.admin-bridge", &b_pub, &["meeting.administer"])]),
        );
        let accounts_only = op::router(
            app.state.clone(),
            verifier(&[("cfmeet.admin-bridge", &b_pub, &["account.administer"])]),
        );
        Some(Self {
            app,
            router,
            bridge,
            meetings_only,
            accounts_only,
            peer,
        })
    }

    async fn exec(&self, sql: &str) {
        self.app
            .db
            .conn
            .execute(Statement::from_string(
                sea_orm::DbBackend::Postgres,
                sql.to_string(),
            ))
            .await
            .unwrap();
    }
}

fn signed(signer: &ServiceTokenSigner, method: &Method, path: &str) -> Vec<(String, String)> {
    let bare = path.split('?').next().unwrap();
    signer
        .sign_request(
            &OutboundServiceRequest::new_operation(ServiceId::meeting_service()),
            method,
            bare,
        )
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
        .collect()
}

async fn send(
    router: &Router,
    method: Method,
    path: &str,
    headers: Vec<(String, String)>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        b = b.header(k, v);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let r = router.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// How a command is sent.
#[derive(Clone, Copy)]
struct Opts {
    key: Option<Uuid>,
    approval: bool,
    linked_user: Option<Uuid>,
}

impl Opts {
    fn key(key: Uuid) -> Self {
        Self {
            key: Some(key),
            approval: true,
            linked_user: None,
        }
    }
}

async fn command_on(
    h: &Harness,
    router: &Router,
    path: &str,
    o: Opts,
    body: Value,
) -> (StatusCode, Value) {
    let mut headers = signed(&h.bridge, &Method::POST, path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    if o.approval {
        headers.push(("x-cf-approver".into(), "hq:staff-0002".into()));
        headers.push(("x-cf-approval-ref".into(), "approval-1".into()));
        headers.push(("x-cf-intent-digest".into(), DIGEST.into()));
    }
    if let Some(k) = o.key {
        headers.push(("idempotency-key".into(), k.to_string()));
    }
    if let Some(u) = o.linked_user {
        headers.push(("x-cf-operator-user-id".into(), u.to_string()));
    }
    send(router, Method::POST, path, headers, Some(body)).await
}

async fn command(h: &Harness, path: &str, key: Uuid, body: Value) -> (StatusCode, Value) {
    command_on(h, &h.router, path, Opts::key(key), body).await
}

async fn read(h: &Harness, path: &str) -> (StatusCode, Value) {
    let mut headers = signed(&h.bridge, &Method::GET, path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    send(&h.router, Method::GET, path, headers, None).await
}

async fn record(h: &Harness, key: Uuid) -> (StatusCode, Value) {
    read(h, &format!("{BASE}/operations/{key}")).await
}

async fn account_version(h: &Harness, user: Uuid) -> i64 {
    let (status, d) = read(h, &format!("{BASE}/accounts/users/{user}")).await;
    assert_eq!(status, StatusCode::OK, "{d}");
    d["version"].as_i64().unwrap()
}

fn code_of(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn account_status_follows_the_owner_contract() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let u = h.app.user_with_email("grace", "grace@example.com").await;
    let path = format!("{BASE}/accounts/users/{}/status", u.id);
    let v1 = account_version(&h, u.id).await;
    let body =
        json!({ "expectedVersion": v1, "status": "SUSPENDED", "reasonCode": "FRAUD_REVIEW" });

    // no key, no second person, unknown field, stale version: refused, nothing done
    let mut no_key = Opts::key(Uuid::new_v4());
    no_key.key = None;
    let (status, v) = command_on(&h, &h.router, &path, no_key, body.clone()).await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::BAD_REQUEST, "VALIDATION_ERROR")
    );
    let mut no_approval = Opts::key(Uuid::new_v4());
    no_approval.approval = false;
    let (status, v) = command_on(&h, &h.router, &path, no_approval, body.clone()).await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "APPROVAL_REQUIRED")
    );
    let (status, _) = command(
        &h,
        &path,
        Uuid::new_v4(),
        json!({ "expectedVersion": v1, "status": "SUSPENDED", "note": "free text" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, v) = command(
        &h,
        &path,
        Uuid::new_v4(),
        json!({ "expectedVersion": v1 + 7, "status": "SUSPENDED" }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::CONFLICT, "VERSION_CONFLICT")
    );
    assert_eq!(account_version(&h, u.id).await, v1, "nothing changed");

    // applied
    let key = Uuid::new_v4();
    let (status, v) = command(&h, &path, key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "SUSPENDED");
    assert_eq!(v["version"], json!((v1 + 1).to_string()));
    assert_eq!(v["resource"]["userId"], json!(u.id));
    assert_eq!(v["resource"]["accountStatus"], "SUSPENDED");
    assert!(
        !v.to_string().contains("grace@"),
        "no address in the answer"
    );

    // replay: the same answer, applied once
    let (status, again) = command(&h, &path, key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        (again["status"].clone(), again["version"].clone()),
        (v["status"].clone(), v["version"].clone())
    );
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_admin_operations WHERE idempotency_key = '{key}'"
            ))
            .await,
        1
    );
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM account_security_events WHERE user_id = '{}' AND event_type = 'ACCOUNT_STATUS_CHANGED'",
                u.id
            ))
            .await,
        1
    );
    // the same key for another command: conflict
    let (status, v) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/role", u.id),
        key,
        json!({ "expectedVersion": v1 + 1, "role": "GUEST" }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT")
    );

    // the settlement record answers the same status and version
    let (status, r) = record(&h, key).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["outcome"], "APPLIED");
    assert_eq!(r["source_status"], "SUSPENDED");
    assert_eq!(r["source_version"], json!((v1 + 1).to_string()));
    assert_eq!(record(&h, Uuid::new_v4()).await.0, StatusCode::NOT_FOUND);
    // the record is append-only and names both people
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_admin_operations WHERE idempotency_key = '{key}' \
                 AND operator_ref = 'hq:staff-0001' AND approver_ref = 'hq:staff-0002' \
                 AND command = 'accounts.user.status.change' AND subject_kind = 'USER'"
            ))
            .await,
        1
    );

    // unknown account: 404, nothing recorded
    let ghost = Uuid::new_v4();
    let gkey = Uuid::new_v4();
    let (status, _) = command(
        &h,
        &format!("{BASE}/accounts/users/{ghost}/status"),
        gkey,
        json!({ "expectedVersion": 1, "status": "ACTIVE" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(record(&h, gkey).await.0, StatusCode::NOT_FOUND);

    // staff never administer their own linked account
    let mut own = Opts::key(Uuid::new_v4());
    own.linked_user = Some(u.id);
    let (status, v) = command_on(
        &h,
        &h.router,
        &path,
        own,
        json!({ "expectedVersion": v1 + 1, "status": "ACTIVE" }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "SELF_ACTION_REFUSED")
    );
}

#[tokio::test]
async fn account_commands_change_role_email_sessions_mfa_and_delete() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let u = h.app.user_with_email("ada", "ada@example.com").await;
    let v = account_version(&h, u.id).await;

    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/role", u.id),
        Uuid::new_v4(),
        json!({ "expectedVersion": v, "role": "GUEST" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(
        (r["status"].as_str(), r["version"].as_str()),
        (Some("GUEST"), Some((v + 1).to_string().as_str()))
    );

    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/email", u.id),
        Uuid::new_v4(),
        json!({ "expectedVersion": v + 1, "newEmail": "ada.new@example.org", "reasonCode": "LOST_MAILBOX" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "EMAIL_CHANGED");
    assert_eq!(r["version"], json!((v + 2).to_string()));
    assert!(
        !r.to_string().contains("ada.new@"),
        "the new address is never echoed"
    );
    assert_eq!(r["resource"]["emailVerified"], false);

    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/sessions/revoke-all", u.id),
        Uuid::new_v4(),
        json!({ "reasonCode": "ACCOUNT_TAKEOVER" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(
        (r["status"].as_str(), r["version"].clone()),
        (Some("REVOKED"), Value::Null)
    );
    assert_eq!(r["resource"]["openSessions"], 0);
    // unversioned commands take no expectedVersion
    let (status, _) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/sessions/revoke-all", u.id),
        Uuid::new_v4(),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/mfa/reset", u.id),
        Uuid::new_v4(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "NOT_ENROLLED");
    assert_eq!(r["resource"]["mfaEnabled"], false);

    let (status, _) = command(
        &h,
        &format!(
            "{BASE}/accounts/users/{}/devices/{}/revoke",
            u.id,
            Uuid::new_v4()
        ),
        Uuid::new_v4(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/duplicates/enforce"),
        Uuid::new_v4(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert!(
        ["ENFORCED", "PARTIALLY_ENFORCED", "NOT_ENFORCED"].contains(&r["status"].as_str().unwrap())
    );

    // deletion: the owner's own path, at the expected version
    let v = account_version(&h, u.id).await;
    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/delete", u.id),
        Uuid::new_v4(),
        json!({ "expectedVersion": v - 1 }),
    )
    .await;
    assert_eq!(
        (status, code_of(&r)),
        (StatusCode::CONFLICT, "VERSION_CONFLICT")
    );
    let key = Uuid::new_v4();
    let (status, r) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/delete", u.id),
        key,
        json!({ "expectedVersion": v }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "DELETED");
    assert_eq!(r["resource"]["accountStatus"], "INACTIVE");
    assert_eq!(record(&h, key).await.1["source_status"], "DELETED");
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM account_privacy_jobs WHERE user_id = '{}'",
                u.id
            ))
            .await,
        3,
        "the erasure jobs of the owner's own deletion"
    );
}

#[tokio::test]
async fn account_commands_need_the_bridge_and_account_administer() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let u = h.app.user("linus").await;
    let path = format!("{BASE}/accounts/users/{}/status", u.id);
    let body = json!({ "expectedVersion": 1, "status": "SUSPENDED" });
    // the bridge without account.administer
    let (status, v) = command_on(
        &h,
        &h.meetings_only,
        &path,
        Opts::key(Uuid::new_v4()),
        body.clone(),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    // another service naming an operator
    let mut headers = signed(&h.peer, &Method::POST, &path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    headers.push(("idempotency-key".into(), Uuid::new_v4().to_string()));
    let (status, _) = send(&h.router, Method::POST, &path, headers, Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // no token
    let (status, _) = send(
        &h.router,
        Method::POST,
        &path,
        vec![("x-cf-operator".into(), "hq:staff-0001".into())],
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // the settlement record answers nobody but the bridge
    let key = Uuid::new_v4();
    let rpath = format!("{BASE}/operations/{key}");
    let (status, _) = send(
        &h.router,
        Method::GET,
        &rpath,
        signed(&h.peer, &Method::GET, &rpath),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT account_version::bigint FROM users WHERE id = '{}'",
                u.id
            ))
            .await,
        1
    );
}

#[tokio::test]
async fn meeting_commands_end_archive_transfer_and_delete_artifacts() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let owner = h.app.user("owner").await;
    let heir = h.app.user("heir").await;
    let (space_id, conference_id) = h
        .app
        .live_conference(&owner, json!({ "title": "Weekly" }))
        .await;
    let cv = h
        .app
        .scalar(&format!(
            "SELECT version::bigint FROM conference_sessions WHERE id = '{conference_id}'"
        ))
        .await;

    // end: stale version refused, then ENDING at the conference's version
    let end = format!("{BASE}/meetings/conferences/{conference_id}/end");
    let (status, v) = command(
        &h,
        &end,
        Uuid::new_v4(),
        json!({ "expectedVersion": cv + 5 }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::CONFLICT, "VERSION_CONFLICT")
    );
    let key = Uuid::new_v4();
    let (status, v) = command(&h, &end, key, json!({ "expectedVersion": cv })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ENDING");
    assert_eq!(v["version"], json!((cv + 1).to_string()));
    assert_eq!(record(&h, key).await.1["source_status"], "ENDING");
    // ending again: a definite refusal
    let cv = h
        .app
        .scalar(&format!(
            "SELECT version::bigint FROM conference_sessions WHERE id = '{conference_id}'"
        ))
        .await;
    let (status, _) = command(&h, &end, Uuid::new_v4(), json!({ "expectedVersion": cv })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // the bridge without approval
    let mut no_approval = Opts::key(Uuid::new_v4());
    no_approval.approval = false;
    let (status, _) = command_on(
        &h,
        &h.router,
        &end,
        no_approval,
        json!({ "expectedVersion": cv }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // transcript deletion (R4)
    let artifact = Uuid::new_v4();
    h.exec(&format!(
        "INSERT INTO meeting_artifacts (id, meeting_space_id, conference_id, artifact_type, status) \
         VALUES ('{artifact}', '{space_id}', '{conference_id}', 'TRANSCRIPT', 'FAILED')"
    ))
    .await;
    // not a recording
    let (status, _) = command(
        &h,
        &format!("{BASE}/meetings/conferences/{conference_id}/recordings/{artifact}/delete"),
        Uuid::new_v4(),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let tpath =
        format!("{BASE}/meetings/conferences/{conference_id}/transcripts/{artifact}/delete");
    let key = Uuid::new_v4();
    let (status, v) = command(&h, &tpath, key, json!({ "expectedVersion": 1 })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["status"].as_str(), v["version"].as_str()),
        (Some("DELETE_REQUESTED"), Some("2"))
    );
    let (status, _) = command(&h, &tpath, Uuid::new_v4(), json!({ "expectedVersion": 2 })).await;
    assert_eq!(status, StatusCode::GONE, "already being deleted");

    // archive at the space's version
    let sv = h
        .app
        .scalar(&format!(
            "SELECT version::bigint FROM meeting_spaces WHERE id = '{space_id}'"
        ))
        .await;
    let (status, v) = command(
        &h,
        &format!("{BASE}/meetings/spaces/{space_id}/archive"),
        Uuid::new_v4(),
        json!({ "expectedVersion": sv }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ARCHIVED");
    assert_eq!(v["resource"]["spaceStatus"], "ARCHIVED");

    // transfer another space
    let other = h
        .app
        .create_space(&owner, json!({ "title": "Board" }))
        .await;
    let other_id = uuid(&other["id"]);
    let ov = other["version"].as_i64().unwrap();
    let (status, v) = command(
        &h,
        &format!("{BASE}/meetings/spaces/{other_id}/owner-transfer"),
        Uuid::new_v4(),
        json!({ "expectedVersion": ov, "newOwnerUserId": heir.id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "TRANSFERRED");
    assert_eq!(v["resource"]["ownerUserId"], json!(heir.id));
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM conference_audit_events WHERE meeting_space_id = '{other_id}' \
                 AND actor_type = 'OPERATOR' AND actor_ref = 'hq:staff-0001'"
            ))
            .await,
        1
    );

    // chat migration without Unified Messaging; an unknown CRITICAL decision
    let (status, v) = command(
        &h,
        &format!("{BASE}/meetings/chat-migration/transitions"),
        Uuid::new_v4(),
        json!({ "expectedVersion": 1, "targetState": "SHADOW_VALIDATION" }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "FEATURE_DISABLED")
    );
    let (status, _) = command(
        &h,
        &format!(
            "{BASE}/meetings/critical-lane/decisions/{}/reroute",
            Uuid::new_v4()
        ),
        Uuid::new_v4(),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // meeting commands need meeting.administer: the account-only bridge is refused
    let (status, v) = command_on(
        &h,
        &h.accounts_only,
        &end,
        Opts::key(Uuid::new_v4()),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
}

#[tokio::test]
async fn the_new_reads_answer_ids_states_and_counts() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let owner = h.app.user("host").await;
    let (space_id, conference_id) = h
        .app
        .live_conference(&owner, json!({ "title": "Weekly" }))
        .await;
    for path in [
        format!("{BASE}/accounts/test-accounts"),
        format!("{BASE}/meetings/conferences/{conference_id}/recordings"),
        format!("{BASE}/meetings/conferences/{conference_id}/transcripts"),
        format!("{BASE}/meetings/webinars"),
        format!("{BASE}/meetings/webinars/{space_id}/registrations"),
    ] {
        let (status, v) = read(&h, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {v}");
        assert!(
            v["items"].is_array() && v.get("nextCursor").is_some(),
            "{path}"
        );
    }
    for path in [
        format!("{BASE}/accounts/duplicates"),
        format!("{BASE}/meetings/conferences/{conference_id}/health"),
        format!("{BASE}/meetings/critical-lane"),
    ] {
        let (status, v) = read(&h, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {v}");
    }
    let (status, d) = read(&h, &format!("{BASE}/accounts/duplicates")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(d["email"]["enforced"].is_boolean());
    // without Unified Messaging
    assert_eq!(
        read(&h, &format!("{BASE}/meetings/chat-migration")).await.0,
        StatusCode::NOT_FOUND
    );
    let ghost = Uuid::new_v4();
    for path in [
        format!("{BASE}/meetings/conferences/{ghost}/health"),
        format!("{BASE}/meetings/conferences/{ghost}/recordings"),
        format!("{BASE}/meetings/conferences/{ghost}/transcripts"),
        format!("{BASE}/meetings/webinars/{ghost}/registrations"),
        format!("{BASE}/meetings/email-deliveries/{ghost}"),
    ] {
        assert_eq!(read(&h, &path).await.0, StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(
        read(&h, &format!("{BASE}/meetings/webinars?limit=0"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    // the meetings-only bridge cannot read accounts
    let path = format!("{BASE}/accounts/test-accounts");
    let mut headers = signed(&h.bridge, &Method::GET, &path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    assert_eq!(
        send(&h.meetings_only, Method::GET, &path, headers, None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
