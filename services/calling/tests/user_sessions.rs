//! ADR 0062 / 0063 (`docs/platform/build-internal-contracts.md` §4, §6 N-P3, §7): the
//! CF Meet Platform device exchange `POST /v1/calling/user-sessions` and partner call push.
//!
//! Platform Access is real (the minting credential's chain is re-verified live through
//! it, exactly like a gateway-forwarded request); its user-token verification
//! (`POST /internal/v1/user-tokens/verify`) is faked by [`FakeTokens`], whose answers name
//! the world's real credential, service account and principals.
//!
//! * the happy path, the endpoint per installation (create, reuse, hand-over), the
//!   signaling credential bound to the token (works on the signaling routes, never
//!   outlives the token), the body read only after authentication;
//! * default off: the route does not exist;
//! * refusals: FIRST_PARTY tenant, missing `call:receive`, invalid / expired / revoked
//!   token, inactive user, revoked minting credential, unreachable verification;
//! * cross-tenant: one tenant's signaling credential never reaches another's offer;
//! * the delivery-app flow: one partner user calls another (server-placed), the callee's
//!   exchanged endpoint receives the offer on the signaling channel, accepts it and gets a
//!   media credential; with `CALLING_PARTNER_PUSH_ENABLED` the offer also produces one
//!   `platform-principal` push recipient per (call, user), first-party offers unchanged.
mod common;

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use calling::{
    domain::{CredentialId, EnvironmentKind, ManualClock, PrincipalStatus, TenantClassification},
    platform::{
        context::PlatformAuthContext,
        principals::ResolvedPrincipal,
        scopes::names,
        user_tokens::{
            UserTokenAuthority, UserTokenError, UserTokenGrants, UserTokenUser, UserTokenVerifier,
            VerifiedUserToken,
        },
    },
    signaling::{CallPushSink, NotificationsPushTransport, PushDispatcher},
};
use cf_service_auth::CorrelationId;
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    fake_media::FakeMediaProvider,
    test_runtime, Harness, TestDb, TestResponse,
};
use futures_util::StreamExt;
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
};
use uuid::Uuid;

const PATH: &str = "/v1/calling/user-sessions";

/// Platform Access's user-token verification, answering per token.
#[derive(Default)]
struct FakeTokens {
    answers: Mutex<HashMap<String, Result<VerifiedUserToken, UserTokenError>>>,
    calls: AtomicUsize,
}

#[async_trait]
impl UserTokenAuthority for FakeTokens {
    async fn verify(
        &self,
        token: &str,
        _: &CorrelationId,
    ) -> Result<VerifiedUserToken, UserTokenError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answers
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .unwrap_or_else(|| Err(UserTokenError::Rejected("USER_TOKEN_INVALID".into())))
    }
}

fn tag(kind: EnvironmentKind) -> &'static str {
    match kind {
        EnvironmentKind::Sandbox => "sbx",
        EnvironmentKind::Test => "test",
        EnvironmentKind::Production => "live",
    }
}

impl FakeTokens {
    /// A token of `principal` minted by `ctx`'s credential, valid for `ttl` seconds on
    /// the service clock, with the `call` grants given.
    fn mint(
        &self,
        ctx: &PlatformAuthContext,
        principal: Uuid,
        call: &[&str],
        now: chrono::DateTime<chrono::Utc>,
        ttl: i64,
    ) -> String {
        let token = format!(
            "cfut_{}_{}.{}.sig",
            tag(ctx.environment_kind()),
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        );
        let verified = VerifiedUserToken {
            token_id: Uuid::new_v4(),
            tenant_id: ctx.tenant_id().as_uuid(),
            application_id: ctx.application_id().as_uuid(),
            environment_id: ctx.environment_id().as_uuid(),
            environment_kind: ctx.environment_kind(),
            credential_id: ctx.credential_id().as_uuid(),
            service_account_id: ctx.service_account_id().as_uuid(),
            user: UserTokenUser {
                id: principal,
                external_id: Some(format!("user-{principal}")),
                display_name: None,
                image_url: None,
                status: PrincipalStatus::Active,
            },
            grants: UserTokenGrants {
                msg: vec!["read".into()],
                call: call.iter().map(|s| s.to_string()).collect(),
                mtg: vec![],
            },
            issued_at: now - chrono::Duration::seconds(5),
            expires_at: now + chrono::Duration::seconds(ttl),
        };
        self.answers
            .lock()
            .unwrap()
            .insert(token.clone(), Ok(verified));
        token
    }

    fn answer(&self, token: &str, answer: Result<VerifiedUserToken, UserTokenError>) {
        self.answers
            .lock()
            .unwrap()
            .insert(token.to_string(), answer);
    }

    fn verified(&self, token: &str) -> VerifiedUserToken {
        self.answers.lock().unwrap()[token].clone().unwrap()
    }
}

/// Turns the exchange on for `a` (as `CALLING_USER_TOKEN_EXCHANGE_ENABLED=true` does).
fn enable(a: &mut AppWorld) -> Arc<FakeTokens> {
    let tokens = Arc::new(FakeTokens::default());
    let verifier = UserTokenVerifier::new(tokens.clone(), a.w.h.state.clock.clone());
    a.w.h.state =
        a.w.h
            .state
            .clone()
            .with_user_sessions(Some(Arc::new(verifier)));
    tokens
}

async fn exchange(h: &Harness, token: &str, installation: &str, kind: &str) -> TestResponse {
    h.request(
        Method::POST,
        PATH,
        Some(token),
        Some(json!({ "installationId": installation, "kind": kind })),
    )
    .await
}

fn now(a: &AppWorld) -> chrono::DateTime<chrono::Utc> {
    a.w.h.state.clock.now()
}

fn device_of(r: &TestResponse) -> (Device, String) {
    (
        Device {
            principal: r.json["principalId"].as_str().unwrap().parse().unwrap(),
            endpoint: r.json["endpointId"].as_str().unwrap().parse().unwrap(),
        },
        r.json["signalingCredential"].as_str().unwrap().to_string(),
    )
}

async fn count(a: &AppWorld, sql: &str) -> i64 {
    a.w.count(sql).await
}

#[tokio::test]
async fn the_route_does_not_exist_unless_enabled() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    for token in ["cfut_live_a.b.c", a.secret()] {
        let r = exchange(a.h(), token, "install-0001", "MOBILE").await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    }
    let r = a.h().request(Method::GET, PATH, None, None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(count(&a, "SELECT COUNT(*) FROM call_endpoints").await, 0);
}

#[tokio::test]
async fn a_user_token_becomes_one_endpoint_per_installation_and_a_bound_signaling_credential() {
    let Some(mut a) = AppWorld::new().await else {
        return;
    };
    let tokens = enable(&mut a);
    let user = a.w.recipient.clone();
    let token = tokens.mint(&a.w.ctx, user.id().as_uuid(), &["receive"], now(&a), 3600);

    // Created: 201, the contract shape exactly.
    let r = exchange(a.h(), &token, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mut keys: Vec<&String> = r.json.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "endpointId",
            "principalId",
            "signalingCredential",
            "signalingCredentialExpiresAt"
        ]
    );
    assert_eq!(r.headers["cache-control"], "no-store");
    assert_eq!(r.json["principalId"], user.id().as_uuid().to_string());
    let (device, signaling) = device_of(&r);
    assert!(signaling.starts_with("cfsig_"));
    // The endpoint is the installation's, registered for the token's user.
    let list = a
        .get(&format!("/v1/calling/endpoints?principalId={}", user.id()))
        .await;
    assert_eq!(list.json["data"].as_array().unwrap().len(), 1);
    assert_eq!(list.json["data"][0]["endpointKey"], "inst:install-0001");
    assert_eq!(list.json["data"][0]["endpointKind"], "MOBILE");
    assert_eq!(
        list.json["data"][0]["capabilities"],
        json!({"audio": true, "video": true})
    );
    // The signaling credential records the token's minting credential.
    let minted_by: Uuid =
        a.w.h
            .conn()
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                    "SELECT registered_by_credential_id FROM call_endpoints WHERE id = '{}'",
                    device.endpoint
                ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
    assert_eq!(minted_by, a.w.ctx.credential_id().as_uuid());
    let claims =
        a.w.h
            .state
            .signaling
            .keys()
            .verify(&signaling, now(&a))
            .unwrap();
    assert_eq!(claims.minted_by_credential_id, a.w.ctx.credential_id());
    assert_eq!(
        claims.minted_by_service_account_id,
        a.w.ctx.service_account_id()
    );
    assert_eq!(claims.endpoint_id.as_uuid(), device.endpoint);
    // … and works on the endpoint's own signaling routes.
    let r = a
        .as_endpoint(
            &signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["endpointId"], device.endpoint.to_string());

    // Reused: the same installation (any number of times) is the same endpoint, 200.
    let r = exchange(a.h(), &token, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(device_of(&r).0.endpoint, device.endpoint);
    // Another installation of the same user: another endpoint.
    let r = exchange(a.h(), &token, "install-0002", "WEB").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_ne!(device_of(&r).0.endpoint, device.endpoint);
    assert_eq!(
        count(
            &a,
            "SELECT COUNT(*) FROM call_endpoints WHERE status = 'ACTIVE'"
        )
        .await,
        2
    );
    // An id too long for the endpoint key is hashed into it (still one per installation).
    let long = "x".repeat(128);
    let first = exchange(a.h(), &token, &long, "WEB").await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.text);
    let again = exchange(a.h(), &token, &long, "WEB").await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(first.json["endpointId"], again.json["endpointId"]);

    // Another user signs in on the same installation: the installation is handed over
    // (the previous user's endpoint is revoked, never shared).
    let other = a.principal("second-user").await;
    let other_token = tokens.mint(&a.w.ctx, other.id().as_uuid(), &["receive"], now(&a), 3600);
    let r = exchange(a.h(), &other_token, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (handed, _) = device_of(&r);
    assert_ne!(handed.endpoint, device.endpoint);
    assert_eq!(handed.principal, other.id().as_uuid());
    let r = a
        .as_endpoint(
            &signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "the previous user's endpoint is revoked"
    );
}

#[tokio::test]
async fn the_signaling_credential_never_outlives_the_user_token() {
    let Some(mut a) = AppWorld::new().await else {
        return;
    };
    let tokens = enable(&mut a);
    let token = tokens.mint(
        &a.w.ctx,
        a.w.recipient.id().as_uuid(),
        &["receive"],
        now(&a),
        120,
    );
    let r = exchange(a.h(), &token, "install-short", "WEB").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let expires = chrono::DateTime::parse_from_rfc3339(
        r.json["signalingCredentialExpiresAt"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(
        expires.timestamp(),
        tokens.verified(&token).expires_at.timestamp(),
        "capped at the token's expiry (the signaling lifetime is 600 s)"
    );
    // Past the token's expiry the token is refused (also from the verification cache).
    a.h().clock.advance(chrono::Duration::seconds(121));
    let r = exchange(a.h(), &token, "install-short", "WEB").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
}

#[tokio::test]
async fn authentication_comes_before_the_body_and_only_user_tokens_are_accepted() {
    let Some(mut a) = AppWorld::new().await else {
        return;
    };
    let tokens = enable(&mut a);
    let caller = a.device(&a.w.initiator, "plain-device").await;
    let signaling = a.signaling_token(&caller).await;
    // No credential, a platform credential, a signaling credential, a malformed token:
    // 401 whatever the body (never a validation answer).
    for token in [
        None,
        Some(a.secret().to_string()),
        Some(signaling),
        Some("cfut_live_".to_string()),
        Some("cfut_live_unknown.token.sig".to_string()),
    ] {
        let r = a
            .h()
            .request(Method::POST, PATH, token.as_deref(), Some(json!(["x"])))
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{token:?}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "AUTHENTICATION_FAILED");
    }
    // A service token alongside is refused too.
    let token = tokens.mint(
        &a.w.ctx,
        a.w.recipient.id().as_uuid(),
        &["receive"],
        now(&a),
        3600,
    );
    let r = a
        .h()
        .request_with(
            Method::POST,
            PATH,
            Some(&token),
            Some(json!({ "installationId": "install-0001", "kind": "WEB" })),
            &[("x-cf-service-authorization", "Bearer x")],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // Authenticated: malformed bodies are validation errors and change nothing.
    for body in [
        json!(["install-0001", "WEB"]),
        json!({ "installationId": "short", "kind": "WEB" }),
        json!({ "installationId": "has:colon-1", "kind": "WEB" }),
        json!({ "installationId": "install-0001", "kind": "DESKTOP" }),
        json!({ "installationId": "install-0001", "kind": "WEB", "principalId": Uuid::new_v4() }),
        json!({ "installationId": "install-0001" }),
    ] {
        let r = a
            .h()
            .request(Method::POST, PATH, Some(&token), Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "VALIDATION_ERROR");
    }
    assert_eq!(
        count(
            &a,
            "SELECT COUNT(*) FROM call_endpoints WHERE endpoint_key LIKE 'inst:%'"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn refused_tokens_and_grants_change_nothing() {
    let Some(mut a) = AppWorld::new().await else {
        return;
    };
    let tokens = enable(&mut a);
    let user = a.w.recipient.id().as_uuid();
    let t = now(&a);

    // No call:receive grant (e.g. the minting key lost calls.write): 403.
    let chat_only = tokens.mint(&a.w.ctx, user, &[], t, 3600);
    let r = exchange(a.h(), &chat_only, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INSUFFICIENT_SCOPE");
    assert!(r.headers["www-authenticate"]
        .to_str()
        .unwrap()
        .contains("call:receive"));

    // Platform Access refuses the token (revoked / expired / invalid): uniform 401.
    for code in [
        "USER_TOKEN_REVOKED",
        "USER_TOKEN_EXPIRED",
        "USER_TOKEN_INVALID",
    ] {
        let token = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
        tokens.answer(&token, Err(UserTokenError::Rejected(code.into())));
        let r = exchange(a.h(), &token, "install-0001", "MOBILE").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{code}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "AUTHENTICATION_FAILED");
    }
    // An answer already past its expiry on the service clock: 401.
    let stale = tokens.mint(&a.w.ctx, user, &["receive"], t, -1);
    let r = exchange(a.h(), &stale, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // A token whose user is suspended: 401.
    let suspended = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
    let mut v = tokens.verified(&suspended);
    v.user.status = PrincipalStatus::Suspended;
    tokens.answer(&suspended, Ok(v));
    let r = exchange(a.h(), &suspended, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // A user that is not in the minting credential's environment: 401.
    let stranger = tokens.mint(&a.w.ctx, Uuid::new_v4(), &["receive"], t, 3600);
    let before = refusals("principal");
    let r = exchange(a.h(), &stranger, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(refusals("principal") > before);
    // A token naming a credential of another service account: 401 (live re-verification).
    let forged = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
    let mut v = tokens.verified(&forged);
    v.service_account_id = Uuid::new_v4();
    tokens.answer(&forged, Ok(v));
    let before = refusals("minting_credential");
    let r = exchange(a.h(), &forged, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(refusals("minting_credential") > before);
    // Platform Access unreachable: 503, fail closed.
    let down = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
    tokens.answer(&down, Err(UserTokenError::Unavailable("connect".into())));
    let r = exchange(a.h(), &down, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(
        count(
            &a,
            "SELECT COUNT(*) FROM call_endpoints WHERE endpoint_key LIKE 'inst:%'"
        )
        .await,
        0
    );

    // A user suspended at Platform Access after the exchange: the signaling credential
    // stops working (the persisted authority re-check), as does a revoked minting key.
    let good = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
    let r = exchange(a.h(), &good, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (_, signaling) = device_of(&r);
    a.h()
        .platform
        .revoke_credential(CredentialId::from_uuid(a.w.ctx.credential_id().as_uuid()))
        .await
        .unwrap();
    a.h().state.identity.catch_up().await.unwrap();
    let r = a
        .as_endpoint(
            &signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // … and no new exchange succeeds on that key's tokens (verified live, uncached).
    let after = tokens.mint(&a.w.ctx, user, &["receive"], t, 3600);
    let before = refusals("minting_credential");
    let r = exchange(a.h(), &after, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(refusals("minting_credential") > before);
}

/// A world of the given classification with a recording push transport.
async fn world(classification: TenantClassification) -> Option<AppWorld> {
    let db = Arc::new(TestDb::new().await?);
    let media = Arc::new(FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let runtime = test_runtime(Some(media.clone()), bus.clone());
    let h = Harness::with_runtime(db, ManualClock::new(common::t0()), media, bus, runtime);
    let consumer = h.consumer("deliveries", classification).await;
    Some(AppWorld {
        w: CallWorld::for_consumer(h, consumer, EnvironmentKind::Production).await,
    })
}

#[tokio::test]
async fn first_party_tenants_never_exchange_user_tokens() {
    let Some(mut a) = world(TenantClassification::FirstParty).await else {
        return;
    };
    let tokens = enable(&mut a);
    let token = tokens.mint(
        &a.w.ctx,
        a.w.recipient.id().as_uuid(),
        &["receive"],
        now(&a),
        3600,
    );
    let r = exchange(a.h(), &token, "install-0001", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(
        refusals("not_external") >= 1,
        "refused for the tenant class"
    );
    assert_eq!(count(&a, "SELECT COUNT(*) FROM call_endpoints").await, 0);
}

/// Refusals counted with `reason` (process-wide counter; tests only compare growth).
fn refusals(reason: &'static str) -> u64 {
    calling::metrics::counter(
        "calling_user_session_exchanges_total",
        &[("outcome", reason)],
    )
}

/// Serves the production router on an ephemeral port (WebSocket signaling).
async fn serve(h: &Harness) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = h.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    format!("ws://{address}/v1/calling/signaling")
}

async fn next_json(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Value {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("a frame within 5 s")
            .expect("open")
            .unwrap();
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn place_call(
    h: &Harness,
    secret: &str,
    from: &Device,
    to: &ResolvedPrincipal,
    key: &str,
) -> Uuid {
    let r = h
        .request_with(
            Method::POST,
            "/v1/calls",
            Some(secret),
            Some(json!({
                "kind": "VOICE",
                "initiatorPrincipalId": from.principal,
                "initiatorEndpointId": from.endpoint,
                "targetPrincipalIds": [to.id()],
            })),
            &[("idempotency-key", key)],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

/// The delivery app: its backend places a call from a courier to a customer; both are
/// partner users whose devices exchanged user tokens.
#[tokio::test]
async fn a_partner_user_receives_a_server_placed_call_end_to_end() {
    let Some(mut a) = world(TenantClassification::External).await else {
        return;
    };
    let tokens = enable(&mut a);
    a.w.h.state = a.w.h.state.clone().with_partner_push(true);
    let courier = a.principal("karrygo-courier-17").await;
    let customer = a.principal("karrygo-customer-42").await;
    let t = now(&a);
    let courier_token = tokens.mint(&a.w.ctx, courier.id().as_uuid(), &["receive"], t, 3600);
    let customer_token = tokens.mint(&a.w.ctx, customer.id().as_uuid(), &["receive"], t, 3600);
    let r = exchange(a.h(), &courier_token, "courier-phone-1", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (courier_device, _) = device_of(&r);
    let r = exchange(a.h(), &customer_token, "customer-browser-1", "WEB").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (customer_device, customer_signaling) = device_of(&r);

    // The customer's device is on its signaling channel.
    let url = serve(a.h()).await;
    let mut request = url.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {customer_signaling}")).unwrap(),
    );
    let (mut socket, _) = connect_async(request).await.unwrap();
    let ready = next_json(&mut socket).await;
    assert_eq!(ready["type"], "signaling.ready");
    assert_eq!(ready["endpointId"], customer_device.endpoint.to_string());

    // The partner's backend places the call (server-placed, platform credential).
    let call = place_call(a.h(), a.secret(), &courier_device, &customer, "karrygo-1").await;
    let incoming = next_json(&mut socket).await;
    assert_eq!(incoming["type"], "call.incoming");
    assert_eq!(incoming["callId"], call.to_string());
    assert_eq!(
        incoming["target"]["endpointId"],
        customer_device.endpoint.to_string()
    );

    // Partner push: one platform-principal recipient for the customer (never the
    // courier, never a first-party offer: the endpoint has no push target).
    let partner = a.h().push.partner_offers();
    assert_eq!(partner.len(), 1, "{partner:#?}");
    let offer = &partner[0];
    assert_eq!(offer.call_id, call);
    assert_eq!(
        offer.recipient.to_json(),
        json!({
            "kind": "platform-principal",
            "tenantId": a.w.ctx.tenant_id().as_uuid(),
            "applicationId": a.w.ctx.application_id().as_uuid(),
            "environmentId": a.w.ctx.environment_id().as_uuid(),
            "principalId": customer.id().as_uuid(),
        })
    );
    assert_eq!(offer.payload["schema"], "cfmeet.calling.push-invitation");
    assert_eq!(offer.payload["callId"], call.to_string());
    assert!(a.h().push.offers().is_empty());

    // The device recovers its offer, accepts it with its own credential, and gets media.
    let r = a
        .as_endpoint(
            &customer_signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let invitation = r.json["data"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(r.json["data"][0]["actionable"], true);
    let (_, version) = a.state(call).await;
    let r = a
        .as_endpoint(
            &customer_signaling,
            Method::POST,
            &format!("/v1/calling/invitations/{invitation}/accept"),
            Some(json!({ "expectedVersion": version })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let session = r.json["sessionId"].as_str().unwrap().to_string();
    let r = a
        .as_endpoint(
            &customer_signaling,
            Method::POST,
            &format!("/v1/calling/sessions/{session}/media-credentials"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["token"].as_str().is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn one_tenants_signaling_credential_never_reaches_another_tenants_offer() {
    let Some(mut a) = AppWorld::new().await else {
        return;
    };
    let tokens = enable(&mut a);
    let kind = a.w.kind;
    // Tenant B in the same deployment: its own credential, users and call.
    let b = a
        .h()
        .consumer("bravo", TenantClassification::External)
        .await;
    let b_credential = a
        .h()
        .issue(
            b.sa(kind),
            &[
                names::CALLS_READ,
                names::CALLS_WRITE,
                names::PRINCIPALS_READ,
            ],
        )
        .await;
    let b_secret = b_credential.secret.expose_once().to_string();
    let b_ctx = a.h().ctx(&b_secret).await;
    let b_caller = common::calls::resolve(a.h(), &b, kind, &b_ctx, "b-caller").await;
    let b_callee = common::calls::resolve(a.h(), &b, kind, &b_ctx, "b-callee").await;
    let t = now(&a);
    let b_caller_token = tokens.mint(&b_ctx, b_caller.id().as_uuid(), &["receive"], t, 3600);
    let b_callee_token = tokens.mint(&b_ctx, b_callee.id().as_uuid(), &["receive"], t, 3600);
    let r = exchange(a.h(), &b_caller_token, "b-install-1", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (b_from, _) = device_of(&r);
    let r = exchange(a.h(), &b_callee_token, "b-install-2", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (b_to, b_signaling) = device_of(&r);
    let call = place_call(a.h(), &b_secret, &b_from, &b_callee, "bravo-1").await;
    let b_invitations = a
        .as_endpoint(
            &b_signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    let invitation = b_invitations.json["data"][0]["invitationId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(b_invitations.json["data"][0]["callId"], call.to_string());
    let version = b_invitations.json["data"][0]["callVersion"]
        .as_i64()
        .unwrap();

    // Tenant A's user, with the SAME installation id as B's callee (scoped keys).
    let a_token = tokens.mint(
        &a.w.ctx,
        a.w.recipient.id().as_uuid(),
        &["receive"],
        t,
        3600,
    );
    let r = exchange(a.h(), &a_token, "b-install-2", "MOBILE").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (a_device, a_signaling) = device_of(&r);
    assert_ne!(a_device.endpoint, b_to.endpoint, "scoped per environment");
    // B's endpoint was not handed over: it still works.
    let r = a
        .as_endpoint(
            &b_signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // A's credential sees nothing of B and cannot answer B's offer.
    let r = a
        .as_endpoint(
            &a_signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.json["data"], json!([]));
    for action in ["accept", "decline"] {
        let r = a
            .as_endpoint(
                &a_signaling,
                Method::POST,
                &format!("/v1/calling/invitations/{invitation}/{action}"),
                Some(json!({ "expectedVersion": version })),
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{action}: {}", r.text);
    }
    // A token minted by A's key for B's user is refused (the user is not in A's scope).
    let crossed = tokens.mint(&a.w.ctx, b_callee.id().as_uuid(), &["receive"], t, 3600);
    let before = refusals("principal");
    let r = exchange(a.h(), &crossed, "cross-install", "MOBILE").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert!(refusals("principal") > before);
    // B's offer is untouched and still actionable by B.
    let r = a
        .as_endpoint(
            &b_signaling,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.json["data"][0]["callVersion"], version, "nothing changed");
    assert_eq!(r.json["data"][0]["actionable"], true);
}

/// The I3 sink of the Notification Service, recording bodies.
#[derive(Default)]
struct FakeI3 {
    bodies: Mutex<Vec<Value>>,
}

#[async_trait]
impl CallPushSink for FakeI3 {
    async fn call_push(&self, body: Value) -> Result<Value, String> {
        self.bodies.lock().unwrap().push(body);
        Ok(json!({ "results": [] }))
    }
}

async fn settle(i3: &FakeI3, n: usize) {
    for _ in 0..100 {
        if i3.bodies.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
}

fn partner_offer(
    call: Uuid,
    principal: Uuid,
    expires_in: i64,
) -> calling::signaling::PartnerPushOffer {
    calling::signaling::PartnerPushOffer {
        recipient: calling::signaling::PlatformPrincipalRecipient {
            tenant_id: Uuid::new_v4(),
            application_id: Uuid::new_v4(),
            environment_id: Uuid::new_v4(),
            principal_id: principal,
        },
        payload: json!({
            "schema": "cfmeet.calling.push-invitation", "schemaVersion": 1, "callId": call,
            "invitationId": Uuid::new_v4(), "callKind": "VOICE", "caller": {},
            "issuedAt": chrono::Utc::now().to_rfc3339(),
            "expiresAt": (chrono::Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339(),
        }),
        call_id: call,
    }
}

#[tokio::test]
async fn the_notification_transport_sends_one_platform_principal_alert_per_call_and_user() {
    let i3 = Arc::new(FakeI3::default());
    let t = NotificationsPushTransport::new(i3.clone(), Duration::from_millis(1000));
    let (call, user) = (Uuid::new_v4(), Uuid::new_v4());
    let first = partner_offer(call, user, 30);
    t.dispatch_partner_offer(first.clone()).await;
    // Another ringing endpoint of the same user in the same call: no second alert.
    t.dispatch_partner_offer(partner_offer(call, user, 30))
        .await;
    // Another user of the call, and the same user in another call: sent.
    t.dispatch_partner_offer(partner_offer(call, Uuid::new_v4(), 30))
        .await;
    t.dispatch_partner_offer(partner_offer(Uuid::new_v4(), user, 30))
        .await;
    // Expired: nothing.
    t.dispatch_partner_offer(partner_offer(Uuid::new_v4(), user, -1))
        .await;
    settle(&i3, 3).await;
    let bodies = i3.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3, "{bodies:#?}");
    let b = &bodies[0];
    assert_eq!(
        b["recipient"],
        json!({
            "kind": "platform-principal",
            "tenantId": first.recipient.tenant_id,
            "applicationId": first.recipient.application_id,
            "environmentId": first.recipient.environment_id,
            "principalId": user,
        })
    );
    assert!(b.get("pushTargetRefs").is_none());
    assert!(b.get("recipientAccountRef").is_none());
    assert_eq!(b["expiresAt"], b["payload"]["expiresAt"]);
    assert_eq!(b["payload"]["schema"], "cfmeet.calling.push-invitation");
    // The default lane of other transports sends nothing.
    calling::signaling::NoPushTransport
        .dispatch_partner_offer(partner_offer(call, user, 30))
        .await;
}

#[tokio::test]
async fn partner_push_is_off_by_default_and_never_for_first_party_or_other_principals() {
    // Off by default: an EXTERNAL partner user's offer produces no partner push.
    let Some(a) = world(TenantClassification::External).await else {
        return;
    };
    let from = a.device(&a.w.initiator, "caller-1").await;
    a.device(&a.w.recipient, "callee-1").await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &from, &[&a.w.recipient], "pp-off")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(a.h().push.partner_offers().is_empty());

    // On, in a FIRST_PARTY tenant: never a partner push; the first-party offer of a CF
    // Meet account is unchanged.
    let Some(mut f) = world(TenantClassification::FirstParty).await else {
        return;
    };
    f.w.h.state = f.w.h.state.clone().with_partner_push(true);
    let from = f.device(&f.w.initiator, "caller-1").await;
    let account = format!("cfmeet-user:{}", Uuid::new_v4());
    let (view, _) = f
        .h()
        .platform
        .register_principal(
            f.w.consumer.env(f.w.kind).id,
            common::NewPrincipal::External {
                kind: calling::domain::PrincipalKind::NativeCfUser,
                external_reference: account.clone(),
            },
        )
        .await
        .unwrap();
    let native = f
        .h()
        .state
        .principals
        .resolve(
            &f.w.ctx,
            &calling::platform::principals::PrincipalReference::Id(view.id),
        )
        .await
        .unwrap();
    let target = Uuid::new_v4();
    let r = f
        .post(
            "/v1/calling/endpoints",
            json!({
                "principalId": native.id(), "endpointKey": "native-1", "endpointKind": "MOBILE",
                "capabilities": { "audio": true, "video": true }, "pushTargetRef": target,
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = f
        .create_call("VOICE", &f.w.initiator, &from, &[&native], "pp-fp")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(f.h().push.partner_offers().is_empty());
    let offers = f.h().push.offers();
    let o = offers.iter().find(|o| o.push_target_ref == target).unwrap();
    assert_eq!(o.first_party_account.as_deref(), Some(account.as_str()));

    // On, EXTERNAL tenant: only the partner users rung are recipients (one offer per
    // ringing endpoint here; the transport keeps one alert per (call, user)); a
    // NATIVE_CF_USER-kind principal of an EXTERNAL tenant is not a partner user.
    let Some(mut e) = world(TenantClassification::External).await else {
        return;
    };
    e.w.h.state = e.w.h.state.clone().with_partner_push(true);
    let from = e.device(&e.w.initiator, "caller-1").await;
    e.device(&e.w.recipient, "callee-1").await;
    let (view, _) = e
        .h()
        .platform
        .register_principal(
            e.w.consumer.env(e.w.kind).id,
            common::NewPrincipal::External {
                kind: calling::domain::PrincipalKind::NativeCfUser,
                external_reference: format!("cfmeet-user:{}", Uuid::new_v4()),
            },
        )
        .await
        .unwrap();
    let lookalike = e
        .h()
        .state
        .principals
        .resolve(
            &e.w.ctx,
            &calling::platform::principals::PrincipalReference::Id(view.id),
        )
        .await
        .unwrap();
    e.device(&lookalike, "lookalike-1").await;
    let r = e
        .create_call(
            "VOICE",
            &e.w.initiator,
            &from,
            &[&e.w.recipient, &lookalike],
            "pp-on",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let partner = e.h().push.partner_offers();
    assert_eq!(partner.len(), 1, "{partner:#?}");
    assert_eq!(
        partner[0].recipient.principal_id,
        e.w.recipient.id().as_uuid()
    );
    assert_eq!(
        partner[0].recipient.environment_id,
        e.w.ctx.environment_id().as_uuid()
    );
}
