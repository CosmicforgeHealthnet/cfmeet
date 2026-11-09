//! INV-002: the operator routes of the Communications Hub that the operation-coverage
//! baseline still listed as untested.
//!
//! * `POST /internal/v1/communications/backfill` — pages the Account's native accounts
//!   into communication profiles (idempotent, cursor driven, owner-validated);
//! * `GET /internal/v1/communications/metrics` — process counters.
//!
//! Both require `communications-backfill.run` (operator console only): a missing token is
//! 401, a trusted peer without the permission 403, a replayed token 401.
mod common;
use axum::http::{HeaderMap, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::*;
use communications::store::one;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const BACKFILL: &str = "/internal/v1/communications/backfill";
const METRICS: &str = "/internal/v1/communications/metrics";

fn headers(signer: &ServiceTokenSigner, delegated: Option<&Harness>) -> HeaderMap {
    let mut request = OutboundServiceRequest::new_operation(ServiceId::communications());
    if let Some(h) = delegated {
        request = request.with_delegated_context(h.scope.clone());
    }
    signer.sign(&request).unwrap().headers()
}

async fn get(h: &Harness, path: &str, headers: HeaderMap) -> (StatusCode, Value) {
    let mut r = axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .body(axum::body::Body::empty())
        .unwrap();
    r.headers_mut().extend(headers);
    let response = communications::api::app(h.state.clone())
        .oneshot(r)
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn profiles(h: &Harness) -> i64 {
    let v: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_profiles",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    v["n"].as_i64().unwrap()
}

async fn backfilled_audits(h: &Harness) -> i64 {
    let v: Value = one(
        &h.state.store.db,
        "SELECT jsonb_build_object('n',count(*)) doc FROM communication_audit WHERE operation='PROFILE_BACKFILLED'",
        vec![],
    )
    .await
    .unwrap()
    .unwrap();
    v["n"].as_i64().unwrap()
}

#[tokio::test]
async fn backfill_is_operator_only_and_validates_its_page_request() {
    let h = Harness::new().await;
    h.account();
    // No token.
    let (status, body) = h
        .http_headers(BACKFILL, json!({"limit":10}), HeaderMap::new())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "AUTHENTICATION_REQUIRED");
    // Trusted peers without communications-backfill.run.
    for signer in [&h.account_signer, &h.calling_signer] {
        let (status, body) = h
            .http(signer, BACKFILL, json!({"limit":10}), h.scope.clone())
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
    }
    // The operator, but without a signed delegated scope.
    let (status, body) = h
        .http_headers(BACKFILL, json!({"limit":10}), headers(&h.operator, None))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    // Bounded page size, strict body.
    for bad in [
        json!({"limit":0}),
        json!({"limit":101}),
        json!({"limit":5,"cursor":"not-a-uuid"}),
        json!({"limit":5,"tenantId":Uuid::new_v4()}),
        json!({}),
    ] {
        let (status, body) = h
            .http(&h.operator, BACKFILL, bad.clone(), h.scope.clone())
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR", "{bad}");
    }
    assert_eq!(
        profiles(&h).await,
        0,
        "nothing is created by refused requests"
    );
    // A replayed token is refused even for the operator.
    let replayed = headers(&h.operator, Some(&h));
    let (status, _) = h
        .http_headers(BACKFILL, json!({"limit":10}), replayed.clone())
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = h
        .http_headers(BACKFILL, json!({"limit":10}), replayed)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    h.close().await;
}

#[tokio::test]
async fn backfill_pages_accounts_into_profiles_once() {
    let h = Harness::new().await;
    let existing = h.account();
    h.profile(&existing).await;
    h.account();
    h.account();
    assert_eq!(profiles(&h).await, 1);

    let mut cursor = Value::Null;
    let mut created = 0;
    let mut pages = 0;
    loop {
        let (status, body) = h
            .http(
                &h.operator,
                BACKFILL,
                json!({"limit":2,"cursor":cursor}),
                h.scope.clone(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        created += body["created"].as_i64().unwrap();
        pages += 1;
        cursor = body["nextCursor"].clone();
        if cursor.is_null() {
            break;
        }
        assert!(pages < 5, "cursor must terminate");
    }
    assert_eq!(pages, 2, "3 accounts in pages of 2");
    assert_eq!(created, 2, "the existing profile is kept, not replaced");
    assert_eq!(profiles(&h).await, 3);
    assert_eq!(backfilled_audits(&h).await, 2);

    // A full re-run is a no-op.
    let (status, body) = h
        .http(&h.operator, BACKFILL, json!({"limit":100}), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["created"], 0);
    assert!(body["nextCursor"].is_null());
    assert_eq!(profiles(&h).await, 3);
    h.close().await;
}

#[tokio::test]
async fn backfill_refuses_malformed_or_unavailable_owner_pages() {
    let h = Harness::new().await;
    // Not a native account reference: refused before anything is written.
    h.world.write().unwrap().backfill_page = Some(json!({"accounts":[
        {"accountReference":"external:42","principalId":Uuid::new_v4()}
    ],"nextCursor":null}));
    let (status, body) = h
        .http(&h.operator, BACKFILL, json!({"limit":10}), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "COMMUNICATIONS_UNAVAILABLE");
    // More accounts than the requested page.
    let account = |_: usize| json!({"accountReference":format!("cfmeet-user:{}",Uuid::new_v4()),"principalId":Uuid::new_v4()});
    h.world.write().unwrap().backfill_page =
        Some(json!({"accounts":(0..3).map(account).collect::<Vec<_>>(),"nextCursor":null}));
    let (status, _) = h
        .http(&h.operator, BACKFILL, json!({"limit":2}), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(profiles(&h).await, 0);
    // The Account is down.
    h.stop_owners();
    let (status, body) = h
        .http(&h.operator, BACKFILL, json!({"limit":10}), h.scope.clone())
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(profiles(&h).await, 0);
    h.close().await;
}

#[tokio::test]
async fn metrics_are_operator_only_counters() {
    let h = Harness::new().await;
    let (status, body) = get(&h, METRICS, HeaderMap::new()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"]["code"], "AUTHENTICATION_REQUIRED");
    for signer in [&h.account_signer, &h.calling_signer] {
        let (status, body) = get(&h, METRICS, headers(signer, None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
    }
    let signed = headers(&h.operator, None);
    let (status, body) = get(&h, METRICS, signed.clone()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["schemaVersion"], 1);
    for counter in ["uptimeSeconds", "requests", "denied", "eventsDelivered"] {
        assert!(body[counter].is_u64(), "{counter}: {body}");
    }
    let object = body.as_object().unwrap();
    assert_eq!(object.len(), 5, "counters only, no personal data: {body}");
    // The token is single use.
    let (status, _) = get(&h, METRICS, signed).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // Counters move with traffic: a denied policy check is counted.
    let before = body["denied"].as_u64().unwrap() + body["requests"].as_u64().unwrap();
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    *h.blocked.write().unwrap() = true;
    let _ = h
        .http(
            &h.calling_signer,
            "/internal/v1/communications/policy/check",
            serde_json::to_value(check(cf_communications_policy::Action::DirectCall, &a, &b))
                .unwrap(),
            h.scope.clone(),
        )
        .await;
    let (_, after) = get(&h, METRICS, headers(&h.operator, None)).await;
    assert!(
        after["denied"].as_u64().unwrap() + after["requests"].as_u64().unwrap() > before,
        "{after}"
    );
    h.close().await;
}
