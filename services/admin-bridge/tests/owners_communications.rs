//! Communications reads through the full chain (real mTLS, signed HQ request) against a
//! fake Communications Hub that records every call: paths, query mapping, the operator
//! attribution, strict translation, minimization and HQ's Summary shape for metrics.

mod common;

use std::sync::{Arc, Mutex};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const CF: &str = "CF-AB7K3Q";
const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";
const CUR: &str = "1793527200123456.6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

/// (owner, path, query, operator) of one owner call.
type Recorded = (Owner, String, Vec<(String, String)>, String);

#[derive(Default)]
struct FakeHub {
    calls: Mutex<Vec<Recorded>>,
}

#[async_trait]
impl OwnerPort for FakeHub {
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
        let p = path.trim_start_matches("/internal/v1/communications/operator");
        Ok(match p {
            x if x == format!("/profiles/{CF}") => json!({
                "cfMeetId": CF, "displayName": "Ada L", "activationState": "ACTIVE",
                "createdAt": T0, "updatedAt": T0, "suspendedAt": null, "deactivatedAt": null, "version": 4,
                "hold": {"reason": "ABUSE", "active": true, "updatedAt": T0},
                "accountReference": "cfmeet-user:secret-ref", "phoneNumber": "+2348030000000"
            }),
            "/reports" => json!({"items": [{
                "reportId": ID, "reason": "HARASSMENT", "createdAt": T0,
                "reporterCfMeetId": "CF-REPORT", "targetCfMeetId": CF, "targetReportCount": 2, "version": 1,
                "review": null, "body": "free text that must never travel"
            }], "nextCursor": CUR}),
            "/holds" => {
                json!({"items": [{"cfMeetId": CF, "reason": "LEGAL", "active": true, "version": 2, "updatedAt": T0}], "nextCursor": null})
            }
            "/audit" => json!({"items": [{
                "seq": 77, "operation": "OPERATOR_HOLD_APPLIED", "actorProfileId": null, "resourceId": ID,
                "createdAt": T0, "details": {"note": "secret detail"}
            }], "nextCursor": 76}),
            "/metrics" => {
                json!({"asOf": T0, "profiles": 120, "connections": 40, "devicesActive": 33, "outboxDead": 0, "operatorHolds": 1, "reportsOpen": 2})
            }
            _ => return Err(OwnerError::NotFound),
        })
    }
}

async fn harness() -> (Harness, Arc<FakeHub>) {
    let fake = Arc::new(FakeHub::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    (h, fake)
}

fn last_call(fake: &FakeHub) -> Recorded {
    fake.calls
        .lock()
        .unwrap()
        .last()
        .cloned()
        .expect("an owner call")
}

#[tokio::test]
async fn communications_profile_never_relays_contact_fields() {
    let (h, fake) = harness().await;
    let c = call(
        &format!("/communications/profiles/{CF}"),
        &scoped("support-review"),
        "communications.profiles.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (owner, path, _, operator) = last_call(&fake);
    assert_eq!(
        (owner, path.as_str(), operator.as_str()),
        (
            Owner::Communications,
            "/internal/v1/communications/operator/profiles/CF-AB7K3Q",
            "hq:staff-0001"
        )
    );
    let r = &v["result"];
    assert_eq!(r["cfmeet_id"], CF);
    assert_eq!(r["display_name"], "Ada L");
    assert_eq!(r["hold"]["reason"], "ABUSE");
    assert_eq!(r["created_at"], "2026-11-01T10:00:00Z");
    let text = v.to_string();
    assert!(
        !text.contains("secret-ref") && !text.contains("2348030000000"),
        "contact fields never reach HQ"
    );
}

#[tokio::test]
async fn communications_reports_and_holds_page_by_time_cursor() {
    let (h, fake) = harness().await;
    let c = call(
        "/communications/reports",
        &format!("{}&limit=5&cursor={CUR}", scoped("abuse-review")),
        "communications.reports.read",
        &["cfmeet.trust_safety"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(path, "/internal/v1/communications/operator/reports");
    assert_eq!(
        query,
        vec![
            ("limit".to_string(), "5".to_string()),
            ("cursor".to_string(), CUR.to_string())
        ]
    );
    let item = &v["result"]["items"][0];
    assert_eq!(item["reason"], "HARASSMENT");
    assert!(item["review"].is_null());
    assert!(
        !v.to_string().contains("free text"),
        "report text never travels"
    );
    assert_eq!(v["result"]["next_cursor"], CUR);

    let c = call(
        "/communications/holds",
        &scoped("abuse-review"),
        "communications.holds.read",
        &["cfmeet.trust_safety"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["result"]["items"][0]["reason"], "LEGAL");

    // a malformed cursor is the caller's error, never sent to the owner
    let calls = fake.calls.lock().unwrap().len();
    let bad = call(
        "/communications/reports",
        &format!("{}&cursor=abc", scoped("abuse-review")),
        "communications.reports.read",
        &["cfmeet.trust_safety"],
    );
    assert_eq!(code(&send(&h, &bad).await.1), "QUERY_INVALID");
    assert_eq!(fake.calls.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn communications_audit_maps_the_window_and_drops_details() {
    let (h, fake) = harness().await;
    let c = call(
        "/communications/audit",
        &format!(
            "{}&limit=10&cursor=78&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
            scoped("abuse-review")
        ),
        "communications.audit.read",
        &["cfmeet.trust_safety"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (_, path, query, _) = last_call(&fake);
    assert_eq!(path, "/internal/v1/communications/operator/audit");
    let q: Vec<(&str, &str)> = query
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        q,
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "10"),
            ("cursor", "78")
        ]
    );
    let e = &v["result"]["items"][0];
    assert_eq!(e["sequence"], 77);
    assert_eq!(e["operation"], "OPERATOR_HOLD_APPLIED");
    assert!(
        !v.to_string().contains("secret detail"),
        "audit details are not relayed"
    );
    assert_eq!(v["result"]["next_cursor"], "76");
}

#[tokio::test]
async fn communications_metrics_are_an_hq_summary() {
    let (h, fake) = harness().await;
    let c = call(
        "/communications/metrics",
        &scoped("support-review"),
        "communications.metrics.read",
        &["cfmeet.support"],
    );
    let (status, v) = send(&h, &c).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        last_call(&fake).1,
        "/internal/v1/communications/operator/metrics"
    );
    let r = &v["result"];
    assert_eq!(r["contract_version"], "cfmeet.admin-summary.v1");
    assert_eq!(
        r["scope"],
        json!({"product": "cfmeet", "market": "GLOBAL", "organization": "cfmeet", "zone": "production", "tenant": "cfmeet-first-party"})
    );
    assert_eq!(r["coverage"], "COMPLETE");
    assert!(r["window"].is_null());
    assert_eq!(r["as_of"], "2026-11-01T10:00:00Z");
    let cells = r["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 6);
    let profiles = cells.iter().find(|c| c["key"] == "profiles").unwrap();
    assert_eq!(
        profiles,
        &json!({"key": "profiles", "value": 120, "unit": "count", "currency": null, "suppression": "NONE"})
    );
    let zero = cells.iter().find(|c| c["key"] == "outbox_dead").unwrap();
    assert_eq!(zero["value"], 0, "a reported zero stays zero");
}

#[tokio::test]
async fn communications_summary_and_scope_are_hqs_components_verbatim() {
    use admin_bridge::contract::schemas_communications::{HQ_SCOPE, HQ_SUMMARY};
    let schemas = admin_bridge::contract::schemas::components();
    let summary: Value = serde_json::from_str(HQ_SUMMARY).unwrap();
    let scope: Value = serde_json::from_str(HQ_SCOPE).unwrap();
    assert_eq!(schemas["Summary"], summary);
    assert_eq!(schemas["Scope"], scope);
    assert_eq!(
        summary["properties"]["contract_version"]["const"],
        "cfmeet.admin-summary.v1"
    );
    assert_eq!(
        summary["properties"]["scope"]["$ref"],
        "#/components/schemas/Scope"
    );
}
