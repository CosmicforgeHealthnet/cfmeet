//! Part VI Chapters 10–13 (§54–§57) certification journeys.
//!
//! Each journey starts its own certification stack — the REAL service binaries, real
//! Ed25519 service authentication, one PostgreSQL database per service, real Redis — and
//! drives it through the real API Gateway exactly as an external developer, a first-party
//! product team and the operator console do. Nothing is stubbed:
//!
//! * §54 — credential self-service delegated to Platform Access (the secret exactly once,
//!   rotation with bounded overlap, immediate suspension / revocation at the gateway,
//!   service-account and credential quotas enforced by the gateway, concurrency);
//! * §55 — the production-access workflow (request → review → approve → activate →
//!   suspend / reinstate, no self-approval), production credentials, the unified usage
//!   ledger fed by the REAL producers (Meeting Service, Calling, Numbering, SMS Gateway,
//!   Messaging exporters and the gateway's API operations), dedupe on replay, conflict
//!   rejection, reconciliation, export and the `/v1/usage` / `/v1/limits` re-pointing;
//! * §56 — two isolated FIRST_PARTY fixtures with cost centres and the explicit
//!   FIRST_PARTY_ONLY operator grant;
//! * §57 — the external journey end to end without database edits or engineer-issued
//!   credentials, including the support / incident profile.
//!
//! Usage evidence: the domain services meter what they committed. No deterministic media
//! or carrier path exists in a certification stack, so the usage journey commits the
//! domain evidence rows (an SMS usage record, call usage records, an active number
//! assignment, a stored attachment, a meeting participant session and a recording) into
//! each OWNING service's database — as that service's own transaction would — and then
//! certifies that the unmodified service exporters publish them to the Developer
//! Platform. The customer journeys never touch a database.
//!
//! Prerequisites and skip rules are those of `tests/journeys.rs`: TEST_DATABASE_URL,
//! TEST_REDIS_URL and the service binaries (CFMEET_BIN_DIR); without them the journeys
//! FAIL unless CFMEET_SKIP_DB_TESTS=1 skips explicitly.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use cfmeet_certification::{
    provision::{self, Auth, Gateway, Resp, Sandbox},
    stack::{self, Stack},
    StackConfig, StackOptions,
};
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

static BUSY: AtomicBool = AtomicBool::new(false);

struct Serial;

impl Serial {
    async fn acquire() -> Serial {
        while BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Serial
    }
}

impl Drop for Serial {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

/// The producers' exporters run every second without a settle delay (production
/// defaults: 30 s / 30 s); the gateway serves `/v1/usage` and `/v1/limits` from the
/// Developer Platform (§55 ownership migration).
fn options() -> StackOptions {
    let mut o = StackOptions::default();
    for p in [
        stack::MEETING_SERVICE,
        stack::CALLING,
        stack::NUMBERING,
        stack::SMS_GATEWAY,
        stack::MESSAGING,
    ] {
        o = o
            .set(p, "CFMEET_USAGE_EXPORT_ENABLED", "true")
            .set(p, "CFMEET_USAGE_EXPORT_INTERVAL_SECONDS", "1")
            .set(p, "CFMEET_USAGE_EXPORT_SETTLE_SECONDS", "0")
            .set(p, "CFMEET_USAGE_EXPORT_BACKFILL_DAYS", "5");
    }
    o.set(
        stack::API_GATEWAY,
        "API_GATEWAY_CENTRAL_USAGE_ENABLED",
        "true",
    )
}

async fn start() -> Option<(Serial, Stack)> {
    let config = StackConfig::from_env().expect("certification prerequisites")?;
    let guard = Serial::acquire().await;
    let stack = Stack::start_with(config, options())
        .await
        .expect("certification stack starts");
    Some((guard, stack))
}

fn email(role: &str) -> String {
    format!(
        "{role}-{}@cert.example",
        &Uuid::new_v4().simple().to_string()[..10]
    )
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

fn week_ago() -> String {
    (chrono::Utc::now().date_naive() - chrono::Duration::days(7)).to_string()
}

/// Polls `f` until it returns `Some` (at most `secs` seconds).
async fn until<T, F, Fut>(secs: u64, what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn facts(gw: &Gateway, auth: Auth<'_>, env: Uuid, usage_type: Option<&str>) -> Vec<Value> {
    let mut base = format!(
        "/v1/environments/{env}/usage/facts?from={}&limit=100",
        week_ago()
    );
    if let Some(t) = usage_type {
        base.push_str(&format!("&usageType={t}"));
    }
    let mut out = Vec::new();
    let mut path = base.clone();
    loop {
        let r = gw.get(&path, auth).await;
        status(&r, 200, "usage facts");
        out.extend(r.body["items"].as_array().cloned().unwrap_or_default());
        match r.body["nextCursor"].as_str() {
            Some(c) => path = format!("{base}&cursor={c}"),
            None => return out,
        }
    }
}

/// Scopes of the external developer's sandbox credential (all STANDARD, self-service).
const DEVELOPER_SCOPES: &[&str] = &[
    "organizations.read",
    "projects.read",
    "usage.read",
    "calls.read",
    "sms.read",
    "numbers.search",
    "meetings.read",
    "webhooks.read",
];

// =====================================================================================
// §57 (+ §54 / §55): external onboarding to production, without database edits or
// engineer-issued credentials
// =====================================================================================

#[tokio::test]
async fn external_onboarding_to_production_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    // 1. Organization -> project -> SANDBOX -> scopes -> service account -> credential,
    //    all self-service through the gateway with the developer's session.
    let sb: Sandbox = provision::developer_sandbox(&stack, &email("onboard"), DEVELOPER_SCOPES)
        .await
        .expect("self-service sandbox");
    let session = Auth::Session(&sb.user.session);
    let cred = Auth::Credential(&sb.customer.secret);
    assert!(
        sb.customer.secret.starts_with("cfmk_"),
        "a Platform Access credential"
    );
    // The secret is shown once: metadata reads never carry it.
    let env = sb.environment_id;
    let meta = gw
        .get(
            &format!(
                "/v1/environments/{env}/credentials/{}",
                sb.customer.credential_id
            ),
            session,
        )
        .await;
    status(&meta, 200, "credential metadata");
    assert!(meta.body.get("secret").is_none(), "{}", meta.text);
    assert!(!meta.text.contains(&sb.customer.secret));
    assert_eq!(meta.body["effectiveState"], "ACTIVE", "{}", meta.text);
    let list = gw
        .get(
            &format!(
                "/v1/environments/{env}/service-accounts/{}/credentials",
                sb.customer.service_account_id
            ),
            session,
        )
        .await;
    status(&list, 200, "credential list");
    assert!(!list.text.contains(&sb.customer.secret));
    // 2. The credential works on the public APIs through the gateway, sandbox-scoped.
    // SDK discovery for the console's quick-starts (public infrastructure, no auth).
    let disc = gw.get("/discovery", Auth::None).await;
    status(&disc, 200, "discovery");
    let packages: Vec<&str> = disc.body["sdks"]["packages"]
        .as_array()
        .expect("sdks")
        .iter()
        .filter_map(|p| p["package"].as_str())
        .collect();
    assert_eq!(packages, ["@cfmeet/platform-sdk", "cfmeet_platform_sdk"]);
    let ctx = gw.get("/v1/developer-platform/context", cred).await;
    status(&ctx, 200, "credential context");
    assert_eq!(ctx.body["environmentKind"], "SANDBOX");
    status(
        &gw.get("/v1/calls", cred).await,
        200,
        "Calling with the sandbox credential",
    );
    // A scope it does not hold is refused at the gateway.
    let r = gw
        .post(
            "/v1/calls",
            cred,
            json!({ "kind": "VOICE", "initiatorPrincipalId": Uuid::new_v4(), "targetPrincipalIds": [Uuid::new_v4()] }),
        )
        .await;
    status(&r, 403, "calls.write not held");
    // 3. Sandbox usage, limits and warnings (session and credential).
    let usage = gw
        .get(
            &format!("/v1/environments/{env}/usage?from={}", week_ago()),
            session,
        )
        .await;
    status(&usage, 200, "usage summary");
    assert_eq!(usage.body["sandbox"], true);
    assert_eq!(usage.body["usageTypes"].as_array().map(Vec::len), Some(8));
    let limits = gw
        .get(&format!("/v1/environments/{env}/limits"), cred)
        .await;
    status(&limits, 200, "limits (credential)");
    assert_eq!(limits.body["environmentKind"], "SANDBOX");
    assert_eq!(limits.body["limits"][0]["limit"], "API_REQUESTS_PER_MINUTE");
    status(
        &gw.get(&format!("/v1/environments/{env}/usage/warnings"), session)
            .await,
        200,
        "usage warnings",
    );
    // The gateway's API operations become ledger facts of the sandbox environment.
    let api = until(30, "API_OPERATION facts", || async {
        let f = facts(&gw, session, env, Some("API_OPERATION")).await;
        (!f.is_empty()).then_some(f)
    })
    .await;
    assert!(api.iter().all(|f| f["sandbox"] == true
        && f["environmentId"] == env.to_string()
        && f["producer"] == "cfmeet.api-gateway"));
    // `/v1/usage` and `/v1/limits` are served by the Developer Platform (re-pointed).
    let u = gw.get("/v1/usage?usageType=API_OPERATION", cred).await;
    status(&u, 200, "GET /v1/usage (central)");
    assert!(u.body["data"].is_array(), "{}", u.text);
    let l = gw.get("/v1/limits", cred).await;
    status(&l, 200, "GET /v1/limits (central)");
    assert!(l.body["limits"].as_array().is_some_and(|a| !a.is_empty()));
    // 4. Production access: request -> review -> approve -> activate.
    let project = sb.project_id;
    let st = gw
        .get(
            &format!("/v1/projects/{project}/production-access"),
            session,
        )
        .await;
    status(&st, 200, "production status");
    assert_eq!(st.body["state"], "SANDBOX_READY", "{}", st.text);
    // Customers still cannot create PRODUCTION environments directly.
    let r = gw
        .post(
            &format!("/v1/projects/{project}/environments"),
            session,
            json!({ "kind": "PRODUCTION" }),
        )
        .await;
    status(&r, 422, "PRODUCTION environment by a customer");
    let req = gw
        .post(
            &format!("/v1/projects/{project}/production-access-requests"),
            session,
            json!({ "capabilities": ["APP_CALLING", "SMS"], "useCaseSummary": "Appointment reminders" }),
        )
        .await;
    status(&req, 201, "production-access request");
    assert_eq!(req.body["state"], "PRODUCTION_REQUESTED");
    let rid = req.uuid("/id");
    let dup = gw
        .post(
            &format!("/v1/projects/{project}/production-access-requests"),
            session,
            json!({ "capabilities": ["MEETINGS"] }),
        )
        .await;
    status(&dup, 409, "one open request per project");
    let early = gw
        .post(
            &format!("/v1/production-access-requests/{rid}/activate"),
            session,
            json!({}),
        )
        .await;
    status(&early, 409, "activation before approval");
    let anon = operator.decide(rid, "approve", None, None).await.unwrap();
    status(&anon, 400, "approval without the reviewing operator");
    assert_eq!(anon.code(), Some("OPERATOR_REQUIRED"));
    let selfie = operator
        .decide(rid, "approve", None, Some(sb.user.id))
        .await
        .unwrap();
    status(&selfie, 403, "self-approval");
    assert_eq!(selfie.code(), Some("SELF_APPROVAL_FORBIDDEN"));
    let reviewer = Some(Uuid::new_v4());
    let skip = operator
        .decide(rid, "approve", None, reviewer)
        .await
        .unwrap();
    status(&skip, 409, "regulated capabilities need a review first");
    let rev = operator
        .decide(rid, "review", None, reviewer)
        .await
        .unwrap();
    status(&rev, 200, "review");
    assert_eq!(rev.body["state"], "UNDER_REVIEW");
    let ok = operator
        .decide(
            rid,
            "approve",
            Some(json!({ "approvalValidDays": 7 })),
            reviewer,
        )
        .await
        .unwrap();
    status(&ok, 200, "approve");
    assert_eq!(ok.body["state"], "APPROVED");
    let live = gw
        .post(
            &format!("/v1/production-access-requests/{rid}/activate"),
            session,
            json!({}),
        )
        .await;
    status(&live, 200, "activation");
    assert_eq!(live.body["state"], "LIVE", "{}", live.text);
    let prod = live.uuid("/productionEnvironmentId");
    let history = gw
        .get(&format!("/v1/production-access-requests/{rid}"), session)
        .await;
    let states: Vec<&str> = history.body["history"]
        .as_array()
        .expect("history")
        .iter()
        .filter_map(|t| t["toState"].as_str())
        .collect();
    assert_eq!(
        states,
        ["PRODUCTION_REQUESTED", "UNDER_REVIEW", "APPROVED", "LIVE"]
    );
    // 5. Production credentials, self-service, bounded by the approved capabilities.
    let pc = provision::self_service_credential(
        &gw,
        session,
        sb.organization_id,
        project,
        prod,
        &["calls.read", "sms.read"],
        "prod-backend",
    )
    .await
    .expect("production credential");
    let pcred = Auth::Credential(&pc.secret);
    let ctx = gw.get("/v1/developer-platform/context", pcred).await;
    status(&ctx, 200, "production context");
    assert_eq!(ctx.body["environmentKind"], "PRODUCTION");
    let r = gw.get("/v1/calls", pcred).await;
    status(&r, 200, "production credential on Calling");
    assert_eq!(
        r.header("x-cf-quota-limit-minute"),
        Some("600"),
        "no sandbox cap"
    );
    // MEETINGS was not approved: meetings.read stays SANDBOX / TEST only.
    let sa = gw
        .post(
            &format!("/v1/environments/{prod}/service-accounts"),
            session,
            json!({ "name": "prod-unapproved" }),
        )
        .await
        .uuid("/id");
    let r = gw
        .post(
            &format!("/v1/environments/{prod}/service-accounts/{sa}/credentials"),
            session,
            json!({ "scopes": ["meetings.read"] }),
        )
        .await;
    assert!(
        (400..500).contains(&r.status),
        "unapproved scope in PRODUCTION: {}",
        r.text
    );
    // Production usage is recorded separately from the sandbox.
    let prod_api = until(30, "production API_OPERATION facts", || async {
        let f = facts(&gw, session, prod, Some("API_OPERATION")).await;
        (!f.is_empty()).then_some(f)
    })
    .await;
    assert!(prod_api
        .iter()
        .all(|f| f["sandbox"] == false && f["environmentKind"] == "PRODUCTION"));
    assert!(facts(&gw, session, env, None)
        .await
        .iter()
        .all(|f| f["environmentId"] == env.to_string()));
    // 6. Support / incident path (§57 §13.3).
    let members = gw
        .get(
            &format!("/v1/organizations/{}/members", sb.organization_id),
            session,
        )
        .await;
    status(&members, 200, "members");
    let member = members.body["items"][0]["id"]
        .as_str()
        .expect("member id")
        .to_string();
    let put = gw
        .request(
            Method::PUT,
            &format!("/v1/organizations/{}/support-profile", sb.organization_id),
            session,
            Some(json!({ "primaryTechnicalContactMemberId": member, "incidentContactMemberId": member })),
            &[],
        )
        .await;
    status(&put, 200, "support contacts");
    let tier = operator
        .dp(
            Method::PUT,
            &format!(
                "/operator/organizations/{}/support-profile",
                sb.organization_id
            ),
            Some(json!({ "supportTier": "PREMIUM", "supportReference": "SUP-CERT-1" })),
        )
        .await
        .unwrap();
    status(&tier, 200, "operator support tier");
    let inc = operator
        .dp(
            Method::POST,
            &format!("/operator/organizations/{}/incidents", sb.organization_id),
            Some(json!({ "incidentRef": "INC-CERT-1", "occurredAt": chrono::Utc::now().to_rfc3339() })),
        )
        .await
        .unwrap();
    status(&inc, 200, "incident reference");
    let sp = gw
        .get(
            &format!("/v1/organizations/{}/support-profile", sb.organization_id),
            cred,
        )
        .await;
    status(&sp, 200, "support profile (credential)");
    assert_eq!(sp.body["productionStatus"], "LIVE", "{}", sp.text);
    assert_eq!(sp.body["supportTier"], "PREMIUM");
    assert_eq!(sp.body["supportReference"], "SUP-CERT-1");
    assert_eq!(sp.body["primaryTechnicalContactMemberId"], member.as_str());
    assert_eq!(sp.body["lastIncident"]["reference"], "INC-CERT-1");
    assert!(
        !sp.text.contains(&sb.user.email),
        "contacts are member references"
    );
    // 7. Suspension is immediate for PRODUCTION only; reinstatement restores it.
    let s = operator
        .decide(
            rid,
            "suspend",
            Some(json!({ "reasonCode": "ABUSE_REPORT" })),
            None,
        )
        .await
        .unwrap();
    status(&s, 200, "suspend");
    assert_eq!(s.body["state"], "SUSPENDED");
    let r = gw.get("/v1/calls", pcred).await;
    assert!(
        r.status == 401 || r.status == 403,
        "suspended production credential: {} {}",
        r.status,
        r.text
    );
    status(&gw.get("/v1/calls", cred).await, 200, "sandbox unaffected");
    let back = operator.decide(rid, "reinstate", None, None).await.unwrap();
    status(&back, 200, "reinstate");
    assert_eq!(back.body["state"], "LIVE");
    until(15, "reinstated production credential", || async {
        (gw.get("/v1/calls", pcred).await.status == 200).then_some(())
    })
    .await;
    // 8. Every step is audited; no secret reaches the activity log.
    let act = gw
        .get(
            &format!(
                "/v1/organizations/{}/activity?limit=100",
                sb.organization_id
            ),
            session,
        )
        .await;
    status(&act, 200, "activity");
    for a in [
        "scope.granted",
        "credential.created",
        "production_access.requested",
        "production_access.approved",
        "production_access.activated",
        "production_access.suspended",
        "support_profile.updated",
    ] {
        assert!(act.text.contains(a), "{a} missing: {}", act.text);
    }
    assert!(!act.text.contains(&sb.customer.secret) && !act.text.contains(&pc.secret));
    stack.teardown().await;
}

// =====================================================================================
// §54: credential lifecycle and caller quotas at the gateway
// =====================================================================================

const LIFECYCLE_SCOPES: &[&str] = &[
    "organizations.read",
    "calls.read",
    "usage.read",
    "credentials.manage",
];

/// Waits until the current calendar minute (the gateway's quota window) has at least 15 s
/// left, so a short sequence of requests is counted in one window.
async fn within_one_quota_minute() {
    let second = chrono::Utc::now().timestamp() % 60;
    if second > 45 {
        tokio::time::sleep(Duration::from_secs((61 - second) as u64)).await;
    }
}

async fn new_credential(
    gw: &Gateway,
    session: Auth<'_>,
    env: Uuid,
    sa: Uuid,
    scopes: &[&str],
) -> (Uuid, String) {
    let r = gw
        .post(
            &format!("/v1/environments/{env}/service-accounts/{sa}/credentials"),
            session,
            json!({ "scopes": scopes }),
        )
        .await;
    status(&r, 201, "create credential");
    (r.uuid("/credential/id"), r.str("/secret"))
}

#[tokio::test]
async fn credential_lifecycle_and_caller_quota_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let a = provision::developer_sandbox(&stack, &email("lifecycle"), LIFECYCLE_SCOPES)
        .await
        .expect("sandbox");
    let session = Auth::Session(&a.user.session);
    let env = a.environment_id;
    let sa = a.customer.service_account_id;
    let base = format!("/v1/environments/{env}/service-accounts/{sa}/credentials");
    // Idempotent creation never reveals the secret twice.
    let key = Uuid::new_v4().to_string();
    let first = gw
        .request(
            Method::POST,
            &base,
            session,
            Some(json!({ "scopes": ["calls.read"] })),
            &[("idempotency-key", key.clone())],
        )
        .await;
    status(&first, 201, "create (idempotency key)");
    let secret = first.str("/secret");
    let replay = gw
        .request(
            Method::POST,
            &base,
            session,
            Some(json!({ "scopes": ["calls.read"] })),
            &[("idempotency-key", key)],
        )
        .await;
    assert!(replay.status < 300, "idempotent replay: {}", replay.text);
    assert_eq!(
        replay.body["credential"]["id"],
        first.body["credential"]["id"]
    );
    assert!(replay.body.get("secret").is_none() && !replay.text.contains(&secret));
    // A machine with credentials.manage manages credentials of its own environment;
    // it can never grant more than it holds.
    let machine = Auth::Credential(&a.customer.secret);
    let (_, m_secret) = new_credential(&gw, machine, env, sa, &["calls.read"]).await;
    status(
        &gw.get("/v1/calls", Auth::Credential(&m_secret)).await,
        200,
        "machine-issued",
    );
    let r = gw
        .post(&base, machine, json!({ "scopes": ["numbers.search"] }))
        .await;
    assert!((400..500).contains(&r.status), "escalation: {}", r.text);
    let limited = Auth::Credential(&secret);
    let r = gw
        .post(&base, limited, json!({ "scopes": ["calls.read"] }))
        .await;
    status(&r, 403, "credentials.manage not held");
    // Rotation with bounded overlap: both work during the overlap.
    let cred = first.uuid("/credential/id");
    let rot = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{cred}/rotate"),
            session,
            json!({ "overlapMinutes": 5 }),
        )
        .await;
    status(&rot, 201, "rotate");
    let successor = rot.uuid("/credential/id");
    let next = rot.str("/secret");
    status(
        &gw.get("/v1/calls", limited).await,
        200,
        "predecessor in overlap",
    );
    status(
        &gw.get("/v1/calls", Auth::Credential(&next)).await,
        200,
        "successor",
    );
    // Suspension is enforced at the very next request; resumption restores it.
    let s = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{successor}/suspend"),
            session,
            json!({ "reasonCode": "INCIDENT_RESPONSE" }),
        )
        .await;
    status(&s, 200, "suspend credential");
    assert_eq!(s.body["effectiveState"], "SUSPENDED", "{}", s.text);
    let r = gw.get("/v1/calls", Auth::Credential(&next)).await;
    status(&r, 401, "suspended credential at the gateway");
    let r = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{successor}/resume"),
            session,
            json!({}),
        )
        .await;
    status(&r, 200, "resume credential");
    status(
        &gw.get("/v1/calls", Auth::Credential(&next)).await,
        200,
        "resumed",
    );
    // Revocation is immediate.
    let r = gw
        .post(
            &format!("/v1/environments/{env}/credentials/{cred}/revoke"),
            session,
            json!({}),
        )
        .await;
    status(&r, 200, "revoke");
    status(
        &gw.get("/v1/calls", limited).await,
        401,
        "revoked credential",
    );
    // Credential quota (stricter than the environment): enforced by the gateway with
    // its own counters, under concurrency, without throttling sibling credentials.
    let (c3, s3) = new_credential(&gw, session, env, sa, &["calls.read", "usage.read"]).await;
    let r = gw
        .request(
            Method::PUT,
            &format!("/v1/environments/{env}/credentials/{c3}/limits"),
            session,
            Some(json!({ "apiRequestsPerMinute": 3 })),
            &[],
        )
        .await;
    status(&r, 200, "credential limits");
    // The gateway's per-minute windows are calendar minutes: start well inside one so the
    // whole check is counted in a single window.
    within_one_quota_minute().await;
    let lim = gw
        .get(
            &format!("/v1/environments/{env}/limits"),
            Auth::Credential(&s3),
        )
        .await;
    status(&lim, 200, "limits of the restricted credential");
    assert_eq!(lim.body["limits"][0]["ceiling"], 3, "{}", lim.text);
    assert_eq!(lim.body["limits"][0]["limitingLevel"], "CREDENTIAL");
    let burst =
        futures_util::future::join_all((0..8).map(|_| gw.get("/v1/calls", Auth::Credential(&s3))))
            .await;
    let ok = burst.iter().filter(|r| r.status == 200).count();
    let limited_n = burst.iter().filter(|r| r.status == 429).count();
    // 3 per minute, one used by /limits: exactly 2 of the 8 concurrent requests pass.
    assert_eq!((ok, limited_n), (2, 6), "{ok} of 8 allowed");
    let denied = burst.iter().find(|r| r.status == 429).expect("a 429");
    assert_eq!(denied.header("x-cf-quota-level"), Some("CREDENTIAL"));
    assert_eq!(
        denied.header("x-cf-quota-window"),
        Some("requests-per-minute")
    );
    status(
        &gw.get("/v1/calls", Auth::Credential(&next)).await,
        200,
        "sibling unaffected",
    );
    // Service-account quota.
    let sa2 = gw
        .post(
            &format!("/v1/environments/{env}/service-accounts"),
            session,
            json!({ "name": "batch" }),
        )
        .await
        .uuid("/id");
    let r = gw
        .request(
            Method::PUT,
            &format!("/v1/environments/{env}/service-accounts/{sa2}/limits"),
            session,
            Some(json!({ "apiRequestsPerMinute": 2 })),
            &[],
        )
        .await;
    status(&r, 200, "service-account limits");
    let (_, s4) = new_credential(&gw, session, env, sa2, &["calls.read"]).await;
    within_one_quota_minute().await;
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(gw.get("/v1/calls", Auth::Credential(&s4)).await);
    }
    assert_eq!(
        codes.iter().map(|r| r.status).collect::<Vec<_>>(),
        [200, 200, 429, 429, 429],
        "2 per minute for the service account"
    );
    assert!(codes[2..]
        .iter()
        .all(|r| r.header("x-cf-quota-level") == Some("SERVICE_ACCOUNT")));
    // Cross-tenant: another organization's member sees nothing and changes nothing.
    let b = provision::developer_sandbox(&stack, &email("other"), &["organizations.read"])
        .await
        .expect("second sandbox");
    let other = Auth::Session(&b.user.session);
    for (m, p) in [
        (
            Method::GET,
            format!("/v1/environments/{env}/credentials/{successor}"),
        ),
        (
            Method::POST,
            format!("/v1/environments/{env}/credentials/{successor}/suspend"),
        ),
        (
            Method::POST,
            format!("/v1/environments/{env}/credentials/{successor}/revoke"),
        ),
        (Method::GET, format!("/v1/environments/{env}/usage")),
    ] {
        let body = (m == Method::POST).then(|| json!({}));
        let r = gw.request(m.clone(), &p, other, body, &[]).await;
        assert!(r.status == 403 || r.status == 404, "{m} {p}: {}", r.text);
    }
    status(
        &gw.get("/v1/calls", Auth::Credential(&next)).await,
        200,
        "still active",
    );
    stack.teardown().await;
}

// =====================================================================================
// §55: the unified usage ledger fed by the real producers
// =====================================================================================

/// Commits a domain evidence row in its OWNING service's database, as that service's
/// own transaction would (foreign keys to rows of flows a certification stack cannot
/// drive — media, carriers — are not checked for these synthetic rows).
async fn commit(stack: &Stack, service: &str, sql: &str) {
    let db = stack.db(service).await.expect("service database");
    db.execute_unprepared(&format!(
        "BEGIN; SET LOCAL session_replication_role = replica; {sql}; COMMIT;"
    ))
    .await
    .unwrap_or_else(|e| panic!("{service} evidence: {e}"));
}

async fn seed_domain_usage(stack: &Stack, t: Uuid, a: Uuid, e: Uuid) -> String {
    let u = Uuid::new_v4;
    commit(
        stack,
        stack::SMS_GATEWAY,
        &format!(
            "INSERT INTO sms_usage_records (id, transport_id, tenant_id, application_id, environment_id, \
               direction, segments, estimated_cost_minor, currency, status, created_at, updated_at) \
             VALUES ('{}', '{}', '{t}', '{a}', '{e}', 'OUTBOUND', 3, 0, 'USD', 'ESTIMATED', \
               now() - interval '1 minute', now() - interval '1 minute')",
            u(),
            u()
        ),
    )
    .await;
    let call = u();
    commit(
        stack,
        stack::CALLING,
        &format!(
            "INSERT INTO call_usage_records (id, call_id, leg_id, tenant_id, application_id, environment_id, \
               usage_type, quantity, unit, idempotency_key, finalized_at, created_at, updated_at, route_class, \
               destination_country, billable_seconds, cost_status, cost_minor, cost_currency, size_bytes) VALUES \
             ('{}', '{call}', NULL, '{t}', '{a}', '{e}', 'CALL_CONNECTED_TIME', 120000, 'MILLISECONDS', 'cert-call-{call}', \
               now() - interval '1 minute', now() - interval '2 minutes', now() - interval '1 minute', NULL, NULL, NULL, NULL, NULL, NULL, NULL), \
             ('{}', '{call}', '{}', '{t}', '{a}', '{e}', 'LEG_CONNECTED_TIME', 60000, 'MILLISECONDS', 'cert-leg-{call}', \
               now() - interval '1 minute', now() - interval '2 minutes', now() - interval '1 minute', 'PSTN', 'US', 60, 'ESTIMATED', 12, 'USD', NULL), \
             ('{}', '{call}', NULL, '{t}', '{a}', '{e}', 'RECORDING_ARTIFACT', 30000, 'MILLISECONDS', 'cert-rec-{call}', \
               now() - interval '1 minute', now() - interval '2 minutes', now() - interval '1 minute', NULL, NULL, NULL, NULL, NULL, NULL, 4096)",
            u(),
            u(),
            u(),
            u()
        ),
    )
    .await;
    let number = u();
    let e164 = format!("+1555010{:04}", rand::random::<u16>() % 10_000);
    commit(
        stack,
        stack::NUMBERING,
        &format!(
            "INSERT INTO number_resources (id, e164, country_code, calling_code, inventory_source, \
               state_changed_at, discovered_at, created_at, updated_at) \
             VALUES ('{number}', '{e164}', 'US', '1', 'SYNTHETIC_TEST', now(), now(), now(), now()); \
             INSERT INTO dcn_assignments (id, number_resource_id, reservation_id, tenant_id, application_id, \
               environment_id, environment_kind, owner_kind, owner_ref, ownership_class, routing_profile_id, \
               routing_profile_revision, capability_snapshot, eligibility_decision_ref, status, requested_at, \
               eligible_at, activated_at, created_by, updated_at) \
             VALUES ('{}', '{number}', '{}', '{t}', '{a}', '{e}', 'SANDBOX', 'ORGANIZATION', '{}', 'ORGANIZATION', \
               '{}', 1, ARRAY['SMS_OUTBOUND','VOICE_INBOUND'], '{}', 'ACTIVE', now() - interval '3 days', \
               now() - interval '3 days', now() - interval '2 days', 'cert', now())",
            u(),
            u(),
            u(),
            u(),
            u()
        ),
    )
    .await;
    commit(
        stack,
        stack::MESSAGING,
        &format!(
            "INSERT INTO messaging_attachments (id, tenant_id, application_id, environment_id, conversation_id, \
               uploader_principal_id, security_profile, kind, state, object_key, declared_size, verified_size, \
               declared_sha256, media_type, scan_status, intent_expires_at, created_at, finalized_at, \
               state_changed_at, committed_bytes) \
             VALUES ('{}', '{t}', '{a}', '{e}', '{}', '{}', 'MANAGED', 'FILE', 'READY', 'att/{}/{}/{}', 2048, 2048, \
               '{}', 'application/pdf', 'CLEAN', now(), now() - interval '3 days', now() - interval '2 days', \
               now() - interval '1 minute', 2048)",
            u(),
            u(),
            u(),
            u().simple(),
            u().simple(),
            u().simple(),
            "a".repeat(64)
        ),
    )
    .await;
    let (space, conference, participant) = (u(), u(), u());
    commit(
        stack,
        stack::MEETING_SERVICE,
        &format!(
            "INSERT INTO meeting_spaces (id, meeting_code, title, platform_tenant_id, platform_application_id, \
               platform_environment_id, created_by_service_account_id) \
             VALUES ('{space}', 'cert-{}', 'Certification', '{t}', '{a}', '{e}', '{}'); \
             INSERT INTO conference_sessions (id, meeting_space_id, occurrence_key, kind) \
             VALUES ('{conference}', '{space}', 'cert', 'INSTANT'); \
             INSERT INTO conference_participants (id, conference_id, identity_type, guest_identity, display_name, \
               role, role_source, admission_state, admitted_at) \
             VALUES ('{participant}', '{conference}', 'GUEST', 'g-{}', 'Guest', 'PARTICIPANT', 'DEFAULT', 'ADMITTED', \
               now() - interval '10 minutes'); \
             INSERT INTO participant_sessions (id, conference_participant_id, conference_id, media_identity, state, \
               established_at, connected_at, disconnected_at) \
             VALUES ('{}', '{participant}', '{conference}', 'm-{}', 'DISCONNECTED', now() - interval '10 minutes', \
               now() - interval '10 minutes', now() - interval '5 minutes'); \
             INSERT INTO meeting_artifacts (id, meeting_space_id, conference_id, artifact_type, status, storage_state, \
               storage_key, checksum_sha256, size_bytes, duration_ms, available_at, finalized_at) \
             VALUES ('{}', '{space}', '{conference}', 'RECORDING', 'READY', 'AVAILABLE', 'cert/rec/{}', '{}', 8192, \
               45000, now() - interval '2 days', now() - interval '2 days')",
            &u().simple().to_string()[..8],
            u(),
            &u().simple().to_string()[..8],
            u(),
            &u().simple().to_string()[..8],
            u(),
            u().simple(),
            "b".repeat(64)
        ),
    )
    .await;
    e164
}

const ALL_TYPES: &[&str] = &[
    "API_OPERATION",
    "MEETING_PARTICIPANT_MINUTE",
    "CALL_MINUTE_APP",
    "CALL_MINUTE_PUBLIC_NETWORK",
    "NUMBER_ACTIVE",
    "SMS_SEGMENT",
    "RECORDING_MINUTE",
    "STORAGE_BYTE_TIME",
];

fn by_type(items: &[Value]) -> BTreeMap<String, BTreeSet<String>> {
    let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for f in items {
        m.entry(f["usageType"].as_str().unwrap_or_default().to_string())
            .or_default()
            .insert(f["producer"].as_str().unwrap_or_default().to_string());
    }
    m
}

fn total(summary: &Value, ty: &str) -> i64 {
    summary["totals"]
        .as_array()
        .and_then(|t| t.iter().find(|x| x["usageType"] == ty))
        .and_then(|x| x["quantity"].as_i64())
        .unwrap_or(0)
}

#[tokio::test]
async fn usage_ledger_producers_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(&stack, &email("usage"), &["usage.read", "calls.read"])
        .await
        .expect("sandbox");
    let session = Auth::Session(&sb.user.session);
    let cred = Auth::Credential(&sb.customer.secret);
    let env = sb.environment_id;
    status(&gw.get("/v1/calls", cred).await, 200, "an API operation");
    let e164 = seed_domain_usage(&stack, sb.organization_id, sb.project_id, env).await;
    // Every usage type arrives from its owning producer.
    let items = until(90, "all eight usage types", || async {
        let f = facts(&gw, session, env, None).await;
        let types = by_type(&f);
        eprintln!("usage types so far: {types:?}");
        let storage = types.get("STORAGE_BYTE_TIME").cloned().unwrap_or_default();
        (ALL_TYPES.iter().all(|t| types.contains_key(*t))
            && storage.contains("cfmeet.messaging")
            && storage.contains("cfmeet.meeting-service"))
        .then_some(f)
    })
    .await;
    let types = by_type(&items);
    let expect = |ty: &str, producers: &[&str]| {
        let want: BTreeSet<String> = producers.iter().map(|s| s.to_string()).collect();
        assert!(types[ty].is_subset(&want), "{ty}: {:?}", types[ty]);
    };
    expect("API_OPERATION", &["cfmeet.api-gateway"]);
    expect("MEETING_PARTICIPANT_MINUTE", &["cfmeet.meeting-service"]);
    expect("CALL_MINUTE_APP", &["cfmeet.calling"]);
    expect("CALL_MINUTE_PUBLIC_NETWORK", &["cfmeet.calling"]);
    expect("NUMBER_ACTIVE", &["cfmeet.numbering"]);
    expect("SMS_SEGMENT", &["cfmeet.sms-gateway"]);
    expect(
        "RECORDING_MINUTE",
        &["cfmeet.meeting-service", "cfmeet.calling"],
    );
    expect(
        "STORAGE_BYTE_TIME",
        &[
            "cfmeet.meeting-service",
            "cfmeet.messaging",
            "cfmeet.calling",
        ],
    );
    let text = serde_json::to_string(&items).unwrap();
    assert!(!text.contains(&e164), "no phone number in customer usage");
    assert!(
        !text.contains("att/"),
        "no storage object key in customer usage"
    );
    assert!(items.iter().all(|f| f["sandbox"] == true));
    // Wait for the streams to quiesce (recorded meeting / storage streams included),
    // then take the baseline.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let summary = |gw: Gateway| async move {
        let r = gw
            .get(
                &format!("/v1/environments/{env}/usage?from={}", week_ago()),
                session,
            )
            .await;
        status(&r, 200, "usage summary");
        r.body
    };
    let before = summary(gw.clone()).await;
    assert_eq!(total(&before, "SMS_SEGMENT"), 3);
    assert_eq!(total(&before, "CALL_MINUTE_PUBLIC_NETWORK"), 60_000);
    assert_eq!(total(&before, "MEETING_PARTICIPANT_MINUTE"), 300_000);
    let non_api = |s: &Value| -> Vec<(String, i64)> {
        s["totals"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["usageType"] != "API_OPERATION")
            .map(|t| {
                (
                    t["usageType"].as_str().unwrap().to_string(),
                    t["quantity"].as_i64().unwrap() * 1_000_000 + t["factCount"].as_i64().unwrap(),
                )
            })
            .collect()
    };
    // Replay: every producer stream is reset to "no watermark" (backlog recovery) and
    // re-exported from the start — duplicates are absorbed, the ledger is unchanged.
    let ov = operator
        .dp(Method::GET, "/operator/usage", None)
        .await
        .unwrap();
    status(&ov, 200, "usage operations overview");
    let streams: Vec<(String, String)> = ov.body["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["producer"].as_str().unwrap().to_string(),
                s["stream"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // Six streams: Calling's artifact-storage stream starts with its first completed
    // storage day (the seeded recording finished minutes ago).
    assert!(streams.len() >= 6, "{streams:?}");
    for (p, s) in &streams {
        let r = operator
            .dp(
                Method::POST,
                &format!("/operator/usage/streams/{p}/{s}/reset"),
                None,
            )
            .await
            .unwrap();
        status(&r, 200, "stream reset");
    }
    until(30, "streams re-exported", || async {
        let ov = operator
            .dp(Method::GET, "/operator/usage", None)
            .await
            .unwrap();
        ov.body["streams"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| !s["positionAt"].is_null())
            .then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let after = summary(gw.clone()).await;
    assert_eq!(
        non_api(&before),
        non_api(&after),
        "replays are deduplicated"
    );
    // A producer replaying a DIFFERENT payload under the same source fact is refused and
    // recorded; the ledger keeps the original.
    commit(
        &stack,
        stack::SMS_GATEWAY,
        &format!("UPDATE sms_usage_records SET segments = 5 WHERE environment_id = '{env}'"),
    )
    .await;
    let r = operator
        .dp(
            Method::POST,
            "/operator/usage/streams/cfmeet.sms-gateway/sms.segments/reset",
            None,
        )
        .await
        .unwrap();
    status(&r, 200, "sms stream reset");
    let rejection = until(30, "conflict rejection", || async {
        let ov = operator
            .dp(Method::GET, "/operator/usage", None)
            .await
            .unwrap();
        ov.body["rejections"].as_array().and_then(|a| {
            a.iter()
                .find(|x| x["producer"] == "cfmeet.sms-gateway")
                .cloned()
        })
    })
    .await;
    assert_eq!(rejection["reasonCode"], "CONFLICTING_REPLAY");
    // §17 monitoring: the refused replay turns the operator ingestion health DEGRADED.
    let health = operator
        .dp(Method::GET, "/operator/ingestion/health", None)
        .await
        .unwrap();
    assert_eq!(health.status, 200, "{}", health.text);
    assert_eq!(health.body["status"], "DEGRADED", "{}", health.text);
    assert!(health.body["usageRejectionsLast24h"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["producer"] == "cfmeet.sms-gateway"));
    assert_eq!(total(&summary(gw.clone()).await, "SMS_SEGMENT"), 3);
    // Shadow reconciliation: interval producers reconcile every completed day.
    let ov = operator
        .dp(Method::GET, "/operator/usage", None)
        .await
        .unwrap();
    let recs = ov.body["reconciliations"].as_array().unwrap();
    assert!(
        recs.iter()
            .any(|r| r["producer"] == "cfmeet.numbering" && r["outcome"] == "MATCH"),
        "{}",
        ov.text
    );
    assert!(recs.iter().all(|r| r["outcome"] == "MATCH"), "{}", ov.text);
    // Rollup rebuild is a no-op on totals.
    let today = chrono::Utc::now().date_naive().to_string();
    let r = operator
        .dp(
            Method::POST,
            "/operator/usage/rollups/rebuild",
            Some(json!({ "from": week_ago(), "to": today })),
        )
        .await
        .unwrap();
    status(&r, 200, "rollup rebuild");
    assert_eq!(non_api(&after), non_api(&summary(gw.clone()).await));
    // Export: CSV and structured.
    let csv = gw
        .get(
            &format!(
                "/v1/environments/{env}/usage/export?format=csv&from={}",
                week_ago()
            ),
            session,
        )
        .await;
    status(&csv, 200, "CSV export");
    assert!(csv
        .header("content-type")
        .is_some_and(|c| c.starts_with("text/csv")));
    for t in ALL_TYPES {
        assert!(csv.text.contains(t), "{t} missing in CSV");
    }
    let js = gw
        .get(
            &format!(
                "/v1/environments/{env}/usage/export?format=json&granularity=facts&from={}",
                week_ago()
            ),
            session,
        )
        .await;
    status(&js, 200, "JSON export");
    assert!(!js.text.contains(&e164));
    // The credential reads the same ledger through the re-pointed `/v1/usage`.
    let u = gw.get("/v1/usage?usageType=SMS_SEGMENT", cred).await;
    status(&u, 200, "GET /v1/usage");
    assert_eq!(u.body["data"][0]["quantity"], 3, "{}", u.text);
    stack.teardown().await;
}

// =====================================================================================
// §56: first-party products are ordinary tenants with explicit classification
// =====================================================================================

#[tokio::test]
async fn first_party_fixtures_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let mut products = Vec::new();
    for cc in ["cc-cert-product-a", "cc-cert-product-b"] {
        let tenant = operator.tenant("FIRST_PARTY").await.unwrap();
        let owner_email = email("product");
        let reg = operator
            .register_first_party(tenant, &owner_email)
            .await
            .unwrap();
        status(&reg, 201, "register first-party organization");
        let owner = provision::register_user(
            stack.url(stack::MEETING_SERVICE),
            &stack.config.redis_url,
            &stack.http,
            &owner_email,
        )
        .await
        .unwrap();
        let session = Auth::Session(&owner.session);
        let r = gw
            .post(
                "/v1/invitations/accept",
                session,
                json!({ "token": reg.str("/ownerInvitation/token") }),
            )
            .await;
        assert!(r.status == 200 || r.status == 201, "accept: {}", r.text);
        let r = operator
            .dp(
                Method::PUT,
                &format!("/operator/organizations/{tenant}/cost-center"),
                Some(json!({ "costCenterRef": cc })),
            )
            .await
            .unwrap();
        status(&r, 200, "cost centre");
        let org = gw
            .get(&format!("/v1/organizations/{tenant}"), session)
            .await;
        assert_eq!(org.body["classification"], "FIRST_PARTY");
        let project = gw
            .post(
                &format!("/v1/organizations/{tenant}/projects"),
                session,
                json!({ "displayName": "Product" }),
            )
            .await
            .expect(201, "first-party project")
            .unwrap()
            .uuid("/id");
        let env = gw
            .post(
                &format!("/v1/projects/{project}/environments"),
                session,
                json!({ "kind": "SANDBOX" }),
            )
            .await
            .expect(201, "first-party environment")
            .unwrap()
            .uuid("/id");
        // FIRST_PARTY_ONLY scopes: never self-service, only the explicit operator grant.
        let r = gw
            .post(
                &format!("/v1/projects/{project}/scopes"),
                session,
                json!({ "scope": "events.private_delivery" }),
            )
            .await;
        status(&r, 422, "FIRST_PARTY_ONLY by self-service");
        let r = operator
            .dp(
                Method::POST,
                &format!("/operator/projects/{project}/scope-grants"),
                Some(
                    json!({ "scope": "events.private_delivery", "environmentKinds": ["SANDBOX"],
                             "reasonCode": "FIRST_PARTY_INTEGRATION" }),
                ),
            )
            .await
            .unwrap();
        status(&r, 200, "explicit first-party grant");
        // The same public self-service path as external customers.
        let c = provision::self_service_credential(
            &gw,
            session,
            tenant,
            project,
            env,
            &["calls.read", "organizations.read"],
            "product-backend",
        )
        .await
        .expect("first-party credential");
        status(
            &gw.get("/v1/calls", Auth::Credential(&c.secret)).await,
            200,
            "first-party call",
        );
        products.push((tenant, env, owner, cc));
    }
    // An external tenant can never receive a FIRST_PARTY_ONLY grant, operator or not.
    let ext = provision::developer_sandbox(&stack, &email("external"), &["organizations.read"])
        .await
        .unwrap();
    let r = operator
        .dp(
            Method::POST,
            &format!("/operator/projects/{}/scope-grants", ext.project_id),
            Some(
                json!({ "scope": "events.private_delivery", "environmentKinds": ["SANDBOX"],
                         "reasonCode": "FIRST_PARTY_INTEGRATION" }),
            ),
        )
        .await
        .unwrap();
    assert!(
        (400..500).contains(&r.status),
        "external first-party grant: {}",
        r.text
    );
    // Isolation between the two products.
    let (a, _, owner_a, _) = &products[0];
    let (b, _, owner_b, _) = &products[1];
    let r = gw
        .get(
            &format!("/v1/organizations/{b}"),
            Auth::Session(&owner_a.session),
        )
        .await;
    assert!(r.status == 403 || r.status == 404, "{}", r.text);
    let r = gw
        .get(
            &format!("/v1/organizations/{a}/support-profile"),
            Auth::Session(&owner_b.session),
        )
        .await;
    assert!(r.status == 403 || r.status == 404, "{}", r.text);
    // Cost allocation per cost centre, never mixing products.
    let today = chrono::Utc::now().date_naive().to_string();
    for (tenant, env, _, cc) in &products {
        let items = until(30, "cost-centre usage", || async {
            let r = operator
                .dp(
                    Method::GET,
                    &format!("/operator/cost-centers/{cc}/usage?from={today}&to={today}"),
                    None,
                )
                .await
                .unwrap();
            let items = r.body["items"].as_array().cloned().unwrap_or_default();
            (!items.is_empty()).then_some(items)
        })
        .await;
        assert!(
            items
                .iter()
                .all(|i| i["organizationId"] == tenant.to_string()),
            "{items:?}"
        );
        assert!(
            items.iter().any(|i| i["usageType"] == "API_OPERATION"),
            "{items:?}"
        );
        let _ = env;
    }
    stack.teardown().await;
}
