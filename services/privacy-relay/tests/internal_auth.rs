//! Internal service authentication: Privacy Relay trusts only signed service tokens,
//! grants permissions only from its own trust bundle, and takes scope only from the
//! signed delegated context.

mod common;

use axum::http::{Method, Request, StatusCode};
use base64::Engine;
use cf_service_auth::{
    headers, ApplicationEnvironmentKind as Kind, CorrelationId, OutboundServiceRequest, RequestId,
    ServiceId,
};
use common::*;
use serde_json::{json, Value};

fn b64(v: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string())
}

/// A token with arbitrary claims, validly signed with Calling's key.
fn forge(h: &Harness, claims: &Value) -> String {
    use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
    let signing =
        ed25519_dalek::SigningKey::from_pkcs8_pem(&h.ids.calling_key.to_pkcs8_pem()).unwrap();
    let encoding =
        jsonwebtoken::EncodingKey::from_ed_der(signing.to_pkcs8_der().unwrap().as_bytes());
    let header = json!({ "alg": "EdDSA", "typ": "cf-service+jwt", "kid": "calling-test-1" });
    let message = format!("{}.{}", b64(&header), b64(claims));
    let signature = cf_service_auth::jwt::sign(
        message.as_bytes(),
        &encoding,
        jsonwebtoken::Algorithm::EdDSA,
    )
    .unwrap();
    format!("{message}.{signature}")
}

async fn with_token(
    h: &Harness,
    token: &str,
    rid: &RequestId,
    cid: &CorrelationId,
    path: &str,
    body: Value,
) -> Response {
    let builder = Request::builder()
        .method(Method::POST)
        .uri(path)
        .header(headers::SERVICE_AUTHORIZATION, format!("Bearer {token}"))
        .header(headers::REQUEST_ID, rid.as_str())
        .header(headers::CORRELATION_ID, cid.as_str());
    h.raw(builder, Some(body)).await
}

fn claims_of(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn customer_credentials_and_unsigned_requests_are_refused() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    for (what, builder) in [
        (
            "no credential",
            Request::builder()
                .method(Method::GET)
                .uri("/internal/v1/contexts/query"),
        ),
        (
            "raw customer credential",
            Request::builder()
                .method(Method::POST)
                .uri("/internal/v1/contexts")
                .header("authorization", "Bearer cfmk_live_abcdef_0123456789abcdef"),
        ),
        (
            "customer credential as service token",
            Request::builder()
                .method(Method::POST)
                .uri("/internal/v1/contexts")
                .header(
                    headers::SERVICE_AUTHORIZATION,
                    "Bearer cfmk_live_abcdef_0123456789abcdef",
                ),
        ),
        (
            "unknown internal path",
            Request::builder()
                .method(Method::GET)
                .uri("/internal/v1/anything"),
        ),
    ] {
        let r = h.raw(builder, Some(json!({}))).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED"),
            "{what}"
        );
    }
    // A valid service token PLUS a customer credential is still refused (no mixing).
    let signed = h
        .ids
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(c.delegated()),
        )
        .unwrap();
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/internal/v1/contexts/query");
    for (n, v) in signed.headers().iter() {
        builder = builder.header(n, v);
    }
    builder = builder.header("authorization", "Bearer cfmk_live_abcdef_0123456789abcdef");
    assert_eq!(
        h.raw(builder, Some(json!({}))).await.status,
        StatusCode::UNAUTHORIZED
    );
    // An untrusted (but correctly signing) service is refused.
    let untrusted = h.ids.untrusted.clone();
    let r = h
        .send_as(
            &untrusted,
            Method::POST,
            "/internal/v1/contexts/query",
            Some(&c),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Health is public and reveals nothing sensitive.
    let health = h
        .raw(Request::builder().method(Method::GET).uri("/health"), None)
        .await;
    assert_eq!(
        health.json,
        json!({ "status": "ok", "service": "privacy-relay" })
    );
}

#[tokio::test]
async fn permissions_are_receiver_authoritative() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    // Telephony is trusted, but only for communication-context.read.
    let telephony = h.ids.telephony.clone();
    let r = h.send_as(&telephony, Method::POST, "/internal/v1/contexts", Some(&c), Some(json!({ "idempotencyKey": "k", "request": context_request(&[a, b], "BIDIRECTIONAL", &[]) }))).await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let r = h
        .send_as(
            &telephony,
            Method::POST,
            "/internal/v1/communication/authorize",
            Some(&c),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
    let r = h
        .send_as(
            &telephony,
            Method::POST,
            "/internal/v1/contexts/query",
            Some(&c),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "granted read");

    // Calling with the right identity but without the local grant is denied.
    let reduced =
        Harness::with_permissions(&["communication-context.read", "communication-context.write"])
            .await
            .unwrap();
    let ctx = reduced.bidirectional(&c, &[a, b]).await;
    let r = reduced
        .command(
            &format!(
                "/internal/v1/contexts/{}/sessions",
                ctx["contextId"].as_str().unwrap()
            ),
            &c,
            json!({ "relayStrategy": "APP_ONLY" }),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED")
    );
    let r = reduced
        .authorize(&c, &uuid::Uuid::new_v4().to_string(), a, &[b])
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");

    // A token that self-declares permissions gains nothing: it is refused outright.
    let signed = reduced
        .ids
        .calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(c.delegated()),
        )
        .unwrap();
    let mut claims = claims_of(signed.token().expose_for_transport());
    claims["permissions"] = json!(["relay-session.write", "communication.authorize"]);
    let token = forge(&reduced, &claims);
    let r = with_token(
        &reduced,
        &token,
        signed.request_id(),
        signed.correlation_id(),
        "/internal/v1/communication/authorize",
        json!({}),
    )
    .await;
    assert_eq!(
        (r.status, r.code()),
        (StatusCode::UNAUTHORIZED, "INTERNAL_AUTHENTICATION_FAILED")
    );
}

#[tokio::test]
async fn scope_comes_only_from_the_signed_delegated_context() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = Consumer::new(Kind::Production);
    let [a, b] = principals(2).try_into().unwrap();
    let ctx = h.bidirectional(&c, &[a, b]).await;
    let id = ctx["contextId"].as_str().unwrap().to_string();

    // Missing delegated context: refused for consumer operations.
    let calling = h.ids.calling.clone();
    for (method, path, body) in [
        (
            Method::POST,
            "/internal/v1/contexts/query".to_string(),
            Some(json!({})),
        ),
        (Method::GET, format!("/internal/v1/contexts/{id}"), None),
        (
            Method::POST,
            "/internal/v1/communication/authorize".to_string(),
            Some(json!({})),
        ),
    ] {
        let r = h.send_as(&calling, method, &path, None, body).await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED"),
            "{path}"
        );
    }

    // Tampering with the signed delegated context breaks the signature.
    let signed = calling
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::privacy_relay())
                .with_delegated_context(Consumer::new(Kind::Production).delegated()),
        )
        .unwrap();
    let token = signed.token().expose_for_transport().to_string();
    let mut claims = claims_of(&token);
    claims["delegated_context"]["tenant_id"] = json!(c.tenant_id);
    claims["delegated_context"]["application_id"] = json!(c.application_id);
    claims["delegated_context"]["application_environment_id"] = json!(c.environment_id);
    let parts: Vec<&str> = token.split('.').collect();
    let tampered = format!("{}.{}.{}", parts[0], b64(&claims), parts[2]);
    let r = with_token(
        &h,
        &tampered,
        signed.request_id(),
        signed.correlation_id(),
        "/internal/v1/contexts/query",
        json!({}),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);

    // Scope in a body is never authority: it is an unknown member.
    for body in [
        json!({ "tenantId": c.tenant_id }),
        json!({ "applicationId": c.application_id }),
        json!({ "environmentId": c.environment_id }),
    ] {
        let r = h
            .post(
                "/internal/v1/contexts/query",
                &Consumer::new(Kind::Production),
                body.clone(),
            )
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{body}");
        let mut auth = json!({ "relaySessionId": uuid::Uuid::new_v4(), "source": { "principalId": a }, "targets": [{ "principalId": b }], "channel": "VOICE" });
        auth.as_object_mut()
            .unwrap()
            .extend(body.as_object().unwrap().clone());
        let r = h
            .post("/internal/v1/communication/authorize", &c, auth)
            .await;
        assert_eq!(r.code(), "VALIDATION_ERROR", "{body}");
    }
    // A client-asserted authorization result is not an input either.
    let r = h.post("/internal/v1/communication/authorize", &c, json!({ "relaySessionId": uuid::Uuid::new_v4(), "source": { "principalId": a }, "targets": [{ "principalId": b }], "channel": "VOICE", "authorization": true })).await;
    assert_eq!(r.code(), "VALIDATION_ERROR");
}
