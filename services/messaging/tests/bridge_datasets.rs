//! The admin bridge's daily dataset pull (ADR 0068 A7, brief §3):
//! `GET /internal/v1/operator/datasets/{datasetId}/days/{day}` (a `yyyy-mm-dd` day) answers counts for a
//! UTC day from data Messaging keeps; a metric it cannot compute is null, never 0.

mod common;

use axum::http::{Method, StatusCode};
use common::{flows::*, *};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};

const OP: (&str, &str) = ("x-cf-operator", "hq:dataset-sync");

async fn pull(h: &Harness, dataset: &str, day: &str) -> Response {
    let peers = peers();
    h.call_internal(
        &peers.admin_bridge,
        Method::GET,
        &format!("/internal/v1/operator/datasets/{dataset}/days/{day}"),
        None,
        None,
        &[OP],
    )
    .await
}

async fn sources_since(h: &Harness, unix: i64) {
    h.conn()
        .execute_unprepared(&format!(
            "UPDATE messaging_seaql_migrations SET applied_at = {unix} WHERE version IN \
             ('m20261113_000011_engagement_usage', 'm20261101_000001_messaging_foundation')"
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn daily_counts_are_exact_or_null() {
    let Some(h) = Harness::new().await else {
        return;
    };
    sources_since(&h, 0).await;
    let c = h.customer(ALL_SCOPES);
    h.set_policy(
        &c,
        json!({ "contexts": { "GROUP": { "allowedProfiles": ["MANAGED"], "defaultProfile": "MANAGED" } } }),
    )
    .await;
    let a = h.principal(&c);
    let conv = h
        .create_conversation(
            &c,
            a,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await
        .json;
    h.send_managed(&c, &conv, a, "one").await;
    h.send_managed(&c, &conv, a, "two").await;
    h.conn()
        .execute_unprepared(
            "UPDATE messaging_outbox_events SET status = 'DEAD_LETTER', last_error = 'TEST', lease_token = NULL, \
               lease_expires_at = NULL WHERE id IN (SELECT id FROM messaging_outbox_events ORDER BY sequence LIMIT 1)",
        )
        .await
        .unwrap();
    let day = h.now().format("%Y-%m-%d").to_string();

    // The day has not ended: null, never a partial count.
    let r = pull(&h, "cfmeet.messaging_daily", &day).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json,
        json!({ "datasetId": "cfmeet.messaging_daily", "date": day, "metrics": { "messages_sent": null } })
    );

    h.clock.advance(chrono::Duration::days(2));
    let r = pull(&h, "cfmeet.messaging_daily", &day).await;
    assert_eq!(
        r.json["metrics"],
        json!({ "messages_sent": 2 }),
        "{}",
        r.text
    );
    let r = pull(&h, "cfmeet.operations_health", &day).await;
    assert_eq!(
        r.json["metrics"],
        json!({ "failed_messages": 1 }),
        "{}",
        r.text
    );
    // A quiet day is a real zero.
    let yesterday = (h.now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let r = pull(&h, "cfmeet.messaging_daily", &yesterday).await;
    assert_eq!(
        r.json["metrics"],
        json!({ "messages_sent": 0 }),
        "{}",
        r.text
    );
    // A day before the source existed: null.
    sources_since(&h, (h.now() - chrono::Duration::days(1)).timestamp()).await;
    let r = pull(&h, "cfmeet.messaging_daily", &day).await;
    assert_eq!(
        r.json["metrics"]["messages_sent"],
        Value::Null,
        "{}",
        r.text
    );

    // Refusals.
    let tomorrow = (h.now() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    assert_eq!(
        pull(&h, "cfmeet.messaging_daily", &tomorrow).await.status,
        StatusCode::BAD_REQUEST
    );
    for bad in ["2026-02-30", "2026-1-01", "yesterday"] {
        assert_eq!(
            pull(&h, "cfmeet.messaging_daily", bad).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    assert_eq!(
        pull(&h, "cfmeet.trust_safety_queue", &day).await.status,
        StatusCode::NOT_FOUND
    );
    // Only the bridge, with an operator.
    let peers = peers();
    let path = format!("/internal/v1/operator/datasets/cfmeet.messaging_daily/days/{day}");
    let r = h
        .call_internal(&peers.meeting, Method::GET, &path, None, None, &[])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let r = h
        .call_internal(&peers.admin_bridge, Method::GET, &path, None, None, &[])
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
}
