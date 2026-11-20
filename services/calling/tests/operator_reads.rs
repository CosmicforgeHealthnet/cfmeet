//! ADR 0068: the admin bridge's tenant-scoped operator reads (`/internal/v1/operator/*`).
//! Only `cfmeet.admin-bridge` holding `calling.administer` and naming an HQ operator
//! (`X-CF-Operator`) may read; operator attribution from any other caller is refused on
//! every internal route; no read crosses tenants; nothing personal beyond opaque ids.

mod common;

use axum::http::{Method, StatusCode};
use cf_service_auth::{OutboundServiceRequest, ServiceId, ServiceTokenSigner};
use common::{app::AppWorld, TestResponse};
use serde_json::{json, Value};
use uuid::Uuid;

const OPERATOR: &str = "hq:staff-0001";

async fn internal(
    w: &AppWorld,
    signer: &ServiceTokenSigner,
    path: &str,
    extra: &[(&str, &str)],
) -> TestResponse {
    let signed = signer
        .sign(&OutboundServiceRequest::new_operation(ServiceId::calling()))
        .unwrap();
    let mut headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    headers.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    w.h()
        .request_with(Method::GET, path, None, None, &refs)
        .await
}

async fn bridge(w: &AppWorld, path: &str) -> TestResponse {
    let signer = w.h().peers.admin_bridge.clone();
    internal(w, &signer, path, &[("x-cf-operator", OPERATOR)]).await
}

fn tenant(w: &AppWorld) -> Uuid {
    w.w.consumer.tenant.id.as_uuid()
}

fn items(r: &TestResponse) -> Vec<Value> {
    r.json["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{}", r.text))
        .clone()
}

/// Every operator read path, for the tenant (paged and windowed ones without extras).
fn reads(t: Uuid) -> Vec<String> {
    vec![
        format!("/internal/v1/operator/calls?tenantId={t}"),
        format!("/internal/v1/operator/live-calls?tenantId={t}"),
        format!("/internal/v1/operator/abuse-reports?tenantId={t}"),
        format!("/internal/v1/operator/blocks?tenantId={t}"),
        format!("/internal/v1/operator/limits?tenantId={t}"),
        format!("/internal/v1/operator/routing-policies?tenantId={t}"),
        format!("/internal/v1/operator/enterprise-policies?tenantId={t}"),
        format!("/internal/v1/operator/spend-policies?tenantId={t}"),
        format!("/internal/v1/operator/recordings?tenantId={t}"),
        format!("/internal/v1/operator/voicemail?tenantId={t}"),
        format!("/internal/v1/operator/retention-holds?tenantId={t}"),
        format!("/internal/v1/operator/incidents?tenantId={t}"),
        format!("/internal/v1/operator/webhook-keys?tenantId={t}"),
        format!("/internal/v1/operator/audit?tenantId={t}"),
    ]
}

#[tokio::test]
async fn the_bridge_reads_the_tenants_calls_without_personal_detail() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (call, _, _) = w.connected_call("VOICE").await;
    let t = tenant(&w);

    let page = bridge(&w, &format!("/internal/v1/operator/calls?tenantId={t}")).await;
    assert_eq!(page.status, StatusCode::OK, "{}", page.text);
    let calls = items(&page);
    assert!(calls.iter().any(|c| c["callId"] == json!(call.to_string())));
    let live = bridge(
        &w,
        &format!("/internal/v1/operator/live-calls?tenantId={t}"),
    )
    .await;
    assert!(
        items(&live)
            .iter()
            .any(|c| c["callId"] == json!(call.to_string())),
        "a connected call is live"
    );

    let detail = bridge(
        &w,
        &format!("/internal/v1/operator/calls/{call}?tenantId={t}"),
    )
    .await;
    assert_eq!(detail.status, StatusCode::OK, "{}", detail.text);
    let participants = detail.json["participants"].as_array().unwrap();
    assert!(!participants.is_empty());
    for p in participants {
        assert!(p.get("displayName").is_none() && p.get("display_name").is_none());
    }
    let text = detail.text.to_lowercase();
    for banned in ["sip:", "tel:", "+234", "displayname"] {
        assert!(!text.contains(banned), "{banned} must never appear");
    }

    // Another tenant reaches nothing: an empty page, and the call itself is 404.
    let other = Uuid::new_v4();
    let none = bridge(&w, &format!("/internal/v1/operator/calls?tenantId={other}")).await;
    assert!(items(&none).is_empty());
    let hidden = bridge(
        &w,
        &format!("/internal/v1/operator/calls/{call}?tenantId={other}"),
    )
    .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn every_read_answers_for_the_tenant_and_validates_its_query() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let t = tenant(&w);
    for path in reads(t) {
        let r = bridge(&w, &path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.text);
    }
    let limits = bridge(&w, &format!("/internal/v1/operator/limits?tenantId={t}")).await;
    assert_eq!(limits.json["tenantId"], json!(t.to_string()));
    assert!(limits.json["currency"]
        .as_str()
        .is_some_and(|c| c.len() == 3));
    let unknown = bridge(
        &w,
        &format!("/internal/v1/operator/limits?tenantId={}", Uuid::new_v4()),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);

    for bad in [
        "/internal/v1/operator/calls".to_string(),
        "/internal/v1/operator/calls?tenantId=acme".to_string(),
        format!("/internal/v1/operator/calls?tenantId={t}&cursor=nope"),
        format!("/internal/v1/operator/calls?tenantId={t}&limit=0"),
        format!("/internal/v1/operator/calls?tenantId={t}&since=2026-01-01T00:00:00Z&until=2026-03-01T00:00:00Z"),
        format!("/internal/v1/operator/limits?tenantId={t}&limit=5"),
        format!("/internal/v1/operator/blocks?tenantId={t}&since=2026-01-01T00:00:00Z"),
        format!("/internal/v1/operator/audit?tenantId={t}&tenantId={t}"),
    ] {
        let r = bridge(&w, &bad).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
    let bad_call = bridge(
        &w,
        &format!("/internal/v1/operator/calls/not-a-uuid?tenantId={t}"),
    )
    .await;
    assert_eq!(bad_call.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_a_named_operator() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let t = tenant(&w);
    for path in reads(t) {
        // Without calling.administer (the operator console holds abuse review only).
        let console = w.h().peers.operator_console.clone();
        let r = internal(&w, &console, &path, &[]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        // The bridge without X-CF-Operator.
        let b = w.h().peers.admin_bridge.clone();
        let r = internal(&w, &b, &path, &[]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
        // A malformed operator reference.
        let r = internal(&w, &b, &path, &[("x-cf-operator", "staff-0001")]).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
}

#[tokio::test]
async fn attribution_from_any_other_caller_is_refused_on_every_route() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    // The operator console may list the review queue, but never with attribution headers.
    let console = w.h().peers.operator_console.clone();
    let ok = internal(&w, &console, "/internal/v1/abuse-reports", &[]).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text);
    let refused = internal(
        &w,
        &console,
        "/internal/v1/abuse-reports",
        &[("x-cf-operator", OPERATOR)],
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    // The bridge's own approval headers are all-or-nothing and never self-approved.
    let b = w.h().peers.admin_bridge.clone();
    let path = format!("/internal/v1/operator/calls?tenantId={}", tenant(&w));
    let incomplete = internal(
        &w,
        &b,
        &path,
        &[
            ("x-cf-operator", OPERATOR),
            ("x-cf-approver", "hq:staff-0002"),
        ],
    )
    .await;
    assert_eq!(incomplete.status, StatusCode::FORBIDDEN);
    let digest = "a".repeat(64);
    let selfie = internal(
        &w,
        &b,
        &path,
        &[
            ("x-cf-operator", OPERATOR),
            ("x-cf-approver", OPERATOR),
            ("x-cf-approval-ref", "ap-1"),
            ("x-cf-intent-digest", digest.as_str()),
        ],
    )
    .await;
    assert_eq!(selfie.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn pages_follow_the_keyset_cursor_newest_first() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    for _ in 0..3 {
        w.connected_call("VOICE").await;
    }
    let t = tenant(&w);
    let first = bridge(
        &w,
        &format!("/internal/v1/operator/calls?tenantId={t}&limit=2"),
    )
    .await;
    let a = items(&first);
    assert_eq!(a.len(), 2);
    let cursor = first.json["nextCursor"]
        .as_str()
        .expect("a next page")
        .to_string();
    let second = bridge(
        &w,
        &format!("/internal/v1/operator/calls?tenantId={t}&limit=2&cursor={cursor}"),
    )
    .await;
    let b = items(&second);
    assert!(!b.is_empty());
    let created: Vec<&str> = a
        .iter()
        .chain(b.iter())
        .map(|c| c["createdAt"].as_str().unwrap())
        .collect();
    let mut sorted = created.clone();
    sorted.sort_by(|x, y| y.cmp(x));
    assert_eq!(created, sorted, "newest first across pages");
    let ids: std::collections::HashSet<&str> = a
        .iter()
        .chain(b.iter())
        .map(|c| c["callId"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), a.len() + b.len(), "no call twice");
}
