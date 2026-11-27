//! ADR 0068 A7 — the daily datasets the HQ admin bridge pulls
//! (`GET /internal/v1/operator/datasets/{datasetId}/days/{yyyy-mm-dd}`), through the REAL
//! router (CI: real PostgreSQL): end-of-day counts reconstructed from history, `null`
//! where a metric cannot be computed.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const SYNC: (&str, &str) = ("x-cf-operator", "hq:dataset-sync");
const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-d");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
);

fn day(offset: i64) -> String {
    (chrono::Utc::now().date_naive() + chrono::Duration::days(offset))
        .format("%Y-%m-%d")
        .to_string()
}

#[tokio::test]
async fn partner_workspaces_are_counted_by_their_state_at_the_end_of_each_day() {
    let Some(c) = Control::new().await else {
        return;
    };
    let (owner, _) = c.sessions.sign_in("owner@datasets.example");
    let o = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let org = o.json["id"].as_str().unwrap().to_string();
    // The workspace existed (ACTIVE) for three days before HQ suspends it today.
    c.db.conn
        .execute_unprepared(&format!(
            "ALTER TABLE devplat_organizations DISABLE TRIGGER USER; UPDATE devplat_organizations SET created_at = now() - interval '3 days' WHERE tenant_id = '{org}'; ALTER TABLE devplat_organizations ENABLE TRIGGER USER"
        ))
        .await
        .unwrap();
    let v = c
        .bridge(
            &format!("/internal/v1/operator/organizations/{org}"),
            &[OPERATOR],
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{org}/state"),
            &[
                OPERATOR,
                APPROVER,
                APPROVAL,
                DIGEST,
                ("idempotency-key", &key),
            ],
            Some(json!({ "state": "SUSPENDED", "reason": "ABUSE_REPORTED", "expectedVersion": v })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    let path = |d: &str| format!("/internal/v1/operator/datasets/cfmeet.partners_daily/days/{d}");
    let r = c.bridge(&path(&day(-1)), &[SYNC]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}DatasetDay"), &r.json);
    assert_eq!(r.json["datasetId"], "cfmeet.partners_daily");
    assert_eq!(r.json["date"], day(-1));
    let m = &r.json["metrics"];
    assert_eq!(
        (
            m["workspaces_active"].as_i64(),
            m["workspaces_suspended"].as_i64()
        ),
        (Some(1), Some(0)),
        "yesterday it was still active: {m}"
    );
    assert_eq!(m["workspaces_restricted"], 0);
    assert_eq!(m["partners_live"], 0);
    assert!(m["partner_mau"].is_null(), "not metered: never a guessed 0");
    let r = c.bridge(&path(&day(0)), &[SYNC]).await;
    let m = &r.json["metrics"];
    assert_eq!(
        (
            m["workspaces_active"].as_i64(),
            m["workspaces_suspended"].as_i64()
        ),
        (Some(0), Some(1))
    );
    // Before the workspace existed.
    let r = c.bridge(&path(&day(-5)), &[SYNC]).await;
    assert_eq!(r.json["metrics"]["workspaces_suspended"], 0);
    assert_eq!(r.json["metrics"]["workspaces_active"], 0);
}

#[tokio::test]
async fn apps_are_counted_and_unknowable_metrics_are_null() {
    let Some(c) = Control::new().await else {
        return;
    };
    let path = format!(
        "/internal/v1/operator/datasets/cfmeet.apps_daily/days/{}",
        day(-1)
    );
    let r = c.bridge(&path, &[SYNC]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}DatasetDay"), &r.json);
    for k in [
        "apps_draft",
        "apps_in_review",
        "apps_published",
        "apps_suspended",
    ] {
        assert_eq!(r.json["metrics"][k], 0, "{k}");
    }
    assert!(
        r.json["metrics"]["installs"].is_null(),
        "the installation projection is off"
    );
    // A future day, a malformed day, an unknown dataset.
    let r = c
        .bridge(
            &format!(
                "/internal/v1/operator/datasets/cfmeet.apps_daily/days/{}",
                day(2)
            ),
            &[SYNC],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = c
        .bridge(
            "/internal/v1/operator/datasets/cfmeet.apps_daily/days/2026-13-01",
            &[SYNC],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = c
        .bridge(
            &format!(
                "/internal/v1/operator/datasets/cfmeet.other/days/{}",
                day(-1)
            ),
            &[SYNC],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The admin bridge only.
    assert_eq!(
        c.operator(Method::GET, &path, None).await.status,
        StatusCode::NOT_FOUND
    );
}
