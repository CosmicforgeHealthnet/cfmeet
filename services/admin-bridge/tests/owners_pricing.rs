//! Pricing, plan and finance reads through the full chain against a fake Developer
//! Platform that records every call: owner paths, query mapping, the POST simulation
//! (checked body, no Idempotency-Key), strict translation and minimization.

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use admin_bridge::{
    contract::model::Owner,
    owners::{OwnerError, OwnerPort},
};
use async_trait::async_trait;
use common::*;
use serde_json::{json, Value};

const BOOK: &str = "0b7e7f0e-2b9c-4c8e-9d7a-1f2e3d4c5b6a";
const ID: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";
const T0: &str = "2026-11-01T10:00:00.123456+00:00";
const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// (path, query, body) of one owner call.
type Recorded = (String, Vec<(String, String)>, Option<Value>);

#[derive(Default)]
struct FakePricing {
    calls: Mutex<Vec<Recorded>>,
    refuse_simulation: Mutex<bool>,
}

fn page(items: Vec<Value>, next: Value) -> Value {
    json!({"items": items, "nextCursor": next})
}

fn limits() -> Value {
    json!({"meetingMinutes": 40, "participants": 100, "recording": false, "storageBytes": 0,
           "attachmentBytes": 1048576, "webinars": false})
}

fn version() -> Value {
    json!({"versionId": ID, "priceBookId": BOOK, "seq": 2, "state": "APPROVED", "ratesDigest": DIGEST,
           "submittedAt": T0, "approvedAt": T0, "approvalExpiresAt": T0, "rejectedReason": null,
           "decidedAt": T0, "createdAt": T0, "updatedAt": T0, "version": 5})
}

fn owner_path(p: &str) -> String {
    p.trim_start_matches("/internal/v1/operator").to_string()
}

#[async_trait]
impl OwnerPort for FakePricing {
    async fn read(
        &self,
        owner: Owner,
        path: &str,
        query: &[(&str, String)],
        _operator: &str,
    ) -> Result<Value, OwnerError> {
        assert_eq!(owner, Owner::DeveloperPlatform);
        self.calls.lock().unwrap().push((
            path.to_string(),
            query
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
            None,
        ));
        let p = owner_path(path);
        Ok(match p.as_str() {
            "/pricing/price-books" => page(
                vec![
                    json!({"priceBookId": ID, "productLine": "PARTNER_PLATFORM", "currency": "USD", "marketCode": "EU",
                       "effectiveVersionId": null, "frozen": false, "updatedAt": T0, "version": 1}),
                    json!({"priceBookId": BOOK, "productLine": "PARTNER_PLATFORM", "currency": "USD", "marketCode": "GLOBAL",
                       "effectiveVersionId": ID, "frozen": false, "updatedAt": T0, "version": 3}),
                ],
                Value::Null,
            ),
            x if x.ends_with("/versions") => page(vec![version()], json!("c2VxOjUw")),
            x if x.ends_with("/activations") => page(
                vec![
                    json!({"activationId": ID, "priceBookId": BOOK, "versionId": ID, "kind": "ACTIVATED",
                       "effectiveFrom": T0, "operatorRef": "hq:staff-0007", "approverRef": "hq:staff-0008", "recordedAt": T0}),
                ],
                Value::Null,
            ),
            x if x.starts_with("/pricing/versions/") => {
                let mut v = version();
                v["productLine"] = json!("CONSUMER");
                v["marketCode"] = json!("GLOBAL");
                v["rates"] = json!({"plans": [{"code": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0, "limits": limits()}]});
                v["submittedDigest"] = json!(DIGEST);
                v["draftedBy"] = json!("hq:staff-0007");
                v["submittedBy"] = json!("hq:staff-0007");
                v["approvedBy"] = json!("hq:staff-0008");
                v["approverRef"] = json!("hq:staff-0009");
                v["effective"] = json!(true);
                v
            }
            "/pricing/freezes" => page(
                vec![
                    json!({"marketCode": "GLOBAL", "state": "FROZEN", "reasonCode": "AUDIT", "changedBy": "hq:staff-0007",
                       "frozenAt": T0, "updatedAt": T0, "version": 2}),
                ],
                Value::Null,
            ),
            "/pricing/plans" => page(
                vec![
                    json!({"priceBookId": BOOK, "currency": "USD", "marketCode": "GLOBAL", "versionId": null,
                       "planCode": "FREE", "label": "Free", "monthlyMinor": 0, "yearlyMinor": 0, "limits": null}),
                ],
                Value::Null,
            ),
            "/pricing/plan-assignments" => page(
                vec![
                    json!({"assignmentId": ID, "holderKind": "USER", "holderId": ID, "planCode": "PRO",
                       "priceBookId": BOOK, "reasonCode": "UPGRADE", "assignedBy": "hq:staff-0007",
                       "effectiveFrom": T0, "current": true, "billingAccountRef": "acct:secret-ref"}),
                ],
                Value::Null,
            ),
            "/pricing/overrides" => page(
                vec![
                    json!({"overrideId": ID, "holderKind": "ORGANIZATION", "holderId": ID, "state": "ACTIVE",
                       "limits": {"participants": 500}, "reasonCode": "PILOT", "changedBy": "hq:staff-0007",
                       "updatedAt": T0, "version": 1}),
                ],
                Value::Null,
            ),
            "/finance/usage" => json!({
                "items": [{"organizationId": ID, "planCode": null, "usageType": "MESSAGE_SENT", "unit": "MESSAGE",
                           "quantity": 1200, "factCount": 30}],
                "nextCursor": "c2VxOjUw", "from": "2026-11-01", "to": "2026-11-09"}),
            "/finance/estimated-charges" => json!({
                "priceBookId": BOOK, "versionId": ID, "currency": "USD", "estimate": true,
                "items": [{"organizationId": ID, "totalMinor": 250, "lines": [{"usageType": "MESSAGES",
                    "quantity": 1200, "includedQuantity": 1000, "billableQuantity": 200, "amountMinor": 250}]}],
                "nextCursor": null, "from": "2026-11-01", "to": "2026-11-09"}),
            "/finance/plan-spread" => json!({
                "items": [{"planCode": "FREE", "holderKind": "USER", "holders": 40, "assignedInWindow": 3}],
                "nextCursor": null, "asOf": T0}),
            _ => return Err(OwnerError::NotFound),
        })
    }

    async fn query(
        &self,
        owner: Owner,
        path: &str,
        body: &Value,
        _operator: &str,
    ) -> Result<Value, OwnerError> {
        assert_eq!(owner, Owner::DeveloperPlatform);
        self.calls
            .lock()
            .unwrap()
            .push((path.to_string(), Vec::new(), Some(body.clone())));
        if *self.refuse_simulation.lock().unwrap() {
            return Err(OwnerError::Rejected);
        }
        Ok(
            json!({"priceBookId": BOOK, "versionId": ID, "versionState": "APPROVED", "currency": "USD",
                  "estimate": true, "totalMinor": 250,
                  "lines": [{"usageType": "MESSAGES", "quantity": 1200, "includedQuantity": 1000,
                             "billableQuantity": 200, "amountMinor": 250}]}),
        )
    }
}

struct Env {
    h: Harness,
    fake: Arc<FakePricing>,
    n: AtomicUsize,
}

async fn env() -> Env {
    let fake = Arc::new(FakePricing::default());
    let h = start(Options {
        owners: Some(fake.clone()),
        switches_all_on: true,
        ..Default::default()
    })
    .await;
    Env {
        h,
        fake,
        n: AtomicUsize::new(0),
    }
}

impl Env {
    /// One read by a fresh staff member (the rate limit is per staff member).
    async fn get(&self, path: &str, query: &str, perm: &str, role: &'static str) -> (u16, Value) {
        let mut c = call(path, query, perm, &[role]);
        c.sub = format!("staff-{:04}", 100 + self.n.fetch_add(1, Ordering::SeqCst));
        send(&self.h, &c).await
    }
    fn last(&self) -> Recorded {
        self.fake.calls.lock().unwrap().last().cloned().unwrap()
    }
}

fn window(purpose: &str) -> String {
    format!(
        "{}&limit=10&start=2026-11-01T00:00:00Z&end=2026-11-10T00:00:00Z",
        scoped(purpose)
    )
}

fn pairs(q: &[(String, String)]) -> Vec<(&str, &str)> {
    q.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[tokio::test]
async fn pricing_lists_translate_strictly() {
    let e = env().await;
    let paged = format!("{}&limit=10&cursor=c2VxOjEw", scoped("pricing-change"));
    for (path, perm, owner_suffix, field) in [
        (
            "/pricing/price-books".to_string(),
            "pricing.price-books.read",
            "/pricing/price-books".to_string(),
            "market_code",
        ),
        (
            format!("/pricing/price-books/{BOOK}/versions"),
            "pricing.versions.read",
            format!("/pricing/price-books/{BOOK}/versions"),
            "version_state",
        ),
        (
            format!("/pricing/price-books/{BOOK}/activations"),
            "pricing.activations.read",
            format!("/pricing/price-books/{BOOK}/activations"),
            "operator_ref",
        ),
        (
            "/pricing/freezes".to_string(),
            "pricing.freezes.read",
            "/pricing/freezes".to_string(),
            "freeze_state",
        ),
        (
            "/pricing/overrides".to_string(),
            "pricing.overrides.read",
            "/pricing/overrides".to_string(),
            "override_state",
        ),
    ] {
        let (status, v) = e.get(&path, &paged, perm, "cfmeet.pricing.editor").await;
        assert_eq!(status, 200, "{path}: {v}");
        let (p, q, _) = e.last();
        assert_eq!(owner_path(&p), owner_suffix);
        assert_eq!(
            pairs(&q),
            [("limit", "10"), ("cursor", "c2VxOjEw")],
            "{path}"
        );
        assert!(v["result"]["items"][0].get(field).is_some(), "{path}: {v}");
    }
    let (_, v) = e
        .get(
            "/pricing/overrides",
            &scoped("pricing-change"),
            "pricing.overrides.read",
            "cfmeet.pricing.editor",
        )
        .await;
    let o = &v["result"]["items"][0];
    assert_eq!(o["limits"]["participants"], 500);
    assert!(o["limits"]["recording"].is_null());
    // A price book id that is not a UUID names nothing; no owner call.
    let before = e.fake.calls.lock().unwrap().len();
    let (_, v) = e
        .get(
            "/pricing/price-books/book-1/versions",
            &scoped("pricing-change"),
            "pricing.versions.read",
            "cfmeet.pricing.editor",
        )
        .await;
    assert_eq!(code(&v), "RESOURCE_NOT_FOUND");
    assert_eq!(e.fake.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn pricing_version_detail_carries_the_content_by_product_line() {
    let e = env().await;
    let (status, v) = e
        .get(
            &format!("/pricing/versions/{ID}"),
            &scoped("billing-review"),
            "pricing.versions.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let r = &v["result"];
    assert_eq!(r["product_line"], "CONSUMER");
    assert!(r["partner_rates"].is_null());
    assert_eq!(r["consumer_plans"][0]["plan_code"], "FREE");
    assert_eq!(r["consumer_plans"][0]["limits"]["participants"], 100);
    assert_eq!(r["approver_ref"], "hq:staff-0009");
    assert_eq!(r["version"], 5);
    assert_eq!(r["updated_at"], "2026-11-01T10:00:00Z");
}

#[tokio::test]
async fn pricing_plans_are_one_page() {
    let e = env().await;
    let (status, v) = e
        .get(
            "/pricing/plans",
            &format!("{}&limit=20", scoped("pricing-change")),
            "pricing.plans.read",
            "cfmeet.pricing.editor",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(pairs(&e.last().1), [("limit", "20")]);
    assert!(v["result"]["items"][0]["limits"].is_null());
    let (_, v) = e
        .get(
            "/pricing/plans",
            &format!("{}&cursor=c2VxOjEw", scoped("pricing-change")),
            "pricing.plans.read",
            "cfmeet.pricing.editor",
        )
        .await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

#[tokio::test]
async fn pricing_plan_assignments_search_by_holder_or_plan() {
    let e = env().await;
    let search = |q: &str| format!("{}&q={q}", scoped("billing-review"));
    let (status, v) = e
        .get(
            "/pricing/plan-assignments",
            &search(ID),
            "pricing.plan-assignments.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(pairs(&e.last().1), [("holderId", ID)]);
    let a = &v["result"]["items"][0];
    assert_eq!(a["plan_code"], "PRO");
    assert!(a.get("billingAccountRef").is_none() && !v.to_string().contains("secret-ref"));
    e.get(
        "/pricing/plan-assignments",
        &search("PRO"),
        "pricing.plan-assignments.read",
        "cfmeet.finance",
    )
    .await;
    assert_eq!(pairs(&e.last().1), [("planCode", "PRO")]);
    let (_, v) = e
        .get(
            "/pricing/plan-assignments",
            &search("some%20one"),
            "pricing.plan-assignments.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(code(&v), "QUERY_INVALID");
}

#[tokio::test]
async fn pricing_simulation_is_a_checked_post_read() {
    let e = env().await;
    let post = |body: Value, n: usize| {
        let mut c = call(
            "/pricing/simulations",
            &scoped("pricing-change"),
            "pricing.charges.read",
            &["cfmeet.pricing.editor"],
        );
        c.method = "POST";
        c.sub = format!("staff-{:04}", 900 + n);
        c.body = serde_json::to_vec(&body).unwrap();
        c
    };
    let c = post(
        json!({"price_book_id": BOOK, "usage": {"MESSAGES": 1200}}),
        1,
    );
    assert!(c.idempotency_key.is_none());
    let (status, v) = send(&e.h, &c).await;
    assert_eq!(status, 200, "{v}");
    let (p, _, body) = e.last();
    assert_eq!(owner_path(&p), "/pricing/simulations");
    assert_eq!(
        body.unwrap(),
        json!({"priceBookId": BOOK, "usage": {"MESSAGES": 1200}})
    );
    let r = &v["result"];
    assert_eq!(
        (r["total_minor"].clone(), r["estimate"].clone()),
        (json!(250), json!(true))
    );
    assert_eq!(r["lines"][0]["billable_quantity"], 200);
    assert!(r["plan_code"].is_null());

    // Refused before the owner: unknown field, unknown usage type, both inputs, neither.
    let before = e.fake.calls.lock().unwrap().len();
    for (n, body) in [
        json!({"price_book_id": BOOK, "usage": {"MESSAGES": 1}, "force": true}),
        json!({"price_book_id": BOOK, "usage": {"SMS": 1}}),
        json!({"price_book_id": BOOK, "usage": {"MESSAGES": 1}, "plan_code": "PRO"}),
        json!({"price_book_id": BOOK}),
        json!({"price_book_id": "book-1", "plan_code": "PRO"}),
    ]
    .into_iter()
    .enumerate()
    {
        let (_, v) = send(&e.h, &post(body, 10 + n)).await;
        assert_eq!(code(&v), "COMMAND_BODY_INVALID", "{n}");
    }
    assert_eq!(e.fake.calls.lock().unwrap().len(), before);

    // The owner refusing the input (e.g. a usage vector for a CONSUMER book).
    *e.fake.refuse_simulation.lock().unwrap() = true;
    let (_, v) = send(
        &e.h,
        &post(
            json!({"price_book_id": BOOK, "plan_code": "PRO", "cycle": "YEARLY"}),
            30,
        ),
    )
    .await;
    assert_eq!(code(&v), "COMMAND_BODY_INVALID");
    assert_eq!(
        e.last().2.unwrap(),
        json!({"priceBookId": BOOK, "planCode": "PRO", "cycle": "YEARLY"})
    );
}

#[tokio::test]
async fn finance_reports_map_the_window() {
    let e = env().await;
    let (status, v) = e
        .get(
            "/finance/usage",
            &window("billing-review"),
            "finance.usage.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let (p, q, _) = e.last();
    assert_eq!(owner_path(&p), "/finance/usage");
    assert_eq!(
        pairs(&q),
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z"),
            ("limit", "10")
        ]
    );
    let r = &v["result"];
    assert_eq!(
        (r["from"].clone(), r["to"].clone()),
        (json!("2026-11-01"), json!("2026-11-09"))
    );
    assert_eq!(r["items"][0]["workspace_id"], ID);

    let (status, v) = e
        .get(
            "/finance/plan-spread",
            &window("billing-review"),
            "finance.plan-spread.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        pairs(&e.last().1),
        [
            ("since", "2026-11-01T00:00:00Z"),
            ("until", "2026-11-10T00:00:00Z")
        ]
    );
    assert_eq!(v["result"]["items"][0]["assigned_in_window"], 3);
    assert_eq!(v["result"]["as_of"], "2026-11-01T10:00:00Z");

    // Over the owner's 31 days: refused here.
    let before = e.fake.calls.lock().unwrap().len();
    let long = format!(
        "{}&start=2026-09-01T00:00:00Z&end=2026-11-10T00:00:00Z",
        scoped("billing-review")
    );
    let (_, v) = e
        .get(
            "/finance/usage",
            &long,
            "finance.usage.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(code(&v), "QUERY_INVALID");
    assert_eq!(e.fake.calls.lock().unwrap().len(), before);
}

#[tokio::test]
async fn finance_estimated_charges_use_the_market_partner_book() {
    let e = env().await;
    let (status, v) = e
        .get(
            "/finance/estimated-charges",
            &window("billing-review"),
            "finance.estimated-charges.read",
            "cfmeet.finance",
        )
        .await;
    assert_eq!(status, 200, "{v}");
    let calls = e.fake.calls.lock().unwrap().clone();
    let (p, q, _) = &calls[calls.len() - 2];
    assert_eq!(owner_path(p), "/pricing/price-books");
    assert_eq!(
        pairs(q),
        [("productLine", "PARTNER_PLATFORM"), ("limit", "100")]
    );
    let (p, q, _) = &calls[calls.len() - 1];
    assert_eq!(owner_path(p), "/finance/estimated-charges");
    // The GLOBAL market's book, not the EU one listed first.
    assert_eq!(pairs(q)[0], ("priceBookId", BOOK));
    let r = &v["result"];
    assert_eq!(r["price_book_id"], BOOK);
    assert_eq!(r["estimate"], true);
    assert_eq!(r["items"][0]["total_minor"], 250);
    assert_eq!(r["items"][0]["lines"][0]["amount_minor"], 250);
}
