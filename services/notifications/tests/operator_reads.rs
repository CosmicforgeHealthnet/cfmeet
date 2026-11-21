//! The admin bridge's operator reads (ADR 0068): `/internal/v1/operator/*` need
//! `notifications.administer` (bridge only) and `X-CF-Operator`; attribution headers from
//! any other caller are refused on every route; nothing returned carries an address,
//! content or credential.

mod common;

use axum::http::Method;
use cf_notification_client::{Producer, SealedPlaintext};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const DELIVERIES: &str = "/internal/v1/operator/deliveries";
const FAILED: &str = "/internal/v1/operator/failed-deliveries";
const SUPPRESSIONS: &str = "/internal/v1/operator/email-suppressions";
const PROVIDERS: &str = "/internal/v1/operator/providers";
/// ADR 0068 A7: the daily dataset pull.
const DATASET_DAY: &str = "/internal/v1/operator/datasets/{datasetId}/days/{date}";
const SYNC: (&str, &str) = ("x-cf-operator", "hq:dataset-sync");

fn dataset_day(dataset: &str, date: &str) -> String {
    DATASET_DAY
        .replace("{datasetId}", dataset)
        .replace("{date}", date)
}
const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");

fn verification(h: &Harness, recipient: &str, code: &str) -> Value {
    sealed_envelope(
        &h.sealing,
        Producer::MeetingService,
        "account.verification-code.requested",
        &[recipient.to_string()],
        &[("ttlMinutes", json!(1440))],
        &format!("otp:verify:{}:1", Uuid::new_v4().simple()),
        SealedPlaintext::default().with_code(code),
    )
}

async fn with_deliveries(h: &Harness) {
    let meeting = h.peers.meeting.clone();
    h.ingest(&meeting, vec![verification(h, &alice(), "135790")])
        .await
        .expect(200);
    h.process_all().await;
}

#[tokio::test]
async fn the_bridge_reads_deliveries_failures_suppressions_and_providers() {
    let Some(h) = Harness::new().await else {
        return;
    };
    with_deliveries(&h).await;
    let bridge = h.peers.bridge.clone();

    let d = h
        .signed(&bridge, Method::GET, DELIVERIES, None, &[OPERATOR])
        .await;
    let page = d.expect(200);
    let items = page["items"].as_array().unwrap().clone();
    assert!(!items.is_empty());
    let text = page.to_string();
    assert!(
        !text.contains(ALICE_EMAIL) && !text.contains("135790"),
        "no address, no content"
    );
    assert!(items[0]["deliveryId"].is_string() && items[0]["channel"].is_string());

    // A FAILED delivery appears in the failure list.
    h.exec("UPDATE notif_deliveries SET status = 'FAILED', last_error_class = 'PROVIDER_REJECTED', completed_at = now()").await.unwrap();
    let f = h
        .signed(&bridge, Method::GET, FAILED, None, &[OPERATOR])
        .await;
    let failed = f.expect(200)["items"].as_array().unwrap().clone();
    assert!(!failed.is_empty());
    assert!(failed.iter().all(|i| i["status"] == "FAILED"));

    // A suppression is found by address; only the keyed digest comes back.
    let digest = h.state.keys.email_digest(BOB_EMAIL);
    h.exec(&format!("INSERT INTO notif_email_suppressions (recipient_digest, reason) VALUES ('{digest}', 'COMPLAINED')")).await.unwrap();
    let by_address = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{SUPPRESSIONS}?address=Bob%40Example.com"),
            None,
            &[OPERATOR],
        )
        .await;
    let found = by_address.expect(200);
    assert_eq!(found["items"][0]["digest"], json!(digest));
    assert_eq!(found["items"][0]["reason"], "COMPLAINED");
    assert!(!found.to_string().to_lowercase().contains("bob@example.com"));
    let by_digest = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{SUPPRESSIONS}?digest={digest}"),
            None,
            &[OPERATOR],
        )
        .await;
    assert_eq!(by_digest.expect(200)["items"].as_array().unwrap().len(), 1);
    let none = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{SUPPRESSIONS}?address=nobody%40example.com"),
            None,
            &[OPERATOR],
        )
        .await;
    assert!(none.expect(200)["items"].as_array().unwrap().is_empty());
    h.signed(&bridge, Method::GET, SUPPRESSIONS, None, &[OPERATOR])
        .await
        .expect_error(400, "VALIDATION_ERROR");

    // Provider status: both configured in the harness; never a credential.
    let p = h
        .signed(&bridge, Method::GET, PROVIDERS, None, &[OPERATOR])
        .await;
    let providers = p.expect(200);
    let list = providers["providers"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().all(|x| x["configured"] == true));
    let email = list.iter().find(|x| x["channel"] == "EMAIL").unwrap();
    assert_eq!(
        email["status"], "DEGRADED",
        "the last email delivery failed within the hour"
    );
    assert_eq!(email["lastErrorClass"], "PROVIDER_REJECTED");
}

#[tokio::test]
async fn operator_reads_need_the_permission_and_a_named_operator() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let digest = "a".repeat(64);
    for path in [
        DELIVERIES.to_string(),
        FAILED.to_string(),
        format!("{SUPPRESSIONS}?digest={digest}"),
        PROVIDERS.to_string(),
        dataset_day("cfmeet.operations_health", "2026-11-20"),
    ] {
        // The operator console holds observability, not notifications.administer.
        let op = h.peers.operator.clone();
        h.signed(&op, Method::GET, &path, None, &[])
            .await
            .expect_error(403, "FORBIDDEN");
        // The bridge without X-CF-Operator.
        let bridge = h.peers.bridge.clone();
        h.signed(&bridge, Method::GET, &path, None, &[])
            .await
            .expect_error(403, "FORBIDDEN");
        // A malformed operator reference.
        h.signed(
            &bridge,
            Method::GET,
            &path,
            None,
            &[("x-cf-operator", "staff-0001")],
        )
        .await
        .expect_error(403, "FORBIDDEN");
    }
}

#[tokio::test]
async fn attribution_from_any_other_caller_is_refused_on_every_route() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // The operator console may read routing, but never while asserting an HQ operator.
    let op = h.peers.operator.clone();
    h.signed(&op, Method::GET, "/internal/v1/admin/routing", None, &[])
        .await
        .expect(200);
    h.signed(
        &op,
        Method::GET,
        "/internal/v1/admin/routing",
        None,
        &[OPERATOR],
    )
    .await
    .expect_error(403, "FORBIDDEN");
    // The bridge may read routing with attribution (notifications-observability.read).
    let bridge = h.peers.bridge.clone();
    h.signed(
        &bridge,
        Method::GET,
        "/internal/v1/admin/routing",
        None,
        &[OPERATOR],
    )
    .await
    .expect(200);
    // Self-approval and incomplete approvals from the bridge are refused.
    h.signed(
        &bridge,
        Method::GET,
        DELIVERIES,
        None,
        &[
            OPERATOR,
            ("x-cf-approver", "hq:staff-0001"),
            ("x-cf-approval-ref", "ap-1"),
            ("x-cf-intent-digest", &"b".repeat(64)),
        ],
    )
    .await
    .expect_error(403, "FORBIDDEN");
    h.signed(
        &bridge,
        Method::GET,
        DELIVERIES,
        None,
        &[OPERATOR, ("x-cf-approver", "hq:staff-0002")],
    )
    .await
    .expect_error(403, "FORBIDDEN");
}

#[tokio::test]
async fn delivery_windows_and_cursors_are_validated() {
    let Some(h) = Harness::new().await else {
        return;
    };
    with_deliveries(&h).await;
    with_deliveries(&h).await;
    let bridge = h.peers.bridge.clone();
    for bad in [
        format!("{DELIVERIES}?since=2026-11-10T00:00:00Z&until=2026-11-01T00:00:00Z"),
        format!("{DELIVERIES}?since=2026-09-01T00:00:00Z&until=2026-11-01T00:00:00Z"),
        format!("{DELIVERIES}?since=yesterday"),
        format!("{DELIVERIES}?cursor=nope"),
        format!("{DELIVERIES}?unknown=1"),
    ] {
        let r = h
            .signed(&bridge, Method::GET, &bad, None, &[OPERATOR])
            .await;
        assert_eq!(r.status.as_u16(), 400, "{bad}");
    }
    // A past window is empty; paging walks newest first without repeats.
    let past = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{DELIVERIES}?since=2020-01-01T00:00:00Z&until=2020-01-02T00:00:00Z"),
            None,
            &[OPERATOR],
        )
        .await;
    assert!(past.expect(200)["items"].as_array().unwrap().is_empty());
    let first = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{DELIVERIES}?limit=1"),
            None,
            &[OPERATOR],
        )
        .await;
    let first = first.expect(200);
    let cursor = first["nextCursor"]
        .as_str()
        .expect("a second page")
        .to_string();
    let second = h
        .signed(
            &bridge,
            Method::GET,
            &format!("{DELIVERIES}?limit=1&cursor={cursor}"),
            None,
            &[OPERATOR],
        )
        .await;
    let second = second.expect(200);
    assert_ne!(
        first["items"][0]["deliveryId"],
        second["items"][0]["deliveryId"]
    );
}

#[test]
fn the_trust_bundle_admits_the_bridge_with_operator_reads() {
    // Builds the harness trust bundle (no database): the registry accepts the bridge's
    // notifications.administer and notifications-observability.read grants.
    let (_verifier, peers) = verifier_and_peers();
    assert_eq!(peers.bridge.issuer().as_str(), "cfmeet.admin-bridge");
}

#[tokio::test]
async fn the_bridge_pulls_daily_failed_deliveries_counts_only() {
    let Some(h) = Harness::new().await else {
        return;
    };
    with_deliveries(&h).await;
    with_deliveries(&h).await;
    with_deliveries(&h).await;
    let total = h.count("SELECT count(*) FROM notif_deliveries").await;
    assert!(total >= 3, "{total}");
    // The database's UTC days: yesterday, the day before, and today.
    let day = |offset: i32| {
        let h = &h;
        async move {
            h.text(&format!(
                "SELECT to_char((now() AT TIME ZONE 'UTC')::date - {offset}, 'YYYY-MM-DD')"
            ))
            .await
            .unwrap()
        }
    };
    let (today, yesterday, before) = (day(0).await, day(1).await, day(2).await);
    let at = |date: &str, time: &str| format!("('{date} {time}'::timestamp AT TIME ZONE 'UTC')");
    // Two deliveries failed yesterday (at its first and last second), one the day before;
    // one other ended yesterday but was SENT; everything else is still in flight.
    h.exec(&format!(
        "UPDATE notif_deliveries SET status = 'FAILED', last_error_class = 'PROVIDER_REJECTED', completed_at = {}
          WHERE id = (SELECT id FROM notif_deliveries ORDER BY id LIMIT 1 OFFSET 0)",
        at(&yesterday, "00:00:00")
    ))
    .await
    .unwrap();
    h.exec(&format!(
        "UPDATE notif_deliveries SET status = 'FAILED', last_error_class = 'PROVIDER_REJECTED', completed_at = {}
          WHERE id = (SELECT id FROM notif_deliveries ORDER BY id LIMIT 1 OFFSET 1)",
        at(&yesterday, "23:59:59.999999")
    ))
    .await
    .unwrap();
    h.exec(&format!(
        "UPDATE notif_deliveries SET status = 'FAILED', last_error_class = 'PROVIDER_REJECTED', completed_at = {}
          WHERE id = (SELECT id FROM notif_deliveries ORDER BY id LIMIT 1 OFFSET 2)",
        at(&before, "23:59:59.999999")
    ))
    .await
    .unwrap();
    if total > 3 {
        h.exec(&format!(
            "UPDATE notif_deliveries SET status = 'SENT', completed_at = {}
              WHERE id = (SELECT id FROM notif_deliveries ORDER BY id LIMIT 1 OFFSET 3)",
            at(&yesterday, "12:00:00")
        ))
        .await
        .unwrap();
    }
    let bridge = h.peers.bridge.clone();
    let pull = |date: String| {
        let (h, bridge) = (&h, bridge.clone());
        async move {
            h.signed(
                &bridge,
                Method::GET,
                &dataset_day("cfmeet.operations_health", &date),
                None,
                &[SYNC],
            )
            .await
        }
    };
    let r = pull(yesterday.clone()).await;
    let body = r.expect(200).clone();
    assert_eq!(
        body,
        json!({"datasetId": "cfmeet.operations_health", "date": yesterday.clone(),
               "metrics": {"failed_deliveries": 2}})
    );
    let r = pull(before.clone()).await;
    assert_eq!(r.expect(200)["metrics"]["failed_deliveries"], 1);
    // A retained day without failures counts zero (it is known).
    let r = pull(day(5).await).await;
    assert_eq!(r.expect(200)["metrics"]["failed_deliveries"], 0);
    // Today has not ended; the future and days past the retained window cannot be
    // counted: null, never a guessed 0.
    for date in [
        today.clone(),
        "2999-01-01".into(),
        day(24).await,
        "2020-01-01".into(),
    ] {
        let r = pull(date.clone()).await;
        let body = r.expect(200);
        assert_eq!(body["date"], json!(date));
        assert_eq!(
            body["metrics"],
            json!({"failed_deliveries": null}),
            "{date}"
        );
    }
    // Only the listed dataset; exact dates only.
    h.signed(
        &bridge,
        Method::GET,
        &dataset_day("cfmeet.messaging_daily", &yesterday),
        None,
        &[SYNC],
    )
    .await
    .expect_error(404, "NOT_FOUND");
    for bad in ["2026-02-30", "2026-1-05", "yesterday", "20261120"] {
        let r = h
            .signed(
                &bridge,
                Method::GET,
                &dataset_day("cfmeet.operations_health", bad),
                None,
                &[SYNC],
            )
            .await;
        assert_eq!(r.status.as_u16(), 400, "{bad}");
    }
    // The operator console cannot pull, with or without attribution.
    let op = h.peers.operator.clone();
    for headers in [&[][..], &[SYNC][..]] {
        h.signed(
            &op,
            Method::GET,
            &dataset_day("cfmeet.operations_health", &yesterday),
            None,
            headers,
        )
        .await
        .expect_error(403, "FORBIDDEN");
    }
}
