//! ADR 0068 A7 — owner events to the HQ admin bridge, through the REAL router (CI: real
//! PostgreSQL): the transactional outbox (`devplat_bridge_events`) records verification
//! cases, partner workspaces, app reviews, price versions and incidents with their state
//! change only while `CF_ADMIN_BRIDGE_BASE_URL` is set, and the publisher posts the
//! bridge's closed body once.

mod common;

use std::sync::Mutex;

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use common::*;
use developer_platform::bridge_events::{self, BridgeEventSink, BridgeEvents, PostFailure};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

const OPERATOR: (&str, &str) = ("x-cf-operator", "hq:staff-0001");
const APPROVER: (&str, &str) = ("x-cf-approver", "hq:staff-0002");
const APPROVAL: (&str, &str) = ("x-cf-approval-ref", "appr-e");
const DIGEST: (&str, &str) = (
    "x-cf-intent-digest",
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
);
const PARTNER_BOOK: &str = "6c0d4a52-5d9e-4f53-9a51-8e0c1f6f0a01";

fn id(v: &Value) -> Uuid {
    Uuid::parse_str(v["id"].as_str().unwrap_or_else(|| panic!("no id: {v}"))).unwrap()
}

/// A workspace with a project and an open production-access request: (workspace, request).
async fn requested(c: &Control, email: &str) -> (Uuid, Uuid) {
    let (owner, _) = c.sessions.sign_in(email);
    let o = c
        .req(
            As::User(&owner),
            Method::POST,
            "/v1/organizations",
            Some(json!({ "displayName": "Acme" })),
        )
        .await;
    assert_eq!(o.status, StatusCode::CREATED, "{}", o.json);
    let p = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/organizations/{}/projects", id(&o.json)),
            Some(json!({ "displayName": "Backend" })),
        )
        .await;
    assert_eq!(p.status, StatusCode::CREATED, "{}", p.json);
    let project = id(&p.json);
    let e = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/environments"),
            Some(json!({ "kind": "SANDBOX" })),
        )
        .await;
    assert_eq!(e.status, StatusCode::CREATED, "{}", e.json);
    let r = c
        .req(
            As::User(&owner),
            Method::POST,
            &format!("/v1/projects/{project}/production-access-requests"),
            Some(json!({ "capabilities": ["MEETINGS"], "useCaseSummary": "Appointments" })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    (id(&o.json), id(&r.json))
}

async fn events(c: &Control) -> Vec<(String, String, String, String, i64)> {
    c.db.conn
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT event_type, subject_kind, subject_id, state, subject_version FROM devplat_bridge_events \
             ORDER BY created_at, event_type, subject_version",
        ))
        .await
        .unwrap()
        .iter()
        .map(|r| {
            (
                r.try_get("", "event_type").unwrap(),
                r.try_get("", "subject_kind").unwrap(),
                r.try_get("", "subject_id").unwrap(),
                r.try_get("", "state").unwrap(),
                r.try_get("", "subject_version").unwrap(),
            )
        })
        .collect()
}

struct Sink {
    answer: Result<(), PostFailure>,
    seen: Mutex<Vec<Value>>,
}

#[async_trait]
impl BridgeEventSink for Sink {
    async fn post(&self, body: &Value) -> Result<(), PostFailure> {
        self.seen.lock().unwrap().push(body.clone());
        self.answer
    }
}

async fn two(c: &Control, path: &str, body: Value) -> Response {
    let key = Uuid::new_v4().to_string();
    c.bridge_send(
        Method::POST,
        path,
        &[
            OPERATOR,
            APPROVER,
            APPROVAL,
            DIGEST,
            ("idempotency-key", &key),
        ],
        Some(body),
    )
    .await
}

#[tokio::test]
async fn owner_events_are_recorded_with_each_state_change_and_posted_once() {
    let Some(c) = Control::with_state(|s| {
        s.with_production_reviewers(vec!["github-alice".into()])
            .with_bridge_events(BridgeEvents::ON)
    })
    .await
    else {
        return;
    };
    let (o, rid) = requested(&c, "owner@events.example").await;
    let case = format!("cfmeet:PARTNER_GO_LIVE:{rid}");
    let all = events(&c).await;
    assert!(all.contains(&(
        "partner.workspace.changed.v1".into(),
        "partner_workspace".into(),
        o.to_string(),
        "ACTIVE".into(),
        1
    )));
    assert!(all.contains(&(
        "verification.case.changed.v1".into(),
        "verification_case".into(),
        case.clone(),
        "OPEN".into(),
        1
    )));

    // HQ claims the case: the case state as HQ sees it, at the row version.
    let r = two(
        &c,
        &format!("/internal/v1/operator/production-access-requests/{rid}/review"),
        json!({ "expectedVersion": 1 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(events(&c).await.contains(&(
        "verification.case.changed.v1".into(),
        "verification_case".into(),
        case.clone(),
        "CLAIMED".into(),
        2
    )));

    // HQ suspends the workspace.
    let v: i64 = c
        .bridge(
            &format!("/internal/v1/operator/organizations/{o}"),
            &[OPERATOR],
        )
        .await
        .json["version"]
        .as_i64()
        .unwrap();
    let r = two(
        &c,
        &format!("/internal/v1/operator/organizations/{o}/state"),
        json!({ "state": "SUSPENDED", "reason": "ABUSE_REPORTED", "expectedVersion": v }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(events(&c).await.contains(&(
        "partner.workspace.changed.v1".into(),
        "partner_workspace".into(),
        o.to_string(),
        "SUSPENDED".into(),
        v + 1
    )));

    // An incident record.
    let key = Uuid::new_v4().to_string();
    let r = c
        .bridge_send(
            Method::POST,
            &format!("/internal/v1/operator/organizations/{o}/incidents"),
            &[OPERATOR, ("idempotency-key", &key)],
            Some(
                json!({ "incidentRef": "INC-9", "occurredAt": "2026-11-20T08:00:00Z",
                         "expectedVersion": 0 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let incident = events(&c)
        .await
        .into_iter()
        .find(|e| e.0 == "incident.changed.v1")
        .expect("incident event");
    assert_eq!(
        (incident.1.as_str(), incident.3.as_str(), incident.4),
        ("incident", "RECORDED", 1)
    );
    let listed = c
        .bridge(
            &format!("/internal/v1/operator/incidents?organizationId={o}"),
            &[OPERATOR],
        )
        .await;
    assert_eq!(
        listed.json["items"][0]["incidentRecordId"],
        incident.2.as_str()
    );

    // A price version drafted by HQ.
    let books = c
        .bridge("/internal/v1/operator/pricing/price-books", &[OPERATOR])
        .await;
    let bv = books.json["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["priceBookId"] == PARTNER_BOOK)
        .unwrap()["version"]
        .as_i64()
        .unwrap();
    let r = two(
        &c,
        &format!("/internal/v1/operator/pricing/price-books/{PARTNER_BOOK}/versions"),
        json!({ "expectedVersion": bv, "rates": { "rates": [
            { "usageType": "MESSAGES", "unitAmountMinor": 1 } ] } }),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let version_id = r.json["resource"]["versionId"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(events(&c).await.contains(&(
        "pricing.version.changed.v1".into(),
        "price_version".into(),
        version_id,
        "DRAFT".into(),
        1
    )));

    // Ids, states and versions only: never an address or a name.
    let text = format!("{:?}", events(&c).await);
    assert!(!text.contains('@') && !text.contains("Acme"), "{text}");

    // The publisher posts every event once, with the bridge's closed body.
    let pending = c
        .count("SELECT count(*) FROM devplat_bridge_events WHERE delivery_state = 'PENDING'")
        .await;
    let sink = Sink {
        answer: Ok(()),
        seen: Mutex::new(vec![]),
    };
    let report = bridge_events::publish_due(&c.db.conn, &sink, 500, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(report.sent as i64, pending);
    for body in sink.seen.lock().unwrap().iter() {
        let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "event_id",
                "occurred_at",
                "state",
                "subject",
                "type",
                "version"
            ]
        );
        assert!(body["version"].as_i64().unwrap() >= 1);
    }
    let again = bridge_events::publish_due(&c.db.conn, &sink, 500, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(again, Default::default(), "never posted twice");
}

#[tokio::test]
async fn a_refused_body_is_dead_lettered_and_an_unreachable_bridge_retried() {
    let Some(c) = Control::with_state(|s| {
        s.with_production_reviewers(vec!["github-alice".into()])
            .with_bridge_events(BridgeEvents::ON)
    })
    .await
    else {
        return;
    };
    requested(&c, "owner@retry.example").await;
    let down = Sink {
        answer: Err(PostFailure::Retry("UNAVAILABLE")),
        seen: Mutex::new(vec![]),
    };
    let r = bridge_events::publish_due(&c.db.conn, &down, 500, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        (r.sent, r.retried),
        (0, 1),
        "the pass stops at the first failure"
    );
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_bridge_events WHERE delivery_state = 'PENDING' AND attempts = 1")
            .await,
        1
    );
    let refused = Sink {
        answer: Err(PostFailure::Dead("EVENT_REFUSED")),
        seen: Mutex::new(vec![]),
    };
    let later = chrono::Utc::now() + chrono::Duration::minutes(10);
    let r = bridge_events::publish_due(&c.db.conn, &refused, 500, later)
        .await
        .unwrap();
    assert!(r.dead >= 1);
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_bridge_events WHERE delivery_state = 'PENDING'")
            .await,
        0
    );
}

#[tokio::test]
async fn without_the_bridge_nothing_is_stored() {
    let Some(c) =
        Control::with_state(|s| s.with_production_reviewers(vec!["github-alice".into()])).await
    else {
        return;
    };
    requested(&c, "owner@off.example").await;
    assert_eq!(
        c.count("SELECT count(*) FROM devplat_bridge_events").await,
        0
    );
}
