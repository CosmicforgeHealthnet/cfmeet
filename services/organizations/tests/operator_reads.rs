//! ADR 0067 / design §7: the admin bridge's operator reads under `/internal/v1/operator/*`
//! (members, teams, policies, cross-organization audit) and operator attribution
//! (`X-CF-Operator` …) accepted only from `cfmeet.admin-bridge`, on every route.

mod common;

use common::{s, Harness};
use serde_json::json;
use uuid::Uuid;

fn organization(org: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}")
}
fn members(org: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}/members")
}
fn teams(org: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}/teams")
}
fn policies(org: &str) -> String {
    format!("/internal/v1/operator/organizations/{org}/policies")
}
const AUDIT: &str = "/internal/v1/operator/audit-events";

/// An organization with an owner, two more members and one team.
async fn fixture(h: &Harness, slug: &str) -> (String, Uuid) {
    let owner = Uuid::new_v4();
    let org = s(&h.create_org(owner, slug).await["id"]);
    h.add_member(&org, owner, Uuid::new_v4(), "MEMBER").await;
    h.add_member(&org, owner, Uuid::new_v4(), "ADMIN").await;
    h.user(owner)
        .post(
            &format!("/internal/v1/organizations/{org}/teams"),
            json!({ "name": "Support" }),
        )
        .await
        .expect(201);
    (org, owner)
}

#[tokio::test]
async fn the_bridge_reads_members_teams_policies_and_audit_for_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, _) = fixture(&h, "operator-reads").await;
    let bridge = || h.by(&h.peers.bridge).operator("staff-0001");

    let o = bridge().get(&organization(&org)).await;
    let detail = o.expect(200);
    assert_eq!(detail["id"], json!(org));
    assert!(detail.get("viewer").is_none(), "no acting user, no viewer");

    let m = bridge().get(&members(&org)).await;
    let items = m.expect(200)["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 3);
    for item in &items {
        assert_eq!(item["organizationId"], json!(org));
        assert!(item.get("email").is_none(), "no personal detail beyond ids");
    }

    let t = bridge().get(&teams(&org)).await;
    let teams_list = t.expect(200)["items"].as_array().unwrap().clone();
    assert_eq!(teams_list.len(), 1);
    assert_eq!(teams_list[0]["name"], "Support");
    let archived = bridge()
        .get(&format!("{}?status=ARCHIVED", teams(&org)))
        .await;
    assert!(archived.expect(200)["items"].as_array().unwrap().is_empty());
    bridge()
        .get(&format!("{}?status=DELETED", teams(&org)))
        .await
        .expect_error(400, "VALIDATION_ERROR");

    let p = bridge().get(&policies(&org)).await;
    let doc = p.expect(200);
    assert_eq!(doc["organizationId"], json!(org));
    assert!(doc["policy"].is_object());

    let a = bridge().get(AUDIT).await;
    let events = a.expect(200)["items"].as_array().unwrap().clone();
    assert!(!events.is_empty());
    assert!(events.iter().any(|e| e["organizationId"] == json!(org)));
    let seqs: Vec<i64> = events.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] > w[1]), "newest first");

    for path in [
        organization(&Uuid::new_v4().to_string()),
        members(&Uuid::new_v4().to_string()),
        teams(&Uuid::new_v4().to_string()),
        policies(&Uuid::new_v4().to_string()),
    ] {
        bridge()
            .get(&path)
            .await
            .expect_error(404, "ORGANIZATION_NOT_FOUND");
    }
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, _) = fixture(&h, "operator-gate").await;
    for path in [
        organization(&org),
        members(&org),
        teams(&org),
        policies(&org),
        AUDIT.to_string(),
    ] {
        // Without organization.operator-read (the Meeting Service, no attribution).
        h.by(&h.peers.meeting)
            .get(&path)
            .await
            .expect_error(403, "INTERNAL_PERMISSION_DENIED");
        // The bridge without X-CF-Operator.
        h.by(&h.peers.bridge)
            .get(&path)
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
        // A malformed operator reference.
        h.by(&h.peers.bridge)
            .header("x-cf-operator", "staff-0001")
            .get(&path)
            .await
            .expect_error(403, "ACTOR_NOT_ALLOWED");
    }
}

#[tokio::test]
async fn attribution_from_any_other_caller_is_refused_on_every_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, owner) = fixture(&h, "operator-foreign").await;
    // The Meeting Service reads members as a service today ...
    h.by(&h.peers.meeting)
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect(200);
    // ... but may never claim an HQ operator, on any route.
    h.by(&h.peers.meeting)
        .operator("staff-0001")
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.user(owner)
        .header("x-cf-approver", "hq:staff-0002")
        .get(&format!("/internal/v1/organizations/{org}"))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    // The bridge's own attribution is checked too: incomplete or self approvals.
    h.by(&h.peers.bridge)
        .operator("staff-0001")
        .header("x-cf-approver", "hq:staff-0002")
        .get(&members(&org))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
    h.by(&h.peers.bridge)
        .operator("staff-0001")
        .header("x-cf-approver", "hq:staff-0001")
        .header("x-cf-approval-ref", "approval-1")
        .header(
            "x-cf-intent-digest",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )
        .get(&members(&org))
        .await
        .expect_error(403, "ACTOR_NOT_ALLOWED");
}

#[tokio::test]
async fn member_pages_follow_the_keyset_cursor() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (org, _) = fixture(&h, "operator-pages").await;
    let bridge = || h.by(&h.peers.bridge).operator("staff-0001");
    let first = bridge()
        .get(&format!("{}?limit=2", members(&org)))
        .await
        .expect(200)
        .clone();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = s(&first["nextCursor"]);
    let second = bridge()
        .get(&format!("{}?limit=2&cursor={cursor}", members(&org)))
        .await
        .expect(200)
        .clone();
    let rest = second["items"].as_array().unwrap();
    assert_eq!(rest.len(), 1);
    assert!(second["nextCursor"].is_null());
    let ids: Vec<String> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .chain(rest.iter())
        .map(|m| s(&m["id"]))
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "id order");
    // Same rows as the authoritative service read.
    let service = h
        .by(&h.peers.meeting)
        .get(&format!("/internal/v1/organizations/{org}/members"))
        .await
        .expect(200)["items"]
        .clone();
    assert_eq!(service.as_array().unwrap().len(), ids.len());
}

#[tokio::test]
async fn the_audit_window_is_validated_and_bounds_the_events() {
    let Some(h) = Harness::new().await else {
        return;
    };
    fixture(&h, "operator-audit").await;
    let bridge = || h.by(&h.peers.bridge).operator("staff-0001");
    for (q, why) in [
        (
            "since=2026-01-02T00:00:00Z&until=2026-01-01T00:00:00Z",
            "until before since",
        ),
        (
            "since=2026-01-01T00:00:00Z&until=2026-03-01T00:00:00Z",
            "over 31 days",
        ),
        ("since=yesterday", "not RFC 3339"),
        ("cursor=0", "cursor below 1"),
        ("other=1", "unknown parameter"),
    ] {
        let r = bridge().get(&format!("{AUDIT}?{q}")).await;
        assert_eq!(r.status.as_u16(), 400, "{why}: {:#}", r.json);
    }
    // A window in the past holds nothing; the window ending now holds the fixture's events.
    let past = bridge()
        .get(&format!(
            "{AUDIT}?since=2020-01-01T00:00:00Z&until=2020-01-31T00:00:00Z"
        ))
        .await;
    assert!(past.expect(200)["items"].as_array().unwrap().is_empty());
    let now = chrono::Utc::now();
    let since =
        (now - chrono::Duration::days(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let until =
        (now + chrono::Duration::minutes(5)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let recent = bridge()
        .get(&format!("{AUDIT}?since={since}&until={until}&limit=1"))
        .await
        .expect(200)
        .clone();
    assert_eq!(recent["items"].as_array().unwrap().len(), 1);
    let cursor = recent["nextCursor"].as_i64().expect("more events than one");
    let older = bridge()
        .get(&format!("{AUDIT}?cursor={cursor}&limit=1"))
        .await
        .expect(200)
        .clone();
    assert!(older["items"][0]["seq"].as_i64().unwrap() < cursor);
}

/// No database needed: the registry admits `organization.operator-read` for the bridge.
#[test]
fn the_trust_bundle_admits_the_bridge_with_operator_read_only() {
    let (_verifier, peers) = common::verifier_and_peers();
    assert_eq!(peers.bridge.issuer().as_str(), "cfmeet.admin-bridge");
}
