//! Part VI §51: customer requests forwarded by the API Gateway. The API Gateway removes the
//! customer's `Authorization` header and attaches a 60-second service token (permission
//! `sms-request.forward`) whose signed delegated context names the credential it
//! authenticated; the SMS Gateway re-verifies that credential LIVE with Platform Access
//! (`/internal/v1/credentials/delegated-authority`) and then treats the request exactly
//! like a direct one. Provider callbacks and internal routes never accept a gateway token.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    services, CorrelationId, DelegatedPlatformContext, InternalPermission, ServiceId,
    ServiceTokenSigner, ServiceTokenVerifier, SystemClock, TokenTtl, TrustPolicy,
};
use common::{Customer, FakeNumber, Harness, ENV};
use serde_json::{json, Value};
use sms_gateway::identity::{PlatformAccessAuthority, DELEGATED_AUTHORITY_PATH};
use uuid::Uuid;

const DCN: &str = "+15550109001";
const TO: &str = "+15550104242";

fn setup(h: &Harness) -> (Customer, FakeNumber) {
    let c = h.customer(&["sms.send", "sms.read"]);
    let n = h.number(&c, DCN, Some(("PRINCIPAL", Uuid::new_v4())));
    (c, n)
}

fn body(n: &FakeNumber, text: &str) -> Value {
    json!({ "from": { "numberResourceId": n.id }, "to": TO, "text": text })
}

fn key() -> String {
    format!("key-{}", Uuid::new_v4().simple())
}

/// A forwarded `GET` by the API Gateway with `delegated`.
async fn forwarded_get(
    h: &Harness,
    path: &str,
    delegated: Option<DelegatedPlatformContext>,
) -> common::Response {
    h.forwarded(
        &h.peers.api_gateway,
        Method::GET,
        path,
        None,
        None,
        delegated,
        &[],
    )
    .await
    .0
}

#[track_caller]
fn assert_refused(r: &common::Response, status: StatusCode, code: &str) {
    assert_eq!(r.status, status, "{}", r.text);
    assert_eq!(r.code(), code, "{}", r.text);
    assert!(r.json["error"]["requestId"].is_string(), "{}", r.text);
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Error", &r.json);
}

#[tokio::test]
async fn a_forwarded_request_is_reverified_live_and_scoped_to_the_returned_chain() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    let delegation = h.gateway_delegation(&c);
    let k = key();
    let (r, correlation) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::POST,
            "/v1/sms/messages",
            Some(body(&n, "forwarded hello")),
            Some(&k),
            Some(delegation.clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    common::conforms("sms-gateway/common/v1.schema.json#/$defs/Message", &r.json);
    assert!(r.headers.contains_key("x-request-id"));
    let id = r.json["messageId"].as_str().unwrap().to_string();
    // Platform Access was asked, with exactly the signed delegation, in the same operation.
    assert_eq!(
        h.authority.delegated.lock().unwrap().clone(),
        vec![(delegation.clone(), correlation.clone())]
    );
    // The transport continues the API Gateway's signed correlation id.
    assert_eq!(
        h.text(&format!(
            "SELECT correlation_id FROM sms_transports WHERE id = '{id}'"
        ))
        .await
        .as_deref(),
        Some(correlation.as_str())
    );
    // The forwarded Idempotency-Key is honoured exactly like a direct one: a forwarded
    // replay and a direct replay with the same credential answer the same message.
    let (replay, _) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::POST,
            "/v1/sms/messages",
            Some(body(&n, "forwarded hello")),
            Some(&k),
            Some(delegation.clone()),
            &[],
        )
        .await;
    assert_eq!(replay.status, StatusCode::OK, "{}", replay.text);
    assert_eq!(replay.json["messageId"], id.as_str());
    let direct = h.send(&c, body(&n, "forwarded hello"), &k).await;
    assert_eq!(direct.json["messageId"], id.as_str(), "{}", direct.text);
    // A missing key is refused the same way on both paths.
    let (r, _) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::POST,
            "/v1/sms/messages",
            Some(body(&n, "no key")),
            None,
            Some(delegation.clone()),
            &[],
        )
        .await;
    assert_refused(&r, StatusCode::BAD_REQUEST, "IDEMPOTENCY_KEY_REQUIRED");
    // Reads are scoped to the returned chain: the owner sees it, another environment does not.
    let r = forwarded_get(
        &h,
        &format!("/v1/sms/messages/{id}"),
        Some(delegation.clone()),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["messageId"], id.as_str());
    let other = h.customer(&["sms.read"]);
    let r = forwarded_get(
        &h,
        &format!("/v1/sms/messages/{id}"),
        Some(h.gateway_delegation(&other)),
    )
    .await;
    assert_refused(&r, StatusCode::NOT_FOUND, "NOT_FOUND");
    let r = forwarded_get(&h, "/v1/sms/messages", Some(h.gateway_delegation(&other))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(
        !r.text.contains(&id),
        "another environment's message: {}",
        r.text
    );
}

#[tokio::test]
async fn the_live_chain_decides_scopes_suspension_and_revocation() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (c, n) = setup(&h);
    // Scopes are the LIVE chain's: a read-only credential cannot send.
    let reader = h.credential(&c, &["sms.read"]);
    let (r, _) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::POST,
            "/v1/sms/messages",
            Some(body(&n, "x")),
            Some(&key()),
            Some(h.gateway_delegation(&reader)),
            &[],
        )
        .await;
    assert_refused(&r, StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE");
    // A SAFETY_ONLY (suspended) chain is refused like a direct one.
    let suspended = h.credential(&c, &["sms.read"]);
    for ctx in h.authority.tokens.lock().unwrap().values_mut() {
        if ctx.credential_id == suspended.context.credential_id {
            ctx.full_authority = false;
        }
    }
    let r = forwarded_get(
        &h,
        "/v1/sms/messages",
        Some(h.gateway_delegation(&suspended)),
    )
    .await;
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    // Revocation takes effect immediately: Platform Access answers REJECTED.
    let r = forwarded_get(&h, "/v1/sms/messages", Some(h.gateway_delegation(&c))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    h.authority.tokens.lock().unwrap().remove(&c.token);
    let r = forwarded_get(&h, "/v1/sms/messages", Some(h.gateway_delegation(&c))).await;
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    assert!(r.headers.contains_key("www-authenticate"));
}

#[tokio::test]
async fn a_credential_outside_the_delegated_scope_is_rejected_and_an_unreachable_authority_fails_closed(
) {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    let other = h.customer(&["sms.read"]);
    // c's credential id under another environment's delegation: Platform Access REJECTED.
    let foreign = other
        .context
        .owner_scope()
        .delegated(None)
        .with_initiating_credential(c.context.credential_id);
    let r = forwarded_get(&h, "/v1/sms/messages", Some(foreign)).await;
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    // An authority answering a chain the delegation does not name is not trusted either.
    *h.authority.misbind.lock().unwrap() = Some(other.context.clone());
    let r = forwarded_get(&h, "/v1/sms/messages", Some(h.gateway_delegation(&c))).await;
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    // Platform Access unreachable: 503, never anonymous.
    *h.authority.down.lock().unwrap() = true;
    let r = forwarded_get(&h, "/v1/sms/messages", Some(h.gateway_delegation(&c))).await;
    assert_refused(&r, StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE");
}

#[tokio::test]
async fn a_gateway_token_without_a_delegated_credential_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    // No delegated context at all, and a delegated scope that names no credential.
    for delegated in [None, Some(c.context.owner_scope().delegated(None))] {
        let r = forwarded_get(&h, "/v1/sms/messages", delegated).await;
        assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    }
    assert!(
        h.authority.delegated.lock().unwrap().is_empty(),
        "the authority is never asked"
    );
}

#[tokio::test]
async fn only_the_trusted_api_gateway_granted_sms_request_forward_may_forward() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    let delegation = h.gateway_delegation(&c);
    let peers = [
        // Trusted issuer, but not granted sms-request.forward.
        &h.peers.messaging,
        // The API Gateway's identity, signed with a key the trust bundle does not hold.
        &h.peers.rogue_api_gateway,
        // Not in the trust bundle at all.
        &h.peers.telephony,
    ];
    for signer in peers {
        let (r, _) = h
            .forwarded(
                signer,
                Method::GET,
                "/v1/sms/messages",
                None,
                None,
                Some(delegation.clone()),
                &[],
            )
            .await;
        assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    }
    assert!(h.authority.delegated.lock().unwrap().is_empty());
    // The same request from the API Gateway is accepted.
    let r = forwarded_get(&h, "/v1/sms/messages", Some(delegation)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn a_gateway_token_next_to_an_authorization_header_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.read"]);
    let bearer = format!("Bearer {}", c.token);
    let (r, _) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::GET,
            "/v1/sms/messages",
            None,
            None,
            Some(h.gateway_delegation(&c)),
            &[("authorization", bearer.as_str())],
        )
        .await;
    // Each alone would authenticate; together they are refused before any lookup.
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");
    assert!(h.authority.delegated.lock().unwrap().is_empty());
    assert_eq!(
        h.get(Some(&c.token), "/v1/sms/messages").await.status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn provider_callbacks_and_internal_routes_refuse_a_gateway_token() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.customer(&["sms.send", "sms.read"]);
    let delegation = h.gateway_delegation(&c);
    for path in [
        "/provider-callbacks/v1/sms/deterministic/inbound",
        "/provider-callbacks/v1/sms/deterministic/delivery-receipts",
    ] {
        let (r, _) = h
            .forwarded(
                &h.peers.api_gateway,
                Method::POST,
                path,
                Some(json!({})),
                None,
                Some(delegation.clone()),
                &[],
            )
            .await;
        assert_refused(&r, StatusCode::UNAUTHORIZED, "CALLBACK_REJECTED");
    }
    let bridge_ref = Uuid::new_v4();
    for (path, body) in [
        (
            "/internal/v1/sms-bridge/bindings",
            json!({ "bridgeRef": bridge_ref, "numberResourceId": Uuid::new_v4(), "to": TO }),
        ),
        (
            "/internal/v1/sms-bridge/messages",
            json!({ "bridgeRef": bridge_ref, "messageRef": Uuid::new_v4(), "text": "x" }),
        ),
    ] {
        let (r, _) = h
            .forwarded(
                &h.peers.api_gateway,
                Method::POST,
                path,
                Some(body),
                None,
                Some(delegation.clone()),
                &[],
            )
            .await;
        assert_refused(&r, StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED");
    }
    assert!(h.authority.delegated.lock().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------
// The real Platform Access adapter over authenticated internal HTTP
// ---------------------------------------------------------------------------------------

/// What the fake Platform Access saw of one delegated-authority request.
#[derive(Debug, Clone)]
struct Seen {
    caller: String,
    body_len: usize,
    content_type: Option<String>,
    delegated: Option<DelegatedPlatformContext>,
    correlation: CorrelationId,
}

struct FakePlatformAccess {
    verifier: ServiceTokenVerifier,
    answer: Mutex<(StatusCode, Value)>,
    seen: Mutex<Vec<Seen>>,
}

async fn delegated_authority(
    State(pa): State<Arc<FakePlatformAccess>>,
    headers: HeaderMap,
    body: Bytes,
) -> axum::response::Response {
    let Ok(ctx) = pa.verifier.verify_headers(&headers) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "code": "INTERNAL_AUTHENTICATION_FAILED" } })),
        )
            .into_response();
    };
    if ctx
        .require_permission(InternalPermission::PlatformCredentialAuthenticate)
        .is_err()
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": { "code": "INTERNAL_PERMISSION_DENIED" } })),
        )
            .into_response();
    }
    pa.seen.lock().unwrap().push(Seen {
        caller: ctx.authenticated_service().as_str().to_string(),
        body_len: body.len(),
        content_type: headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
        delegated: ctx.delegated_platform_context().cloned(),
        correlation: ctx.correlation_id().clone(),
    });
    let (status, answer) = pa.answer.lock().unwrap().clone();
    (status, Json(answer)).into_response()
}

/// Platform Access's `AuthorityChain` for `c`.
fn chain_of(c: &Customer) -> Value {
    let x = &c.context;
    json!({
        "authorityMode": "FULL",
        "tenant": { "id": x.tenant_id, "classification": "EXTERNAL" },
        "application": { "id": x.application_id },
        "environment": { "id": x.environment_id, "kind": "SANDBOX", "liveTelecomAllowed": true },
        "serviceAccount": { "id": x.service_account_id },
        "credential": { "id": x.credential_id },
        "scopes": x.scopes.iter().collect::<Vec<_>>(),
    })
}

#[tokio::test]
async fn the_platform_access_adapter_reverifies_the_delegated_credential_without_a_body() {
    // The SMS Gateway's outbound identity and Platform Access's trust in it.
    let me = common::key("sms-gateway-test-pa");
    let bundle = json!({
        "bundle_version": 1,
        "receiver": services::PLATFORM_ACCESS,
        "runtime_environment": ENV,
        "trusted_issuers": [{
            "issuer": services::SMS_GATEWAY,
            "audience": services::PLATFORM_ACCESS,
            "runtime_environment": ENV,
            "keys": [me.public_jwk()],
            "permissions": ["platform-credential.authenticate"],
        }],
    })
    .to_string();
    let platform_access = ServiceId::parse(services::PLATFORM_ACCESS).unwrap();
    let policy = TrustPolicy::from_json(&bundle, &platform_access, ENV).unwrap();
    let pa = Arc::new(FakePlatformAccess {
        verifier: ServiceTokenVerifier::new(policy, Arc::new(SystemClock)),
        answer: Mutex::new((StatusCode::OK, json!({}))),
        seen: Mutex::new(Vec::new()),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new()
        .route(DELEGATED_AUTHORITY_PATH, post(delegated_authority))
        .with_state(pa.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::sms_gateway(),
        ENV,
        me,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(ENV);
    targets
        .insert(platform_access, &format!("http://{addr}"))
        .unwrap();
    let client = Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(2)).unwrap(),
    );

    let Some(h) = Harness::build(|state| {
        state.authority = Arc::new(PlatformAccessAuthority::new(client));
    })
    .await
    else {
        server.abort();
        return;
    };
    let c = h.customer(&["sms.read"]);
    let delegation = h.gateway_delegation(&c);

    // AUTHENTICATED: admitted with the chain Platform Access returned.
    *pa.answer.lock().unwrap() = (
        StatusCode::OK,
        json!({ "outcome": "AUTHENTICATED", "authority": chain_of(&c) }),
    );
    let (r, correlation) = h
        .forwarded(
            &h.peers.api_gateway,
            Method::GET,
            "/v1/sms/messages",
            None,
            None,
            Some(delegation.clone()),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let seen = pa.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].caller, services::SMS_GATEWAY);
    assert_eq!(seen[0].body_len, 0, "no credential or body is presented");
    assert_eq!(seen[0].content_type, None);
    assert_eq!(seen[0].delegated.as_ref(), Some(&delegation));
    assert_eq!(
        seen[0].correlation, correlation,
        "the forwarded operation continues"
    );
    // A read-only chain still cannot send (scopes come from Platform Access).
    let r = h
        .forwarded(
            &h.peers.api_gateway,
            Method::POST,
            "/v1/sms/messages",
            Some(json!({ "from": { "numberResourceId": Uuid::new_v4() }, "to": TO, "text": "x" })),
            Some(&key()),
            Some(delegation.clone()),
            &[],
        )
        .await
        .0;
    assert_refused(&r, StatusCode::FORBIDDEN, "INSUFFICIENT_SCOPE");

    // REJECTED (revoked, suspended, foreign): 401.
    *pa.answer.lock().unwrap() = (
        StatusCode::OK,
        json!({ "outcome": "REJECTED", "reason": "revoked_credential" }),
    );
    let r = forwarded_get(&h, "/v1/sms/messages", Some(delegation.clone())).await;
    assert_refused(&r, StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED");

    // Platform Access refusing or unavailable: 503 (fail closed).
    for status in [StatusCode::SERVICE_UNAVAILABLE, StatusCode::FORBIDDEN] {
        *pa.answer.lock().unwrap() = (
            status,
            json!({ "error": { "code": "SERVICE_UNAVAILABLE" } }),
        );
        let r = forwarded_get(&h, "/v1/sms/messages", Some(delegation.clone())).await;
        assert_refused(&r, StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE");
    }
    server.abort();
    let _ = server.await;
    let r = forwarded_get(&h, "/v1/sms/messages", Some(delegation)).await;
    assert_refused(&r, StatusCode::SERVICE_UNAVAILABLE, "SERVICE_UNAVAILABLE");
}
