//! ADR 0068 A6 — pricing and plans administered by the HQ admin bridge, through the REAL
//! router (CI: real PostgreSQL): the price version lifecycle with two people, activation,
//! rollback, market freezes, CF Meet user plans, overrides, the charge simulation and the
//! finance reports. Nothing charges anyone.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::{json, Value};
use uuid::Uuid;

const CTRL: &str = "platform-developer/control/v1.schema.json#/$defs/";
const PARTNER: &str = "6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a01";
const CONSUMER: &str = "6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a02";
const A: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const B: (&str, &str) = ("x-cf-operator", "hq:staff-0003");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-p");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
);

/// A two-person command by `who` with a fresh key; returns (response, key).
async fn run(c: &Control, who: (&str, &str), path: &str, body: Value) -> (Response, String) {
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            path,
            &[who, APPROVER, APPROVAL, DIGEST, ("idempotency-key", &key)],
            Some(body),
        )
        .await;
    (r, key)
}

fn ok(r: &Response, status: &str) -> i64 {
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    conforms(&format!("{CTRL}AdminCommandResult"), &r.json);
    assert_eq!(r.json["status"], status, "{}", r.json);
    r.json["version"].as_str().unwrap().parse().unwrap()
}

async fn book_version(c: &Control, book: &str) -> i64 {
    let r = c
        .bridge("/internal/v1/operator/pricing/price-books", &[A])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    r.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["priceBookId"] == book)
        .unwrap()["version"]
        .as_i64()
        .unwrap()
}

fn card(unit: u64) -> Value {
    json!({ "rates": [
        { "usageType": "MEETING_MINUTES", "unitAmountMinor": unit, "includedQuantity": 1000,
          "minimumMonthlyMinor": 500 },
        { "usageType": "MESSAGES", "unitAmountMinor": 0,
          "tiers": [ { "upTo": 100, "unitAmountMinor": 3 }, { "upTo": null, "unitAmountMinor": 1 } ] },
    ]})
}

/// Drafts, submits (A), approves (B) and returns (version id, row version).
async fn approved(c: &Control, book: &str, rates: Value) -> (String, i64) {
    let v = book_version(c, book).await;
    let (r, key) = run(
        c,
        A,
        &format!("/internal/v1/operator/pricing/price-books/{book}/versions"),
        json!({ "expectedVersion": v, "rates": rates }),
    )
    .await;
    let mut n = ok(&r, "DRAFT");
    assert_eq!(
        r.json["resource"]["versionId"], key,
        "the version id is the key"
    );
    let id = key;
    let (r, _) = run(
        c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/submit"),
        json!({ "expectedVersion": n }),
    )
    .await;
    n = ok(&r, "SUBMITTED");
    let (r, _) = run(
        c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{id}/approve"),
        json!({ "expectedVersion": n }),
    )
    .await;
    n = ok(&r, "APPROVED");
    (id, n)
}

#[tokio::test]
async fn a_price_version_is_drafted_approved_by_a_second_person_activated_and_rolled_back() {
    let Some(c) = Control::new().await else {
        return;
    };
    let books = c
        .bridge("/internal/v1/operator/pricing/price-books", &[A])
        .await;
    assert_eq!(books.status, StatusCode::OK, "{}", books.json);
    conforms(&format!("{CTRL}PriceBookList"), &books.json);
    assert_eq!(books.json["items"].as_array().unwrap().len(), 2);
    let v = book_version(&c, PARTNER).await;
    let drafts = format!("/internal/v1/operator/pricing/price-books/{PARTNER}/versions");
    // R3: the second person, the key and the version are required.
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &drafts,
            &[A, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": v, "rates": card(2) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let r = c
        .bridge_send(
            Method::POST,
            &drafts,
            &[A, APPROVER, APPROVAL, DIGEST],
            Some(json!({ "expectedVersion": v, "rates": card(2) })),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let (r, _) = run(
        &c,
        A,
        &drafts,
        json!({ "expectedVersion": v + 3, "rates": card(2) }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let (r, _) = run(
        &c,
        A,
        &drafts,
        json!({ "expectedVersion": v, "rates": { "rates": [{ "usageType": "SMS", "unitAmountMinor": 1 }] } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);

    // Draft → edit → submit by A.
    let (r, id) = run(
        &c,
        A,
        &drafts,
        json!({ "expectedVersion": v, "rates": card(1) }),
    )
    .await;
    let n = ok(&r, "DRAFT");
    let again = c
        .bridge_send(
            Method::POST,
            &drafts,
            &[A, APPROVER, APPROVAL, DIGEST, ("idempotency-key", &id)],
            Some(json!({ "expectedVersion": v, "rates": card(1) })),
        )
        .await;
    ok(&again, "DRAFT");
    assert_eq!(
        c.count(&format!(
            "SELECT count(*) FROM devplat_price_versions WHERE price_book_id = '{PARTNER}'"
        ))
        .await,
        1,
        "a replay never drafts twice"
    );
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/edit"),
        json!({ "expectedVersion": n, "rates": card(2) }),
    )
    .await;
    let n = ok(&r, "DRAFT");
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/submit"),
        json!({ "expectedVersion": n }),
    )
    .await;
    let n = ok(&r, "SUBMITTED");
    // Frozen once submitted: no edit.
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/edit"),
        json!({ "expectedVersion": n, "rates": card(9) }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // The submitter never approves.
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/approve"),
        json!({ "expectedVersion": n }),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    // R4 without the approver headers.
    let r = c
        .bridge_send(
            Method::POST,
            &format!("/internal/v1/operator/pricing/versions/{id}/approve"),
            &[B, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(json!({ "expectedVersion": n })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let (r, approve_key) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{id}/approve"),
        json!({ "expectedVersion": n }),
    )
    .await;
    let n = ok(&r, "APPROVED");
    assert!(r.json["resource"]["approvalExpiresAt"].is_string());
    let rec = c
        .bridge(
            &format!("/internal/v1/operator/operations/{approve_key}"),
            &[B],
        )
        .await;
    assert_eq!(rec.json["source_status"], "APPROVED");
    assert_eq!(rec.json["source_version"], n.to_string());
    // Activate (R4).
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{id}/activate"),
        json!({ "expectedVersion": n }),
    )
    .await;
    ok(&r, "ACTIVE");
    let read = c
        .bridge(
            &format!("/internal/v1/operator/pricing/versions/{id}"),
            &[A],
        )
        .await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.json);
    conforms(&format!("{CTRL}PriceVersionDetail"), &read.json);
    assert_eq!(read.json["effective"], true);
    assert_eq!(read.json["submittedBy"], "hq:staff-0001");
    assert_eq!(read.json["approvedBy"], "hq:staff-0003");

    // A second version is rejected; a third replaces the first, then is rolled back.
    let v2 = book_version(&c, PARTNER).await;
    let (r, rejected) = run(&c, A, &drafts, json!({ "expectedVersion": v2 })).await;
    let n2 = ok(&r, "DRAFT");
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{rejected}/submit"),
        json!({ "expectedVersion": n2 }),
    )
    .await;
    let n2 = ok(&r, "SUBMITTED");
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{rejected}/reject"),
        json!({ "expectedVersion": n2, "reasonCode": "PRICE_TOO_LOW" }),
    )
    .await;
    ok(&r, "REJECTED");
    let (third, n3) = approved(&c, PARTNER, card(3)).await;
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{third}/activate"),
        json!({ "expectedVersion": n3 }),
    )
    .await;
    ok(&r, "ACTIVE");
    let first = c
        .bridge(
            &format!("/internal/v1/operator/pricing/versions/{id}"),
            &[A],
        )
        .await;
    assert_eq!(first.json["state"], "SUPERSEDED");
    let bv = book_version(&c, PARTNER).await;
    let (r, rb_key) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/rollback"),
        json!({ "expectedVersion": bv, "versionId": id }),
    )
    .await;
    ok(&r, "ROLLED_BACK");
    assert_eq!(r.json["resource"]["effectiveVersionId"], id);
    assert_eq!(
        c.bridge(&format!("/internal/v1/operator/operations/{rb_key}"), &[B])
            .await
            .json["source_status"],
        "ROLLED_BACK"
    );
    // Rolling back to a version that was never active: refused.
    let bv = book_version(&c, PARTNER).await;
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/rollback"),
        json!({ "expectedVersion": bv, "versionId": rejected }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    // The history: append-only activations, newest first.
    let acts = c
        .bridge(
            &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/activations"),
            &[A],
        )
        .await;
    assert_eq!(acts.status, StatusCode::OK, "{}", acts.json);
    conforms(&format!("{CTRL}PriceActivationList"), &acts.json);
    assert_eq!(acts.json["items"][0]["kind"], "ROLLED_BACK");
    assert_eq!(acts.json["items"].as_array().unwrap().len(), 3);
    let list = c
        .bridge(
            &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/versions"),
            &[A],
        )
        .await;
    conforms(&format!("{CTRL}PriceVersionList"), &list.json);
    assert_eq!(list.json["items"].as_array().unwrap().len(), 3);
    // Scheduling needs a future time.
    let (fourth, n4) = approved(&c, PARTNER, card(4)).await;
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{fourth}/schedule"),
        json!({ "expectedVersion": n4, "effectiveFrom": "2020-01-01T00:00:00Z" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let at = (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339();
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{fourth}/schedule"),
        json!({ "expectedVersion": n4, "effectiveFrom": at }),
    )
    .await;
    ok(&r, "SCHEDULED");
    // The schedule does not change today's prices.
    let first = c
        .bridge(
            &format!("/internal/v1/operator/pricing/versions/{id}"),
            &[A],
        )
        .await;
    assert_eq!(first.json["effective"], true);
    // Rates can never change in the database once submitted.
    use sea_orm::ConnectionTrait;
    assert!(c
        .db
        .conn
        .execute_unprepared(&format!(
            "UPDATE devplat_price_versions SET rates = '{{\"rates\": []}}' WHERE id = '{id}'"
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn a_frozen_market_refuses_price_changes_until_two_people_unfreeze_it() {
    let Some(c) = Control::new().await else {
        return;
    };
    let freeze = "/internal/v1/operator/pricing/markets/GLOBAL/freeze".to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &freeze,
            &[A, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(json!({ "expectedVersion": 0, "reasonCode": "PRICE_REVIEW" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "R4: {}", r.json);
    let (r, key) = run(
        &c,
        A,
        &freeze,
        json!({ "expectedVersion": 0, "reasonCode": "PRICE_REVIEW" }),
    )
    .await;
    let n = ok(&r, "FROZEN");
    let again = c
        .bridge_send(
            Method::POST,
            &freeze,
            &[A, APPROVER, APPROVAL, DIGEST, ("idempotency-key", &key)],
            Some(json!({ "expectedVersion": 0, "reasonCode": "PRICE_REVIEW" })),
        )
        .await;
    ok(&again, "FROZEN");
    // Price changes are refused while frozen (a draft is not a price change).
    let v = book_version(&c, PARTNER).await;
    let (r, id) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/versions"),
        json!({ "expectedVersion": v, "rates": card(2) }),
    )
    .await;
    let dn = ok(&r, "DRAFT");
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/submit"),
        json!({ "expectedVersion": dn }),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let list = c
        .bridge("/internal/v1/operator/pricing/freezes?state=FROZEN", &[A])
        .await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}MarketFreezeList"), &list.json);
    assert_eq!(list.json["items"][0]["marketCode"], "GLOBAL");
    // A stale version, then the unfreeze.
    let unfreeze = "/internal/v1/operator/pricing/markets/GLOBAL/unfreeze".to_string();
    let (r, _) = run(
        &c,
        B,
        &unfreeze,
        json!({ "expectedVersion": 0, "reasonCode": "REVIEWED" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let (r, _) = run(
        &c,
        B,
        &unfreeze,
        json!({ "expectedVersion": n, "reasonCode": "REVIEWED" }),
    )
    .await;
    ok(&r, "UNFROZEN");
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{id}/submit"),
        json!({ "expectedVersion": dn }),
    )
    .await;
    ok(&r, "SUBMITTED");
    // A market never frozen cannot be unfrozen; bad codes are refused.
    let (r, _) = run(
        &c,
        B,
        "/internal/v1/operator/pricing/markets/NG/unfreeze",
        json!({ "expectedVersion": 0, "reasonCode": "REVIEWED" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.json);
    let (r, _) = run(
        &c,
        B,
        "/internal/v1/operator/pricing/markets/ng/freeze",
        json!({ "expectedVersion": 0, "reasonCode": "REVIEWED" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
}

#[tokio::test]
async fn cf_meet_user_plans_are_assigned_overridden_simulated_and_reported() {
    let Some(c) = Control::new().await else {
        return;
    };
    let limits = json!({ "meetingMinutes": 40, "participants": 100, "recording": false,
        "storageBytes": 0, "attachmentBytes": 10_000_000, "webinars": false });
    let plans = json!({ "plans": [
        { "code": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0, "limits": limits },
        { "code": "PRO", "label": "Pro", "monthlyMinor": 1200, "yearlyMinor": 12000, "limits": limits },
    ]});
    // Before any version: everyone is on Free.
    let list = c.bridge("/internal/v1/operator/pricing/plans", &[A]).await;
    assert_eq!(list.status, StatusCode::OK, "{}", list.json);
    conforms(&format!("{CTRL}PricingPlanList"), &list.json);
    assert_eq!(list.json["items"][0]["planCode"], "FREE");
    let user = Uuid::new_v4();
    let assign = "/internal/v1/operator/pricing/plan-assignments".to_string();
    let body = |plan: &str, v: i64| {
        json!({ "expectedVersion": v, "holderKind": "USER", "holderId": user, "planCode": plan,
                "priceBookId": CONSUMER })
    };
    let (r, _) = run(&c, A, &assign, body("PRO", 0)).await;
    assert_eq!(
        r.status,
        StatusCode::CONFLICT,
        "PRO not priced yet: {}",
        r.json
    );
    let (id, n) = approved(&c, CONSUMER, plans).await;
    let (r, _) = run(
        &c,
        B,
        &format!("/internal/v1/operator/pricing/versions/{id}/activate"),
        json!({ "expectedVersion": n }),
    )
    .await;
    ok(&r, "ACTIVE");
    let list = c
        .bridge(
            &format!("/internal/v1/operator/pricing/plans?priceBookId={CONSUMER}"),
            &[A],
        )
        .await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 2);
    // R3: approval required.
    let r = c
        .bridge_send(
            Method::POST,
            &assign,
            &[A, ("idempotency-key", &Uuid::new_v4().to_string())],
            Some(body("PRO", 0)),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.json);
    let (r, key) = run(&c, A, &assign, body("PRO", 0)).await;
    let hv = ok(&r, "PRO");
    assert_eq!(r.json["resource"]["holderId"], json!(user));
    assert_eq!(
        c.bridge(&format!("/internal/v1/operator/operations/{key}"), &[A])
            .await
            .json["source_status"],
        "PRO"
    );
    let (r, _) = run(&c, A, &assign, body("FREE", 0)).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "stale: {}", r.json);
    let (r, _) = run(&c, A, &assign, body("BUSINESS", hv)).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "unknown plan: {}", r.json);
    let history = c
        .bridge(
            &format!("/internal/v1/operator/pricing/plan-assignments?holderId={user}"),
            &[A],
        )
        .await;
    assert_eq!(history.status, StatusCode::OK, "{}", history.json);
    conforms(&format!("{CTRL}ConsumerPlanAssignmentList"), &history.json);
    assert_eq!(history.json["items"][0]["planCode"], "PRO");
    assert_eq!(history.json["items"][0]["current"], true);
    // Overrides (R4).
    let overrides = "/internal/v1/operator/pricing/overrides".to_string();
    let (r, _) = run(
        &c,
        B,
        &overrides,
        json!({ "expectedVersion": 0, "holderKind": "USER", "holderId": user,
                "limits": { "participants": 500 }, "reasonCode": "EVENT_PARTNER" }),
    )
    .await;
    let ov = ok(&r, "ACTIVE");
    let (r, _) = run(
        &c,
        B,
        &overrides,
        json!({ "expectedVersion": ov, "holderKind": "USER", "holderId": user,
                "limits": { "participants": "many" }, "reasonCode": "EVENT_PARTNER" }),
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.json);
    let (r, _) = run(
        &c,
        B,
        &overrides,
        json!({ "expectedVersion": ov, "holderKind": "USER", "holderId": user,
                "limits": null, "reasonCode": "EVENT_OVER" }),
    )
    .await;
    ok(&r, "REVOKED");
    let ol = c
        .bridge(
            "/internal/v1/operator/pricing/overrides?state=REVOKED",
            &[A],
        )
        .await;
    assert_eq!(ol.status, StatusCode::OK, "{}", ol.json);
    conforms(&format!("{CTRL}PlanOverrideList"), &ol.json);
    assert_eq!(ol.json["items"].as_array().unwrap().len(), 1);

    // Simulation: a POST read, nothing recorded.
    let before = c
        .count("SELECT count(*) FROM devplat_admin_operations")
        .await;
    let sim = c
        .bridge_send(
            Method::POST,
            "/internal/v1/operator/pricing/simulations",
            &[A],
            Some(json!({ "priceBookId": CONSUMER, "planCode": "PRO", "cycle": "YEARLY" })),
        )
        .await;
    assert_eq!(sim.status, StatusCode::OK, "{}", sim.json);
    conforms(&format!("{CTRL}ChargeSimulation"), &sim.json);
    assert_eq!(sim.json["totalMinor"], 12000);
    assert_eq!(sim.json["currency"], "USD");
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_admin_operations")
            .await,
        before
    );
    let (pid, pn) = approved(&c, PARTNER, card(2)).await;
    let sim = c
        .bridge_send(
            Method::POST,
            "/internal/v1/operator/pricing/simulations",
            &[A],
            Some(json!({ "priceBookId": PARTNER, "versionId": pid,
                         "usage": { "MEETING_MINUTES": 1100, "MESSAGES": 150 } })),
        )
        .await;
    assert_eq!(sim.status, StatusCode::OK, "{}", sim.json);
    assert_eq!(sim.json["totalMinor"], 850);
    assert_eq!(sim.json["estimate"], true);
    let bad = c
        .bridge_send(
            Method::POST,
            "/internal/v1/operator/pricing/simulations",
            &[A],
            Some(json!({ "priceBookId": PARTNER, "versionId": pid, "usage": { "SMS": 1 } })),
        )
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.json);
    let _ = pn;

    // Finance reports.
    let spread = c
        .bridge("/internal/v1/operator/finance/plan-spread", &[A])
        .await;
    assert_eq!(spread.status, StatusCode::OK, "{}", spread.json);
    conforms(&format!("{CTRL}PlanSpreadReport"), &spread.json);
    assert_eq!(spread.json["items"][0]["planCode"], "PRO");
    assert_eq!(spread.json["items"][0]["holders"], 1);
    let usage = c.bridge("/internal/v1/operator/finance/usage", &[A]).await;
    assert_eq!(usage.status, StatusCode::OK, "{}", usage.json);
    conforms(&format!("{CTRL}FinanceUsageReport"), &usage.json);
    let est = c
        .bridge(
            &format!("/internal/v1/operator/finance/estimated-charges?priceBookId={PARTNER}"),
            &[A],
        )
        .await;
    assert_eq!(est.status, StatusCode::OK, "{}", est.json);
    conforms(&format!("{CTRL}EstimatedChargesReport"), &est.json);
    assert_eq!(est.json["estimate"], true);
    for bad in [
        "/internal/v1/operator/finance/estimated-charges".to_string(),
        format!("/internal/v1/operator/finance/estimated-charges?priceBookId={CONSUMER}"),
        "/internal/v1/operator/finance/usage?since=2026-10-01T00:00:00Z&until=2026-11-20T00:00:00Z"
            .to_string(),
        "/internal/v1/operator/finance/usage?cursor=nope".to_string(),
    ] {
        assert_eq!(
            c.bridge(&bad, &[A]).await.status,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
}

#[tokio::test]
async fn pricing_is_the_admin_bridges_only() {
    let Some(c) = Control::new().await else {
        return;
    };
    // The operator console does not hold pricing.administer.
    for path in [
        "/internal/v1/operator/pricing/price-books".to_string(),
        "/internal/v1/operator/pricing/freezes".to_string(),
        "/internal/v1/operator/pricing/plans".to_string(),
        "/internal/v1/operator/pricing/plan-assignments".to_string(),
        "/internal/v1/operator/pricing/overrides".to_string(),
        "/internal/v1/operator/finance/usage".to_string(),
        "/internal/v1/operator/finance/plan-spread".to_string(),
    ] {
        let r = c.operator(Method::GET, &path, None).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}: {}", r.json);
        assert_eq!(c.bridge(&path, &[A]).await.status, StatusCode::OK, "{path}");
    }
    let r = c
        .operator(
            Method::POST,
            &format!("/internal/v1/operator/pricing/price-books/{PARTNER}/versions"),
            Some(json!({ "expectedVersion": 1 })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    let r = c
        .internal(
            &c.peers.gateway,
            Method::GET,
            "/internal/v1/operator/pricing/price-books",
            None,
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.json);
    // Unknown subjects.
    let missing = Uuid::new_v4();
    for path in [
        format!("/internal/v1/operator/pricing/price-books/{missing}/versions"),
        format!("/internal/v1/operator/pricing/price-books/{missing}/activations"),
        format!("/internal/v1/operator/pricing/versions/{missing}"),
        format!("/internal/v1/operator/pricing/plans?priceBookId={missing}"),
    ] {
        assert_eq!(
            c.bridge(&path, &[A]).await.status,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{missing}/submit"),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
    let (r, _) = run(
        &c,
        A,
        &format!("/internal/v1/operator/pricing/versions/{missing}/promote"),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.json);
}
