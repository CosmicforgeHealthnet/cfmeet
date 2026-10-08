//! Section 11 in the real Calling service: internal routes under /internal/v1 require a
//! service token; customer and service credentials never fall through into each other;
//! permissions come from Calling's trust bundle; ids are bound to the signed claims.

mod common;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::{Method, StatusCode};
use calling::domain::{EnvironmentKind, TenantClassification};
use cf_service_auth::{
    CorrelationId, OutboundServiceRequest, RequestId, ServiceId, ServiceTokenSigner,
};
use common::{scalar_i64, secret_part, Harness, TestResponse};
use serde_json::{json, Value};
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt};

fn signed_headers(
    signer: &ServiceTokenSigner,
    request: &OutboundServiceRequest,
) -> Vec<(String, String)> {
    let signed = signer.sign(request).unwrap();
    let mut out = Vec::new();
    for (name, value) in signed.headers().iter() {
        out.push((
            name.as_str().to_string(),
            value.to_str().unwrap().to_string(),
        ));
    }
    out
}

fn to_calling(signer: &ServiceTokenSigner) -> Vec<(String, String)> {
    signed_headers(
        signer,
        &OutboundServiceRequest::new_operation(ServiceId::calling()),
    )
}

async fn call(
    h: &Harness,
    method: Method,
    path: &str,
    headers: &[(String, String)],
) -> TestResponse {
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let body = (method == Method::POST).then(|| json!({}));
    h.request_with(method, path, None, body, &refs).await
}

fn assert_internal_auth_failure(response: &TestResponse) {
    assert_eq!(
        response.status,
        StatusCode::UNAUTHORIZED,
        "{}",
        response.text
    );
    assert_eq!(
        response.normalized_error(),
        json!({ "error": {
            "code": "INTERNAL_AUTHENTICATION_FAILED",
            "message": "Internal service authentication failed.",
            "requestId": null
        }})
    );
    let request_id = response.json["error"]["requestId"].as_str().unwrap();
    assert!(RequestId::parse(request_id).is_ok());
    assert_eq!(response.headers.get("x-request-id").unwrap(), request_id);
}

#[tokio::test]
async fn trusted_peer_with_a_valid_token_is_authenticated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let correlation = CorrelationId::generate();
    let request = OutboundServiceRequest::continuing(ServiceId::calling(), &correlation);
    let headers = signed_headers(&h.peers.telephony, &request);
    let response = call(&h, Method::GET, "/internal/v1/test/caller", &headers).await;
    assert_eq!(response.status, StatusCode::OK, "{}", response.text);
    assert_eq!(response.json["service"], "cfmeet.telephony");
    assert_eq!(response.json["audience"], "cfmeet.calling");
    assert_eq!(response.json["runtime_environment"], "test");
    assert_eq!(response.json["kid"], "telephony-test-1");
    assert_eq!(
        response.json["permissions"],
        json!([
            "operational-callback.deliver",
            "telephony-event.apply",
            "telephony-reconciliation.apply",
            "telephony-inbound.offer"
        ])
    );
    // The hop runs in the signed request / correlation context.
    assert_eq!(response.json["request_id"], request.request_id().as_str());
    assert_eq!(response.json["correlation_id"], correlation.as_str());
    assert_eq!(
        response.json["current_request_id"],
        request.request_id().as_str()
    );
    assert_eq!(
        response.headers.get("x-cf-request-id").unwrap(),
        request.request_id().as_str()
    );

    let ok = call(
        &h,
        Method::POST,
        "/internal/v1/test/callback",
        &to_calling(&h.peers.privacy_relay),
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
}

#[tokio::test]
async fn private_network_location_is_not_authentication() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // No headers at all (as from localhost / the same network).
    assert_internal_auth_failure(&call(&h, Method::GET, "/internal/v1/test/caller", &[]).await);
    // Id headers without a token.
    let mut ids_only = to_calling(&h.peers.telephony);
    ids_only.retain(|(k, _)| k != "x-cf-service-authorization");
    assert_internal_auth_failure(
        &call(&h, Method::GET, "/internal/v1/test/caller", &ids_only).await,
    );
    // Forwarded-for / internal-network style headers grant nothing.
    let spoof = vec![
        ("x-forwarded-for".to_string(), "127.0.0.1".to_string()),
        ("x-real-ip".to_string(), "10.0.0.1".to_string()),
        ("x-cf-internal".to_string(), "true".to_string()),
    ];
    assert_internal_auth_failure(&call(&h, Method::GET, "/internal/v1/test/caller", &spoof).await);
    // Unknown internal paths are not enumerable without a token…
    assert_internal_auth_failure(&call(&h, Method::GET, "/internal/v1/does-not-exist", &[]).await);
    // …and are a plain 404 with one.
    let found = call(
        &h,
        Method::GET,
        "/internal/v1/does-not-exist",
        &to_calling(&h.peers.telephony),
    )
    .await;
    assert_eq!(found.status, StatusCode::NOT_FOUND);
    assert_eq!(found.json["error"]["code"], "NOT_FOUND");
    let wrong_method = call(
        &h,
        Method::DELETE,
        "/internal/v1/test/callback",
        &to_calling(&h.peers.telephony),
    )
    .await;
    assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn audience_environment_trust_and_id_binding_are_enforced() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Token addressed to Privacy Relay presented to Calling.
    let for_relay = signed_headers(
        &h.peers.telephony,
        &OutboundServiceRequest::new_operation(ServiceId::privacy_relay()),
    );
    assert_internal_auth_failure(
        &call(&h, Method::GET, "/internal/v1/test/caller", &for_relay).await,
    );
    // A correctly signing but untrusted service.
    assert_internal_auth_failure(
        &call(
            &h,
            Method::GET,
            "/internal/v1/test/caller",
            &to_calling(&h.peers.untrusted),
        )
        .await,
    );
    // Tampered id headers.
    let mut headers = to_calling(&h.peers.telephony);
    for (k, v) in headers.iter_mut() {
        if k == "x-cf-request-id" {
            *v = RequestId::generate().to_string();
        }
    }
    assert_internal_auth_failure(
        &call(&h, Method::GET, "/internal/v1/test/caller", &headers).await,
    );
    let mut headers = to_calling(&h.peers.telephony);
    for (k, v) in headers.iter_mut() {
        if k == "x-cf-correlation-id" {
            *v = CorrelationId::generate().to_string();
        }
    }
    assert_internal_auth_failure(
        &call(&h, Method::GET, "/internal/v1/test/caller", &headers).await,
    );
    // Expired.
    let headers = to_calling(&h.peers.telephony);
    h.peers.clock.advance(120 + 30);
    assert_internal_auth_failure(
        &call(&h, Method::GET, "/internal/v1/test/caller", &headers).await,
    );
}

#[tokio::test]
async fn permissions_come_from_callings_policy_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Calling can never grant a Privacy Relay permission: the route is forbidden to all.
    for signer in [&h.peers.telephony, &h.peers.privacy_relay] {
        let r = call(
            &h,
            Method::POST,
            "/internal/v1/test/never-granted",
            &to_calling(signer),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
        assert_eq!(r.json["error"]["code"], "INTERNAL_PERMISSION_DENIED");
    }
}

#[tokio::test]
async fn customer_and_service_credentials_never_fall_through() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let customer = h
        .issue(c.sa(EnvironmentKind::Production), &["principals.read"])
        .await;
    let cfmk = customer.secret.expose_once().to_string();

    // Customer credential on an internal route (customer channel or service channel).
    let r = h
        .request(Method::GET, "/internal/v1/test/caller", Some(&cfmk), None)
        .await;
    assert_internal_auth_failure(&r);
    let r = call(
        &h,
        Method::GET,
        "/internal/v1/test/caller",
        &[(
            "x-cf-service-authorization".into(),
            format!("Bearer {cfmk}"),
        )],
    )
    .await;
    assert_internal_auth_failure(&r);
    // A valid service token plus a customer credential is refused (no forwarding).
    let mut both = to_calling(&h.peers.telephony);
    both.push(("authorization".into(), format!("Bearer {cfmk}")));
    assert_internal_auth_failure(&call(&h, Method::GET, "/internal/v1/test/caller", &both).await);

    // Service token on a customer route: in the service header, or in Authorization.
    let service_headers = to_calling(&h.peers.telephony);
    let r = call(&h, Method::POST, "/test/context", &service_headers).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.json["error"]["code"], "AUTHENTICATION_FAILED");
    let token = service_headers
        .iter()
        .find(|(k, _)| k == "x-cf-service-authorization")
        .unwrap()
        .1
        .clone();
    let r = h
        .request(
            Method::POST,
            "/test/context",
            Some(token.trim_start_matches("Bearer ")),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Valid customer credential + a service token header on a customer route: refused.
    let mut mixed = service_headers.clone();
    mixed.push(("authorization".into(), format!("Bearer {cfmk}")));
    let r = call(&h, Method::POST, "/test/context", &mixed).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The customer credential alone still works.
    assert_eq!(h.whoami(&cfmk).await.status, StatusCode::OK);
}

#[tokio::test]
async fn service_authentication_never_consults_customer_credential_tables() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Make every customer-credential table unreadable: internal auth must not care.
    common::exec(
        h.conn(),
        "ALTER TABLE platform_api_credentials RENAME TO platform_api_credentials_hidden",
    )
    .await
    .unwrap();
    common::exec(
        h.conn(),
        "ALTER TABLE platform_service_accounts RENAME TO platform_service_accounts_hidden",
    )
    .await
    .unwrap();
    let r = call(
        &h,
        Method::GET,
        "/internal/v1/test/caller",
        &to_calling(&h.peers.telephony),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // And with the database gone entirely.
    h.state.db.clone().close().await.unwrap();
    let r = call(
        &h,
        Method::GET,
        "/internal/v1/test/caller",
        &to_calling(&h.peers.privacy_relay),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let _ = scalar_i64; // (helper shared with other suites)
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn service_tokens_never_reach_logs_or_responses() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let capture = Capture::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_writer(capture.clone()),
        ),
    );
    let good = to_calling(&h.peers.telephony);
    let token = good
        .iter()
        .find(|(k, _)| k == "x-cf-service-authorization")
        .unwrap()
        .1
        .clone();
    let signature = token.rsplit('.').next().unwrap().to_string();
    let ok = call(&h, Method::GET, "/internal/v1/test/caller", &good).await;
    let mut bad = good.clone();
    for (k, v) in bad.iter_mut() {
        if k == "x-cf-service-authorization" {
            *v = format!("{}x", v);
        }
    }
    let rejected = call(&h, Method::GET, "/internal/v1/test/caller", &bad).await;
    assert_eq!(rejected.status, StatusCode::UNAUTHORIZED);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(logs.contains("internal service authentication rejected"));
    assert!(
        logs.contains("telephony-test-1"),
        "kid is safe operator telemetry"
    );
    for haystack in [&logs, &ok.text, &rejected.text] {
        assert!(!haystack.contains(&signature), "token signature leaked");
        assert!(!haystack.contains(token.trim_start_matches("Bearer ")));
    }
    for value in rejected.headers.values().chain(ok.headers.values()) {
        assert!(!value.to_str().unwrap_or("").contains(&signature));
    }
    let _ = secret_part;
}

#[tokio::test]
async fn internal_error_envelope_matches_the_internal_http_contract() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let contract: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts/platform/internal-http/v1.openapi.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let codes: Vec<String> = contract
        .pointer("/components/schemas/InternalErrorCode/enum")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let unauthenticated = call(&h, Method::GET, "/internal/v1/test/caller", &[]).await;
    let denied = call(
        &h,
        Method::POST,
        "/internal/v1/test/never-granted",
        &to_calling(&h.peers.telephony),
    )
    .await;
    for r in [&unauthenticated, &denied] {
        let error = r.json["error"].as_object().unwrap();
        let mut keys: Vec<&str> = error.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["code", "message", "requestId"]);
        assert!(codes.contains(&error["code"].as_str().unwrap().to_string()));
    }
    let internal_rust: Vec<&str> = calling::error::ErrorCode::ALL
        .iter()
        .filter(|c| c.is_internal())
        .map(|c| c.as_str())
        .collect();
    assert_eq!(internal_rust, codes);
}
