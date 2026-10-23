//! The customer ingress: default deny before authentication, Platform Access as the only
//! credential authority, the signed delegated context as the only upstream authority,
//! transported end-user tokens for domain-authenticated routes, header / body / query
//! policy, quotas, CORS and safe upstream failures.

mod common;

use std::{sync::Arc, time::Duration};

use api_gateway::{
    quota::{Limits, QuotaAuthority, QuotaEngine},
    routing::RoutingControls,
};
use axum::http::{Method, StatusCode};
use cf_service_auth::{
    CorrelationId, OutboundServiceRequest, ServiceId, ServiceTokenSigner, SystemClock, TokenTtl,
};
use common::*;
use serde_json::json;
use uuid::Uuid;

const ERR: &str = "platform-developer/gateway/common/v1.schema.json#/$defs/Error";
const CRED: &str = "cfmk_sbx_aaaaaaaaaaaaaaaaaaaaaaaa_bbbb";
const OTHER: &str = "cfmk_test_cccccccccccccccccccccccc_dddd";

#[tokio::test]
async fn probes_docs_and_discovery_are_public_infrastructure() {
    let g = Gateway::new().await;
    let r = g.get("/ready", None).await;
    assert_eq!(r.json["publicRouting"], "enabled");
    assert_eq!(r.json["upstreams"]["cfmeet.calling"], "reachable");
    assert!(r.headers.get("x-request-id").is_some());
    let d = g.get("/discovery", None).await;
    assert!(d.json["families"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["domain"] == "calling"));
    let agg = g.get("/openapi.json", None).await;
    assert_eq!(agg.status, StatusCode::OK);
    assert_eq!(agg.json["openapi"], "3.1.0");
    let one = g.get("/openapi/messaging.json", None).await;
    assert!(one.json["paths"]["/v1/realtime"].is_object());
    assert_eq!(
        g.get("/openapi/telephony.json", None).await.status,
        StatusCode::NOT_FOUND
    );
    let page = g.get("/docs", None).await;
    assert!(page.headers.get("content-security-policy").is_some());
}

#[tokio::test]
async fn unknown_internal_callback_and_operator_paths_are_not_found_before_authentication() {
    let g = Gateway::new().await;
    for path in [
        "/internal/v1/gateway/context",
        "/internal/v1/credentials/authenticate",
        "/v1/admin/numbers",
        "/provider-callbacks/v1/sms/x/inbound",
        "/internal/provider/media/events",
        "/v1/unknown",
        "/api/v1/meetings",
        "/",
    ] {
        let r = g.get(path, Some(CRED)).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.code(), "NOT_FOUND");
        conforms(ERR, &r.json);
    }
    assert_eq!(
        *g.authority.calls.lock().unwrap(),
        0,
        "no credential was examined"
    );
    let r = g.call(Method::DELETE, "/v1/test/items", &[], None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(r.headers.get("allow").unwrap(), "POST");
}

#[tokio::test]
async fn credentials_are_verified_by_platform_access_only_and_fail_closed() {
    let g = Gateway::new().await;
    g.authority.add(CRED, customer(&["test.read"]));
    let ok = g.get("/v1/test/items/abc", Some(CRED)).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    assert_eq!(
        g.get("/v1/test/items/abc", Some("cfmk_sbx_unknown"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        g.get("/v1/test/items/abc", None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = g.get("/v1/test/items/abc", Some("cfmk_sbx_unknown")).await;
    assert!(r.headers.get("www-authenticate").is_some());
    g.authority.throttle("cfmk_sbx_throttled", 7);
    let r = g
        .get("/v1/test/items/abc", Some("cfmk_sbx_throttled"))
        .await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.headers.get("retry-after").unwrap(), "7");
    let mut suspended = customer(&["test.read"]);
    suspended.full_authority = false;
    g.authority.add("cfmk_sbx_suspended", suspended);
    assert_eq!(
        g.get("/v1/test/items/abc", Some("cfmk_sbx_suspended"))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    *g.authority.down.lock().unwrap() = true;
    let r = g.get("/v1/test/items/abc", Some(CRED)).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        g.calling().count(),
        1,
        "only the authenticated request was forwarded"
    );
}

#[tokio::test]
async fn scopes_come_from_platform_access() {
    let g = Gateway::new().await;
    g.authority.add(CRED, customer(&["test.read"]));
    let r = g
        .call(
            Method::POST,
            "/v1/test/items",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("content-type", "application/json"),
            ],
            Some("{}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    assert_eq!(g.calling().count(), 0);
}

#[tokio::test]
async fn the_owning_service_receives_only_the_signed_authority_never_client_claims() {
    let g = Gateway::new().await;
    let c = customer(&["test.read"]);
    g.authority.add(CRED, c.clone());
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/abc?tenant_id=evil&limit=5",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("x-tenant-id", &Uuid::new_v4().to_string()),
                ("x-cf-delegated-context", "{}"),
                (
                    "x-cf-correlation-id",
                    "corr_00000000000000000000000000000000",
                ),
                ("x-forwarded-for", "10.0.0.1"),
                ("cookie", "session=abc"),
                ("idempotency-key", "k-1"),
                ("if-none-match", "\"e1\""),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let got = g.calling().last();
    // The credential's own chain, signed, short-lived, with the credential id the
    // receiver re-verifies — and nothing the client claimed.
    let d = got.delegated.unwrap();
    assert_eq!(d.tenant_id(), c.tenant_id);
    assert_eq!(d.application_environment_id(), c.environment_id);
    assert_eq!(d.initiating_credential_id(), Some(c.credential_id));
    assert_eq!(got.issuer.as_deref(), Some("cfmeet.api-gateway"));
    assert!(
        got.expires_in <= 60,
        "60-second context: {}",
        got.expires_in
    );
    assert_eq!(
        got.correlation_id.as_deref(),
        r.headers
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok()),
        "one correlation id"
    );
    for h in [
        "authorization",
        "x-tenant-id",
        "x-cf-delegated-context",
        "x-forwarded-for",
        "cookie",
    ] {
        assert!(!got.headers.contains_key(h), "{h} reached the upstream");
    }
    assert_eq!(got.headers.get("idempotency-key").unwrap(), "k-1");
    assert_eq!(got.headers.get("if-none-match").unwrap(), "\"e1\"");
    assert_eq!(
        got.query.as_deref(),
        Some("tenant_id=evil&limit=5"),
        "queries are data, never authority"
    );
    // Internal response headers never leave; allow-listed ones do.
    assert!(!r.headers.contains_key("x-cf-internal-detail"));
    let created = g
        .call(
            Method::POST,
            "/v1/test/created",
            &[("authorization", &format!("Bearer {CRED}"))],
            None,
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(
        created.headers.get("location").unwrap(),
        "/v1/test/items/abc"
    );
    assert_eq!(created.headers.get("idempotent-replayed").unwrap(), "true");
    assert!(!created.headers.contains_key("set-cookie"));
    assert!(!created.headers.contains_key("x-cf-internal-detail"));
}

#[tokio::test]
async fn a_client_service_token_never_authenticates_and_forged_hops_are_rejected_upstream() {
    let g = Gateway::new().await;
    g.authority.add(CRED, customer(&["test.read"]));
    let forged = ServiceTokenSigner::new(
        ServiceId::api_gateway(),
        ENV,
        key("api-gateway-test-1"),
        TokenTtl::default(),
        Arc::new(SystemClock),
    )
    .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
    .unwrap();
    let mut headers: Vec<(String, String)> = forged
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
        .collect();
    headers.push(("authorization".into(), format!("Bearer {CRED}")));
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let r = g.call(Method::GET, "/v1/test/items/abc", &refs, None).await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "a service token at the ingress is refused"
    );
    // The same forged token sent AROUND the gateway (another key with the gateway's
    // name) is rejected by the owning service itself.
    let client = reqwest::Client::new();
    let mut req = client.get(format!("{}/v1/test/items/abc", g.calling().base_url));
    for (k, v) in forged.headers().iter() {
        req = req.header(k.as_str(), v.as_bytes());
    }
    assert_eq!(req.send().await.unwrap().status(), 401);
    assert_eq!(*g.calling().rejected.lock().unwrap(), 1);
}

#[tokio::test]
async fn end_user_tokens_are_transported_and_platform_credentials_never_are() {
    let g = Gateway::new().await;
    // Meeting session / participant token: transported verbatim, no gateway token.
    let r = g
        .get(
            "/api/v2/test-spaces/s1",
            Some("eyJhbGciOiJIUzI1NiJ9.session.sig"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["auth"], "passthrough");
    assert_eq!(
        r.json["authorization"],
        "Bearer eyJhbGciOiJIUzI1NiJ9.session.sig"
    );
    let r = g
        .call(
            Method::GET,
            "/api/v2/test-spaces/s1",
            &[("x-participant-token", "pt_123")],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        g.meeting()
            .last()
            .headers
            .get("x-participant-token")
            .unwrap(),
        "pt_123"
    );
    // A platform credential is never transported to a service (refused at the edge).
    let r = g.get("/api/v2/test-spaces/s1", Some(CRED)).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(g.meeting().count(), 2);
    // Signaling device credentials: transported.
    let r = g.get("/v1/test/devices", Some("cfsig_abc.def.ghi")).await;
    assert_eq!(r.json["auth"], "passthrough");
    // Messaging: a credential is authenticated here; a client token is transported.
    g.authority.add(CRED, customer(&[]));
    let r = g
        .call(
            Method::GET,
            "/v1/test/mixed",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("cf-acting-principal", "p-1"),
            ],
            None,
        )
        .await;
    assert_eq!(r.json["auth"], "service", "{}", r.json);
    assert_eq!(
        g.messaging()
            .last()
            .headers
            .get("cf-acting-principal")
            .unwrap(),
        "p-1"
    );
    let r = g
        .call(
            Method::GET,
            "/v1/test/mixed",
            &[("authorization", "CF-Client-Token ct_1")],
            None,
        )
        .await;
    assert_eq!(r.json["auth"], "passthrough");
    assert_eq!(r.json["authorization"], "CF-Client-Token ct_1");
    // Developer Console: the session goes to the Developer Platform inside a signed hop.
    let r = g.get("/v1/test-orgs", Some("eyJ.session.token")).await;
    assert_eq!(r.json["auth"], "service");
    let got = g.dp().last();
    assert!(
        got.delegated.is_none(),
        "a human session is not a delegated machine context"
    );
    assert_eq!(
        got.headers.get("x-cf-user-session").unwrap(),
        "eyJ.session.token"
    );
    assert!(!got.headers.contains_key("authorization"));
}

#[tokio::test]
async fn platform_credential_meetings_requests_are_forwarded_with_the_signed_delegated_context() {
    let g = Gateway::new().await;
    let c = customer(&["meetings.write"]);
    g.authority.add(CRED, c.clone());
    let r = g
        .call(
            Method::POST,
            "/api/v2/test-instant-meetings",
            &[
                ("authorization", &format!("Bearer {CRED}")),
                ("x-participant-token", "pt_forged"),
                ("x-client-kind", "server"),
            ],
            Some(r#"{"title":"Support call"}"#),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["auth"], "service", "{}", r.json);
    let got = g.meeting().last();
    // The Meeting Service receives the gateway's signed hop (it re-verifies the credential
    // named in the delegated context with Platform Access) — never the credential, never
    // a client-supplied participant token.
    assert_eq!(got.issuer.as_deref(), Some("cfmeet.api-gateway"));
    let d = got.delegated.expect("delegated context");
    assert_eq!(d.tenant_id(), c.tenant_id);
    assert_eq!(d.application_id(), c.application_id);
    assert_eq!(d.application_environment_id(), c.environment_id);
    assert_eq!(d.initiating_service_account_id(), c.service_account_id);
    assert_eq!(d.initiating_credential_id(), Some(c.credential_id));
    assert!(got.expires_in <= 60);
    assert_eq!(
        got.correlation_id.as_deref(),
        r.headers
            .get("x-correlation-id")
            .and_then(|v| v.to_str().ok())
    );
    assert!(!got.headers.contains_key("authorization"));
    assert!(!got.headers.contains_key("x-participant-token"));
    assert_eq!(got.headers.get("x-client-kind").unwrap(), "server");
    assert_eq!(got.body, json!({ "title": "Support call" }));
    // The route scope is pre-checked at the edge.
    let reader = customer(&["meetings.read"]);
    g.authority.add("cfmk_sbx_reader_secret", reader);
    let before = g.meeting().count();
    let r = g
        .call(
            Method::POST,
            "/api/v2/test-instant-meetings",
            &[("authorization", "Bearer cfmk_sbx_reader_secret")],
            Some("{}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    assert_eq!(r.code(), "INSUFFICIENT_SCOPE");
    assert_eq!(g.meeting().count(), before);
    // A CF Meet session on the same route is still transported verbatim, unsigned.
    let r = g
        .call(
            Method::POST,
            "/api/v2/test-instant-meetings",
            &[("authorization", "Bearer eyJhbGciOiJIUzI1NiJ9.session.sig")],
            Some("{}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["auth"], "passthrough");
    let got = g.meeting().last();
    assert!(got.issuer.is_none() && got.delegated.is_none());
}

#[tokio::test]
async fn upstream_decisions_errors_and_streams_are_relayed_faithfully() {
    let g = Gateway::new().await;
    g.authority.add(CRED, customer(&[]));
    // Domain errors pass through unchanged (status, code, content type).
    let r = g.get("/v1/test/not-yours", Some(CRED)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.code(), "CALL_NOT_FOUND_OR_FORBIDDEN");
    // Non-JSON content types and large bodies are streamed unchanged.
    let t = g.get("/v1/test/text", Some(CRED)).await;
    assert_eq!(
        t.headers.get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    assert_eq!(t.bytes, b"plain upstream text");
    let big = g.get("/v1/test/big", Some(CRED)).await;
    assert_eq!(big.status, StatusCode::OK);
    assert_eq!(big.bytes.len(), 48 * 64 * 1024);
    assert_eq!(big.headers.get("cache-control").unwrap(), "no-store");
}

#[tokio::test]
async fn upstream_failures_are_safe_errors() {
    let g = Gateway::with(Options {
        timeout: Duration::from_millis(300),
        ..Options::default()
    })
    .await;
    g.authority.add(CRED, customer(&["test.read"]));
    *g.calling().delay.lock().unwrap() = Duration::from_secs(2);
    let r = g.get("/v1/test/items/abc", Some(CRED)).await;
    assert_eq!(r.status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(r.code(), "UPSTREAM_TIMEOUT");
    conforms(ERR, &r.json);
    // No upstream configured / unreachable: 503 / 502, never another service.
    let g = Gateway::with(Options {
        upstream: false,
        ..Options::default()
    })
    .await;
    g.authority.add(CRED, customer(&["test.read"]));
    assert_eq!(
        g.get("/v1/test/items/abc", Some(CRED)).await.status,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn bodies_headers_queries_and_protocols_are_bounded() {
    let g = Gateway::new().await;
    g.authority
        .add(CRED, customer(&["test.read", "test.write"]));
    let auth = format!("Bearer {CRED}");
    let big = "x".repeat(5000);
    let r = g
        .call(
            Method::POST,
            "/v1/test/items",
            &[
                ("authorization", &auth),
                ("content-type", "application/json"),
            ],
            Some(&format!("\"{big}\"")),
        )
        .await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/abc",
            &[("authorization", &auth)],
            Some("{\"a\":1}"),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "no body on GET");
    let many: Vec<(String, String)> = (0..70).map(|i| (format!("x-h{i}"), "v".into())).collect();
    let refs: Vec<(&str, &str)> = many.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r = g.call(Method::GET, "/v1/test/items/abc", &refs, None).await;
    assert_eq!(r.status, StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE);
    let long = format!("/v1/test/items/abc?q={}", "a".repeat(5000));
    assert_eq!(
        g.call(Method::GET, &long, &[], None).await.status,
        StatusCode::URI_TOO_LONG
    );
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/abc?q=%20x#frag",
            &[("authorization", &auth)],
            None,
        )
        .await;
    assert!(r.status.is_success() || r.status == StatusCode::BAD_REQUEST);
    // A WebSocket upgrade on a REST route is refused.
    let r = g
        .call(
            Method::GET,
            "/v1/test/items/abc",
            &[
                ("authorization", &auth),
                ("upgrade", "websocket"),
                ("connection", "upgrade"),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn cors_allows_only_configured_origins() {
    let g = Gateway::with(Options {
        cors: vec!["https://console.cfmeet.com".into()],
        ..Options::default()
    })
    .await;
    let pre = g
        .call(
            Method::OPTIONS,
            "/v1/test/items",
            &[
                ("origin", "https://console.cfmeet.com"),
                ("access-control-request-method", "POST"),
            ],
            None,
        )
        .await;
    assert_eq!(pre.status, StatusCode::NO_CONTENT);
    assert_eq!(
        pre.headers.get("access-control-allow-origin").unwrap(),
        "https://console.cfmeet.com"
    );
    assert!(pre
        .headers
        .get("access-control-allow-methods")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("POST"));
    let bad = g
        .call(
            Method::OPTIONS,
            "/v1/test/items",
            &[
                ("origin", "https://evil.example"),
                ("access-control-request-method", "POST"),
            ],
            None,
        )
        .await;
    assert_eq!(bad.status, StatusCode::FORBIDDEN);
    let simple = g
        .call(
            Method::GET,
            "/ready",
            &[("origin", "https://console.cfmeet.com")],
            None,
        )
        .await;
    assert_eq!(
        simple.headers.get("access-control-allow-origin").unwrap(),
        "https://console.cfmeet.com"
    );
    let other = g
        .call(
            Method::GET,
            "/ready",
            &[("origin", "https://evil.example")],
            None,
        )
        .await;
    assert!(other.headers.get("access-control-allow-origin").is_none());
}

#[derive(Default)]
struct Tight {
    batches: std::sync::Mutex<Vec<api_gateway::quota::UsageBatch>>,
}

#[async_trait::async_trait]
impl QuotaAuthority for Tight {
    async fn limits(
        &self,
        _e: Uuid,
        _k: cf_service_auth::ApplicationEnvironmentKind,
        _c: &CorrelationId,
    ) -> Option<Limits> {
        Some(Limits {
            per_minute: 2,
            per_day: 1000,
            minute_level: "ENVIRONMENT",
            day_level: "ENVIRONMENT",
        })
    }
    async fn durable_count(&self, _e: Uuid, _d: &str) -> Option<u64> {
        Some(0)
    }
    async fn record(&self, _b: Uuid, u: &api_gateway::quota::UsageBatch) -> bool {
        self.batches.lock().unwrap().push(u.clone());
        true
    }
}

#[tokio::test]
async fn platform_request_quotas_aggregate_across_service_families() {
    let tight = Arc::new(Tight::default());
    let quota = Arc::new(QuotaEngine::new(None, "t:".into(), tight.clone()));
    let g = Gateway::with(Options {
        quota: Some(quota.clone()),
        ..Options::default()
    })
    .await;
    let c = customer(&["test.read"]);
    g.authority.add(CRED, c.clone());
    let mut other = customer(&["test.read"]);
    other.environment_id = Uuid::new_v4();
    g.authority.add(OTHER, other);
    // One Calling request and one Messaging request use the same environment budget.
    let r = g.get("/v1/test/items/abc", Some(CRED)).await;
    assert_eq!(r.headers.get("x-cf-quota-limit-minute").unwrap(), "2");
    assert_eq!(
        g.get("/v1/test/mixed", Some(CRED)).await.status,
        StatusCode::OK
    );
    let r = g.get("/v1/test/items/abc", Some(CRED)).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(r.code(), "QUOTA_EXCEEDED");
    assert_eq!(
        r.headers.get("x-cf-quota-window").unwrap(),
        "requests-per-minute"
    );
    assert_eq!(r.headers.get("x-cf-quota-level").unwrap(), "ENVIRONMENT");
    assert!(r.headers.get("retry-after").is_some());
    conforms(ERR, &r.json);
    // Another environment is unaffected; end-user token routes are not platform-metered.
    assert_eq!(
        g.get("/v1/test/items/abc", Some(OTHER)).await.status,
        StatusCode::OK
    );
    assert_eq!(
        g.get("/api/v2/test-spaces/s1", Some("eyJ.a.b"))
            .await
            .status,
        StatusCode::OK
    );
    // Normalized API_OPERATION usage: the two forwarded requests of the environment are
    // counted by domain / operation / status class (the quota refusal and end-user
    // token requests are not platform operations of a credential).
    assert!(quota.flush().await > 0);
    let batches = tight.batches.lock().unwrap().clone();
    let ops: Vec<_> = batches
        .iter()
        .flat_map(|b| b.operations.iter())
        .filter(|o| o.environment == c.environment_id)
        .collect();
    assert_eq!(ops.iter().map(|o| o.count).sum::<u64>(), 2, "{ops:?}");
    assert!(
        ops.iter()
            .all(|o| o.status_class == "2xx" && !o.operation.contains('/')),
        "{ops:?}"
    );
    let domains: std::collections::BTreeSet<_> = ops.iter().map(|o| o.domain.as_str()).collect();
    assert_eq!(domains.len(), 2, "two service families: {ops:?}");
}

#[tokio::test]
async fn rollback_controls_disable_families_and_move_webhook_ownership() {
    let mut controls = RoutingControls::default();
    controls.disabled_domains.insert("meetings".into());
    let routes: Vec<_> = test_routes()
        .into_iter()
        .filter(|r| !controls.disabled_domains.contains(&r.domain))
        .collect();
    let g = Gateway::with(Options {
        routes: Some(routes),
        controls,
        ..Options::default()
    })
    .await;
    assert_eq!(
        g.get("/api/v2/test-spaces/s1", Some("eyJ.a.b"))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    let r = g.get("/ready", None).await;
    assert_eq!(r.json["disabledFamilies"], json!(["meetings"]));
}
