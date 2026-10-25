//! Part VI §51: requests the public API Gateway forwards to Calling's REAL public routes.
//!
//! The gateway authenticates the customer's bearer credential, drops `Authorization` and
//! attaches its own service token whose signed delegated context names the credential.
//! Calling accepts that ONLY from an issuer its trust bundle grants
//! `calling-request.forward`, re-verifies the credential live with Platform Access, and
//! scopes the request to the authority chain Platform Access returns — every other
//! combination is the normalized 401 (503 when the authority is unreachable).

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    domain::{EnvironmentKind, TenantClassification, TenantStatus},
    platform::scopes::names,
};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner,
};
use common::{Consumer, Harness, IssuedCredential, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

/// The delegated context the gateway derives from the authority chain of `issued`
/// (production environment of `c`), naming the credential unless `credential` is `None`.
fn delegated(c: &Consumer, credential: Option<Uuid>) -> DelegatedPlatformContext {
    let env = c.env(EnvironmentKind::Production);
    let context = DelegatedPlatformContext::new(
        c.tenant.id.as_uuid(),
        c.application.id.as_uuid(),
        env.id.as_uuid(),
        ApplicationEnvironmentKind::Production,
        c.sa(EnvironmentKind::Production).as_uuid(),
    );
    match credential {
        Some(id) => context.with_initiating_credential(id),
        None => context,
    }
}

fn credential_id(issued: &IssuedCredential) -> Uuid {
    issued.credential.id.as_uuid()
}

/// Signed hop headers (service token, request id, correlation id) and the signed
/// correlation id.
fn signed(
    signer: &ServiceTokenSigner,
    context: Option<DelegatedPlatformContext>,
) -> (Vec<(String, String)>, String) {
    let mut request = OutboundServiceRequest::new_operation(ServiceId::calling());
    if let Some(context) = context {
        request = request.with_delegated_context(context);
    }
    let correlation = request.correlation_id().as_str().to_string();
    let signed = signer.sign(&request).unwrap();
    let headers = signed
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    (headers, correlation)
}

async fn forward(
    h: &Harness,
    method: Method,
    path: &str,
    headers: &[(String, String)],
    body: Option<Value>,
) -> TestResponse {
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.request_with(method, path, None, body, &refs).await
}

async fn whoami(h: &Harness, headers: &[(String, String)]) -> TestResponse {
    forward(h, Method::POST, "/test/context", headers, Some(json!({}))).await
}

fn assert_rejected(response: &TestResponse, headers: &[(String, String)]) {
    assert_eq!(
        response.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        response.text
    );
    assert_eq!(
        response.normalized_error(),
        json!({ "error": {
            "code": "AUTHENTICATION_FAILED",
            "message": "Authentication failed.",
            "requestId": null
        }})
    );
    for (_, value) in headers {
        assert!(!response.text.contains(value.trim_start_matches("Bearer ")));
    }
}

#[tokio::test]
async fn forwarded_request_reaches_a_real_public_route_scoped_to_the_live_chain() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(
            c.sa(EnvironmentKind::Production),
            &[names::CALLS_READ, names::PRINCIPALS_READ],
        )
        .await;
    let (headers, correlation) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&issued)))),
    );

    // A real public route: the call list of the authenticated environment.
    let r = forward(&h, Method::GET, "/v1/calls", &headers, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["data"], json!([]), "{}", r.text);

    // The context is exactly the chain Platform Access returned, continuing the gateway's
    // signed correlation id; it equals the context of the same credential presented
    // directly (apart from the per-request ids).
    let r = whoami(&h, &headers).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let env = c.env(EnvironmentKind::Production);
    assert_eq!(r.json["tenant_id"], json!(c.tenant.id));
    assert_eq!(r.json["application_id"], json!(c.application.id));
    assert_eq!(r.json["environment_id"], json!(env.id));
    assert_eq!(r.json["environment_kind"], "PRODUCTION");
    assert_eq!(
        r.json["service_account_id"],
        json!(c.sa(EnvironmentKind::Production))
    );
    assert_eq!(r.json["credential_id"], json!(issued.credential.id));
    assert_eq!(r.json["correlation_id"], correlation);
    let direct = h.whoami(issued.secret.expose_once()).await;
    assert_eq!(direct.status, StatusCode::OK, "{}", direct.text);
    let strip = |v: &Value| {
        let mut v = v.clone();
        v["request_id"] = Value::Null;
        v["correlation_id"] = Value::Null;
        v
    };
    assert_eq!(strip(&r.json), strip(&direct.json));

    // Scopes come from the live chain, not from the gateway: a credential without
    // calls.read is refused by the route's scope check.
    let narrow = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&narrow)))),
    );
    let r = forward(&h, Method::GET, "/v1/calls", &headers, None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = forward(&h, Method::GET, "/test/principals-read", &headers, None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn a_delegated_scope_that_does_not_own_the_credential_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let other = h.consumer("other", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    // The credential of `c` claimed for `other`'s tenant / application / environment.
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&other, Some(credential_id(&issued)))),
    );
    assert_rejected(&whoami(&h, &headers).await, &headers);
    // A credential id Platform Access does not know.
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(Uuid::new_v4()))),
    );
    assert_rejected(&whoami(&h, &headers).await, &headers);
}

#[tokio::test]
async fn a_forwarded_request_without_the_credential_id_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let (headers, _) = signed(&h.peers.api_gateway, Some(delegated(&c, None)));
    assert_rejected(&whoami(&h, &headers).await, &headers);
    // No delegated context at all.
    let (headers, _) = signed(&h.peers.api_gateway, None);
    assert_rejected(&whoami(&h, &headers).await, &headers);
}

#[tokio::test]
async fn only_an_issuer_granted_calling_request_forward_may_forward() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let context = delegated(&c, Some(credential_id(&issued)));

    // Trusted issuers WITHOUT the grant (each holds other permissions on Calling).
    for peer in [&h.peers.privacy_relay, &h.peers.telephony] {
        let (headers, _) = signed(peer, Some(context.clone()));
        assert_rejected(&whoami(&h, &headers).await, &headers);
    }
    // A correctly signing service Calling does not trust at all.
    let (headers, _) = signed(&h.peers.untrusted, Some(context.clone()));
    assert_rejected(&whoami(&h, &headers).await, &headers);
    // A token addressed to another audience, and a tampered token.
    let request = OutboundServiceRequest::new_operation(ServiceId::platform_access())
        .with_delegated_context(context.clone());
    let other_audience: Vec<(String, String)> = h
        .peers
        .api_gateway
        .sign(&request)
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    assert_rejected(&whoami(&h, &other_audience).await, &other_audience);
    let (mut tampered, _) = signed(&h.peers.api_gateway, Some(context.clone()));
    for (k, v) in tampered.iter_mut() {
        if k == "x-cf-service-authorization" {
            // Change one signature character (always to a different value).
            let i = v.len() - 10;
            let replacement = if &v[i..=i] == "A" { "B" } else { "A" };
            v.replace_range(i..=i, replacement);
        }
    }
    assert_rejected(&whoami(&h, &tampered).await, &tampered);

    // The genuine gateway token is accepted.
    let (headers, _) = signed(&h.peers.api_gateway, Some(context));
    assert_eq!(whoami(&h, &headers).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_service_token_together_with_authorization_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let cfmk = issued.secret.expose_once().to_string();
    let (mut headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&issued)))),
    );
    assert_eq!(whoami(&h, &headers).await.status, StatusCode::OK);
    // Both the valid gateway token and the valid customer credential: never mixed.
    headers.push(("authorization".into(), format!("Bearer {cfmk}")));
    assert_rejected(&whoami(&h, &headers).await, &headers);
    // Any Authorization value at all.
    headers.pop();
    headers.push(("authorization".into(), "Basic abc".into()));
    assert_rejected(&whoami(&h, &headers).await, &headers);
}

#[tokio::test]
async fn a_credential_revoked_after_the_gateway_check_is_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&issued)))),
    );
    assert_eq!(whoami(&h, &headers).await.status, StatusCode::OK);
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    // The very same signed token: Platform Access answers REJECTED (live re-verification).
    assert_rejected(&whoami(&h, &headers).await, &headers);
}

#[tokio::test]
async fn an_unreachable_authority_fails_closed_for_forwarded_requests() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&issued)))),
    );
    assert_eq!(whoami(&h, &headers).await.status, StatusCode::OK);
    h.db.pa().stop().await;
    let r = whoami(&h, &headers).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "SERVICE_UNAVAILABLE");
}

#[tokio::test]
async fn a_safety_only_chain_reaches_only_safety_routes() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(
            c.sa(EnvironmentKind::Production),
            &[names::CALLS_READ, names::CALLS_WRITE],
        )
        .await;
    let (headers, _) = signed(
        &h.peers.api_gateway,
        Some(delegated(&c, Some(credential_id(&issued)))),
    );
    assert_eq!(
        forward(&h, Method::GET, "/v1/calls", &headers, None)
            .await
            .status,
        StatusCode::OK
    );
    h.platform
        .set_tenant_status(c.tenant.id, TenantStatus::Suspended)
        .await
        .unwrap();
    // Not a safety operation: the normalized authentication failure.
    assert_rejected(
        &forward(&h, Method::GET, "/v1/calls", &headers, None).await,
        &headers,
    );
    assert_rejected(&whoami(&h, &headers).await, &headers);
    // A safety operation passes authentication (the handler then answers for the
    // unknown call, never 401).
    let r = forward(
        &h,
        Method::POST,
        &format!("/v1/calls/{}/end", Uuid::new_v4()),
        &headers,
        Some(json!({})),
    )
    .await;
    assert_ne!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert_ne!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
}
