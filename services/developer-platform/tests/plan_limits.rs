//! ADR 0068 A6 — plan enforcement support: the signed internal read of a CF Meet plan
//! holder's effective limits (`GET /internal/v1/plan-limits/{holderKind}/{holderId}`,
//! `developer-entitlement.read`) for the Meeting Service and Messaging, through the REAL
//! router (CI: real PostgreSQL).

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::config::FeatureFlags;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const CONSUMER: &str = "6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a02";
const A: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const B: (&str, &str) = ("x-cf-operator", "hq:staff-0003");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-l");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
);

async fn run(c: &Control, who: (&str, &str), path: &str, body: Value) -> Response {
    let key = Uuid::new_v4().to_string();
    c.bridge_send(
        Method::POST,
        path,
        &[who, APPROVER, APPROVAL, DIGEST, ("idempotency-key", &key)],
        Some(body),
    )
    .await
}

fn version(r: &Response) -> i64 {
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["version"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn the_meeting_service_and_messaging_read_a_holders_effective_limits() {
    let Some((c, producers)) = Control::with_flags_and_producers(FeatureFlags::default()).await
    else {
        return;
    };
    let user = Uuid::new_v4();
    let path = format!("/internal/v1/plan-limits/USER/{user}");

    // No version priced yet: FREE, every limit unknown (callers keep today's behaviour).
    let r = c
        .internal(&producers.meeting, Method::GET, &path, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}EffectivePlanLimits"), &r.json);
    assert_eq!(r.json["planCode"], "FREE");
    assert_eq!(r.json["assigned"], false);
    assert!(r.json["limits"]["participants"].is_null());
    assert!(r
        .headers
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("max-age=60"));

    // HQ prices FREE and PRO, assigns PRO and overrides one limit.
    let free = json!({ "meetingMinutes": 40, "participants": 100, "recording": false,
        "storageBytes": 0, "attachmentBytes": 10_000_000, "webinars": false });
    let pro = json!({ "meetingMinutes": 1440, "participants": 300, "recording": true,
        "storageBytes": 5_000_000_000u64, "attachmentBytes": 100_000_000, "webinars": false });
    let books = c
        .bridge("/internal/v1/operator/pricing/price-books", &[A])
        .await;
    let bv = books.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["priceBookId"] == CONSUMER)
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let r = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/price-books/{CONSUMER}/versions"),
        json!({ "expectedVersion": bv, "rates": { "plans": [
            { "code": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0, "limits": free },
            { "code": "PRO", "label": "Pro", "monthlyMinor": 1200, "yearlyMinor": 12000, "limits": pro },
        ] } }),
    )
    .await;
    let mut n = version(&r);
    let id = r.json["resource"]["versionId"]
        .as_str()
        .unwrap()
        .to_string();
    n = version(
        &run(
            &c,
            A,
            &format!("/internal/v1/operator/pricing/versions/{id}/submit"),
            json!({ "expectedVersion": n }),
        )
        .await,
    );
    n = version(
        &run(
            &c,
            B,
            &format!("/internal/v1/operator/pricing/versions/{id}/approve"),
            json!({ "expectedVersion": n }),
        )
        .await,
    );
    version(
        &run(
            &c,
            B,
            &format!("/internal/v1/operator/pricing/versions/{id}/activate"),
            json!({ "expectedVersion": n }),
        )
        .await,
    );
    let r = c
        .internal(&producers.messaging, Method::GET, &path, None)
        .await;
    assert_eq!(r.json["planCode"], "FREE");
    assert_eq!(r.json["limits"]["participants"], 100);
    assert_eq!(r.json["versionId"], id.as_str());

    version(
        &run(
            &c,
            A,
            "/internal/v1/operator/pricing/plan-assignments",
            json!({ "expectedVersion": 0, "holderKind": "USER", "holderId": user,
                    "planCode": "PRO", "priceBookId": CONSUMER }),
        )
        .await,
    );
    version(
        &run(
            &c,
            B,
            "/internal/v1/operator/pricing/overrides",
            json!({ "expectedVersion": 0, "holderKind": "USER", "holderId": user,
                    "limits": { "participants": 500 }, "reasonCode": "EVENT_PARTNER" }),
        )
        .await,
    );
    let r = c
        .internal(&producers.meeting, Method::GET, &path, None)
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}EffectivePlanLimits"), &r.json);
    assert_eq!(r.json["planCode"], "PRO");
    assert_eq!(r.json["assigned"], true);
    assert_eq!(r.json["overrideApplied"], true);
    assert_eq!(r.json["limits"]["participants"], 500, "the override wins");
    assert_eq!(r.json["limits"]["recording"], true, "the plan's own limit");

    // A malformed holder kind; a caller without the permission.
    let r = c
        .internal(
            &producers.meeting,
            Method::GET,
            &format!("/internal/v1/plan-limits/TEAM/{user}"),
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let r = c
        .internal(&producers.calling, Method::GET, &path, None)
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
}
