//! The customer ingress against a real verifying upstream: default deny, Platform Access
//! authentication, scopes, signed delegated context, header stripping, correlation,
//! upstream failures and protocol handling.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use cf_service_auth::{
    OutboundServiceRequest, ServiceId, ServiceTokenSigner, SystemClock, TokenTtl,
};
use common::*;
use serde_json::json;

#[tokio::test]
async fn probes_report_flags_and_route_counts() {
    let g = Gateway::new().await;
    let r = g.get("/health", None).await;
    assert_eq!(r.status, StatusCode::OK);
    conforms(
        "platform-developer/gateway/common/v1.schema.json#/$defs/HealthStatus",
        &r.json,
    );
    let r = g.get("/ready", None).await;
    conforms(
        "platform-developer/gateway/common/v1.schema.json#/$defs/ReadinessStatus",
        &r.json,
    );
    assert_eq!(r.json["routes"], test_routes().len());
    assert_eq!(r.json["quotas"], "not_enforced");
}

#[tokio::test]
async fn unknown_internal_callback_and_operator_paths_are_not_found_before_authentication() {
    let g = Gateway::new().await;
    g.authority.add("good", customer(&[]));
    for path in [
        "/",
        "/v1/unknown",
        "/internal/v1/gateway/context",
        "/internal/v1/credentials/authenticate",
        "/provider-callbacks/v1/sms/inbound",
        "/v1/admin/tenants",
        "/v1/telephony/legs",
        "/v1/operator/tenants",
        "/openapi.json",
        "/metrics",
    ] {
        for token in [None, Some("good")] {
            let r = g.get(path, token).await;
            assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
            assert_eq!(r.code(), "NOT_FOUND");
            conforms(
                "platform-developer/gateway/common/v1.schema.json#/$defs/Error",
                &r.json,
            );
            assert!(r.headers.get("x-request-id").is_some());
        }
    }
    assert_eq!(
        *g.authority.calls.lock().unwrap(),
        0,
        "default deny never reaches the authority"
    );
    assert_eq!(g.upstream().count(), 0);
    let r = g
        .call(
            Method::DELETE,
            "/v1/developer-platform/context",
            &[("authorization", "Bearer good")],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn credentials_are_verified_by_the_authority_only_and_fail_closed() {
    let g = Gateway::new().await;
    let path = "/v1/developer-platform/context";
    for headers in [
        vec![],
        vec![("authorization", "Bearer unknown")],
        vec![("authorization", "Basic dXNlcjpwYXNz")],
        vec![("authorization", "Bearer ")],
    ] {
        let r = g.call(Method::GET, path, &headers, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{headers:?}");
        assert_eq!(r.code(), "AUTHENTICATION_FAILED");
        assert!(r.headers.get("www-authenticate").is_some());
        assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
    }
    // A SAFETY_ONLY context is refused.
    let mut suspended = customer(&[]);
    suspended.full_authority = false;
    g.authority.add("suspended", suspended);
    assert_eq!(
        g.get(path, Some("suspended")).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Throttling is relayed with Retry-After.
    g.authority.throttle("throttled", 30);
    let r = g.get(path, Some("throttled")).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers.get("retry-after").unwrap(), "30");
    // The authority unreachable: 503, nothing forwarded.
    g.authority.add("good", customer(&[]));
    *g.authority.down.lock().unwrap() = true;
    let r = g.get(path, Some("good")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(g.upstream().count(), 0);
}

#[tokio::test]
async fn a_client_service_token_never_authenticates_a_customer() {
    let g = Gateway::new().await;
    g.authority.add("good", customer(&[]));
    // Even a GENUINE gateway-signed token presented by a client is refused.
    let k = cf_service_auth::ServiceSigningKey::from_pkcs8_pem(
        cf_service_auth::KeyId::parse("api-gateway-test-1").unwrap(),
        &g.gateway_key_pem,
    )
    .unwrap();
    let signer = ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        ENV,
        k,
        TokenTtl::default(),
        std::sync::Arc::new(SystemClock),
    );
    let signed = signer
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::developer_platform(),
        ))
        .unwrap();
    let headers = signed.headers();
    let token = headers
        .get("x-cf-service-authorization")
        .unwrap()
        .to_str()
        .unwrap();
    for auth in [None, Some("Bearer good")] {
        let mut h = vec![("x-cf-service-authorization", token)];
        if let Some(a) = auth {
            h.push(("authorization", a));
        }
        let r = g
            .call(Method::GET, "/v1/developer-platform/context", &h, None)
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(g.upstream().count(), 0);
}

#[tokio::test]
async fn scopes_come_from_platform_access() {
    let g = Gateway::new().await;
    g.authority.add("reader", customer(&["test.read"]));
    g.authority.add("nobody", customer(&[]));
    let r = g.get("/v1/test/items/item-1", Some("nobody")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    // A scope claimed by the client is not a scope.
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/item-1",
            &[
                ("authorization", "Bearer nobody"),
                ("x-scopes", "test.read"),
                ("x-cf-scopes", "test.read"),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = g.get("/v1/test/items/item-1", Some("reader")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(g.upstream().last().path, "/internal/v1/test/items/item-1");
    let r = g
        .call(
            Method::POST,
            "/v1/test/items",
            &[
                ("authorization", "Bearer reader"),
                ("content-type", "application/json"),
            ],
            Some("{}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_upstream_receives_only_the_signed_authority_never_client_claims() {
    let g = Gateway::new().await;
    let c = customer(&["test.write"]);
    g.authority.add("writer", c.clone());
    let forged_tenant = uuid::Uuid::new_v4().to_string();
    let body = json!({ "tenantId": forged_tenant, "environmentId": forged_tenant, "name": "item" })
        .to_string();
    let r = g
        .call(
            Method::POST,
            "/v1/test/items?tenant_id=evil&page_size=10",
            &[
                ("authorization", "Bearer writer"),
                ("content-type", "application/json"),
                ("idempotency-key", "idem-123"),
                ("x-cf-request-id", "req_00000000000000000000000000000000"),
                (
                    "x-cf-correlation-id",
                    "corr_00000000000000000000000000000000",
                ),
                ("x-cf-delegated-context", "{\"tenant_id\":\"evil\"}"),
                ("x-tenant-id", forged_tenant.as_str()),
                ("x-environment-id", forged_tenant.as_str()),
                ("x-forwarded-for", "10.0.0.1"),
                ("x-request-id", "client-chosen"),
            ],
            Some(&body),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let u = g.upstream().last();
    assert_eq!(u.issuer, "cfmeet.api-gateway");
    let d = u.delegated.expect("the signed delegated context");
    assert_eq!(
        d.tenant_id(),
        c.tenant_id,
        "authority comes from Platform Access only"
    );
    assert_eq!(d.application_id(), c.application_id);
    assert_eq!(d.application_environment_id(), c.environment_id);
    assert_eq!(d.initiating_service_account_id(), c.service_account_id);
    assert!(u.expires_in <= 60, "short-lived: {}s", u.expires_in);
    // No client header reached the upstream except Idempotency-Key.
    for name in [
        "x-tenant-id",
        "x-environment-id",
        "x-cf-delegated-context",
        "x-forwarded-for",
        "x-request-id",
        "authorization",
    ] {
        assert!(u.headers.get(name).is_none(), "{name} reached the upstream");
    }
    assert_eq!(u.headers.get("idempotency-key").unwrap(), "idem-123");
    assert_ne!(u.correlation_id, "corr_00000000000000000000000000000000");
    assert_ne!(u.request_id, "req_00000000000000000000000000000000");
    // The body is relayed as data; the domain service decides what it means.
    assert_eq!(u.body["tenantId"], forged_tenant);
    assert_eq!(r.json["tenantId"], c.tenant_id.to_string());
    // Correlation: the client sees the operation's correlation id, the hop has its own id.
    assert_eq!(
        r.headers.get("x-correlation-id").unwrap().to_str().unwrap(),
        u.correlation_id
    );
    assert_ne!(
        r.headers.get("x-request-id").unwrap().to_str().unwrap(),
        u.request_id
    );
    assert_eq!(
        r.headers
            .get("x-request-id")
            .unwrap()
            .to_str()
            .unwrap()
            .len(),
        36
    );
    // Internal response headers are not relayed; safe ones are.
    assert!(r.headers.get("x-cf-internal-detail").is_none());
    assert_eq!(r.headers.get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn forged_contexts_sent_around_the_gateway_are_rejected_by_the_upstream() {
    let g = Gateway::new().await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let url = format!("{}/internal/v1/gateway/context", g.upstream().base_url);
    // No token, a forged context header, a token from a key the upstream does not trust.
    let r = http
        .get(&url)
        .header("x-cf-delegated-context", "{\"tenant_id\":\"evil\"}")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let rogue = ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        ENV,
        key("api-gateway-test-1"),
        TokenTtl::default(),
        std::sync::Arc::new(SystemClock),
    );
    let forged = rogue
        .sign(
            &OutboundServiceRequest::new_operation(ServiceId::developer_platform())
                .with_delegated_context(customer(&[]).delegated()),
        )
        .unwrap();
    let mut request = http.get(&url);
    for (k, v) in forged.headers().iter() {
        request = request.header(k.as_str(), v.as_bytes());
    }
    assert_eq!(request.send().await.unwrap().status(), 401);
    assert_eq!(g.upstream().count(), 0);
    assert_eq!(*g.upstream().rejected.lock().unwrap(), 2);
}

#[tokio::test]
async fn upstream_authorization_decisions_are_relayed_unchanged() {
    let g = Gateway::new().await;
    g.authority.add("good", customer(&[]));
    let r = g.get("/v1/test/not-yours", Some("good")).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json["error"]["code"], "FORBIDDEN");
    assert!(r.headers.get("x-cf-internal-detail").is_none());
}

#[tokio::test]
async fn upstream_failures_are_safe_errors() {
    // Deadline.
    let g = Gateway::with(Options {
        timeout: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    g.authority.add("good", customer(&[]));
    *g.upstream().delay.lock().unwrap() = Duration::from_millis(1_500);
    let r = g.get("/v1/developer-platform/context", Some("good")).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(r.code(), "UPSTREAM_TIMEOUT");
    // Upstream not configured.
    let g = Gateway::with(Options {
        upstream: false,
        ..Default::default()
    })
    .await;
    g.authority.add("good", customer(&[]));
    let r = g.get("/v1/developer-platform/context", Some("good")).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    // Upstream down.
    let g = Gateway::new().await;
    g.authority.add("good", customer(&[]));
    let base = g.upstream().base_url.clone();
    drop(g);
    let k = key("api-gateway-test-1");
    let routes = api_gateway::routing::RouteTable::new(test_routes(), true).unwrap();
    let authority = std::sync::Arc::new(FakeAuthority::default());
    authority.add("good", customer(&[]));
    tokio::time::sleep(Duration::from_millis(50)).await;
    let state = api_gateway::state::AppState::new(
        authority,
        runtime(k, &[(ServiceId::developer_platform(), base)]),
        routes,
    );
    let g2 = Gateway {
        router: api_gateway::http::app(state),
        authority: Default::default(),
        upstream: None,
        gateway_key_pem: String::new(),
    };
    let r = g2.get("/v1/developer-platform/context", Some("good")).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY, "{}", r.json);
    assert_eq!(r.code(), "UPSTREAM_UNAVAILABLE");
    assert!(
        !r.json.to_string().contains("127.0.0.1"),
        "never leaks upstream details"
    );
}

#[tokio::test]
async fn bodies_queries_and_protocols_are_bounded() {
    let g = Gateway::new().await;
    g.authority.add("w", customer(&["test.write", "test.read"]));
    let auth = ("authorization", "Bearer w");
    let json_ct = ("content-type", "application/json");
    let big = json!({ "x": "y".repeat(10_000) }).to_string();
    let r = g
        .call(Method::POST, "/v1/test/items", &[auth, json_ct], Some(&big))
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let r = g
        .call(
            Method::POST,
            "/v1/test/items",
            &[auth, ("content-type", "text/plain")],
            Some("hello"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = g
        .call(
            Method::POST,
            "/v1/test/items",
            &[auth, json_ct],
            Some("{not json"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let many: String = (0..17)
        .map(|i| format!("k{i}=v"))
        .collect::<Vec<_>>()
        .join("&");
    let r = g.get(&format!("/v1/test/items/a?{many}"), Some("w")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = g.get("/v1/test/items/a?bad-key=1", Some("w")).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // WebSocket routes and upgrades: forwarding is Section 51.
    let r = g.get("/v1/test/stream", Some("w")).await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(r.code(), "PROTOCOL_NOT_ENABLED");
    let r = g
        .call(
            Method::GET,
            "/v1/developer-platform/context",
            &[auth, ("upgrade", "websocket"), ("connection", "upgrade")],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(g.upstream().count(), 0);
}

#[tokio::test]
async fn first_party_and_external_tenants_use_the_same_routes() {
    let g = Gateway::new().await;
    let external = customer(&["test.read"]);
    let mut first_party = customer(&["test.read"]);
    first_party.classification = api_gateway::identity::TrustClassification::FirstParty;
    g.authority.add("ext", external);
    g.authority.add("fp", first_party);
    for t in ["ext", "fp"] {
        let r = g.get("/v1/test/items/x1", Some(t)).await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let fp_only = customer(&[]);
    g.authority.add(
        "fp-noscope",
        api_gateway::identity::AuthenticatedCustomer {
            classification: api_gateway::identity::TrustClassification::FirstParty,
            ..fp_only
        },
    );
    let r = g.get("/v1/test/items/x1", Some("fp-noscope")).await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "first party has no implicit scope"
    );
}
