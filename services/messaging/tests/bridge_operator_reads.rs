//! The admin bridge's operator reads (ADR 0068, design §6.3): `/internal/v1/operator/*`
//! answer only `cfmeet.admin-bridge` holding `messaging-firstparty.administer` with a named
//! HQ operator; operator attribution from any other caller is refused on every internal
//! route; nothing beyond ids, categories, states, counts and times is returned.

mod common;

use axum::http::{Method, StatusCode};
use chrono::Utc;
use common::*;
use serde_json::json;
use uuid::Uuid;

const OP: (&str, &str) = ("x-cf-operator", "hq:staff-0001");

const ROUTES: &[&str] = &[
    "/internal/v1/operator/reports",
    "/internal/v1/operator/holds",
    "/internal/v1/operator/exports",
    "/internal/v1/operator/deletion-requests",
    "/internal/v1/operator/audit",
    "/internal/v1/operator/switches",
    "/internal/v1/operator/incidents",
    "/internal/v1/operator/failed-messages",
    "/internal/v1/operator/key-transparency",
];

async fn exec(h: &Harness, sql: &str, values: Vec<sea_orm::Value>) {
    messaging::db::exec(&h.state.db, sql, values).await.unwrap();
}

async fn seed(h: &Harness) -> (Uuid, Uuid) {
    let (tenant, app, env) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let now = Utc::now();
    let hold = Uuid::new_v4();
    exec(
        h,
        "INSERT INTO messaging_legal_holds (id, tenant_id, application_id, environment_id, scope_kind, reason_category, \
         case_reference, created_by_credential_id, created_at) VALUES ($1, $2, $3, $4, 'ENVIRONMENT', 'LITIGATION', \
         'CASE-SECRET-42', $5, $6)",
        vec![hold.into(), tenant.into(), app.into(), env.into(), Uuid::new_v4().into(), now.into()],
    )
    .await;
    for i in 0..3 {
        exec(
            h,
            "INSERT INTO messaging_deletion_requests (id, tenant_id, application_id, environment_id, kind, principal_id, \
             requested_by_credential_id, created_at) VALUES ($1, $2, $3, $4, 'PRINCIPAL_ERASURE', $5, $6, $7)",
            vec![
                Uuid::new_v4().into(),
                tenant.into(),
                app.into(),
                env.into(),
                Uuid::new_v4().into(),
                Uuid::new_v4().into(),
                (now - chrono::Duration::seconds(i)).into(),
            ],
        )
        .await;
    }
    exec(
        h,
        "INSERT INTO messaging_incidents (id, category, severity, dedupe_key, summary, first_seen_at, last_seen_at) \
         VALUES ($1, 'OUTBOX_DEAD_LETTER', 'HIGH', 'OUTBOX_DEAD_LETTER:x', 'summary-never-relayed', $2, $2)",
        vec![Uuid::new_v4().into(), now.into()],
    )
    .await;
    exec(
        h,
        "INSERT INTO messaging_audit (id, occurred_at, tenant_id, application_id, environment_id, action, target_kind, \
         target_id, correlation_id, detail) VALUES ($1, $2, $3, $4, $5, 'REPORT_CREATED', 'REPORT', $6, 'corr_12345678', \
         '{\"note\": \"detail-never-relayed\"}'::jsonb)",
        vec![Uuid::new_v4().into(), now.into(), tenant.into(), app.into(), env.into(), Uuid::new_v4().into()],
    )
    .await;
    (tenant, hold)
}

#[tokio::test]
async fn the_bridge_reads_every_operator_route_for_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    let (tenant, hold) = seed(&h).await;
    for route in ROUTES {
        let r = h
            .call_internal(&peers.admin_bridge, Method::GET, route, None, None, &[OP])
            .await;
        assert_eq!(r.status, StatusCode::OK, "{route}: {}", r.text);
    }
    let holds = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            "/internal/v1/operator/holds",
            None,
            None,
            &[OP],
        )
        .await;
    let item = holds.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == json!(hold))
        .cloned()
        .unwrap();
    assert_eq!(item["tenantId"], json!(tenant));
    assert_eq!(item["reasonCategory"], "LITIGATION");
    assert!(
        !holds.text.contains("CASE-SECRET"),
        "the case reference is never returned"
    );
    let incidents = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            "/internal/v1/operator/incidents",
            None,
            None,
            &[OP],
        )
        .await;
    assert!(!incidents.json["items"].as_array().unwrap().is_empty());
    assert!(!incidents.text.contains("summary-never-relayed"));
    let audit = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            "/internal/v1/operator/audit",
            None,
            None,
            &[OP],
        )
        .await;
    assert!(!audit.text.contains("detail-never-relayed"));
    let kt = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            "/internal/v1/operator/key-transparency",
            None,
            None,
            &[OP],
        )
        .await;
    assert!(kt.json["witnesses"].is_array());
    let missing = format!("/internal/v1/operator/reports/{}", Uuid::new_v4());
    let r = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            &missing,
            None,
            None,
            &[OP],
        )
        .await;
    assert_eq!((r.status, r.code()), (StatusCode::NOT_FOUND, "NOT_FOUND"));
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    let report = format!("/internal/v1/operator/reports/{}", Uuid::new_v4());
    for route in ROUTES
        .iter()
        .copied()
        .chain(std::iter::once(report.as_str()))
    {
        // The Meeting Service does not hold messaging-firstparty.administer.
        let r = h
            .call_internal(&peers.meeting, Method::GET, route, None, None, &[])
            .await;
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
        // The bridge without X-CF-Operator.
        let r = h
            .call_internal(&peers.admin_bridge, Method::GET, route, None, None, &[])
            .await;
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
        // A malformed operator reference.
        let r = h
            .call_internal(
                &peers.admin_bridge,
                Method::GET,
                route,
                None,
                None,
                &[("x-cf-operator", "staff-0001")],
            )
            .await;
        assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED", "{route}");
    }
}

#[tokio::test]
async fn attribution_from_any_other_caller_is_refused_on_every_internal_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    // Without attribution an unknown internal path is a plain 404 ...
    let r = h
        .call_internal(
            &peers.meeting,
            Method::GET,
            "/internal/v1/no-such-route",
            None,
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // ... with it, the middleware refuses before any handler (never silently ignored).
    for header in ["x-cf-operator", "x-cf-approver"] {
        let r = h
            .call_internal(
                &peers.meeting,
                Method::GET,
                "/internal/v1/no-such-route",
                None,
                None,
                &[(header, "hq:staff-0001")],
            )
            .await;
        assert_eq!(
            (r.status, r.code()),
            (StatusCode::FORBIDDEN, "INTERNAL_PERMISSION_DENIED"),
            "{header}"
        );
    }
    // The bridge's own self-approval is refused too.
    let r = h
        .call_internal(
            &peers.admin_bridge,
            Method::GET,
            "/internal/v1/operator/switches",
            None,
            None,
            &[
                OP,
                ("x-cf-approver", "hq:staff-0001"),
                ("x-cf-approval-ref", "appr-1"),
                (
                    "x-cf-intent-digest",
                    "aa11bb22cc33dd44ee55ff6600778899aa11bb22cc33dd44ee55ff6600778899",
                ),
            ],
        )
        .await;
    assert_eq!(r.code(), "INTERNAL_PERMISSION_DENIED");
}

#[tokio::test]
async fn pages_windows_and_cursors_are_validated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let peers = peers();
    seed(&h).await;
    let get = |path: String| {
        let peers = &peers;
        let h = &h;
        async move {
            h.call_internal(&peers.admin_bridge, Method::GET, &path, None, None, &[OP])
                .await
        }
    };
    // keyset paging over the three deletion requests
    let first = get("/internal/v1/operator/deletion-requests?limit=2".into()).await;
    assert_eq!(first.json["items"].as_array().unwrap().len(), 2);
    let cursor = first.json["nextCursor"].as_str().unwrap().to_string();
    let second = get(format!(
        "/internal/v1/operator/deletion-requests?limit=2&cursor={cursor}"
    ))
    .await;
    assert_eq!(second.json["items"].as_array().unwrap().len(), 1);
    assert!(second.json["nextCursor"].is_null());
    for bad in [
        "/internal/v1/operator/audit?since=2026-11-20T00:00:00Z&until=2026-11-01T00:00:00Z",
        "/internal/v1/operator/audit?since=2026-09-01T00:00:00Z&until=2026-11-20T00:00:00Z",
        "/internal/v1/operator/incidents?since=yesterday",
        "/internal/v1/operator/reports?limit=0",
        "/internal/v1/operator/reports?limit=101",
        "/internal/v1/operator/reports?cursor=abc",
        "/internal/v1/operator/reports?status=GONE",
        "/internal/v1/operator/holds?status=DELETED",
        "/internal/v1/operator/exports?since=2026-11-01T00:00:00Z",
        "/internal/v1/operator/reports?unknown=1",
    ] {
        let r = get(bad.into()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.text);
    }
}
