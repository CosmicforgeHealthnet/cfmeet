//! ADR 0068 — the admin bridge's operator reads of partner resources and the operator
//! attribution rule, through the REAL router (CI: real PostgreSQL).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::config::FeatureFlags;
use serde_json::{json, Value};
use uuid::Uuid;

const HQ: (&str, &str) = ("x-cf-operator", "hq:staff-0001");

async fn org(c: &Control, token: &str, name: &str) -> Uuid {
    let r = c
        .req(
            As::User(token),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": name })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    Uuid::parse_str(r.json["id"].as_str().unwrap()).unwrap()
}

fn reads(o: Uuid) -> Vec<String> {
    vec![
        format!("/internal/v1/operator/organizations/{o}/projects"),
        format!("/internal/v1/operator/organizations/{o}/environments"),
        format!("/internal/v1/operator/organizations/{o}/members"),
        format!("/internal/v1/operator/organizations/{o}/invitations"),
        format!("/internal/v1/operator/organizations/{o}/push-providers"),
        format!("/internal/v1/operator/organizations/{o}/webhooks"),
        format!("/internal/v1/operator/organizations/{o}/webhook-deliveries"),
    ]
}

#[tokio::test]
async fn the_bridge_reads_partner_resources_for_a_named_hq_operator() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let acme = org(&c, &alice, "Acme").await;
    for path in reads(acme) {
        let r = c.bridge(&path, &[HQ]).await;
        assert_eq!(r.status, StatusCode::OK, "{path}: {}", r.json);
        assert!(r.json["items"].is_array(), "{path}");
        assert!(r.json.get("nextCursor").is_some(), "{path}");
        assert!(
            !r.json.to_string().contains("acme.example"),
            "{path}: no address ever leaves through an operator read"
        );
    }
    let members = c
        .bridge(
            &format!("/internal/v1/operator/organizations/{acme}/members"),
            &[HQ],
        )
        .await;
    let first = &members.json["items"][0];
    assert_eq!(first["role"], json!("OWNER"));
    assert!(Uuid::parse_str(first["userId"].as_str().unwrap()).is_ok());
    // The existing operator reads accept the bridge's attribution too.
    let list = c
        .bridge("/internal/v1/operator/organizations?query=acme", &[HQ])
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    assert_eq!(list.json["items"][0]["id"], json!(acme));
    // Unknown workspace: 404.
    let missing = c
        .bridge(
            &format!(
                "/internal/v1/operator/organizations/{}/projects",
                Uuid::new_v4()
            ),
            &[HQ],
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.json);
}

#[tokio::test]
async fn windows_and_cursors_are_validated() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let acme = org(&c, &alice, "Acme").await;
    let deliveries = format!("/internal/v1/operator/organizations/{acme}/webhook-deliveries");
    let ok = c
        .bridge(
            &format!("{deliveries}?since=2026-11-01T00:00:00Z&until=2026-11-20T00:00:00Z"),
            &[HQ],
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.json);
    for bad in [
        format!("{deliveries}?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z"),
        format!("{deliveries}?since=2026-11-20T00:00:00Z&until=2026-11-01T00:00:00Z"),
        format!("{deliveries}?since=yesterday"),
        format!("{deliveries}?cursor=not-a-cursor"),
        format!("{deliveries}?limit=0"),
        format!("{deliveries}?unknown=1"),
        "/internal/v1/operator/activity?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z"
            .to_string(),
    ] {
        let r = c.bridge(&bad, &[HQ]).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}: {}", r.json);
    }
    let windowed = c
        .bridge(
            &format!(
                "/internal/v1/operator/activity?organizationId={acme}&since=2026-11-01T00:00:00Z&until=2030-11-20T00:00:00Z"
            ),
            &[HQ],
        )
        .await;
    assert_eq!(
        windowed.status,
        StatusCode::BAD_REQUEST,
        "window over 31 days"
    );
    let recent = c
        .bridge(
            &format!("/internal/v1/operator/activity?organizationId={acme}"),
            &[HQ],
        )
        .await;
    assert_eq!(recent.status, StatusCode::OK, "{}", recent.json);
}

#[tokio::test]
async fn app_reads_search_and_answer_404_for_unknown_apps() {
    let Some(c) = Control::new().await else {
        return;
    };
    let search = c
        .bridge("/internal/v1/operator/app-search?q=chat%25_bot", &[HQ])
        .await;
    assert_eq!(search.status, StatusCode::OK, "{}", search.json);
    assert_eq!(search.json, json!({ "items": [], "nextCursor": null }));
    let app = Uuid::new_v4();
    for path in [
        format!("/internal/v1/operator/apps/{app}/versions"),
        format!("/internal/v1/operator/apps/{app}/installations"),
    ] {
        let r = c.bridge(&path, &[HQ]).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}: {}", r.json);
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

#[tokio::test]
async fn operator_attribution_is_accepted_only_from_the_bridge_and_the_console() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags {
        gateway_api: true,
        ..Default::default()
    })
    .await
    else {
        return;
    };
    let (alice, _) = c.sessions.sign_in("alice@acme.example");
    let acme = org(&c, &alice, "Acme").await;
    let members = format!("/internal/v1/operator/organizations/{acme}/members");
    // The bridge must name a well-formed HQ operator on every request.
    for attribution in [
        vec![],
        vec![("x-cf-operator", "staff-0001")],
        vec![
            HQ,
            ("x-cf-approver", "hq:staff-0001"),
            ("x-cf-approval-ref", "a-1"),
            (
                "x-cf-intent-digest",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        ],
        vec![HQ, ("x-cf-approver", "hq:staff-0002")],
    ] {
        let r = c.bridge(&members, &attribution).await;
        assert_eq!(
            r.status,
            StatusCode::FORBIDDEN,
            "{attribution:?}: {}",
            r.json
        );
        assert_eq!(code(&r.json), "INTERNAL_PERMISSION_DENIED");
    }
    // Any other caller is refused as soon as it sends an attribution header, on any route
    // (an unknown path answers 404 without one).
    let unknown = "/internal/v1/no-such-route";
    let plain = c.signed_with(&producers.calling, unknown, &[]).await;
    assert_eq!(plain.status, StatusCode::NOT_FOUND, "{}", plain.json);
    for header in [
        "x-cf-operator",
        "x-cf-approver",
        "x-cf-approval-ref",
        "x-cf-intent-digest",
    ] {
        let r = c
            .signed_with(&producers.calling, unknown, &[(header, "hq:staff-0001")])
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{header}: {}", r.json);
        assert_eq!(code(&r.json), "INTERNAL_PERMISSION_DENIED");
    }
    // The operator console keeps its legacy named operator, but never an approval.
    let console = c
        .operator(Method::GET, "/internal/v1/operator/organizations", None)
        .await;
    assert_eq!(console.status, StatusCode::OK, "{}", console.json);
    let approval = c
        .signed_with(
            &c.operator,
            "/internal/v1/operator/organizations",
            &[
                ("x-cf-operator", "ops.tester"),
                ("x-cf-approver", "hq:staff-0002"),
            ],
        )
        .await;
    assert_eq!(approval.status, StatusCode::FORBIDDEN, "{}", approval.json);
}
