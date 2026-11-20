//! ADR 0067: the admin bridge's operator reads of the Communications Hub.
//!
//! * `GET /internal/v1/communications/operator/profiles/{cfMeetId}`
//! * `GET /internal/v1/communications/operator/reports`
//! * `GET /internal/v1/communications/operator/holds`
//! * `GET /internal/v1/communications/operator/audit`
//! * `GET /internal/v1/communications/operator/metrics`
//!
//! Each requires `communications.administer` (admin bridge only) AND a named operator
//! (`X-CF-Operator: hq:<staff>`). Attribution headers from any other caller are refused on
//! every route.
mod common;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::*;
use communications::store::execute;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

const REPORTS: &str = "/internal/v1/communications/operator/reports";
const HOLDS: &str = "/internal/v1/communications/operator/holds";
const AUDIT: &str = "/internal/v1/communications/operator/audit";
const METRICS: &str = "/internal/v1/communications/operator/metrics";

fn profile_path(cf: &str) -> String {
    format!("/internal/v1/communications/operator/profiles/{cf}")
}

fn signed(signer: &ServiceTokenSigner, operator: Option<&str>) -> HeaderMap {
    let request = OutboundServiceRequest::new_operation(ServiceId::communications());
    let mut h = signer.sign(&request).unwrap().headers();
    if let Some(op) = operator {
        h.insert("x-cf-operator", HeaderValue::from_str(op).unwrap());
    }
    h
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
    let bytes = axum::body::to_bytes(response.into_body(), 262_144)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn bridge(h: &Harness, path: &str) -> (StatusCode, Value) {
    get(h, path, signed(&h.bridge, Some("hq:staff-0001"))).await
}

/// Two profiles, a report between them and an operator hold on the target.
async fn fixture(h: &Harness) -> (String, String) {
    let a = h.account();
    let b = h.account();
    let pa = h.profile(&a).await;
    let pb = h.profile(&b).await;
    execute(
        &h.state.store.db,
        "INSERT INTO communication_profile_reports(id,reporter_profile_id,target_profile_id,reason) VALUES($1,$2,$3,'SPAM')",
        vec![Uuid::new_v4().into(), pa.id.into(), pb.id.into()],
    )
    .await
    .unwrap();
    execute(
        &h.state.store.db,
        "INSERT INTO communication_operator_holds(profile_id,reason,active) VALUES($1,'ABUSE',true)",
        vec![pb.id.into()],
    )
    .await
    .unwrap();
    (h.public_id(&pa).await, h.public_id(&pb).await)
}

#[tokio::test]
async fn the_bridge_reads_profiles_reports_holds_audit_and_metrics() {
    let h = Harness::new().await;
    let (reporter, target) = fixture(&h).await;

    let (status, p) = bridge(&h, &profile_path(&target)).await;
    assert_eq!(status, StatusCode::OK, "{p}");
    assert_eq!(p["cfMeetId"], json!(target));
    assert!(p["displayName"].is_string());
    assert_eq!(p["hold"]["reason"], "ABUSE");
    let text = p.to_string();
    for forbidden in ["accountReference", "principalId", "phone", "email"] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} must never be relayed"
        );
    }
    // case-insensitive, hyphen optional (what a person types)
    let (status, _) = bridge(&h, &profile_path(&target.to_lowercase().replace('-', ""))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = bridge(&h, &profile_path("CF-ZZZZZZ")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, r) = bridge(&h, REPORTS).await;
    assert_eq!(status, StatusCode::OK, "{r}");
    let item = &r["items"][0];
    assert_eq!(item["reason"], "SPAM");
    assert_eq!(item["reporterCfMeetId"], json!(reporter));
    assert_eq!(item["targetCfMeetId"], json!(target));
    assert!(item["review"].is_null());

    let (status, holds) = bridge(&h, HOLDS).await;
    assert_eq!(status, StatusCode::OK, "{holds}");
    assert_eq!(holds["items"][0]["cfMeetId"], json!(target));
    assert!(holds["items"][0].get("profileId").is_none());

    let (status, audit) = bridge(&h, AUDIT).await;
    assert_eq!(status, StatusCode::OK, "{audit}");
    let seqs: Vec<i64> = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[0] > w[1]), "newest first");
    assert!(audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e.get("details").is_none()));

    let (status, m) = bridge(&h, METRICS).await;
    assert_eq!(status, StatusCode::OK, "{m}");
    assert!(m["profiles"].as_i64().unwrap() >= 2);
    assert_eq!(m["operatorHolds"], 1);
    assert_eq!(m["reportsOpen"], 1);
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_a_named_operator() {
    let h = Harness::new().await;
    let (_, target) = fixture(&h).await;
    for path in [
        profile_path(&target),
        REPORTS.into(),
        HOLDS.into(),
        AUDIT.into(),
        METRICS.into(),
    ] {
        // The operator console holds no communications.administer.
        let (status, _) = get(&h, &path, signed(&h.operator, None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        // The bridge without X-CF-Operator.
        let (status, _) = get(&h, &path, signed(&h.bridge, None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        // A malformed operator reference.
        let (status, _) = get(&h, &path, signed(&h.bridge, Some("staff-0001"))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn attribution_from_any_other_caller_is_refused_on_every_route() {
    let h = Harness::new().await;
    // The operator console's own metrics route, with attribution it may not send.
    let mut headers = signed(&h.operator, Some("hq:staff-0001"));
    headers.insert("x-cf-operator", HeaderValue::from_static("hq:staff-0001"));
    let (status, _) = get(&h, "/internal/v1/communications/metrics", headers).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The same route without attribution still works for the operator console.
    let (status, _) = get(
        &h,
        "/internal/v1/communications/metrics",
        signed(&h.operator, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn pages_and_windows_are_validated() {
    let h = Harness::new().await;
    fixture(&h).await;
    for bad in [
        format!("{REPORTS}?status=MAYBE"),
        format!("{REPORTS}?limit=0"),
        format!("{REPORTS}?cursor=nope"),
        format!("{HOLDS}?limit=101"),
        format!("{AUDIT}?cursor=0"),
        format!("{AUDIT}?since=2026-11-10T00:00:00Z&until=2026-11-01T00:00:00Z"),
        format!("{AUDIT}?since=2026-09-01T00:00:00Z&until=2026-11-01T00:00:00Z"),
        format!("{AUDIT}?since=yesterday"),
        format!("{AUDIT}?unknown=1"),
    ] {
        let (status, body) = bridge(&h, &bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    // keyset paging over reports: one per page, then the end
    let (_, first) = bridge(&h, &format!("{REPORTS}?limit=1")).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    assert!(
        first["nextCursor"].is_null(),
        "a single report: no next page"
    );
    // released holds are listed separately
    let (_, released) = bridge(&h, &format!("{HOLDS}?active=false")).await;
    assert!(released["items"].as_array().unwrap().is_empty());
}
