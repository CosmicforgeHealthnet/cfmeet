//! Part VI Chapters 10–13 (Sections 54–57) through the REAL router with REAL service
//! tokens, against the in-memory Platform Access (which mirrors its delegation rules).
//! The real Platform Access process is exercised by the certification journeys.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::config::FeatureFlags;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";

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
    let e = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{p}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    (o, p, id(&e.json))
}

async fn join(
    c: &Control,
    owner: &str,
    org: Uuid,
    email: &str,
    role: &str,
) -> (String, Uuid, Uuid) {
    let (token, user) = c.sessions.sign_in(email);
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
    (token, user, id(&acc.json))
}

async fn service_account(c: &Control, who: As<'_>, env: Uuid, name: &str) -> Uuid {
    let r = c
        .req(
            who,
            Method::POST,
            &format!("/v1/environments/{env}/service-accounts"),
            Some(json!({ "name": name })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    id(&r.json)
}

async fn grant(c: &Control, owner: &str, project: Uuid, scope: &str) -> Value {
    let r = c
        .req(
            As::User(owner),
            Method::POST,
            &format!("/v1/projects/{project}/scopes"),
            Some(json!({ "scope": scope })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{scope}: {}", r.json);
    conforms(&format!("{CTRL}ScopeGrant"), &r.json);
    r.json
}

#[tokio::test]
async fn credentials_are_self_service_with_the_secret_exactly_once() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let (dev, _, _) = join(&c, &owner, org, "dev@acme.example", "DEVELOPER").await;
    let (analyst, _, _) = join(&c, &owner, org, "analyst@acme.example", "ANALYST").await;
    for s in [
        "calls.read",
        "calls.write",
        "credentials.manage",
        "usage.read",
    ] {
        let g = grant(&c, &owner, project, s).await;
        assert_eq!(
            g["environmentKinds"],
            json!(["SANDBOX", "TEST"]),
            "no PRODUCTION without approval"
        );
    }
    let sa = service_account(&c, As::User(&owner), env, "backend").await;
    let path = format!("/v1/environments/{env}/service-accounts/{sa}/credentials");
    // A DEVELOPER creates a credential; the secret is in this response only.
    let body = json!({ "scopes": ["calls.read", "credentials.manage"], "label": "ci" });
    let created = c
        .req_with(
            As::User(&dev),
            Method::POST,
            &path,
            Some(body.clone()),
            &[("idempotency-key", "cred-1")],
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.json);
    conforms(&format!("{CTRL}IssuedCredential"), &created.json);
    assert!(created.json["secret"]
        .as_str()
        .unwrap()
        .starts_with("cfmk_"));
    assert_eq!(created.headers.get("cache-control").unwrap(), "no-store");
    let cred = id(&created.json["credential"]);
    // An idempotent replay NEVER shows the secret again.
    let replay = c
        .req_with(
            As::User(&dev),
            Method::POST,
            &path,
            Some(body),
            &[("idempotency-key", "cred-1")],
        )
        .await;
    assert_eq!(replay.status, StatusCode::CREATED);
    assert_eq!(replay.headers.get("idempotent-replayed").unwrap(), "true");
    assert!(replay.json.get("secret").is_none(), "{}", replay.json);
    assert_eq!(replay.json["secretRedacted"], true);
    assert_eq!(id(&replay.json["credential"]), cred);
    let secret = created.json["secret"].as_str().unwrap().to_string();
    let stored = c
        .count(&format!("SELECT count(*) FROM devplat_idempotency_records WHERE response_body::text LIKE '%{secret}%'"))
        .await;
    assert_eq!(
        stored, 0,
        "no secret is ever stored by the Developer Platform"
    );
    // Metadata only afterwards.
    let list = c.req(As::User(&dev), Method::GET, &path, None).await;
    assert_eq!(list.status, StatusCode::OK);
    conforms(&format!("{CTRL}CredentialList"), &list.json);
    assert!(!list.json.to_string().contains(&secret));
    // ANALYST has no credential authority; a scope the project was never allowed is refused.
    assert_eq!(
        c.req(As::User(&analyst), Method::GET, &path, None)
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    let bad = c
        .req(
            As::User(&dev),
            Method::POST,
            &path,
            Some(json!({ "scopes": ["numbers.search"] })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.json);
    // A machine credential (credentials.manage) cannot grant more than it holds.
    let machine = c.pa.machine_of(cred);
    let esc = c
        .req(
            As::Machine(&machine),
            Method::POST,
            &path,
            Some(json!({ "scopes": ["calls.write"] })),
        )
        .await;
    assert_eq!(esc.status, StatusCode::FORBIDDEN, "{}", esc.json);
    assert_eq!(esc.json["error"]["code"], "INSUFFICIENT_SCOPE");
    let sub = c
        .req(
            As::Machine(&machine),
            Method::POST,
            &path,
            Some(json!({ "scopes": ["calls.read"] })),
        )
        .await;
    assert_eq!(sub.status, StatusCode::CREATED, "{}", sub.json);
    // Rotation: a successor with a fresh one-time secret.
    let rot = c
        .req(
            As::User(&dev),
            Method::POST,
            &format!("/v1/environments/{env}/credentials/{cred}/rotate"),
            Some(json!({ "overlapMinutes": 5 })),
        )
        .await;
    assert_eq!(rot.status, StatusCode::CREATED, "{}", rot.json);
    conforms(&format!("{CTRL}IssuedCredential"), &rot.json);
    let successor = id(&rot.json["credential"]);
    assert_ne!(successor, cred);
    // Suspension is immediate and reversible; revocation is immediate and final.
    let susp = c
        .req(
            As::User(&dev),
            Method::POST,
            &format!("/v1/environments/{env}/credentials/{successor}/suspend"),
            Some(json!({ "reasonCode": "INCIDENT_RESPONSE" })),
        )
        .await;
    assert_eq!(susp.status, StatusCode::OK, "{}", susp.json);
    conforms(&format!("{CTRL}Credential"), &susp.json);
    assert_eq!(susp.json["effectiveState"], "SUSPENDED");
    let m2 = c.pa.machine_of(successor);
    assert_eq!(
        c.req(
            As::Machine(&m2),
            Method::GET,
            &format!("/v1/organizations/{org}"),
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED,
        "a suspended credential is refused at once"
    );
    let res = c
        .req(
            As::User(&dev),
            Method::POST,
            &format!("/v1/environments/{env}/credentials/{successor}/resume"),
            None,
        )
        .await;
    assert_eq!(res.json["effectiveState"], "ACTIVE", "{}", res.json);
    let rev = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{env}/credentials/{successor}/revoke"),
            None,
        )
        .await;
    assert_eq!(rev.json["effectiveState"], "REVOKED", "{}", rev.json);
    assert_eq!(
        c.req(
            As::Machine(&m2),
            Method::GET,
            &format!("/v1/organizations/{org}"),
            None
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    // Another tenant's owner sees nothing.
    let (other, _) = c.sessions.sign_in("other@evil.example");
    let _ = sandbox(&c, &other).await;
    assert_eq!(
        c.req(As::User(&other), Method::GET, &path, None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.req(
            As::User(&other),
            Method::GET,
            &format!("/v1/environments/{env}/credentials/{cred}"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
    // Organization credential policy: lifetime cap.
    let pol = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/organizations/{org}/credential-policy"),
            Some(json!({ "maxLifetimeDays": 30, "maxRotationOverlapMinutes": 60 })),
        )
        .await;
    assert_eq!(pol.status, StatusCode::OK, "{}", pol.json);
    conforms(&format!("{CTRL}CredentialPolicy"), &pol.json);
    let long = (chrono::Utc::now() + chrono::Duration::days(60)).to_rfc3339();
    let too_long = c
        .req(
            As::User(&dev),
            Method::POST,
            &path,
            Some(json!({ "scopes": ["calls.read"], "expiresAt": long })),
        )
        .await;
    assert_eq!(
        too_long.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        too_long.json
    );
    let overlap = c
        .req(
            As::User(&dev),
            Method::POST,
            &format!("/v1/environments/{env}/credentials/{cred}/rotate"),
            Some(json!({ "overlapMinutes": 600 })),
        )
        .await;
    assert_eq!(
        overlap.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        overlap.json
    );
    // Every mutation is in the activity history.
    let act = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/organizations/{org}/activity?limit=100"),
            None,
        )
        .await;
    for a in [
        "credential.created",
        "credential.rotated",
        "credential.suspended",
        "credential.resumed",
        "credential.revoked",
        "scope.granted",
    ] {
        assert!(act.json.to_string().contains(a), "{a}");
    }
}

#[tokio::test]
async fn the_scope_registry_and_grants_follow_trust_levels() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, _) = sandbox(&c, &owner).await;
    let reg = c
        .req(As::User(&owner), Method::GET, "/v1/scopes", None)
        .await;
    assert_eq!(reg.status, StatusCode::OK);
    conforms(&format!("{CTRL}ScopeRegistry"), &reg.json);
    let fp = reg.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "events.private_delivery")
        .unwrap();
    assert_eq!(fp["selfServiceGrantable"], false);
    for (scope, status) in [
        ("events.private_delivery", StatusCode::UNPROCESSABLE_ENTITY),
        ("numbers.admin", StatusCode::UNPROCESSABLE_ENTITY),
        ("Not A Scope", StatusCode::BAD_REQUEST),
    ] {
        let r = c
            .req(
                As::User(&owner),
                Method::POST,
                &format!("/v1/projects/{project}/scopes"),
                Some(json!({ "scope": scope })),
            )
            .await;
        assert_eq!(r.status, status, "{scope}: {}", r.json);
    }
    // FIRST_PARTY_ONLY: an explicit operator grant, refused for an EXTERNAL tenant.
    let op = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/projects/{project}/scope-grants"),
            Some(json!({ "scope": "events.private_delivery", "environmentKinds": ["SANDBOX"], "reasonCode": "FIRST_PARTY_INTEGRATION" })),
        )
        .await;
    assert_eq!(op.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", op.json);
    // A withdrawn scope disappears from the project.
    grant(&c, &owner, project, "calls.read").await;
    let w = c
        .req(
            As::User(&owner),
            Method::DELETE,
            &format!("/v1/projects/{project}/scopes/calls.read"),
            None,
        )
        .await;
    assert_eq!(w.json["removed"], true, "{}", w.json);
    let _ = org;
}

#[tokio::test]
async fn quotas_follow_organization_project_environment_service_account_credential_precedence() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let plan = |code: &str, rpm: u64| {
        json!({ "code": code, "displayName": code, "entitlementProfileRef": format!("p.{code}"),
                "entitlements": { "apiRequestsPerMinute": rpm, "apiRequestsPerDay": 5000, "smsPerDay": 10 } })
    };
    let org_plan = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(plan("org", 100)),
        )
        .await;
    let proj_plan = c
        .operator(
            Method::POST,
            "/internal/v1/operator/commercial-plans",
            Some(plan("proj", 60)),
        )
        .await;
    for (p, project) in [(&org_plan, None), (&proj_plan, Some(project))] {
        let a = c
            .operator(
                Method::POST,
                &format!("/internal/v1/operator/organizations/{org}/plan-assignments"),
                Some(json!({ "planId": p.json["id"], "projectId": project, "source": "CONTRACT" })),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.json);
    }
    let gateway = c.peers.gateway.clone();
    let ent = |sa: Option<Uuid>, cred: Option<Uuid>| {
        let mut path = format!("/internal/v1/entitlements/environments/{env}?kind=SANDBOX");
        if let Some(s) = sa {
            path.push_str(&format!("&serviceAccountId={s}"));
        }
        if let Some(cr) = cred {
            path.push_str(&format!("&credentialId={cr}"));
        }
        path
    };
    let e = c
        .internal(&gateway, Method::GET, &ent(None, None), None)
        .await;
    assert_eq!(e.status, StatusCode::OK, "{}", e.json);
    conforms(&format!("{CTRL}Entitlements"), &e.json);
    assert_eq!(
        (
            e.json["apiRequestsPerMinute"].as_u64(),
            e.json["limitingLevel"]["apiRequestsPerMinute"].as_str()
        ),
        (Some(60), Some("PROJECT_PLAN"))
    );
    let v1 = e.json["snapshotVersion"].as_i64().unwrap();
    // Same effective value: same snapshot version.
    let again = c
        .internal(&gateway, Method::GET, &ent(None, None), None)
        .await;
    assert_eq!(again.json["snapshotVersion"].as_i64().unwrap(), v1);
    // An explicit operator override lets the project exceed the organization plan.
    let ov = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{org}/entitlement-overrides"),
            Some(json!({ "projectId": project, "entitlements": { "apiRequestsPerMinute": 110 }, "reasonCode": "COMMERCIAL_AGREEMENT" })),
        )
        .await;
    assert_eq!(ov.status, StatusCode::CREATED, "{}", ov.json);
    let e = c
        .internal(&gateway, Method::GET, &ent(None, None), None)
        .await;
    assert_eq!(e.json["apiRequestsPerMinute"], 110);
    assert_eq!(
        e.json["limitingLevel"]["apiRequestsPerMinute"],
        "OPERATOR_OVERRIDE"
    );
    assert!(
        e.json["snapshotVersion"].as_i64().unwrap() > v1,
        "a change appends a snapshot version"
    );
    // Without the override the sandbox cap (120) does not bind; with a 200 override it does.
    let ov2 = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{org}/entitlement-overrides"),
            Some(json!({ "projectId": project, "entitlements": { "apiRequestsPerMinute": 200 }, "reasonCode": "COMMERCIAL_AGREEMENT" })),
        )
        .await;
    assert_eq!(ov2.status, StatusCode::CREATED);
    let e = c
        .internal(&gateway, Method::GET, &ent(None, None), None)
        .await;
    assert_eq!(
        (
            e.json["apiRequestsPerMinute"].as_u64(),
            e.json["limitingLevel"]["apiRequestsPerMinute"].as_str()
        ),
        (Some(120), Some("ENVIRONMENT"))
    );
    // Service account and credential restrictions only reduce.
    grant(&c, &owner, project, "calls.read").await;
    let sa = service_account(&c, As::User(&owner), env, "worker").await;
    let cred = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{env}/service-accounts/{sa}/credentials"),
            Some(json!({ "scopes": ["calls.read"] })),
        )
        .await;
    let cred = id(&cred.json["credential"]);
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/environments/{env}/service-accounts/{sa}/limits"),
            Some(json!({ "apiRequestsPerMinute": 50 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}QuotaRestriction"), &r.json);
    let r = c
        .req(
            As::User(&owner),
            Method::PUT,
            &format!("/v1/environments/{env}/credentials/{cred}/limits"),
            Some(json!({ "apiRequestsPerMinute": 10, "apiRequestsPerDay": 3 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let e = c
        .internal(&gateway, Method::GET, &ent(Some(sa), None), None)
        .await;
    let s = &e.json["subject"];
    assert_eq!(
        (
            s["apiRequestsPerMinute"].as_u64(),
            s["limitingLevel"]["apiRequestsPerMinute"].as_str()
        ),
        (Some(50), Some("SERVICE_ACCOUNT"))
    );
    let e = c
        .internal(&gateway, Method::GET, &ent(Some(sa), Some(cred)), None)
        .await;
    conforms(&format!("{CTRL}Entitlements"), &e.json);
    let s = &e.json["subject"];
    assert_eq!(
        (
            s["apiRequestsPerMinute"].as_u64(),
            s["limitingLevel"]["apiRequestsPerMinute"].as_str()
        ),
        (Some(10), Some("CREDENTIAL"))
    );
    assert_eq!(s["apiRequestsPerDay"], 3);
    assert_eq!(
        e.json["apiRequestsPerMinute"], 120,
        "the environment value stays on top"
    );
    // A restriction above the effective value never raises it.
    c.req(
        As::User(&owner),
        Method::PUT,
        &format!("/v1/environments/{env}/credentials/{cred}/limits"),
        Some(json!({ "apiRequestsPerMinute": 100000 })),
    )
    .await;
    let e = c
        .internal(&gateway, Method::GET, &ent(Some(sa), Some(cred)), None)
        .await;
    assert_eq!(e.json["subject"]["apiRequestsPerMinute"], 50);
    // The customer limits view names the levels and today's remaining allowance.
    let l = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/limits"),
            None,
        )
        .await;
    assert_eq!(l.status, StatusCode::OK, "{}", l.json);
    conforms(&format!("{CTRL}EnvironmentLimits"), &l.json);
    let day = l.json["limits"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["limit"] == "API_REQUESTS_PER_DAY")
        .unwrap()
        .clone();
    assert_eq!(day["remaining"], 5000);
    // Restricting another environment's credential is NOT_FOUND.
    let (other, _) = c.sessions.sign_in("o@x.example");
    let (_, _, other_env) = sandbox(&c, &other).await;
    let r = c
        .req(
            As::User(&other),
            Method::PUT,
            &format!("/v1/environments/{other_env}/credentials/{cred}/limits"),
            Some(json!({ "apiRequestsPerMinute": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}

async fn submit(c: &Control, who: As<'_>, project: Uuid, caps: &[&str]) -> Value {
    let r = c
        .req(
            who,
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": caps, "useCaseSummary": "Appointments" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    conforms(&format!("{CTRL}ProductionAccessRequest"), &r.json);
    r.json
}

#[tokio::test]
async fn production_access_is_requested_reviewed_approved_activated_and_suspended() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, owner_id) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let (dev, _, _) = join(&c, &owner, org, "dev@acme.example", "DEVELOPER").await;
    let st = c
        .req(
            As::User(&dev),
            Method::GET,
            &format!("/v1/projects/{project}/production-access"),
            None,
        )
        .await;
    assert_eq!(st.status, StatusCode::OK, "{}", st.json);
    conforms(&format!("{CTRL}ProductionAccessStatus"), &st.json);
    assert_eq!(st.json["state"], "SANDBOX_READY");
    // Only an OWNER submits.
    let denied = c
        .req(
            As::User(&dev),
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": ["SMS"] })),
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let req = submit(&c, As::User(&owner), project, &["APP_CALLING", "SMS"]).await;
    assert_eq!(
        req["state"], "PRODUCTION_REQUESTED",
        "regulated capabilities are never automatic"
    );
    let rid = id(&req);
    // One open request per project.
    let dup = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": ["MEETINGS"] })),
        )
        .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    // A customer can never approve or activate early.
    let early = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/production-access-requests/{rid}/activate"),
            None,
        )
        .await;
    assert_eq!(early.status, StatusCode::CONFLICT);
    let base = format!("/internal/v1/operator/production-access-requests/{rid}");
    let reviewer = Uuid::new_v4();
    // Approval needs the reviewing operator's identity; a member can never decide.
    let anon = c
        .operator_as(Method::POST, &format!("{base}/approve"), None, None)
        .await;
    assert_eq!(anon.status, StatusCode::BAD_REQUEST);
    assert_eq!(anon.json["error"]["code"], "OPERATOR_REQUIRED");
    let selfie = c
        .operator_as(
            Method::POST,
            &format!("{base}/approve"),
            None,
            Some(owner_id),
        )
        .await;
    assert_eq!(selfie.status, StatusCode::FORBIDDEN);
    assert_eq!(selfie.json["error"]["code"], "SELF_APPROVAL_FORBIDDEN");
    // Regulated capabilities need an explicit review first.
    let skip = c
        .operator_as(
            Method::POST,
            &format!("{base}/approve"),
            None,
            Some(reviewer),
        )
        .await;
    assert_eq!(skip.status, StatusCode::CONFLICT, "{}", skip.json);
    let rev = c
        .operator_as(
            Method::POST,
            &format!("{base}/review"),
            None,
            Some(reviewer),
        )
        .await;
    assert_eq!(rev.json["state"], "UNDER_REVIEW", "{}", rev.json);
    // Approve a subset: SMS only.
    let ok = c
        .operator_as(
            Method::POST,
            &format!("{base}/approve"),
            Some(json!({ "capabilities": ["SMS"], "approvalValidDays": 7 })),
            Some(reviewer),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    assert_eq!(ok.json["state"], "APPROVED");
    assert_eq!(ok.json["approvedCapabilities"], json!(["SMS"]));
    // Activation: Platform Access activates PRODUCTION with only the approved scopes.
    let live = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/production-access-requests/{rid}/activate"),
            None,
        )
        .await;
    assert_eq!(live.status, StatusCode::OK, "{}", live.json);
    assert_eq!(live.json["state"], "LIVE");
    let prod = Uuid::parse_str(live.json["productionEnvironmentId"].as_str().unwrap()).unwrap();
    assert_eq!(c.pa.activated.lock().unwrap()[&project], prod);
    assert!(
        c.pa.live_telecom.lock().unwrap()[&prod],
        "approved regulated capability"
    );
    let grants = c.pa.app_scopes.lock().unwrap().clone();
    assert!(grants[&(project, "sms.send".to_string())].contains(&"PRODUCTION".to_string()));
    assert!(
        !grants.contains_key(&(project, "calls.write".to_string())),
        "APP_CALLING was not approved"
    );
    let h = c
        .req(
            As::User(&dev),
            Method::GET,
            &format!("/v1/production-access-requests/{rid}"),
            None,
        )
        .await;
    let states: Vec<&str> = h.json["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["toState"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        ["PRODUCTION_REQUESTED", "UNDER_REVIEW", "APPROVED", "LIVE"]
    );
    // Production machine identities now work through delegation, approved scopes only.
    let sa = service_account(&c, As::User(&owner), prod, "prod-backend").await;
    let pc = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{prod}/service-accounts/{sa}/credentials"),
            Some(json!({ "scopes": ["sms.send"] })),
        )
        .await;
    assert_eq!(pc.status, StatusCode::CREATED, "{}", pc.json);
    assert_eq!(pc.json["credential"]["environmentKind"], "PRODUCTION");
    let unapproved = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{prod}/service-accounts/{sa}/credentials"),
            Some(json!({ "scopes": ["calls.write"] })),
        )
        .await;
    assert_eq!(unapproved.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Suspension: Platform Access suspends the environment and live telecom at once.
    let s = c
        .operator_as(
            Method::POST,
            &format!("{base}/suspend"),
            Some(json!({ "reasonCode": "ABUSE_REPORT" })),
            None,
        )
        .await;
    assert_eq!(s.json["state"], "SUSPENDED", "{}", s.json);
    assert_eq!(c.pa.env_status(prod), "SUSPENDED");
    assert!(!c.pa.live_telecom.lock().unwrap()[&prod]);
    let blocked = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/environments/{prod}/service-accounts/{sa}/credentials"),
            Some(json!({ "scopes": ["sms.send"] })),
        )
        .await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN, "{}", blocked.json);
    let back = c
        .operator_as(Method::POST, &format!("{base}/reinstate"), None, None)
        .await;
    assert_eq!(back.json["state"], "LIVE");
    assert_eq!(c.pa.env_status(prod), "ACTIVE");
    // The support profile reflects the production status.
    let sp = c
        .req(
            As::User(&dev),
            Method::GET,
            &format!("/v1/organizations/{org}/support-profile"),
            None,
        )
        .await;
    assert_eq!(sp.json["productionStatus"], "LIVE", "{}", sp.json);
    let _ = env;
}

#[tokio::test]
async fn approvals_expire_rejections_carry_reasons_and_software_only_may_be_automatic() {
    let Some((c, _)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        production_auto_approval: true,
    })
    .await
    else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (_, project, _) = sandbox(&c, &owner).await;
    // Software-only: the automatic policy approves at once.
    let auto = submit(&c, As::User(&owner), project, &["MEETINGS", "MESSAGING"]).await;
    assert_eq!(auto["state"], "APPROVED", "{auto}");
    assert_eq!(auto["decision"]["policy"], "AUTOMATIC");
    let rid = id(&auto);
    // Expiry: an approval not activated in time is REJECTED (APPROVAL_EXPIRED).
    c.db.conn
        .execute_unprepared(&format!(
            "UPDATE devplat_production_access_requests SET approval_expires_at = now() - interval '1 minute' WHERE id = '{rid}'"
        ))
        .await
        .unwrap();
    let exp = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/production-access-requests/{rid}/activate"),
            None,
        )
        .await;
    assert_eq!(exp.status, StatusCode::CONFLICT);
    assert_eq!(exp.json["error"]["code"], "APPROVAL_EXPIRED");
    let r = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/production-access-requests/{rid}"),
            None,
        )
        .await;
    assert_eq!(
        (
            r.json["state"].as_str(),
            r.json["decision"]["reasonCode"].as_str()
        ),
        (Some("REJECTED"), Some("APPROVAL_EXPIRED"))
    );
    // Regulated: never automatic; an operator rejection carries a safe reason code.
    let reg = submit(&c, As::User(&owner), project, &["NUMBERS"]).await;
    assert_eq!(reg["state"], "PRODUCTION_REQUESTED");
    let rej = c
        .operator_as(
            Method::POST,
            &format!(
                "/internal/v1/operator/production-access-requests/{}/reject",
                id(&reg)
            ),
            Some(json!({ "reasonCode": "KYC_INCOMPLETE" })),
            Some(Uuid::new_v4()),
        )
        .await;
    assert_eq!(rej.json["state"], "REJECTED", "{}", rej.json);
    assert_eq!(rej.json["decision"]["reasonCode"], "KYC_INCOMPLETE");
    // A rejected project may resubmit; the customer can cancel.
    let again = submit(&c, As::User(&owner), project, &["SMS"]).await;
    let can = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/production-access-requests/{}/cancel", id(&again)),
            None,
        )
        .await;
    assert_eq!(can.json["state"], "CANCELLED", "{}", can.json);
    let list = c
        .operator(
            Method::GET,
            "/internal/v1/operator/production-access-requests?state=REJECTED",
            None,
        )
        .await;
    assert_eq!(
        list.json["items"].as_array().unwrap().len(),
        2,
        "{}",
        list.json
    );
}

fn fact(
    env: (Uuid, Uuid, Uuid),
    source: &str,
    ty: &str,
    unit: &str,
    quantity: u64,
) -> cf_usage_facts::UsageFact {
    cf_usage_facts::UsageFact {
        source_fact_id: source.into(),
        usage_type: ty.into(),
        unit: unit.into(),
        quantity,
        tenant_id: env.0,
        application_id: env.1,
        environment_id: env.2,
        environment_kind: Some("SANDBOX".into()),
        resource_ref: format!("res:{source}"),
        occurred_at: chrono::Utc::now(),
        period_start: None,
        period_end: None,
        dimensions: Default::default(),
        digest: String::new(),
    }
    .normalized()
    .sealed()
}

#[tokio::test]
async fn usage_facts_are_ingested_once_rolled_up_queried_exported_and_reconciled() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    let scope = (org, project, env);
    let mut sms = fact(scope, "sms-1", "SMS_SEGMENT", "SEGMENT", 2);
    sms.dimensions.insert("direction".into(), json!("OUTBOUND"));
    let sms = sms.sealed();
    let call = fact(scope, "call-1", "CALL_MINUTE_APP", "MILLISECOND", 90_000);
    let post = |signer: std::sync::Arc<cf_service_auth::ServiceTokenSigner>,
                facts: Vec<cf_usage_facts::UsageFact>| {
        let c = &c;
        async move {
            c.internal(&signer, Method::POST, "/internal/v1/usage/facts",
                Some(json!({ "stream": "test.events", "facts": facts, "advanceTo": { "at": chrono::Utc::now(), "key": "k1" } })))
                .await
        }
    };
    let r = post(producers.sms.clone(), vec![sms.clone()]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}IngestUsageFactsResponse"), &r.json);
    assert_eq!(r.json["recorded"], 1);
    // Duplicate delivery: harmless; a conflicting replay is refused and recorded.
    let r = post(producers.sms.clone(), vec![sms.clone()]).await;
    assert_eq!(
        (r.json["recorded"].as_u64(), r.json["duplicates"].as_u64()),
        (Some(0), Some(1))
    );
    let mut changed = sms.clone();
    changed.quantity = 9;
    let r = post(producers.sms.clone(), vec![changed.sealed()]).await;
    assert_eq!(
        r.json["results"][0]["reasonCode"], "CONFLICTING_REPLAY",
        "{}",
        r.json
    );
    // Producer binding: SMS may not report call minutes; the digest must match; the
    // ownership must be the environment's own.
    let r = post(producers.sms.clone(), vec![call.clone()]).await;
    assert_eq!(r.json["results"][0]["reasonCode"], "PRODUCER_NOT_ALLOWED");
    let mut forged = call.clone();
    forged.quantity = 1;
    let r = post(producers.calling.clone(), vec![forged]).await;
    assert_eq!(r.json["results"][0]["reasonCode"], "DIGEST_MISMATCH");
    let wrong = fact(
        (Uuid::new_v4(), project, env),
        "call-x",
        "CALL_MINUTE_APP",
        "MILLISECOND",
        1,
    );
    let r = post(producers.calling.clone(), vec![wrong]).await;
    assert_eq!(r.json["results"][0]["reasonCode"], "OWNERSHIP_MISMATCH");
    let r = post(producers.calling.clone(), vec![call.clone()]).await;
    assert_eq!(r.json["recorded"], 1, "{}", r.json);
    // A non-producer (the gateway) cannot publish facts.
    let r = c
        .internal(
            &c.peers.gateway.clone(),
            Method::POST,
            "/internal/v1/usage/facts",
            Some(json!({ "facts": [] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // The watermark is durable per producer stream.
    let w = c
        .internal(
            &producers.sms.clone(),
            Method::GET,
            "/internal/v1/usage/streams/test.events",
            None,
        )
        .await;
    conforms(&format!("{CTRL}UsageStream"), &w.json);
    assert_eq!(w.json["position"]["key"], "k1");
    let w = c
        .internal(
            &producers.messaging.clone(),
            Method::GET,
            "/internal/v1/usage/streams/test.events",
            None,
        )
        .await;
    assert!(
        w.json["position"].is_null(),
        "another producer has its own stream"
    );
    // Customer reads: summary, facts, export, warnings.
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
    assert_eq!(s.json["sandbox"], true);
    let totals = s.json["totals"].as_array().unwrap();
    assert!(totals
        .iter()
        .any(|t| t["usageType"] == "SMS_SEGMENT" && t["quantity"] == 2));
    assert!(totals.iter().any(|t| t["usageType"] == "CALL_MINUTE_APP"
        && t["quantity"] == 90_000
        && t["derived"]["minutes"] == 1.5));
    let f = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage/facts?usageType=SMS_SEGMENT&limit=1"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}UsageFactList"), &f.json);
    assert_eq!(f.json["items"].as_array().unwrap().len(), 1);
    let csv = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage/export"),
            None,
        )
        .await;
    assert_eq!(csv.status, StatusCode::OK);
    assert!(csv
        .headers
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/csv"));
    // Rollups are rebuildable from the facts with identical totals.
    let today = chrono::Utc::now().date_naive().to_string();
    let rb = c
        .operator(
            Method::POST,
            "/internal/v1/operator/usage/rollups/rebuild",
            Some(json!({ "from": today, "to": today })),
        )
        .await;
    assert_eq!(rb.json["rebuilt"], 2, "{}", rb.json);
    let s2 = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage"),
            None,
        )
        .await;
    assert_eq!(s2.json["totals"], s.json["totals"]);
    // Reconciliation: a matching and a mismatching producer total.
    let start = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let rec = c
        .internal(&producers.sms.clone(), Method::POST, "/internal/v1/usage/reconciliations", Some(json!({
            "windowStart": start, "windowEnd": start + chrono::Duration::days(1),
            "entries": [{ "environmentId": env, "usageType": "SMS_SEGMENT", "quantity": 2, "factCount": 1 },
                        { "environmentId": env, "usageType": "SMS_SEGMENT", "quantity": 3, "factCount": 2 }]})))
        .await;
    assert_eq!(rec.json["mismatches"], 1, "{}", rec.json);
    let ov = c
        .operator(Method::GET, "/internal/v1/operator/usage", None)
        .await;
    assert!(ov.json["rejections"].as_array().unwrap().len() >= 4);
    assert_eq!(ov.json["reconciliations"].as_array().unwrap().len(), 2);
    // Machines read only their own environment; other tenants see nothing.
    c.pa.allow_scope(project, "usage.read", &["SANDBOX"]);
    let m = c.pa.machine(env, &["usage.read"]);
    let own = c
        .req(
            As::Machine(&m),
            Method::GET,
            &format!("/v1/environments/{env}/usage"),
            None,
        )
        .await;
    assert_eq!(own.status, StatusCode::OK, "{}", own.json);
    let compat = c
        .req(
            As::Machine(&m),
            Method::GET,
            "/v1/usage?usageType=SMS_SEGMENT",
            None,
        )
        .await;
    assert_eq!(compat.status, StatusCode::OK, "{}", compat.json);
    assert_eq!(compat.json["data"].as_array().unwrap().len(), 1);
    let lim = c
        .req(As::Machine(&m), Method::GET, "/v1/limits", None)
        .await;
    assert_eq!(lim.status, StatusCode::OK, "{}", lim.json);
    assert!(lim.json["limits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["limit"] == "API_REQUESTS_PER_MINUTE"));
    let test_env = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": "TEST" })),
        )
        .await;
    let test_env = id(&test_env.json);
    assert_eq!(
        c.req(
            As::Machine(&m),
            Method::GET,
            &format!("/v1/environments/{test_env}/usage"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND,
        "environment isolation"
    );
    let (other, _) = c.sessions.sign_in("x@other.example");
    let _ = sandbox(&c, &other).await;
    assert_eq!(
        c.req(
            As::User(&other),
            Method::GET,
            &format!("/v1/environments/{env}/usage"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn gateway_batches_become_api_operation_facts_and_history_is_backfilled() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (_, _, env) = sandbox(&c, &owner).await;
    let gw = c.peers.gateway.clone();
    let today = chrono::Utc::now().date_naive();
    let credential = Uuid::new_v4();
    let batch = json!({ "batchId": Uuid::new_v4(), "entries": [{ "environmentId": env, "day": today.to_string(), "count": 3 }],
        "operations": [{ "environmentId": env, "day": today.to_string(), "domain": "calling", "operationId": "listCalls",
                         "statusClass": "2xx", "count": 3, "credentialId": credential, "environmentKind": "SANDBOX" }] });
    for _ in 0..2 {
        let r = c
            .internal(
                &gw,
                Method::POST,
                "/internal/v1/usage/api-requests",
                Some(batch.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    }
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_usage_facts WHERE usage_type = 'API_OPERATION'")
            .await,
        1,
        "exactly once"
    );
    let f = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/usage/facts?usageType=API_OPERATION"),
            None,
        )
        .await;
    assert_eq!(f.json["items"][0]["quantity"], 3);
    assert_eq!(
        f.json["items"][0]["dimensions"]["credentialId"],
        credential.to_string()
    );
    // Legacy counters of an earlier day become facts once (backfill), then reconcile.
    let yesterday = today - chrono::Duration::days(1);
    c.db.conn
        .execute_unprepared(&format!(
            "INSERT INTO devplat_api_operation_usage (environment_id, usage_day, domain, operation_id, status_class, request_count, updated_at) \
             VALUES ('{env}', '{yesterday}', 'sms', 'sendSms', '2xx', 7, now())"
        ))
        .await
        .unwrap();
    for expected in [1, 0] {
        let b = c
            .operator(
                Method::POST,
                "/internal/v1/operator/usage/backfill/api-operations",
                Some(json!({ "day": today.to_string() })),
            )
            .await;
        assert_eq!(b.json["recorded"], expected, "{}", b.json);
    }
    let rec = c
        .operator(
            Method::POST,
            "/internal/v1/operator/usage/reconcile/api-operations",
            Some(json!({ "day": yesterday.to_string() })),
        )
        .await;
    assert_eq!(rec.json["mismatches"], 0, "{}", rec.json);
}

#[tokio::test]
async fn first_party_usage_carries_cost_centres_and_support_profiles_route_incidents() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    // Two isolated first-party products (generic synthetic fixtures).
    let mut scopes = Vec::new();
    for (i, cc) in ["cc-product-a", "cc-product-b"].iter().enumerate() {
        let tenant = c.pa.first_party_tenant();
        let email = format!("owner{i}@cosmicforge.example");
        let reg = c
            .operator(Method::POST, "/internal/v1/operator/organizations", Some(json!({ "tenantId": tenant, "displayName": format!("Product {i}"), "ownerEmail": email })))
            .await;
        assert_eq!(reg.status, StatusCode::CREATED, "{}", reg.json);
        let (owner, _) = c.sessions.sign_in(&email);
        let acc = c
            .req(
                As::User(&owner),
                Method::POST,
                "/v1/invitations/accept",
                Some(json!({ "token": reg.json["ownerInvitation"]["token"] })),
            )
            .await;
        assert_eq!(acc.status, StatusCode::OK, "{}", acc.json);
        let cc_r = c
            .operator(
                Method::PUT,
                &format!("/internal/v1/operator/organizations/{tenant}/cost-center"),
                Some(json!({ "costCenterRef": cc })),
            )
            .await;
        assert_eq!(cc_r.status, StatusCode::OK, "{}", cc_r.json);
        let p = c
            .req(
                As::User(&owner),
                Method::POST,
                &format!("/v1/organizations/{tenant}/projects"),
                Some(json!({ "displayName": "App" })),
            )
            .await;
        let p = id(&p.json);
        let e = c
            .req(
                As::User(&owner),
                Method::POST,
                &format!("/v1/projects/{p}/environments"),
                Some(json!({ "kind": "SANDBOX" })),
            )
            .await;
        let e = id(&e.json);
        // FIRST_PARTY_ONLY: the explicit, audited operator grant works for first-party tenants.
        let g = c
            .operator(Method::POST, &format!("/internal/v1/operator/projects/{p}/scope-grants"),
                Some(json!({ "scope": "events.private_delivery", "environmentKinds": ["SANDBOX"], "reasonCode": "FIRST_PARTY_INTEGRATION" })))
            .await;
        assert_eq!(g.status, StatusCode::OK, "{}", g.json);
        scopes.push((
            tenant,
            p,
            e,
            owner,
            acc.json["id"].as_str().unwrap().to_string(),
        ));
    }
    for (i, (t, p, e, _, _)) in scopes.iter().enumerate() {
        let f = fact(
            (*t, *p, *e),
            &format!("meeting-{i}"),
            "MEETING_PARTICIPANT_MINUTE",
            "MILLISECOND",
            60_000 * (i as u64 + 1),
        );
        let r = c
            .internal(
                &producers.meeting.clone(),
                Method::POST,
                "/internal/v1/usage/facts",
                Some(json!({ "facts": [f] })),
            )
            .await;
        assert_eq!(r.json["recorded"], 1, "{}", r.json);
    }
    let today = chrono::Utc::now().date_naive().to_string();
    for (i, cc) in ["cc-product-a", "cc-product-b"].iter().enumerate() {
        let u = c
            .operator(
                Method::GET,
                &format!("/internal/v1/operator/cost-centers/{cc}/usage?from={today}&to={today}"),
                None,
            )
            .await;
        let items = u.json["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{}", u.json);
        assert_eq!(items[0]["quantity"], 60_000 * (i as u64 + 1));
        assert_eq!(
            items[0]["organizationId"],
            scopes[i].0.to_string(),
            "cost allocation never mixes products"
        );
    }
    // Support profile: member references, operator tier and incident reference.
    let (t, _, _, owner, member) = &scopes[0];
    let sp = c
        .req(
            As::User(owner),
            Method::GET,
            &format!("/v1/organizations/{t}/support-profile"),
            None,
        )
        .await;
    conforms(&format!("{CTRL}SupportProfile"), &sp.json);
    assert_eq!(sp.json["productionStatus"], "SANDBOX_ONLY");
    let put = c
        .req(As::User(owner), Method::PUT, &format!("/v1/organizations/{t}/support-profile"),
            Some(json!({ "primaryTechnicalContactMemberId": member, "incidentContactMemberId": member })))
        .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.json);
    let bad = c
        .req(
            As::User(owner),
            Method::PUT,
            &format!("/v1/organizations/{t}/support-profile"),
            Some(json!({ "primaryTechnicalContactMemberId": Uuid::new_v4() })),
        )
        .await;
    assert_eq!(
        bad.status,
        StatusCode::BAD_REQUEST,
        "only members of the organization"
    );
    let tier = c
        .operator(
            Method::PUT,
            &format!("/internal/v1/operator/organizations/{t}/support-profile"),
            Some(json!({ "supportTier": "INTERNAL", "supportReference": "SUP-001" })),
        )
        .await;
    assert_eq!(tier.json["supportTier"], "INTERNAL", "{}", tier.json);
    let inc = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{t}/incidents"),
            Some(json!({ "incidentRef": "INC-2026-17", "occurredAt": chrono::Utc::now() })),
        )
        .await;
    assert_eq!(
        inc.json["lastIncident"]["reference"], "INC-2026-17",
        "{}",
        inc.json
    );
    let other = &scopes[1];
    assert_eq!(
        c.req(
            As::User(&other.3),
            Method::GET,
            &format!("/v1/organizations/{t}/support-profile"),
            None
        )
        .await
        .status,
        StatusCode::NOT_FOUND,
        "first-party products are isolated tenants"
    );
}

/// Part VI Chapters 14–15: the immutable control-plane / ledger records refuse rewrites,
/// list operations page, and activity filters by project and action.
#[tokio::test]
async fn chapter_14_15_immutability_pagination_and_activity_filters() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@acme.example");
    let (org, project, env) = sandbox(&c, &owner).await;
    // Production-access history: three requests (submit, cancel), paged newest first.
    let mut ids = Vec::new();
    for _ in 0..3 {
        let r = submit(&c, As::User(&owner), project, &["MEETINGS"]).await;
        let rid = id(&r);
        let x = c
            .req(
                As::User(&owner),
                Method::POST,
                &format!("/v1/production-access-requests/{rid}/cancel"),
                None,
            )
            .await;
        assert_eq!(x.status, StatusCode::OK, "{}", x.json);
        ids.push(rid.to_string());
    }
    let base = format!("/v1/projects/{project}/production-access-requests?limit=2");
    let p1 = c.req(As::User(&owner), Method::GET, &base, None).await;
    assert_eq!(p1.status, StatusCode::OK, "{}", p1.json);
    let cursor = p1.json["nextCursor"]
        .as_str()
        .expect("a next page")
        .to_string();
    let p2 = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("{base}&cursor={cursor}"),
            None,
        )
        .await;
    let seen: Vec<String> = p1.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(p2.json["items"].as_array().unwrap())
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(p2.json["nextCursor"], Value::Null);
    ids.reverse();
    assert_eq!(seen, ids, "newest first, each once");
    let bad = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("{base}&cursor=czox"),
            None,
        )
        .await;
    assert_eq!(
        bad.status,
        StatusCode::BAD_REQUEST,
        "a sequence cursor is not a keyset one"
    );
    // Credentials page over Platform Access's list.
    grant(&c, &owner, project, "calls.read").await;
    let sa = service_account(&c, As::User(&owner), env, "backend").await;
    let path = format!("/v1/environments/{env}/service-accounts/{sa}/credentials");
    let mut created = Vec::new();
    for _ in 0..3 {
        let r = c
            .req(
                As::User(&owner),
                Method::POST,
                &path,
                Some(json!({ "scopes": ["calls.read"] })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
        created.push(r.json["credential"]["id"].as_str().unwrap().to_string());
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let mut listed = Vec::new();
    let mut next = Some(format!("{path}?limit=2"));
    while let Some(p) = next.take() {
        let r = c.req(As::User(&owner), Method::GET, &p, None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert!(r.json["items"].as_array().unwrap().len() <= 2);
        assert!(!r.json.to_string().contains("\"secret\""));
        listed.extend(
            r.json["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v["id"].as_str().unwrap().to_string()),
        );
        next = r.json["nextCursor"]
            .as_str()
            .map(|cur| format!("{path}?limit=2&cursor={cur}"));
    }
    created.reverse();
    assert_eq!(listed, created, "newest first, each once");
    // Activity: by project (its own events and its environments'), by action (exact or
    // family prefix); another organization's project is not found.
    let other = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{org}/projects"),
            Some(json!({ "displayName": "Other" })),
        )
        .await;
    let other_project = id(&other.json);
    let activity = |q: String| {
        let c = &c;
        let owner = owner.clone();
        async move {
            c.req(
                As::User(&owner),
                Method::GET,
                &format!("/v1/organizations/{org}/activity?limit=100{q}"),
                None,
            )
            .await
        }
    };
    let all = activity(String::new()).await;
    let mine = activity(format!("&projectId={project}")).await;
    assert_eq!(mine.status, StatusCode::OK, "{}", mine.json);
    let actions = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["action"].as_str().unwrap().to_string())
            .collect()
    };
    let mine_actions = actions(&mine.json);
    for a in [
        "environment.created",
        "scope.granted",
        "service_account.created",
        "credential.created",
        "production_access.requested",
    ] {
        assert!(
            mine_actions.iter().any(|x| x == a),
            "{a} missing: {mine_actions:?}"
        );
    }
    assert!(
        mine.json["items"].as_array().unwrap().len() < all.json["items"].as_array().unwrap().len()
    );
    assert!(
        !mine.json.to_string().contains(&other_project.to_string()),
        "other project's events"
    );
    let creds = activity("&action=credential.".into()).await;
    assert!(!actions(&creds.json).is_empty());
    assert!(actions(&creds.json)
        .iter()
        .all(|a| a.starts_with("credential.")));
    let exact = activity("&action=credential.created".into()).await;
    assert_eq!(actions(&exact.json), vec!["credential.created"; 3]);
    let (stranger, _) = c.sessions.sign_in("stranger@other.example");
    let (_, foreign_project, _) = sandbox(&c, &stranger).await;
    let r = activity(format!("&projectId={foreign_project}")).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    let r = activity("&action=DROP%20TABLE".into()).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // Ledger / history rows: a snapshot, a usage fact, a refused replay, a reconciliation.
    let lim = c
        .req(
            As::User(&owner),
            Method::GET,
            &format!("/v1/environments/{env}/limits"),
            None,
        )
        .await;
    assert_eq!(lim.status, StatusCode::OK, "{}", lim.json);
    let f = fact((org, project, env), "sms-imm", "SMS_SEGMENT", "SEGMENT", 1);
    let post = |facts: Vec<cf_usage_facts::UsageFact>| {
        let c = &c;
        let signer = producers.sms.clone();
        async move {
            c.internal(
                &signer,
                Method::POST,
                "/internal/v1/usage/facts",
                Some(json!({ "facts": facts })),
            )
            .await
        }
    };
    assert_eq!(post(vec![f.clone()]).await.json["recorded"], 1);
    let mut changed = f.clone();
    changed.quantity = 7;
    assert_eq!(
        post(vec![changed.sealed()]).await.json["results"][0]["reasonCode"],
        "CONFLICTING_REPLAY"
    );
    let start = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc();
    let rec = c
        .internal(&producers.sms.clone(), Method::POST, "/internal/v1/usage/reconciliations", Some(json!({
            "windowStart": start, "windowEnd": start + chrono::Duration::days(1),
            "entries": [{ "environmentId": env, "usageType": "SMS_SEGMENT", "quantity": 1, "factCount": 1 }]})))
        .await;
    assert_eq!(rec.status, StatusCode::OK, "{}", rec.json);
    for table in [
        "devplat_usage_facts",
        "devplat_entitlement_snapshots",
        "devplat_production_access_transitions",
        "devplat_usage_rejections",
        "devplat_usage_reconciliations",
    ] {
        assert!(
            c.count(&format!("SELECT count(*) FROM {table}")).await > 0,
            "{table} empty"
        );
        for sql in [
            format!("UPDATE {table} SET environment_id = environment_id"),
            format!("DELETE FROM {table}"),
        ] {
            let sql = if table.ends_with("transitions") || table.ends_with("rejections") {
                sql.replace("environment_id = environment_id", "id = id")
            } else {
                sql
            };
            assert!(
                c.db.conn.execute_unprepared(&sql).await.is_err(),
                "{sql} succeeded"
            );
        }
    }
    assert!(c
        .db
        .conn
        .execute_unprepared("TRUNCATE devplat_usage_facts")
        .await
        .is_err());
    // Production-access requests are never deleted; the ledger keeps the original quantity.
    assert!(c
        .db
        .conn
        .execute_unprepared("DELETE FROM devplat_production_access_requests")
        .await
        .is_err());
    assert_eq!(
        c.count("SELECT quantity FROM devplat_usage_facts WHERE source_fact_id = 'sms-imm'")
            .await,
        1
    );
}
