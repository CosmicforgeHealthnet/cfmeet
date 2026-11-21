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
    // The request deletes at once when it can (`delete_now` after the commit), which moves
    // the artifact past version 2: refused at its CURRENT version, as already deleted
    // (410); at a stale version it would be a 409 VERSION_CONFLICT, nothing done.
    let av = h
        .app
        .scalar(&format!(
            "SELECT version::bigint FROM meeting_artifacts WHERE id = '{artifact}'"
        ))
        .await;
    assert!(av >= 2, "the request moved the version forward");
    let (status, v) = command(&h, &tpath, Uuid::new_v4(), json!({ "expectedVersion": av })).await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::GONE, "ARTIFACT_DELETED"),
        "already being deleted"
    );

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

// ---------------------------------------------------------------------------
// A7: accounts.user.export (R4), owner-delivered
// ---------------------------------------------------------------------------

/// Waits until the export built in the background (or builds it here) is past REQUESTED.
async fn export_built(h: &Harness, export_id: Uuid) -> String {
    for _ in 0..100 {
        backend::services::account_export::process(&h.app.state, export_id)
            .await
            .unwrap();
        let (status, v) = read(h, &format!("{BASE}/accounts/exports/{export_id}")).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        let state = v["state"].as_str().unwrap().to_string();
        if state != "REQUESTED" {
            return state;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the export was never built");
}

/// The single-use token of the download link emailed for `export_id`.
async fn emailed_token(h: &Harness, export_id: Uuid) -> (String, String) {
    let row = h
        .app
        .db
        .conn
        .query_one(Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "SELECT d.recipient, d.text_body FROM email_deliveries d \
               JOIN account_data_exports e ON e.email_delivery_id = d.id WHERE e.id = $1",
            [export_id.into()],
        ))
        .await
        .unwrap()
        .expect("the download link was queued");
    let recipient: String = row.try_get("", "recipient").unwrap();
    let text: String = row.try_get("", "text_body").unwrap();
    let marker = format!("/settings/data-export/download?export={export_id}&token=");
    let start = text.find(&marker).expect("the link names the export") + marker.len();
    let token: String = text[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    (recipient, token)
}

/// The owner's download (`GET /api/v2/me/data-export?exportId` + the link's token): (status,
/// raw body).
async fn download(
    h: &Harness,
    export_id: Uuid,
    bearer: &str,
    token: &str,
) -> (StatusCode, Vec<u8>) {
    let r = h
        .app
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(format!("/api/v2/me/data-export?exportId={export_id}"))
                .header("authorization", format!("Bearer {bearer}"))
                .header("x-cf-data-export-token", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = r.status();
    let bytes = axum::body::to_bytes(r.into_body(), 1 << 24).await.unwrap();
    (status, bytes.to_vec())
}

#[tokio::test]
async fn account_export_is_delivered_to_the_owner_only() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let mut owner = h.app.user_with_email("ada", "ada.export@example.com").await;
    let pre_session = owner.token.clone();
    h.exec(&format!(
        "UPDATE users SET email_verified_at = now() WHERE id = '{}'",
        owner.id
    ))
    .await;
    h.app.trust_device(&mut owner).await;
    let path = format!("{BASE}/accounts/users/{}/export", owner.id);
    let v1 = account_version(&h, owner.id).await;
    let body = json!({ "expectedVersion": v1, "reasonCode": "DATA_SUBJECT_REQUEST" });

    // refusals: nothing recorded
    let mut no_key = Opts::key(Uuid::new_v4());
    no_key.key = None;
    assert_eq!(
        command_on(&h, &h.router, &path, no_key, body.clone())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut no_approval = Opts::key(Uuid::new_v4());
    no_approval.approval = false;
    let (status, v) = command_on(&h, &h.router, &path, no_approval, body.clone()).await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "APPROVAL_REQUIRED")
    );
    let (status, v) = command(
        &h,
        &path,
        Uuid::new_v4(),
        json!({ "expectedVersion": v1 + 3 }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::CONFLICT, "VERSION_CONFLICT")
    );
    assert_eq!(
        command(
            &h,
            &path,
            Uuid::new_v4(),
            json!({ "expectedVersion": v1, "to": "x@y.z" })
        )
        .await
        .0,
        StatusCode::BAD_REQUEST,
        "no recipient can be named"
    );
    let mut own = Opts::key(Uuid::new_v4());
    own.linked_user = Some(owner.id);
    let (status, v) = command_on(&h, &h.router, &path, own, body.clone()).await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "SELF_ACTION_REFUSED")
    );
    let unverified = h.app.user_with_email("bob", "bob.export@example.com").await;
    let (status, v) = command(
        &h,
        &format!("{BASE}/accounts/users/{}/export", unverified.id),
        Uuid::new_v4(),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(
        (status, code_of(&v)),
        (StatusCode::UNPROCESSABLE_ENTITY, "STATE_CONFLICT"),
        "no verified address to deliver to"
    );
    let (status, _) = command_on(
        &h,
        &h.meetings_only,
        &path,
        Opts::key(Uuid::new_v4()),
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let mut headers = signed(&h.peer, &Method::POST, &path);
    headers.push(("x-cf-operator".into(), "hq:staff-0001".into()));
    headers.push(("idempotency-key".into(), Uuid::new_v4().to_string()));
    assert_eq!(
        send(&h.router, Method::POST, &path, headers, Some(body.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        h.app
            .scalar("SELECT count(*) FROM account_data_exports")
            .await,
        0
    );

    // applied: a reference only
    let key = Uuid::new_v4();
    let (status, v) = command(&h, &path, key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "REQUESTED");
    assert_eq!(v["version"], "1");
    let r = &v["resource"];
    assert_eq!(r["delivery"], "OWNER_DELIVERED");
    assert_eq!(r["deliveryChannel"], "DOWNLOAD_LINK_EMAIL");
    assert_eq!(r["recipientMasked"], "a***@e***.com");
    assert_eq!(r["userId"], json!(owner.id));
    assert!(r["deliveryRef"].as_str().is_some() && r["expiresAt"].as_str().is_some());
    let export_id = Uuid::parse_str(r["exportId"].as_str().unwrap()).unwrap();
    let text = v.to_string();
    assert!(!text.contains("ada.export@") && !text.contains("token") && !text.contains("http"));
    let (status, rec) = record(&h, key).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (rec["source_status"].clone(), rec["source_version"].clone()),
        (json!("REQUESTED"), json!("1"))
    );

    // built, sealed and the link emailed to the owner's verified address only
    assert_eq!(export_built(&h, export_id).await, "READY");
    let (status, state) = read(&h, &format!("{BASE}/accounts/exports/{export_id}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(state["sizeBytes"].as_i64().unwrap() > 0);
    let sha = state["sha256"].as_str().unwrap().to_string();
    assert_eq!(sha.len(), 64);
    assert!(!state.to_string().contains("ada.export@"));
    let (recipient, token) = emailed_token(&h, export_id).await;
    assert_eq!(recipient, "ada.export@example.com");
    assert_eq!(token.len(), 43);
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM account_data_exports WHERE id = '{export_id}' \
                   AND token_digest = encode(sha256('{token}'::bytea), 'hex') \
                   AND position('ada.export@'::bytea IN sealed) = 0"
            ))
            .await,
        1,
        "only the token's digest and a sealed copy are stored"
    );
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM meeting_admin_operations WHERE idempotency_key = '{key}' \
                   AND command = 'accounts.user.export' AND subject_id = '{}' \
                   AND resulting_state = 'REQUESTED' AND operator_ref = 'hq:staff-0001' \
                   AND approver_ref = 'hq:staff-0002'",
                owner.id
            ))
            .await,
        1
    );
    let (status, list) = read(
        &h,
        &format!("{BASE}/accounts/users/{}/exports?limit=1", owner.id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["exportId"], json!(export_id));
    assert_eq!(list["items"][0]["state"], "READY");
    assert!(list.get("nextCursor").is_some());
    assert_eq!(
        read(
            &h,
            &format!("{BASE}/accounts/users/{}/exports", Uuid::new_v4())
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        read(&h, &format!("{BASE}/accounts/exports/{}", Uuid::new_v4()))
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    // replay: same answer, applied once
    let (status, again) = command(&h, &path, key, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(
        (again["status"].clone(), again["version"].clone()),
        (json!("REQUESTED"), json!("1"))
    );
    assert_eq!(again["resource"]["exportId"], json!(export_id));
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM account_data_exports WHERE user_id = '{}'",
                owner.id
            ))
            .await,
        1
    );

    // the owner downloads it: own recent sign-in, the right token, once
    assert_eq!(
        download(&h, export_id, &pre_session, &token).await.0,
        StatusCode::FORBIDDEN,
        "a sign-in without a session must sign in again"
    );
    let stranger = {
        let mut s = h.app.user_with_email("eve", "eve.export@example.com").await;
        h.app.trust_device(&mut s).await;
        s
    };
    assert_eq!(
        download(&h, export_id, &stranger.token, &token).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        download(&h, export_id, &owner.token, "not-the-token")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = h
        .app
        .get(
            &format!("/api/v2/me/data-export?exportId={export_id}"),
            Auth::Bearer(&owner.token),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the token goes with the export id"
    );
    let (status, live) = h
        .app
        .get("/api/v2/me/data-export", Auth::Bearer(&owner.token))
        .await;
    assert_eq!(status, StatusCode::OK, "the live export is unchanged");
    assert_eq!(live["account"]["email"], "ada.export@example.com");
    let (status, bytes) = download(&h, export_id, &owner.token, &token).await;
    assert_eq!(status, StatusCode::OK);
    {
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex::encode(Sha256::digest(&bytes)),
            sha,
            "the digest HQ sees"
        );
    }
    let doc: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(doc["account"]["email"], "ada.export@example.com");
    assert_eq!(
        download(&h, export_id, &owner.token, &token).await.0,
        StatusCode::NOT_FOUND,
        "single use"
    );
    let (_, state) = read(&h, &format!("{BASE}/accounts/exports/{export_id}")).await;
    assert_eq!(state["state"], "DELIVERED");
    assert_eq!(
        h.app
            .scalar(&format!(
                "SELECT count(*) FROM account_data_exports WHERE id = '{export_id}' \
                   AND sealed IS NULL AND token_digest IS NULL AND delivered_at IS NOT NULL"
            ))
            .await,
        1,
        "the copy is purged once downloaded"
    );

    // retention: an export not downloaded within 7 days is purged and its link dies
    let v2 = account_version(&h, owner.id).await;
    let (status, v) = command(&h, &path, Uuid::new_v4(), json!({ "expectedVersion": v2 })).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let late = Uuid::parse_str(v["resource"]["exportId"].as_str().unwrap()).unwrap();
    assert_eq!(export_built(&h, late).await, "READY");
    let (_, late_token) = emailed_token(&h, late).await;
    h.exec(&format!(
        "UPDATE account_data_exports SET expires_at = now() - interval '1 second' WHERE id = '{late}'"
    ))
    .await;
    let swept = backend::services::account_export::sweep(&h.app.state)
        .await
        .unwrap();
    assert!(swept.expired >= 1);
    let (_, state) = read(&h, &format!("{BASE}/accounts/exports/{late}")).await;
    assert_eq!(state["state"], "EXPIRED");
    assert!(state["purgedAt"].as_str().is_some());
    assert_eq!(
        download(&h, late, &owner.token, &late_token).await.0,
        StatusCode::NOT_FOUND
    );
}

// ---------------------------------------------------------------------------
// A7: the daily dataset pull
// ---------------------------------------------------------------------------

async fn dataset(h: &Harness, router: &Router, path: &str) -> (StatusCode, Value) {
    let mut headers = signed(&h.bridge, &Method::GET, path);
    headers.push(("x-cf-operator".into(), "hq:dataset-sync".into()));
    send(router, Method::GET, path, headers, None).await
}

#[tokio::test]
async fn datasets_answer_one_utc_day_of_counts() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let today = chrono::Utc::now()
        .date_naive()
        .format("%Y-%m-%d")
        .to_string();
    let owner = h.app.user("dataset-host").await;
    let guest = h.app.user("dataset-guest").await;
    let (_, conference_id) = h
        .app
        .live_conference(
            &owner,
            json!({ "title": "Counted", "accessPolicy": "PUBLIC" }),
        )
        .await;
    let (status, v) = h
        .app
        .join(conference_id, Auth::Bearer(&guest.token), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    h.exec(&format!(
        "UPDATE participant_sessions SET state = 'CONNECTED', connected_at = GREATEST(now() - interval '5 minutes', (date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')) \
          WHERE conference_id = '{conference_id}'"
    ))
    .await;

    let (status, e) = dataset(
        &h,
        &h.router,
        &format!("{BASE}/datasets/cfmeet.engagement_daily/days/{today}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{e}");
    assert_eq!(e["datasetId"], "cfmeet.engagement_daily");
    assert_eq!(e["date"], json!(today));
    let m = e["metrics"].as_object().unwrap();
    assert_eq!(
        m.keys().cloned().collect::<Vec<_>>(),
        ["dau", "mau", "signups"]
    );
    assert!(m["signups"].as_i64().unwrap() >= 2, "{e}");
    assert!(m["dau"].as_i64().unwrap() >= 1, "the connected guest: {e}");
    assert!(m["mau"].as_i64().unwrap() >= m["dau"].as_i64().unwrap());

    let (status, d) = dataset(
        &h,
        &h.router,
        &format!("{BASE}/datasets/cfmeet.meetings_daily/days/{today}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{d}");
    let m = d["metrics"].as_object().unwrap();
    assert_eq!(
        m.keys().cloned().collect::<Vec<_>>(),
        ["meetings", "participant_minutes", "peak_live_meetings"]
    );
    assert!(m["meetings"].as_i64().unwrap() >= 1, "{d}");
    let midnight = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let since_midnight = (chrono::Utc::now() - midnight).num_minutes();
    assert!(m["peak_live_meetings"].as_i64().unwrap() >= 1, "{d}");
    assert!(
        m["participant_minutes"].as_i64().unwrap() >= 4.min(since_midnight - 1).max(0),
        "{d}"
    );
    assert!(
        !d.to_string().contains(&conference_id.to_string()),
        "counts only"
    );

    // a day before any data: computed zeros, not nulls
    let (status, old) = dataset(
        &h,
        &h.router,
        &format!("{BASE}/datasets/cfmeet.meetings_daily/days/2001-01-01"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(old["metrics"]["meetings"], 0);
    assert_eq!(old["metrics"]["peak_live_meetings"], 0);

    // refusals
    for (path, expected) in [
        (
            format!("{BASE}/datasets/cfmeet.meetings_daily/days/2999-01-01"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("{BASE}/datasets/cfmeet.meetings_daily/days/2026-13-01"),
            StatusCode::BAD_REQUEST,
        ),
        (
            format!("{BASE}/datasets/cfmeet.messaging_daily/days/{today}"),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(dataset(&h, &h.router, &path).await.0, expected, "{path}");
    }
    assert_eq!(
        dataset(
            &h,
            &h.meetings_only,
            &format!("{BASE}/datasets/cfmeet.engagement_daily/days/{today}")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        dataset(
            &h,
            &h.accounts_only,
            &format!("{BASE}/datasets/cfmeet.meetings_daily/days/{today}")
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let path = format!("{BASE}/datasets/cfmeet.meetings_daily/days/{today}");
    let mut headers = signed(&h.peer, &Method::GET, &path);
    headers.push(("x-cf-operator".into(), "hq:dataset-sync".into()));
    assert_eq!(
        send(&h.router, Method::GET, &path, headers, None).await.0,
        StatusCode::FORBIDDEN
    );
    // no operator attribution
    assert_eq!(
        send(
            &h.router,
            Method::GET,
            &path,
            signed(&h.bridge, &Method::GET, &path),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}
