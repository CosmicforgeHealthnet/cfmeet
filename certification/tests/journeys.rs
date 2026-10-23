//! Part VI (Chapters 6–9) cross-service certification journeys.
//!
//! Every journey starts its own certification stack — the REAL service binaries with
//! real Ed25519 service authentication, one PostgreSQL database per service, real Redis
//! and deterministic adapters — and drives it only through the real API Gateway (plus
//! the operator console and, where a journey proves receiver-side enforcement, a direct
//! call to a receiver). Nothing is stubbed.
//!
//! Prerequisites: TEST_DATABASE_URL, TEST_REDIS_URL and the service binaries in
//! CFMEET_BIN_DIR (default `$CARGO_TARGET_DIR/debug`; `make certification` builds them).
//! Without them every journey FAILS, unless CFMEET_SKIP_DB_TESTS=1 skips explicitly.
//! Journeys run one at a time (each stack holds ~40 database connections).

use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use cf_service_auth::{
    services, ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest,
    ServiceId,
};
use cfmeet_certification::{
    provision::{
        self, Auth, Customer, Gateway, Resp, Sandbox, FIRST_PARTY_ONLY_SCOPES,
        PRIVILEGED_OPERATOR_SCOPES, SANDBOX_SCOPES,
    },
    stack::{self, Stack},
    ws, StackConfig,
};
use reqwest::Method;
use serde_json::{json, Value};
use uuid::Uuid;

/// Journeys run one at a time across the test threads (each `#[tokio::test]` has its own
/// runtime, so the lock is a plain flag released on drop — also when a journey panics).
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

/// Starts a stack (`None`: explicitly skipped). Holds the journey lock while it lives.
async fn start() -> Option<(Serial, Stack)> {
    let config = StackConfig::from_env().expect("certification prerequisites")?;
    let guard = Serial::acquire().await;
    let stack = Stack::start(config)
        .await
        .expect("certification stack starts");
    Some((guard, stack))
}

fn tag() -> String {
    Uuid::new_v4().simple().to_string()[..10].to_string()
}

fn email(role: &str) -> String {
    format!("{role}-{}@cert.example", tag())
}

#[track_caller]
fn status(r: &Resp, expected: u16, what: &str) {
    assert_eq!(r.status, expected, "{what}: {}", r.text);
}

/// Allows MANAGED conversations of `class` in the credential's environment policy.
async fn allow_managed(gw: &Gateway, secret: &str) {
    let cur = gw
        .get("/v1/messaging/policy", Auth::Credential(secret))
        .await;
    status(&cur, 200, "GET /v1/messaging/policy");
    let r = gw
        .request(
            Method::PUT,
            "/v1/messaging/policy",
            Auth::Credential(secret),
            Some(json!({ "expectedVersion": cur.body["version"], "policy": { "contexts": {
                "GROUP": { "allowedProfiles": ["MANAGED", "PRIVATE_E2EE"], "defaultProfile": "MANAGED" } } } })),
            &[],
        )
        .await;
    status(&r, 200, "PUT /v1/messaging/policy");
}

/// Creates a MANAGED group conversation owned by `owner` through the gateway.
async fn conversation(gw: &Gateway, secret: &str, owner: Uuid) -> String {
    let r = gw
        .acting(
            Method::POST,
            "/v1/conversations",
            Auth::Credential(secret),
            owner,
            Some(json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" })),
        )
        .await;
    status(&r, 201, "POST /v1/conversations");
    r.str("/conversationId")
}

/// Registers a Calling endpoint for `principal` (eventually: Calling learns the
/// principal from Platform Access's identity feed).
async fn endpoint(gw: &Gateway, secret: &str, principal: Uuid) -> Uuid {
    let mut last = None;
    for _ in 0..100 {
        let r = gw
            .post(
                "/v1/calling/endpoints",
                Auth::Credential(secret),
                json!({ "principalId": principal, "endpointKey": format!("dev-{}", tag()),
                        "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": true } }),
            )
            .await;
        if r.status == 201 || r.status == 200 {
            return r.uuid("/endpointId");
        }
        last = Some(r);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("POST /v1/calling/endpoints: {:?}", last.map(|r| r.text));
}

/// A Calling signaling credential for `principal` / `endpoint`.
async fn signaling_credential(
    gw: &Gateway,
    secret: &str,
    principal: Uuid,
    endpoint: Uuid,
) -> String {
    let r = gw
        .post(
            "/v1/calling/signaling-credentials",
            Auth::Credential(secret),
            json!({ "principalId": principal, "endpointId": endpoint }),
        )
        .await;
    status(&r, 201, "POST /v1/calling/signaling-credentials");
    r.body["credential"]
        .as_str()
        .or_else(|| r.body["token"].as_str())
        .unwrap_or_else(|| panic!("no signaling credential in {}", r.text))
        .to_string()
}

/// Durable gateway request accounting of `environment` at the Developer Platform.
async fn durable_usage(stack: &Stack, environment: Uuid) -> i64 {
    let db = stack.db(stack::DEVELOPER_PLATFORM).await.unwrap();
    let n = cfmeet_certification::db::scalar_i64(
        &db,
        &format!(
            "SELECT COALESCE(SUM(request_count), 0)::bigint FROM devplat_api_usage_daily \
             WHERE environment_id = '{environment}'"
        ),
    )
    .await
    .unwrap();
    db.close().await.ok();
    n
}

// =====================================================================================
// External developer — sandbox
// =====================================================================================

#[tokio::test]
async fn external_developer_sandbox_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    // 1. User -> organization -> project -> SANDBOX environment (Developer Platform via
    //    the gateway with the CF Meet session; DP delegates to Platform Access) -> a
    //    credential (operator-issued: no DP credential self-service exists yet).
    let sb: Sandbox = provision::developer_sandbox(&stack, &email("developer"), SANDBOX_SCOPES)
        .await
        .expect("developer sandbox");
    let session = Auth::Session(&sb.user.session);
    let orgs = gw.get("/v1/organizations", session).await;
    status(&orgs, 200, "GET /v1/organizations");
    assert!(
        orgs.text.contains(&sb.organization_id.to_string()),
        "{}",
        orgs.text
    );
    let envs = gw
        .get(
            &format!("/v1/projects/{}/environments", sb.project_id),
            session,
        )
        .await;
    status(&envs, 200, "GET /v1/projects/{id}/environments");
    assert!(envs.text.contains(&sb.environment_id.to_string()));
    assert!(envs.text.contains("SANDBOX"));
    let ent = gw
        .get(
            &format!("/v1/environments/{}/entitlements", sb.environment_id),
            session,
        )
        .await;
    status(&ent, 200, "GET /v1/environments/{id}/entitlements");
    assert_eq!(
        ent.body["apiRequestsPerMinute"], 120,
        "sandbox cap: {}",
        ent.text
    );

    let secret = sb.customer.secret.clone();
    let cred = Auth::Credential(&secret);
    // The credential's authority chain is the developer-created hierarchy.
    let ctx = gw.get("/v1/developer-platform/context", cred).await;
    status(&ctx, 200, "GET /v1/developer-platform/context");
    assert_eq!(ctx.body["tenantId"], json!(sb.organization_id));
    assert_eq!(ctx.body["applicationId"], json!(sb.project_id));
    assert_eq!(ctx.body["environmentId"], json!(sb.environment_id));
    let mut credential_requests = 1;

    // 2. Meeting (with the user session): organizations, an instant meeting, its view.
    let r = gw.get("/api/v2/me/organizations", session).await;
    status(&r, 200, "GET /api/v2/me/organizations");
    let r = gw
        .post(
            "/api/v2/instant-meetings",
            session,
            json!({ "title": "Certification sync" }),
        )
        .await;
    assert!(
        r.status == 201 || r.status == 200,
        "POST /api/v2/instant-meetings: {}",
        r.text
    );
    let conference = conference_id(&r);
    let r = gw
        .get(&format!("/api/v2/conferences/{conference}"), session)
        .await;
    status(&r, 200, "GET /api/v2/conferences/{id}");

    // 3. Calling: list calls, limits, endpoints and an application call.
    let operator = stack.operator();
    let alice = operator.principal(sb.environment_id).await.unwrap();
    let bob = operator.principal(sb.environment_id).await.unwrap();
    let r = gw.get("/v1/calls", cred).await;
    status(&r, 200, "GET /v1/calls");
    let r = gw.get("/v1/limits", cred).await;
    status(&r, 200, "GET /v1/limits");
    credential_requests += 2;
    let alice_ep = endpoint(&gw, &secret, alice).await;
    let _bob_ep = endpoint(&gw, &secret, bob).await;
    // An application call reaches Calling's own call admission. The stack has no media
    // provider (Calling offers only `livekit` or `none`; there is no deterministic media
    // adapter), so Calling itself refuses the media capability — a domain decision, after
    // the gateway authenticated, scope-checked, quota-counted and forwarded the request.
    let call = gw
        .post(
            "/v1/calls",
            cred,
            json!({ "kind": "VOICE", "initiatorPrincipalId": alice,
                    "initiatorEndpointId": alice_ep, "targetPrincipalIds": [bob] }),
        )
        .await;
    status(&call, 422, "POST /v1/calls without a media provider");
    assert_eq!(call.code(), Some("CAPABILITY_UNAVAILABLE"), "{}", call.text);
    let r = gw.get(&format!("/v1/calls/{}", Uuid::new_v4()), cred).await;
    status(&r, 404, "GET /v1/calls/{unknown}");
    credential_requests += 2;

    // 4. Messaging: policy, a conversation, a message, the list.
    allow_managed(&gw, &secret).await;
    let cid = conversation(&gw, &secret, alice).await;
    let m = gw
        .acting(
            Method::POST,
            &format!("/v1/conversations/{cid}/messages"),
            cred,
            alice,
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "hello from the sandbox" })),
        )
        .await;
    status(&m, 201, "POST /v1/conversations/{id}/messages");
    let list = gw
        .acting(Method::GET, "/v1/conversations", cred, alice, None)
        .await;
    status(&list, 200, "GET /v1/conversations");
    assert!(list.text.contains(&cid));
    credential_requests += 4;

    // 5. Numbering: the catalogue and a search against the synthetic provider.
    let r = gw.get("/v1/numbers/countries", cred).await;
    status(&r, 200, "GET /v1/numbers/countries");
    let r = gw.get("/v1/numbers/search?countryCode=US", cred).await;
    status(&r, 200, "GET /v1/numbers/search");
    assert_eq!(
        r.body["inventory"]["inventoryClass"], "SYNTHETIC",
        "deterministic inventory only"
    );
    assert!(!r.body["data"].as_array().unwrap().is_empty(), "{}", r.text);
    credential_requests += 2;

    // 6. SMS: the sandbox message list; a send from a number the environment does not
    //    hold is refused by the SMS Gateway itself (domain authorization, no provider).
    let r = gw.get("/v1/sms/messages", cred).await;
    status(&r, 200, "GET /v1/sms/messages");
    let r = gw
        .post(
            "/v1/sms/messages",
            cred,
            json!({ "fromNumber": Uuid::new_v4(), "to": "+15555550100", "text": "sandbox" }),
        )
        .await;
    assert!(
        (400..500).contains(&r.status) && r.status != 401 && r.status != 429,
        "POST /v1/sms/messages from an unknown number: {}",
        r.text
    );
    credential_requests += 2;

    // 7. Privacy Relay (through Calling's public routes).
    let r = gw.get("/v1/privacy-relay/contexts", cred).await;
    status(&r, 200, "GET /v1/privacy-relay/contexts");
    credential_requests += 1;

    // 8. Usage counters: every accepted credential request is durably accounted at the
    //    Developer Platform (the gateway flushes `devplat_api_usage_daily`).
    let mut seen = 0;
    for _ in 0..200 {
        seen = durable_usage(&stack, sb.environment_id).await;
        if seen >= credential_requests {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        seen >= credential_requests,
        "durable usage {seen} < {credential_requests} accepted credential requests"
    );
    let r = gw
        .get(
            &format!("/v1/environments/{}/entitlements", sb.environment_id),
            cred,
        )
        .await;
    status(&r, 200, "entitlements with the credential");
    stack.teardown().await;
}

fn conference_id(r: &Resp) -> String {
    for p in ["/conference/id", "/id", "/conferenceId", "/data/id"] {
        if let Some(id) = r.body.pointer(p).and_then(Value::as_str) {
            return id.to_string();
        }
    }
    panic!("no conference id in {}", r.text);
}

// =====================================================================================
// Credential revocation
// =====================================================================================

/// A gateway hop for `customer` signed with `signer` (the REAL gateway key replays what
/// the gateway forwards; any other key is a forgery).
fn forwarded_headers(
    signer: &cf_service_auth::ServiceTokenSigner,
    audience: ServiceId,
    c: &Customer,
) -> reqwest::header::HeaderMap {
    let ctx = DelegatedPlatformContext::new(
        c.tenant_id,
        c.application_id,
        c.environment_id,
        ApplicationEnvironmentKind::Sandbox,
        c.service_account_id,
    )
    .with_initiating_credential(c.credential_id);
    signer
        .sign(&OutboundServiceRequest::new_operation(audience).with_delegated_context(ctx))
        .expect("signs")
        .headers()
}

type Receiver = (&'static str, ServiceId, Method, String, bool, Option<Value>);

/// A direct call to a receiver (revocation journey) carrying `signer`'s gateway hop for
/// `c` — the real gateway key replays a forwarded request, any other key forges one.
async fn hop(
    stack: &Stack,
    c: &Customer,
    acting: Uuid,
    rx: &Receiver,
    signer: &cf_service_auth::ServiceTokenSigner,
) -> Resp {
    let (service, audience, method, path, needs_acting, body) = rx.clone();
    let mut r = stack
        .http
        .request(method, format!("{}{path}", stack.url(service)))
        .headers(forwarded_headers(signer, audience, c))
        .header("idempotency-key", Uuid::new_v4().to_string());
    if needs_acting {
        r = r.header("cf-acting-principal", acting.to_string());
    }
    if let Some(b) = body {
        r = r.json(&b);
    }
    Resp::from(r.send().await.expect("receiver reachable")).await
}

#[tokio::test]
async fn credential_revocation_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(&stack, &email("revocation"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let c = sb.customer.clone();
    let alice = operator.principal(c.environment_id).await.unwrap();
    let cred = Auth::Credential(&c.secret);
    let gateway_key = stack.keys.signer(services::API_GATEWAY);
    // A forger who knows the gateway's identity and key id but not its key.
    let forger = cf_service_auth::ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        cfmeet_certification::keys::RUNTIME,
        cf_service_auth::ServiceSigningKey::generate(
            cf_service_auth::KeyId::parse(&cfmeet_certification::keys::kid(services::API_GATEWAY))
                .unwrap(),
        ),
        cf_service_auth::TokenTtl::default(),
        std::sync::Arc::new(cf_service_auth::SystemClock),
    );
    allow_managed(&gw, &c.secret).await;
    let conv_body = json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" });
    // (service, audience, method, path, needs acting principal, body)
    let receivers: Vec<Receiver> = vec![
        (
            stack::CALLING,
            ServiceId::calling(),
            Method::GET,
            "/v1/calls".into(),
            false,
            None,
        ),
        (
            stack::MESSAGING,
            ServiceId::messaging(),
            Method::POST,
            "/v1/conversations".into(),
            true,
            Some(conv_body.clone()),
        ),
        (
            stack::NUMBERING,
            ServiceId::numbering(),
            Method::GET,
            "/v1/numbers/countries".into(),
            false,
            None,
        ),
        (
            stack::SMS_GATEWAY,
            ServiceId::sms_gateway(),
            Method::GET,
            "/v1/sms/messages".into(),
            false,
            None,
        ),
        (
            stack::DEVELOPER_PLATFORM,
            ServiceId::developer_platform(),
            Method::GET,
            format!("/v1/organizations/{}", sb.organization_id),
            false,
            None,
        ),
    ];
    // 1. Before revocation: the gateway path and a correctly signed forwarded hop work;
    //    a forged hop is refused by every receiver.
    status(
        &gw.get("/v1/calls", cred).await,
        200,
        "gateway GET /v1/calls",
    );
    status(
        &gw.get("/v1/sms/messages", cred).await,
        200,
        "gateway GET /v1/sms/messages",
    );
    let r = gw
        .acting(
            Method::POST,
            "/v1/conversations",
            cred,
            alice,
            Some(conv_body.clone()),
        )
        .await;
    status(&r, 201, "gateway POST /v1/conversations");
    for rx in &receivers {
        let ok = hop(&stack, &c, alice, rx, &gateway_key).await;
        assert!(
            ok.status == 200 || ok.status == 201,
            "{} {}: a gateway-signed hop is accepted before revocation: {}",
            rx.0,
            rx.3,
            ok.text
        );
        let forged = hop(&stack, &c, alice, rx, &forger).await;
        status(
            &forged,
            401,
            &format!("{} {}: forged gateway hop", rx.0, rx.3),
        );
    }
    // 2. Revocation at the identity authority.
    operator.revoke(c.credential_id).await.unwrap();
    // 3. The very next gateway request is refused (Platform Access verifies every one) ...
    for path in [
        "/v1/calls",
        "/v1/sms/messages",
        "/v1/numbers/countries",
        "/v1/developer-platform/context",
    ] {
        let r = gw.get(path, cred).await;
        status(&r, 401, &format!("gateway {path} after revocation"));
    }
    let r = gw
        .acting(
            Method::POST,
            "/v1/conversations",
            cred,
            alice,
            Some(conv_body.clone()),
        )
        .await;
    status(&r, 401, "gateway POST /v1/conversations after revocation");
    // ... and so is a replayed, CORRECTLY signed gateway hop carrying the revoked
    // credential: every receiver re-verifies the initiating credential with Platform
    // Access and never trusts the forwarded context alone.
    for rx in &receivers {
        let r = hop(&stack, &c, alice, rx, &gateway_key).await;
        assert!(
            r.status == 401 || r.status == 403,
            "{} {}: a forwarded hop of a revoked credential must be refused: {} {}",
            rx.0,
            rx.3,
            r.status,
            r.text
        );
    }
    stack.teardown().await;
}

// =====================================================================================
// Quota enforcement
// =====================================================================================

#[tokio::test]
async fn quota_enforcement_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(&stack, &email("quota"), SANDBOX_SCOPES)
        .await
        .unwrap();
    // A strict PROJECT plan (stricter than the platform default and the SANDBOX cap),
    // assigned through the Developer Platform operator API BEFORE the environment's
    // first request (the gateway caches effective limits briefly).
    const LIMIT: u64 = 6;
    let plan = operator
        .publish_plan(
            &format!("cert-quota-{}", &tag()[..6]),
            json!({ "apiRequestsPerMinute": LIMIT, "apiRequestsPerDay": 1000 }),
        )
        .await
        .unwrap();
    let r = operator
        .assign_plan(sb.organization_id, plan, Some(sb.project_id))
        .await
        .unwrap();
    status(&r, 201, "operator plan assignment");
    let ent = gw
        .get(
            &format!("/v1/environments/{}/entitlements", sb.environment_id),
            Auth::Session(&sb.user.session),
        )
        .await;
    status(&ent, 200, "entitlements");
    assert_eq!(ent.body["apiRequestsPerMinute"], LIMIT, "{}", ent.text);
    assert_eq!(
        ent.body["limitingLevel"]["apiRequestsPerMinute"],
        "PROJECT_PLAN"
    );
    // Stay inside one minute window.
    let sec = chrono::Utc::now().timestamp() % 60;
    if sec > 40 {
        tokio::time::sleep(Duration::from_secs((61 - sec) as u64)).await;
    }
    let secret = sb.customer.secret.clone();
    let cred = Auth::Credential(&secret);
    // One credential, five services: the per-minute limit is AGGREGATE.
    let paths = [
        "/v1/calls",
        "/v1/messaging/policy",
        "/v1/numbers/countries",
        "/v1/sms/messages",
        "/v1/developer-platform/context",
        "/v1/privacy-relay/contexts",
    ];
    for (i, p) in paths.iter().enumerate() {
        let r = gw.get(p, cred).await;
        status(&r, 200, p);
        assert_eq!(
            r.header("x-cf-quota-limit-minute"),
            Some(LIMIT.to_string().as_str()),
            "{p}"
        );
        let remaining: u64 = r
            .header("x-cf-quota-remaining-minute")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            remaining,
            LIMIT - 1 - i as u64,
            "{p}: remaining across services"
        );
    }
    // The next request, on yet another service, is over the aggregate limit.
    let r = gw.get("/v1/numbers/search?countryCode=US", cred).await;
    status(&r, 429, "aggregate quota exceeded");
    assert_eq!(r.code(), Some("QUOTA_EXCEEDED"), "{}", r.text);
    let retry: u64 = r
        .header("retry-after")
        .expect("Retry-After")
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry), "Retry-After {retry}");
    assert_eq!(r.header("x-cf-quota-window"), Some("requests-per-minute"));
    assert_eq!(r.header("x-cf-quota-level"), Some("PROJECT_PLAN"));
    // Quotas are per environment: another tenant's credential is unaffected.
    let other = operator
        .customer("EXTERNAL", "SANDBOX", SANDBOX_SCOPES)
        .await
        .unwrap();
    let r = gw.get("/v1/calls", Auth::Credential(&other.secret)).await;
    status(&r, 200, "another environment");
    assert_eq!(
        r.header("x-cf-quota-limit-minute"),
        Some("120"),
        "SANDBOX cap"
    );
    // Domain-owned limits still apply below the platform quota: Numbering's own
    // number-search limit answers 429 RATE_LIMITED while the platform quota has room.
    let mut domain_limited = None;
    for _ in 0..(stack::NUMBER_SEARCHES_PER_MINUTE + 2) {
        let r = gw
            .get(
                "/v1/numbers/search?countryCode=US",
                Auth::Credential(&other.secret),
            )
            .await;
        if r.status == 429 {
            domain_limited = Some(r);
            break;
        }
        status(&r, 200, "number search under the domain limit");
    }
    let r = domain_limited.expect("Numbering's own search limit applies");
    assert_eq!(
        r.code(),
        Some("RATE_LIMITED"),
        "owned by Numbering: {}",
        r.text
    );
    assert!(r.header("retry-after").is_some());
    stack.teardown().await;
}

// =====================================================================================
// Tenant isolation
// =====================================================================================

#[tokio::test]
async fn tenant_isolation_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let a = provision::developer_sandbox(&stack, &email("tenant-a"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let b = provision::developer_sandbox(&stack, &email("tenant-b"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let b_session = Auth::Session(&b.user.session);
    let b_cred = Auth::Credential(&b.customer.secret);
    // Developer Platform IDOR — sessions and credentials of tenant B on A's hierarchy.
    for (method, path, body) in [
        (
            Method::GET,
            format!("/v1/organizations/{}", a.organization_id),
            None,
        ),
        (
            Method::PATCH,
            format!("/v1/organizations/{}", a.organization_id),
            Some(json!({ "expectedVersion": 1, "displayName": "pwned" })),
        ),
        (
            Method::GET,
            format!("/v1/organizations/{}/members", a.organization_id),
            None,
        ),
        (
            Method::GET,
            format!("/v1/organizations/{}/projects", a.organization_id),
            None,
        ),
        (
            Method::POST,
            format!("/v1/organizations/{}/projects", a.organization_id),
            Some(json!({ "displayName": "intruder" })),
        ),
        (Method::GET, format!("/v1/projects/{}", a.project_id), None),
        (
            Method::GET,
            format!("/v1/projects/{}/environments", a.project_id),
            None,
        ),
        (
            Method::POST,
            format!("/v1/projects/{}/environments", a.project_id),
            Some(json!({ "kind": "SANDBOX" })),
        ),
        (
            Method::GET,
            format!("/v1/environments/{}", a.environment_id),
            None,
        ),
        (
            Method::GET,
            format!("/v1/environments/{}/entitlements", a.environment_id),
            None,
        ),
        (
            Method::POST,
            format!("/v1/environments/{}/suspend", a.environment_id),
            Some(json!({})),
        ),
    ] {
        for (who, auth) in [("session", b_session), ("credential", b_cred)] {
            let r = gw
                .request(method.clone(), &path, auth, body.clone(), &[])
                .await;
            assert_eq!(r.status, 404, "{who} {method} {path}: {}", r.text);
            assert!(
                !r.text.contains(&a.organization_id.to_string()),
                "leak: {}",
                r.text
            );
        }
    }
    let list = gw.get("/v1/organizations", b_session).await;
    status(&list, 200, "B lists organizations");
    assert!(
        !list.text.contains(&a.organization_id.to_string()),
        "leak: {}",
        list.text
    );
    // Domain resources of A: a conversation (Messaging), an endpoint (Calling).
    let a_cred = Auth::Credential(&a.customer.secret);
    let alice = operator.principal(a.environment_id).await.unwrap();
    let mallory = operator.principal(b.environment_id).await.unwrap();
    allow_managed(&gw, &a.customer.secret).await;
    allow_managed(&gw, &b.customer.secret).await;
    let cid = conversation(&gw, &a.customer.secret, alice).await;
    let ep = endpoint(&gw, &a.customer.secret, alice).await;
    let m = gw
        .acting(
            Method::POST,
            &format!("/v1/conversations/{cid}/messages"),
            a_cred,
            alice,
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "tenant A secret" })),
        )
        .await;
    status(&m, 201, "A posts");
    for (method, path, body) in [
        (Method::GET, format!("/v1/conversations/{cid}"), None),
        (
            Method::GET,
            format!("/v1/conversations/{cid}/messages"),
            None,
        ),
        (
            Method::POST,
            format!("/v1/conversations/{cid}/messages"),
            Some(json!({ "clientMessageId": Uuid::new_v4(), "text": "intrusion" })),
        ),
        (
            Method::GET,
            format!("/v1/conversations/{cid}/members"),
            None,
        ),
    ] {
        let r = gw
            .acting(method.clone(), &path, b_cred, mallory, body)
            .await;
        assert_eq!(r.status, 404, "B {method} {path}: {}", r.text);
        assert!(!r.text.contains("tenant A secret"));
    }
    // Acting as A's principal with B's credential does not cross the tenant boundary.
    let r = gw
        .acting(
            Method::GET,
            &format!("/v1/conversations/{cid}"),
            b_cred,
            alice,
            None,
        )
        .await;
    assert!(
        r.status == 404 || r.status == 403 || r.status == 422,
        "B acting as A's principal: {}",
        r.text
    );
    assert!(!r.text.contains("tenant A secret"));
    let r = gw
        .acting(Method::GET, "/v1/conversations", b_cred, mallory, None)
        .await;
    status(&r, 200, "B lists conversations");
    assert!(!r.text.contains(&cid), "leak: {}", r.text);
    let r = gw
        .request(
            Method::DELETE,
            &format!("/v1/calling/endpoints/{ep}"),
            b_cred,
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, 404, "B revokes A's endpoint: {}", r.text);
    let r = gw
        .get(
            &format!("/v1/calling/endpoints?principalId={alice}"),
            b_cred,
        )
        .await;
    assert!(
        r.status == 404 || (r.status == 200 && !r.text.contains(&ep.to_string())),
        "B lists A's endpoints: {}",
        r.text
    );
    // A's endpoint is intact.
    let r = gw
        .get(
            &format!("/v1/calling/endpoints?principalId={alice}"),
            a_cred,
        )
        .await;
    status(&r, 200, "A lists its endpoints");
    assert!(r.text.contains(&ep.to_string()));
    // Calls and SMS messages: ids of another environment are indistinguishable from
    // unknown ids (neither can be created in the stack — no media provider, no number).
    for path in [
        format!("/v1/calls/{}", Uuid::new_v4()),
        format!("/v1/sms/messages/{}", Uuid::new_v4()),
    ] {
        let r = gw.get(&path, b_cred).await;
        status(&r, 404, &path);
    }
    let r = gw.get("/v1/sms/messages", b_cred).await;
    status(&r, 200, "B lists SMS");
    stack.teardown().await;
}

// =====================================================================================
// WebSocket gateway
// =====================================================================================

/// Registers a Messaging device for `principal` (real MLS KeyPackage) and returns its id.
async fn messaging_device(gw: &Gateway, secret: &str, principal: Uuid) -> Uuid {
    let ch = gw
        .acting(
            Method::POST,
            "/v1/messaging/devices/registration-challenges",
            Auth::Credential(secret),
            principal,
            Some(json!({})),
        )
        .await;
    status(&ch, 201, "registration challenge");
    let device_id = ch.uuid("/deviceId");
    let device = cf_messaging_crypto::device::Device::generate(device_id).unwrap();
    let nonce: [u8; 32] = hex::decode(ch.str("/nonce")).unwrap().try_into().unwrap();
    let kp = device
        .registration_key_package(ch.uuid("/challengeId"), &nonce)
        .unwrap();
    use base64::Engine;
    let d = gw
        .acting(
            Method::POST,
            "/v1/messaging/devices",
            Auth::Credential(secret),
            principal,
            Some(json!({ "challengeId": ch.str("/challengeId"),
                         "keyPackage": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&kp) })),
        )
        .await;
    status(&d, 201, "device registration");
    device_id
}

#[tokio::test]
async fn websocket_gateway_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(&stack, &email("realtime"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let secret = sb.customer.secret.clone();
    let base = stack.gateway_url().to_string();
    let wait = Duration::from_secs(10);

    // --- Messaging realtime: client token -> ticket -> /v1/realtime (subprotocol).
    let alice = operator.principal(sb.environment_id).await.unwrap();
    allow_managed(&gw, &secret).await;
    let cid = conversation(&gw, &secret, alice).await;
    let device = messaging_device(&gw, &secret, alice).await;
    let ct = gw
        .acting(
            Method::POST,
            "/v1/messaging/client-tokens",
            Auth::Credential(&secret),
            alice,
            Some(json!({ "deviceId": device })),
        )
        .await;
    status(&ct, 201, "client token");
    let client_token = ct.str("/token");
    let ticket = |gw: Gateway, token: String| async move {
        let r = gw
            .post("/v1/realtime/tickets", Auth::ClientToken(&token), json!({}))
            .await;
        status(&r, 201, "realtime ticket");
        r.str("/ticket")
    };
    let t1 = ticket(gw.clone(), client_token.clone()).await;
    let proto = |t: &str| format!("cfmeet.messaging.v1, cfmeet.ticket.{t}");
    let mut ws = ws::open(&format!("{base}/v1/realtime"), Some(&proto(&t1)), None)
        .await
        .unwrap()
        .expect("messaging realtime through the gateway");
    assert_eq!(
        ws.protocol.as_deref(),
        Some("cfmeet.messaging.v1"),
        "ticket never echoed"
    );
    let welcome = ws.next_of("welcome", wait).await.unwrap();
    assert_eq!(welcome["protocolVersion"], 1);
    ws.send_json(&json!({ "type": "ping", "requestId": "hb-1" }))
        .await
        .unwrap();
    ws.next_of("pong", wait).await.unwrap();
    ws.ping(wait).await.unwrap();
    ws.send_json(&json!({ "type": "subscribe", "conversationId": cid, "requestId": "s1" }))
        .await
        .unwrap();
    let sub = ws.next_of("subscribed", wait).await.unwrap();
    assert_eq!(sub["conversationId"], json!(cid));
    let code = ws.close().await.unwrap();
    assert!(
        code.is_none() || code == Some(1000),
        "clean close: {code:?}"
    );
    // A used ticket is single-use; a fresh one reconnects.
    let reused = ws::open(&format!("{base}/v1/realtime"), Some(&proto(&t1)), None)
        .await
        .unwrap();
    let (st, _) = reused.err().expect("a used ticket is refused");
    assert_eq!(st, 401, "Messaging's refusal relayed");
    let t2 = ticket(gw.clone(), client_token.clone()).await;
    let mut ws = ws::open(&format!("{base}/v1/realtime"), Some(&proto(&t2)), None)
        .await
        .unwrap()
        .expect("reconnect with a fresh ticket");
    ws.next_of("welcome", wait).await.unwrap();
    ws.close().await.unwrap();
    let bad = ws::open(
        &format!("{base}/v1/realtime"),
        Some(&proto("not-a-ticket")),
        None,
    )
    .await
    .unwrap();
    assert_eq!(bad.err().expect("bad ticket refused").0, 401);

    // --- Calling signaling: signaling credential -> /v1/calling/signaling.
    let ep = endpoint(&gw, &secret, alice).await;
    let sig = signaling_credential(&gw, &secret, alice, ep).await;
    let sig_proto = |t: &str| format!("cfmeet.signaling.v1, bearer.{t}");
    let mut ws = ws::open(
        &format!("{base}/v1/calling/signaling"),
        Some(&sig_proto(&sig)),
        None,
    )
    .await
    .unwrap()
    .expect("calling signaling through the gateway");
    assert_eq!(ws.protocol.as_deref(), Some("cfmeet.signaling.v1"));
    ws.send_json(&json!({ "type": "ping" })).await.unwrap();
    ws.next_of("signaling.pong", wait).await.unwrap();
    ws.ping(wait).await.unwrap();
    let code = ws.close().await.unwrap();
    assert!(
        code.is_none() || code == Some(1000),
        "clean close: {code:?}"
    );
    // Reconnect with a fresh credential, in the Authorization header this time.
    let sig2 = signaling_credential(&gw, &secret, alice, ep).await;
    let mut ws = ws::open(&format!("{base}/v1/calling/signaling"), None, Some(&sig2))
        .await
        .unwrap()
        .expect("signaling reconnect");
    ws.send_json(&json!({ "type": "ping" })).await.unwrap();
    ws.next_of("signaling.pong", wait).await.unwrap();
    ws.close().await.unwrap();
    let bad = ws::open(
        &format!("{base}/v1/calling/signaling"),
        Some(&sig_proto("cfsig_forged.token.value")),
        None,
    )
    .await
    .unwrap();
    assert_eq!(bad.err().expect("bad signaling credential refused").0, 401);
    // A platform credential is never transported to a WebSocket.
    let r = ws::open(&format!("{base}/v1/calling/signaling"), None, Some(&secret))
        .await
        .unwrap();
    assert_eq!(r.err().expect("platform credential refused").0, 401);

    // --- Meeting realtime: conference ticket -> /api/v2/conferences/{id}/realtime.
    let session = Auth::Session(&sb.user.session);
    let r = gw
        .post(
            "/api/v2/instant-meetings",
            session,
            json!({ "title": "Realtime" }),
        )
        .await;
    assert!(r.status == 201 || r.status == 200, "{}", r.text);
    let conf = conference_id(&r);
    let mticket = |gw: Gateway, token: String, conf: String| async move {
        let r = gw
            .post(
                &format!("/api/v2/conferences/{conf}/realtime/ticket"),
                Auth::Session(&token),
                json!({}),
            )
            .await;
        status(&r, 200, "meeting realtime ticket");
        r.str("/ticket")
    };
    let mt = mticket(gw.clone(), sb.user.session.clone(), conf.clone()).await;
    let murl = |t: &str| format!("{base}/api/v2/conferences/{conf}/realtime?ticket={t}");
    let mut ws = ws::open(&murl(&mt), None, None)
        .await
        .unwrap()
        .expect("meeting realtime through the gateway");
    let sub = ws.next_of("subscribed", wait).await.unwrap();
    assert_eq!(sub["scope"], "FULL", "{sub}");
    ws.send_text("ping").await.unwrap();
    ws.next_of("pong", wait).await.unwrap();
    ws.ping(wait).await.unwrap();
    let code = ws.close().await.unwrap();
    assert!(
        code.is_none() || code == Some(1000),
        "clean close: {code:?}"
    );
    // (Meeting realtime tickets are documented as one-shot but are stateless 60-second
    // tokens today — see the certification report; reuse is not asserted here.)
    let mt2 = mticket(gw.clone(), sb.user.session.clone(), conf.clone()).await;
    let mut ws = ws::open(&murl(&mt2), None, None)
        .await
        .unwrap()
        .expect("meeting reconnect");
    ws.next_of("subscribed", wait).await.unwrap();
    ws.close().await.unwrap();
    let bad = ws::open(&murl("bad.ticket.value"), None, None)
        .await
        .unwrap();
    let (st, _) = bad.err().expect("bad meeting ticket refused");
    assert!(st == 401 || st == 403, "Meeting's refusal relayed: {st}");
    stack.teardown().await;
}

// =====================================================================================
// Internal CosmicForge product (first-party)
// =====================================================================================

#[tokio::test]
async fn first_party_product_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    // 1. The operator registers a FIRST_PARTY tenant and its Developer Platform
    //    organization (INVITED, owner invitation returned once).
    let tenant = operator.tenant("FIRST_PARTY").await.unwrap();
    let owner_email = email("product-owner");
    let reg = operator
        .register_first_party(tenant, &owner_email)
        .await
        .unwrap();
    status(&reg, 201, "register_first_party");
    let token = reg.str("/ownerInvitation/token");
    // 2. The product team's owner accepts with their CF Meet session — the same public
    //    Developer Platform routes external customers use.
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
        .post("/v1/invitations/accept", session, json!({ "token": token }))
        .await;
    assert!(r.status == 200 || r.status == 201, "accept: {}", r.text);
    let org = gw
        .get(&format!("/v1/organizations/{tenant}"), session)
        .await;
    status(&org, 200, "first-party organization");
    assert_eq!(org.body["classification"], "FIRST_PARTY");
    assert_eq!(org.body["state"], "ACTIVE");
    let project = gw
        .post(
            &format!("/v1/organizations/{tenant}/projects"),
            session,
            json!({ "displayName": "Internal product" }),
        )
        .await;
    status(&project, 201, "first-party project");
    let project_id = project.uuid("/id");
    let env = gw
        .post(
            &format!("/v1/projects/{project_id}/environments"),
            session,
            json!({ "kind": "SANDBOX" }),
        )
        .await;
    status(&env, 201, "first-party environment");
    let env_id = env.uuid("/id");
    // 3. First-party-only and privileged operator scopes are grantable to it ...
    for s in FIRST_PARTY_ONLY_SCOPES {
        let r = operator.grant_scope(project_id, s).await.unwrap();
        assert!(
            (200..300).contains(&r.status),
            "first-party grant {s}: {}",
            r.text
        );
    }
    for s in PRIVILEGED_OPERATOR_SCOPES {
        let r = operator
            .grant_privileged_scope(project_id, s)
            .await
            .unwrap();
        assert!(
            (200..300).contains(&r.status),
            "privileged grant {s}: {}",
            r.text
        );
    }
    let mut standard: Vec<&str> = SANDBOX_SCOPES.to_vec();
    standard.extend_from_slice(FIRST_PARTY_ONLY_SCOPES);
    operator.grant_scopes(project_id, &standard).await.unwrap();
    let mut scopes = standard.clone();
    scopes.extend_from_slice(PRIVILEGED_OPERATOR_SCOPES);
    let sa = operator.service_account(env_id).await.unwrap();
    let (_, fp_secret) = operator
        .credential(sa, &scopes)
        .await
        .expect("a first-party credential holding first-party-only and privileged scopes");
    let fp_cred = Auth::Credential(&fp_secret);
    // ... and its credential uses the SAME public routes as any customer.
    let ctx = gw.get("/v1/developer-platform/context", fp_cred).await;
    status(&ctx, 200, "first-party context");
    assert_eq!(ctx.body["tenantId"], json!(tenant));
    for p in [
        "/v1/calls",
        "/v1/sms/messages",
        "/v1/numbers/countries",
        "/v1/messaging/policy",
        "/v1/privacy-relay/contexts",
    ] {
        status(&gw.get(p, fp_cred).await, 200, &format!("first-party {p}"));
    }
    // 4. An external tenant can neither be granted those scopes nor obtain them.
    let ext = operator
        .customer("EXTERNAL", "SANDBOX", &["calls.read"])
        .await
        .unwrap();
    for s in FIRST_PARTY_ONLY_SCOPES {
        let r = operator.grant_scope(ext.application_id, s).await.unwrap();
        assert!(
            (400..500).contains(&r.status),
            "external grant {s} must be refused: {}",
            r.text
        );
    }
    for s in PRIVILEGED_OPERATOR_SCOPES {
        let r = operator
            .grant_privileged_scope(ext.application_id, s)
            .await
            .unwrap();
        assert!(
            (400..500).contains(&r.status),
            "external privileged grant {s} must be refused: {}",
            r.text
        );
    }
    let r = operator
        .pa(
            Method::POST,
            &format!(
                "/admin/service-accounts/{}/credentials",
                ext.service_account_id
            ),
            Some(json!({ "scopes": FIRST_PARTY_ONLY_SCOPES, "label": "escalation" })),
        )
        .await
        .unwrap();
    assert!(
        (400..500).contains(&r.status),
        "external credential with first-party scopes: {}",
        r.text
    );
    // A route scope the external credential lacks is refused at the gateway.
    let r = gw
        .get("/v1/sms/messages", Auth::Credential(&ext.secret))
        .await;
    assert!(
        r.status == 403,
        "external credential without sms.read: {}",
        r.text
    );
    // 5. No internal route is reachable through the gateway, first-party or not.
    for p in [
        "/internal/v1/gateway/context",
        "/internal/v1/credentials/authenticate",
        "/internal/v1/admin/tenants",
        "/internal/v1/operator/organizations",
        "/internal/v1/user-sessions/verify",
        "/internal/v1/entitlements/environments/x",
        "/internal/v1/usage/api-requests",
        "/internal/v1/platform/events",
        "/internal/rtc/metrics",
        "/internal/metrics",
        "/v1/admin/catalog/countries",
        "/api/v1/admin/users",
        "/metrics",
    ] {
        for m in [Method::GET, Method::POST] {
            let r = gw
                .request(
                    m.clone(),
                    p,
                    fp_cred,
                    (m == Method::POST).then(|| json!({})),
                    &[],
                )
                .await;
            assert_eq!(r.status, 404, "{m} {p} through the gateway: {}", r.text);
        }
    }
    stack.teardown().await;
}

// =====================================================================================
// Production approval
// =====================================================================================

#[tokio::test]
async fn production_approval_journey() {
    let Some((_g, stack)) = start().await else {
        return;
    };
    let gw = stack.gateway();
    let operator = stack.operator();
    let sb = provision::developer_sandbox(&stack, &email("prod"), SANDBOX_SCOPES)
        .await
        .unwrap();
    let session = Auth::Session(&sb.user.session);
    // 1. Customers cannot create PRODUCTION environments (session or credential): the
    //    Developer Platform refuses before delegating to Platform Access.
    for auth in [session, Auth::Credential(&sb.customer.secret)] {
        let r = gw
            .post(
                &format!("/v1/projects/{}/environments", sb.project_id),
                auth,
                json!({ "kind": "PRODUCTION" }),
            )
            .await;
        status(&r, 422, "PRODUCTION environment by a customer");
        assert_eq!(r.code(), Some("POLICY_VIOLATION"));
    }
    let envs = gw
        .get(
            &format!("/v1/projects/{}/environments", sb.project_id),
            session,
        )
        .await;
    status(&envs, 200, "environments");
    assert!(!envs.text.contains("PRODUCTION"), "{}", envs.text);
    // 2. There is no production-access request workflow in the current Developer
    //    Platform: such routes are not in the gateway catalogue (default deny).
    for p in [
        format!("/v1/projects/{}/production-access", sb.project_id),
        format!("/v1/projects/{}/production-access-requests", sb.project_id),
        "/v1/production-access-requests".to_string(),
    ] {
        status(&gw.post(&p, session, json!({})).await, 404, &p);
    }
    // 3. The operator creates the PRODUCTION environment through Platform Access.
    let prod = operator
        .environment(sb.project_id, "PRODUCTION")
        .await
        .unwrap();
    assert!(
        (200..300).contains(&prod.status),
        "operator PRODUCTION environment: {}",
        prod.text
    );
    let prod_env = prod.uuid("/id");
    let pc = operator
        .credential_for(
            sb.organization_id,
            sb.project_id,
            prod_env,
            &["calls.read", "numbers.search", "sms.read"],
        )
        .await
        .unwrap();
    let pcred = Auth::Credential(&pc.secret);
    let ctx = gw.get("/v1/developer-platform/context", pcred).await;
    status(&ctx, 200, "production context");
    assert_eq!(ctx.body["environmentKind"], "PRODUCTION");
    let r = gw.get("/v1/calls", pcred).await;
    status(&r, 200, "production credential on Calling");
    assert_eq!(
        r.header("x-cf-quota-limit-minute"),
        Some("600"),
        "no sandbox cap in PRODUCTION"
    );
    // 4. Unauthorized capabilities stay blocked: a scope the credential does not hold is
    //    refused at the gateway, and Platform Access refuses to issue a credential with a
    //    scope the application is not allowed.
    let r = gw
        .post("/v1/calls", pcred, json!({ "kind": "VOICE", "initiatorPrincipalId": Uuid::new_v4(), "targetPrincipalIds": [Uuid::new_v4()] }))
        .await;
    status(&r, 403, "calls.write not held");
    let r = operator
        .pa(
            Method::POST,
            &format!(
                "/admin/service-accounts/{}/credentials",
                pc.service_account_id
            ),
            Some(json!({ "scopes": ["numbers.portability"], "label": "escalation" })),
        )
        .await
        .unwrap();
    assert!(
        (400..500).contains(&r.status),
        "scope not allowed on the application: {}",
        r.text
    );
    // Live telecom stays an explicit operator decision per environment: a PRODUCTION
    // credential without it sees only what Numbering allows without live telecom.
    let r = gw.get("/v1/numbers/search?countryCode=US", pcred).await;
    assert!(
        r.status == 200 || (400..500).contains(&r.status),
        "{}",
        r.text
    );
    if r.status == 200 {
        assert!(
            !r.text.contains("\"inventoryClass\":\"LIVE\""),
            "{}",
            r.text
        );
    }
    stack.teardown().await;
}
