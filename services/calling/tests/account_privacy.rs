//! S2: the Meeting Service erases and exports a deleted account's call history over
//! internal service authentication (`src/account_privacy.rs`, `calling-account.privacy`).

mod common;

use axum::http::{Method, StatusCode};
use calling::domain::{EnvironmentKind, PrincipalKind, TenantClassification};
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner,
};
use common::{Consumer, Harness, NewPrincipal, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

const KIND: EnvironmentKind = EnvironmentKind::Production;
const ERASE: &str = "/internal/v1/account-privacy/erasure";
const EXPORT: &str = "/internal/v1/account-privacy/export";

fn delegated(c: &Consumer) -> DelegatedPlatformContext {
    DelegatedPlatformContext::new(
        c.tenant.id.as_uuid(),
        c.application.id.as_uuid(),
        c.env(KIND).id.as_uuid(),
        ApplicationEnvironmentKind::Production,
        c.sa(KIND).as_uuid(),
    )
}

async fn internal(
    h: &Harness,
    signer: &ServiceTokenSigner,
    context: Option<DelegatedPlatformContext>,
    path: &str,
    body: Value,
) -> TestResponse {
    let mut request = OutboundServiceRequest::new_operation(ServiceId::calling());
    if let Some(context) = context {
        request = request.with_delegated_context(context);
    }
    let headers: Vec<(String, String)> = signer
        .sign(&request)
        .unwrap()
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    h.request_with(Method::POST, path, None, Some(body), &refs)
        .await
}

#[tokio::test]
async fn only_the_meeting_service_erases_and_exports_an_accounts_call_history() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("cfmeet", TenantClassification::FirstParty).await;
    let (view, _) = h
        .platform
        .register_principal(
            c.env(KIND).id,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: format!("cfmeet-user:{}", Uuid::new_v4()),
            },
        )
        .await
        .unwrap();
    let principal = view.id.as_uuid();
    let body = json!({ "principalId": principal });
    let ms = h.peers.meeting_service.clone();

    // Another trusted service without the grant, and an untrusted one.
    let r = internal(
        &h,
        &h.peers.api_gateway,
        Some(delegated(&c)),
        ERASE,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = internal(
        &h,
        &h.peers.untrusted,
        Some(delegated(&c)),
        ERASE,
        body.clone(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // The scope must travel signed.
    let r = internal(&h, &ms, None, ERASE, body.clone()).await;
    assert!(r.status.is_client_error(), "{}", r.text);
    // Closed bodies.
    let r = internal(
        &h,
        &ms,
        Some(delegated(&c)),
        ERASE,
        json!({ "principalId": principal, "extra": true }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // Nothing live, nothing left: complete, and idempotent.
    for _ in 0..2 {
        let r = internal(&h, &ms, Some(delegated(&c)), ERASE, body.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        common::contracts::check_openapi_component("AccountCallHistoryErasureResult", &r.json);
        assert_eq!(r.json["complete"], true);
        assert_eq!(r.json["pendingLiveCalls"], 0);
        assert_eq!(r.json["historyEntriesHidden"], 0);
    }
    let r = internal(&h, &ms, Some(delegated(&c)), EXPORT, body).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check_openapi_component("AccountCallHistoryExport", &r.json);
    assert_eq!(r.json["calls"], json!([]));
    assert_eq!(r.json["truncated"], false);
}
