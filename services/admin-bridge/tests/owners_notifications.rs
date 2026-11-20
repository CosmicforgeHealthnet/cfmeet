//! Notifications reads through the full chain (real mTLS, signed HQ request) against a
//! recording fake Notification Service: paths, query mapping, operator attribution,
//! translation into the closed schemas, and that no address, content or credential
//! reaches HQ.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const DELIVERY: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const NOTIFICATION: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const DIGEST: &str = "3f0a3e1f5b2c4d6e8f9a0b1c2d3e4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeNotifications {
    calls: Mutex<Vec<Recorded>>,
}

fn delivery(status: &str) -> Value {
    json!({
        "deliveryId": DELIVERY, "notificationId": NOTIFICATION, "category": "ACCOUNT_SECURITY",
        "lane": "CRITICAL", "channel": "EMAIL", "status": status, "attempts": 2,
        "lastErrorClass": if status == "FAILED" { json!("PROVIDER_REJECTED") } else { Value::Null },
        "createdAt": "2026-11-19T10:00:00.123456Z", "completedAt": "2026-11-19T10:00:05Z"
    })
}

#[async_trait]
impl OwnerPort for FakeNotifications {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        operator: &str,
    ) -> Result<Value, OwnerError> {
        self.calls.lock().unwrap().push((
            owner,
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            operator.to_string(),
        ));
        Ok(match path {
            "/internal/v1/operator/deliveries" => {
                json!({"items": [delivery("SENT")], "nextCursor": format!("1795000000123456.{DELIVERY}")})
            }
            "/internal/v1/operator/failed-deliveries" => {
                json!({"items": [delivery("FAILED")], "nextCursor": null})
            }
            "/internal/v1/operator/email-suppressions" => {
                json!({"items": [{"digest": DIGEST, "reason": "COMPLAINED", "createdAt": "2026-11-18T09:00:00Z"}], "nextCursor": null})
            }
            "/internal/v1/admin/routing" => json!({
                "routingProtocol": [1],
                "routing": [{"category": "ACCOUNT_SECURITY", "channel": "EMAIL", "mode": "PRIMARY", "fenceAt": "2026-11-01T00:00:00Z",
                             "version": 3, "canaryPercent": 100, "reminderBoundary": null, "changedByRun": "run-7"}],
                "audit": [{"producer": "cfmeet.meeting-service", "category": "ACCOUNT_SECURITY", "hour": "2026-11-19T10:00:00Z",
                           "acceptedDecisions": 12, "digest": "secret-looking-digest-value"}]
            }),
            "/internal/v1/operator/providers" => json!({
                "checkedAt": "2026-11-19T11:00:00Z",
                "providers": [
                    {"channel": "EMAIL", "configured": true, "status": "DEGRADED", "lastSuccessAt": "2026-11-19T09:00:00Z",
                     "lastFailureAt": "2026-11-19T10:59:00Z", "lastErrorClass": "PROVIDER_REJECTED", "recentDeliveries": 4},
                    {"channel": "PUSH", "configured": false, "status": "NOT_CONFIGURED", "lastSuccessAt": null,
                     "lastFailureAt": null, "lastErrorClass": null, "recentDeliveries": 0}
                ]
            }),
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeNotifications>) {
    let fake = Arc::new(FakeNotifications::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last_call(fake: &FakeNotifications) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

/// HQ refuses any result object whose key is on its personal-data list.
fn assert_no_personal_keys(v: &Value) {
    const BANNED: &[&str] = &[
        "email",
        "email_address",
        "phone",
        "phone_number",
        "token",
        "secret",
        "message_body",
        "body_text",
        "raw_payload",
        "msisdn",
    ];
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                assert!(
                    !BANNED.contains(&k.to_ascii_lowercase().as_str()),
                    "banned key {k}"
                );
                assert_no_personal_keys(x);
            }
        }
        Value::Array(a) => a.iter().for_each(assert_no_personal_keys),
        _ => {}
    }
}

#[tokio::test]
async fn notifications_deliveries_map_the_window_and_carry_no_content() {
    let (h, fake) = harness().await;
    let c = call(
        "/notifications/deliveries",
        &format!(
            "{}&limit=20&start=2026-11-10T00:00:00Z&end=2026-11-19T00:00:00Z",
            scoped("ops-incident")
        ),
        "notifications.deliveries.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, query, operator) = last_call(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::Notifications,
            "/internal/v1/operator/deliveries",
            "hq:staff-0001"
        )
    );
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("since", "2026-11-10T00:00:00Z"),
            ("until", "2026-11-19T00:00:00Z"),
            ("limit", "20")
        ]
    );
    let item = &v["result"]["items"][0];
    assert_eq!(item["delivery_id"], DELIVERY);
    assert_eq!(item["created_at"], "2026-11-19T10:00:00Z");
    assert_eq!(
        v["result"]["next_cursor"],
        format!("1795000000123456.{DELIVERY}")
    );
    assert_no_personal_keys(&v);
    // a malformed cursor is the caller's error, never sent to the owner
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/notifications/deliveries",
        &format!(
            "{}&cursor=abc&start=2026-11-10T00:00:00Z&end=2026-11-19T00:00:00Z",
            scoped("ops-incident")
        ),
        "notifications.deliveries.read",
        &["cfmeet.platform_ops"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn notifications_failed_deliveries_use_the_failure_route() {
    let (h, fake) = harness().await;
    let c = call(
        "/notifications/failed-deliveries",
        &format!(
            "{}&start=2026-11-10T00:00:00Z&end=2026-11-19T00:00:00Z",
            scoped("ops-incident")
        ),
        "notifications.failed-deliveries.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last_call(&fake).1,
        "/internal/v1/operator/failed-deliveries"
    );
    assert_eq!(
        v["result"]["items"][0]["last_error_class"],
        "PROVIDER_REJECTED"
    );
}

#[tokio::test]
async fn notifications_suppressions_relay_only_the_digest() {
    let (h, fake) = harness().await;
    let c = call(
        "/notifications/suppressions",
        &format!("{}&q=ada%40example.com", scoped("support-review")),
        "notifications.suppressions.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(path, "/internal/v1/operator/email-suppressions");
    assert_eq!(
        query,
        vec![("address".to_string(), "ada@example.com".to_string())]
    );
    let s = &v["result"]["items"][0];
    assert_eq!(s["recipient_digest"], DIGEST);
    assert_eq!(s["channel"], "EMAIL");
    assert!(v["result"]["next_cursor"].is_null());
    assert!(
        !v.to_string().contains("ada@example.com"),
        "the address never comes back"
    );
    assert_no_personal_keys(&v);
    // by digest
    let c = call(
        "/notifications/suppressions",
        &format!("{}&q={}", scoped("support-review"), DIGEST.to_uppercase()),
        "notifications.suppressions.read",
        &["cfmeet.support"],
    );
    assert_eq!(send(&h, &c).await.0, 200);
    assert_eq!(
        last_call(&fake).2,
        vec![("digest".to_string(), DIGEST.to_string())]
    );
    // neither an address nor a digest
    let c = call(
        "/notifications/suppressions",
        &format!("{}&q=ada", scoped("support-review")),
        "notifications.suppressions.read",
        &["cfmeet.support"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "QUERY_INVALID");
    // the audit row records the path, never the query (the address)
    assert!(h
        .store
        .audit_entries()
        .iter()
        .all(|(e, _)| !e.path.contains('@')));
}

#[tokio::test]
async fn notifications_routing_drops_the_audit_digest() {
    let (h, fake) = harness().await;
    let c = call(
        "/notifications/routing",
        &scoped("ops-incident"),
        "notifications.routing.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last_call(&fake).1, "/internal/v1/admin/routing");
    let r = &v["result"];
    assert_eq!(r["routing_protocols"], json!([1]));
    assert_eq!(r["routes"][0]["mode"], "PRIMARY");
    assert_eq!(r["audit"][0]["accepted_decisions"], 12);
    assert!(!v.to_string().contains("secret-looking-digest-value"));
    assert!(r["routes"][0].get("changed_by_run").is_none());
}

#[tokio::test]
async fn notifications_providers_report_status_only() {
    let (h, fake) = harness().await;
    let c = call(
        "/notifications/providers",
        &scoped("ops-incident"),
        "notifications.providers.read",
        &["cfmeet.platform_ops"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(last_call(&fake).1, "/internal/v1/operator/providers");
    let p = v["result"]["providers"].as_array().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0]["status"], "DEGRADED");
    assert_eq!(p[1]["status"], "NOT_CONFIGURED");
    assert_no_personal_keys(&v);
}

#[tokio::test]
async fn the_module_switch_gates_notification_reads() {
    let fake = Arc::new(FakeNotifications::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: false,
        ..Default::default()
    })
    .await;
    let c = call(
        "/notifications/providers",
        &scoped("ops-incident"),
        "notifications.providers.read",
        &["cfmeet.platform_ops"],
    );
    assert_eq!(code(&send(&h, &c).await.1), "CAPABILITY_DISABLED");
    assert!(fake.calls.lock().unwrap().is_empty());
}
