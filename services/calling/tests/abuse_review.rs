//! C21: operator review of abuse reports over internal service authentication. Only the
//! operator console holding `calling-abuse-reports.review` may list the queue (across
//! scopes, oldest first, keyset-paginated, filtered) or resolve a report; a resolution
//! is the same audited transition as `platform_admin abuse-report set-status`.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::{app::AppWorld, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

async fn internal(
    w: &AppWorld,
    signer: &ServiceTokenSigner,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> TestResponse {
    let signed = signer
        .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
        .unwrap();
    let headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    w.h().request_with(method, path, None, body, &refs).await
}

async fn console(w: &AppWorld, method: Method, path: &str, body: Option<Value>) -> TestResponse {
    let signer = w.h().peers.operator_console.clone();
    internal(w, &signer, method, path, body).await
}

/// A connected call reported by its initiator; returns the report id.
async fn reported_call(w: &AppWorld, category: &str) -> Uuid {
    let (call, _, _) = w.connected_call("VOICE").await;
    let r = w
        .post(
            &format!("/v1/calls/{call}/abuse-reports"),
            json!({ "principalId": w.w.initiator.id(), "category": category }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    r.json["abuseReportId"].as_str().unwrap().parse().unwrap()
}

fn ids(r: &TestResponse) -> Vec<String> {
    r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["abuseReportId"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn the_operator_console_lists_and_resolves_reports_with_an_audit() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let first = reported_call(&w, "SPAM").await;
    w.h().clock.advance(chrono::Duration::seconds(5));
    let second = reported_call(&w, "HARASSMENT").await;
    w.h().clock.advance(chrono::Duration::seconds(5));
    let third = reported_call(&w, "OTHER").await;

    // The open queue, oldest first, with the owning scope; pages follow the cursor.
    let r = console(&w, Method::GET, "/internal/v1/abuse-reports?limit=2", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers["cache-control"], "no-store");
    common::contracts::check_openapi_component("OperatorAbuseReportPage", &r.json);
    assert_eq!(ids(&r), [first.to_string(), second.to_string()]);
    let row = &r.json["data"][0];
    assert_eq!(row["status"], "OPEN");
    assert_eq!(row["category"], "SPAM");
    assert_eq!(
        row["environmentId"],
        w.w.consumer.env(w.w.kind).id.to_string()
    );
    let cursor = r.json["nextCursor"].as_str().unwrap().to_string();
    let r = console(
        &w,
        Method::GET,
        &format!("/internal/v1/abuse-reports?limit=2&cursor={cursor}"),
        None,
    )
    .await;
    assert_eq!(ids(&r), [third.to_string()]);
    assert!(r.json["nextCursor"].is_null());

    // Resolve: REVIEWED leaves the open queue, is audited, and can be reopened.
    let r = console(
        &w,
        Method::POST,
        &format!("/internal/v1/abuse-reports/{second}/resolve"),
        Some(json!({ "status": "REVIEWED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check_openapi_component("OperatorAbuseReport", &r.json);
    assert_eq!(r.json["status"], "REVIEWED");
    assert!(r.json["reviewedAt"].is_string());
    let audited =
        w.w.count(&format!(
            "SELECT count(*) FROM platform_operator_audit WHERE action = 'ABUSE_REPORT_STATUS_SET' \
             AND target_id = '{second}' AND actor = 'cfmeet.operator-console'"
        ))
        .await;
    assert_eq!(audited, 1);
    let r = console(&w, Method::GET, "/internal/v1/abuse-reports", None).await;
    assert_eq!(ids(&r), [first.to_string(), third.to_string()]);
    let r = console(
        &w,
        Method::GET,
        "/internal/v1/abuse-reports?status=REVIEWED",
        None,
    )
    .await;
    assert_eq!(ids(&r), [second.to_string()]);
    let r = console(
        &w,
        Method::POST,
        &format!("/internal/v1/abuse-reports/{second}/resolve"),
        Some(json!({ "status": "OPEN" })),
    )
    .await;
    assert_eq!(r.json["status"], "OPEN");
    assert!(r.json["reviewedAt"].is_null());

    // Filters, validation and unknown reports.
    let r = console(
        &w,
        Method::GET,
        &format!(
            "/internal/v1/abuse-reports?environmentId={}",
            Uuid::new_v4()
        ),
        None,
    )
    .await;
    assert!(ids(&r).is_empty());
    for bad in ["?status=CLOSED", "?limit=0", "?cursor=nope", "?x=1"] {
        let r = console(
            &w,
            Method::GET,
            &format!("/internal/v1/abuse-reports{bad}"),
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
    let r = console(
        &w,
        Method::POST,
        &format!("/internal/v1/abuse-reports/{first}/resolve"),
        Some(json!({ "status": "ESCALATED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = console(
        &w,
        Method::POST,
        &format!("/internal/v1/abuse-reports/{}/resolve", Uuid::new_v4()),
        Some(json!({ "status": "DISMISSED" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn only_the_operator_console_with_the_permission_reviews() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let report = reported_call(&w, "SPAM").await;
    let peers = w.h().peers.clone();
    for signer in [
        &peers.developer_platform,
        &peers.communications,
        &peers.api_gateway,
    ] {
        let r = internal(&w, signer, Method::GET, "/internal/v1/abuse-reports", None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
        let r = internal(
            &w,
            signer,
            Method::POST,
            &format!("/internal/v1/abuse-reports/{report}/resolve"),
            Some(json!({ "status": "DISMISSED" })),
        )
        .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    }
    let r = internal(
        &w,
        &peers.untrusted,
        Method::GET,
        "/internal/v1/abuse-reports",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // A platform API credential never reaches internal routes.
    let r = w
        .h()
        .request(
            Method::GET,
            "/internal/v1/abuse-reports",
            Some(w.secret()),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    // Nothing changed.
    assert_eq!(
        w.w.count("SELECT count(*) FROM calling_abuse_reports WHERE status = 'OPEN'")
            .await,
        1
    );
}
