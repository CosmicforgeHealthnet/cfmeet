//! ADR 0068 A7: the Hub's owner events to the HQ admin bridge. `abuse.report.changed.v1` for
//! profile reports (OPEN at version 1 when reported; DISMISSED / ACTIONED at version 2 when
//! the operator console or the bridge reviews it), written to the transactional outbox
//! `communication_admin_bridge_events` with the change and relayed to a fake bridge event
//! listener (`POST /internal/v1/hq-bridge/events`) that verifies the REAL service token
//! (issuer `cfmeet.communications`, audience `cfmeet.admin-bridge`,
//! `hq-bridge-events.publish`) and the bridge's closed body. Real PostgreSQL.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::State as AxumState,
    http::{HeaderMap, HeaderValue, StatusCode},
    routing::post,
    Json, Router,
};
use cf_communications_policy::Dispatch;
use cf_service_auth::{
    InternalPermission, OutboundServiceRequest, PublicJwk, RuntimeEnvironment, ServiceId,
    ServiceTokenVerifier, SystemClock, TrustPolicy,
};
use common::*;
use communications::{
    admin_events::{
        event_id, maintain, relay_due, AdminBridgeEvents, RelayReport, EVENTS_PATH, REPORT_EVENT,
    },
    model::{Profile, VerifiedAccount},
    store::{execute, rows},
};
use serde_json::{json, Value};
use uuid::Uuid;

const DISPATCH: &str = "/internal/v1/communications/dispatch";
const CONSOLE_REVIEW: &str = "/internal/v1/communications/admin/reports/review";
const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

// ------------------------------------------------------------------------ fake bridge

#[derive(Default)]
struct Listener {
    /// Every authenticated, well-formed body, in arrival order.
    posts: Vec<Value>,
    /// Statuses to answer next (instead of 202).
    script: Vec<u16>,
    seen: BTreeSet<String>,
    unauthenticated: usize,
    /// Bodies that are not the bridge's closed shape (must stay empty).
    invalid: Vec<String>,
    /// Delay before answering (replica tests).
    delay_ms: u64,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<Listener>>,
}

fn closed(body: &Value) -> bool {
    let Some(o) = body.as_object() else {
        return false;
    };
    let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
    keys.sort_unstable();
    keys == [
        "event_id",
        "occurred_at",
        "state",
        "subject",
        "type",
        "version",
    ] && body["subject"].as_object().is_some_and(|s| s.len() == 2)
        && body["subject"]["kind"] == "abuse_report"
        && body["type"] == REPORT_EVENT
        && body["version"].as_i64().is_some_and(|v| v >= 1)
        && body["event_id"].as_str().and_then(|s| {
            Uuid::parse_str(s)
                .ok()
                .map(|u| u.hyphenated().to_string() == s)
        }) == Some(true)
        && body["occurred_at"]
            .as_str()
            .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
}

async fn events(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let refuse = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({ "error": { "code": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        ctx.inner.lock().unwrap().unauthenticated += 1;
        return refuse(401, "UNAUTHENTICATED");
    };
    if !caller.has_permission(InternalPermission::HqBridgeEventsPublish) {
        return refuse(403, "FORBIDDEN");
    }
    let delay = ctx.inner.lock().unwrap().delay_ms;
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    let mut inner = ctx.inner.lock().unwrap();
    if !closed(&body) {
        inner.invalid.push(body.to_string());
        return refuse(400, "EVENT_INVALID");
    }
    if !inner.script.is_empty() {
        let status = inner.script.remove(0);
        return refuse(status, "SCRIPTED");
    }
    inner.posts.push(body.clone());
    let id = body["event_id"].as_str().unwrap().to_owned();
    let status = if inner.seen.insert(id) {
        "APPENDED"
    } else {
        "DUPLICATE"
    };
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": status, "sequence": null })),
    )
}

fn verifier(hub: &PublicJwk) -> ServiceTokenVerifier {
    let bundle = json!({
        "bundle_version": 1,
        "receiver": ServiceId::admin_bridge(),
        "runtime_environment": "test",
        "trusted_issuers": [{
            "issuer": ServiceId::communications(),
            "audience": ServiceId::admin_bridge(),
            "runtime_environment": "test",
            "keys": [hub],
            "permissions": ["hq-bridge-events.publish"],
        }],
    })
    .to_string();
    ServiceTokenVerifier::new(
        TrustPolicy::from_json(
            &bundle,
            &ServiceId::admin_bridge(),
            RuntimeEnvironment::Test,
        )
        .unwrap(),
        Arc::new(SystemClock),
    )
}

struct Fixture {
    h: Harness,
    bridge: Arc<Mutex<Listener>>,
    sink: AdminBridgeEvents,
}

impl Fixture {
    /// A Hub with `CF_ADMIN_BRIDGE_BASE_URL` pointing at the fake listener, which trusts the
    /// Hub's real signing key with `hq-bridge-events.publish` only.
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let h = Harness::with_admin_bridge(&url).await;
        assert!(h.state.config.admin_bridge_events);
        let bridge = Arc::new(Mutex::new(Listener::default()));
        let ctx = Arc::new(Ctx {
            verifier: verifier(&h.hub_key),
            inner: bridge.clone(),
        });
        let app = Router::new()
            .route(EVENTS_PATH, post(events))
            .with_state(ctx);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let sink = AdminBridgeEvents::new(h.state.config.client.clone());
        Self { h, bridge, sink }
    }

    async fn relay(&self) -> RelayReport {
        relay_due(&self.h.state, &self.sink, 20).await.unwrap()
    }

    fn posts(&self) -> Vec<Value> {
        self.bridge.lock().unwrap().posts.clone()
    }

    fn script(&self, statuses: &[u16]) {
        self.bridge
            .lock()
            .unwrap()
            .script
            .extend_from_slice(statuses);
    }

    fn assert_valid(&self) {
        let inner = self.bridge.lock().unwrap();
        assert!(inner.invalid.is_empty(), "{:?}", inner.invalid);
        assert_eq!(inner.unauthenticated, 0);
    }

    async fn close(self) {
        self.h.close().await;
    }
}

// --------------------------------------------------------------------------- helpers

async fn stored(h: &Harness) -> Vec<Value> {
    rows(
        &h.state.store.db,
        "SELECT to_jsonb(e) doc FROM communication_admin_bridge_events e ORDER BY occurred_at, subject_version",
        vec![],
    )
    .await
    .unwrap()
}

async fn sql(h: &Harness, statement: &str) -> u64 {
    execute(&h.state.store.db, statement, vec![]).await.unwrap()
}

/// Makes every waiting event due now (instead of moving the clock).
async fn due_now(h: &Harness) {
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET next_attempt_at=now()-interval '1 second' WHERE delivery='PENDING'",
    )
    .await;
}

/// `reporter` reports `target` through the Account's user route; returns the report id.
async fn report(
    h: &Harness,
    reporter: &VerifiedAccount,
    target: &Profile,
    key: Uuid,
) -> (StatusCode, Value) {
    let body = serde_json::to_value(Dispatch {
        method: "POST".into(),
        path: format!("profiles/{}/reports", h.public_id(target).await),
        query: Default::default(),
        body: Some(json!({"reason": "HARASSMENT"})),
        idempotency_key: Some(key),
        account: reporter.clone(),
    })
    .unwrap();
    h.http(&h.account_signer, DISPATCH, body, h.delegated(reporter))
        .await
}

async fn reported(h: &Harness) -> (Uuid, Profile, Profile) {
    let a = h.account();
    let reporter = h.profile(&a).await;
    let target = h.profile(&h.account()).await;
    let (status, v) = report(h, &a, &target, Uuid::new_v4()).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = Uuid::parse_str(v["reportId"].as_str().unwrap()).unwrap();
    (id, reporter, target)
}

/// The bridge's R3 review of a report, approved by a second person.
async fn bridge_review(h: &Harness, report: Uuid, body: Value) -> (StatusCode, Value) {
    let mut headers = h
        .bridge
        .sign(&OutboundServiceRequest::new_operation(
            ServiceId::communications(),
        ))
        .unwrap()
        .headers();
    for (k, v) in [
        ("x-cf-operator", "hq:staff-0001"),
        ("x-cf-approver", "hq:staff-0002"),
        ("x-cf-approval-ref", "appr-1"),
        ("x-cf-intent-digest", DIGEST),
        ("idempotency-key", &Uuid::new_v4().to_string()),
    ] {
        headers.insert(k, HeaderValue::from_str(v).unwrap());
    }
    h.http_headers(
        &format!("/internal/v1/communications/operator/reports/{report}/review"),
        body,
        headers,
    )
    .await
}

#[track_caller]
fn assert_event(e: &Value, report: Uuid, state: &str, version: i64) {
    assert_eq!(
        e["event_id"],
        json!(event_id(REPORT_EVENT, report, version)),
        "{e}"
    );
    assert_eq!(e["type"], REPORT_EVENT);
    assert_eq!(e["subject"], json!({"kind": "abuse_report", "id": report}));
    assert_eq!(e["state"], state);
    assert_eq!(e["version"], version);
}

/// Ids, states, versions and times only: nothing about the people or the reason.
#[track_caller]
fn assert_ids_only(text: &str, people: &[&Profile], public_ids: &[&str]) {
    for p in people {
        assert!(!text.contains(&p.id.to_string()), "{text}");
        assert!(!text.contains(&p.principal_id.to_string()), "{text}");
        assert!(!text.contains(&p.display_name), "{text}");
        assert!(!text.contains(&p.account_reference), "{text}");
    }
    for id in public_ids {
        assert!(!text.contains(id), "{text}");
    }
    for word in ["HARASSMENT", "NO_VIOLATION", "ABUSE\"", "reason"] {
        assert!(!text.contains(word), "{text}");
    }
}

// ----------------------------------------------------------------------------- tests

#[tokio::test]
async fn a_report_and_its_console_review_are_published_ids_only() {
    let f = Fixture::start().await;
    let h = &f.h;
    let a = h.account();
    let reporter = h.profile(&a).await;
    let target = h.profile(&h.account()).await;
    let key = Uuid::new_v4();
    let (status, v) = report(h, &a, &target, key).await;
    assert_eq!(status, StatusCode::OK, "{v}");
    let id = Uuid::parse_str(v["reportId"].as_str().unwrap()).unwrap();
    // Written with the report, once: a replayed request adds nothing.
    let (status, _) = report(h, &a, &target, key).await;
    assert_eq!(status, StatusCode::OK);
    let rows = stored(h).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["delivery"], "PENDING");
    assert_eq!(rows[0]["subject_state"], "OPEN");
    assert_eq!(rows[0]["subject_version"], 1);

    let r = f.relay().await;
    assert_eq!((r.acked, r.retried, r.dead), (1, 0, 0), "{r:?}");
    let posts = f.posts();
    assert_eq!(posts.len(), 1);
    assert_event(&posts[0], id, "OPEN", 1);
    let rows = stored(h).await;
    assert_eq!(rows[0]["delivery"], "ACKED");
    assert_eq!(rows[0]["outcome"], "APPENDED");
    assert_eq!(rows[0]["lease_id"], Value::Null);
    // Nothing left to send.
    assert_eq!(f.relay().await, RelayReport::default());

    // The operator console dismisses it: version 2, DISMISSED.
    let (status, v) = h
        .http(
            &h.operator,
            CONSOLE_REVIEW,
            json!({"reportId": id, "decision": "DISMISSED", "reason": "NO_VIOLATION", "holdTarget": false}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(f.relay().await.acked, 1);
    let posts = f.posts();
    assert_eq!(posts.len(), 2);
    assert_event(&posts[1], id, "DISMISSED", 2);
    let public = h.public_id(&target).await;
    let text =
        serde_json::to_string(&posts).unwrap() + &serde_json::to_string(&stored(h).await).unwrap();
    assert_ids_only(&text, &[&reporter, &target], &[&public]);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn the_bridge_review_publishes_the_version_it_answered() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (id, reporter, target) = reported(h).await;
    let (status, v) = bridge_review(
        h,
        id,
        json!({"expectedVersion": 1, "decision": "ACTIONED", "reason": "ABUSE", "holdTarget": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(
        (v["status"].as_str(), v["version"].as_str()),
        (Some("ACTIONED"), Some("2"))
    );
    // A refused review (stale version) writes no event.
    let (status, _) = bridge_review(
        h,
        id,
        json!({"expectedVersion": 1, "decision": "DISMISSED", "reason": "NO_VIOLATION"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let rows = stored(h).await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(f.relay().await.acked, 2);
    let posts = f.posts();
    assert_event(&posts[0], id, "OPEN", 1);
    assert_event(&posts[1], id, "ACTIONED", 2);
    let public = h.public_id(&target).await;
    assert_ids_only(
        &serde_json::to_string(&posts).unwrap(),
        &[&reporter, &target],
        &[&public],
    );
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn transient_failures_retry_with_backoff_and_redelivery_is_byte_identical() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (id, _, _) = reported(h).await;
    for status in [503, 429, 401, 404] {
        f.script(&[status]);
        let r = f.relay().await;
        assert_eq!((r.acked, r.retried), (0, 1), "{status}: {r:?}");
        let row = &stored(h).await[0];
        assert_eq!(row["delivery"], "PENDING");
        assert_eq!(row["last_error"], "UNAVAILABLE");
        assert_eq!(row["lease_id"], Value::Null);
        // Backed off: not due again yet.
        assert_eq!(f.relay().await, RelayReport::default(), "{status}");
        due_now(h).await;
    }
    assert_eq!(stored(h).await[0]["attempts"], 4);
    assert_eq!(f.relay().await.acked, 1);
    let first = f.posts()[0].clone();
    assert_event(&first, id, "OPEN", 1);

    // A relay that crashed after posting leaves its lease behind: once the lease lapses,
    // another pass posts the same bytes again and the bridge answers DUPLICATE.
    let (other, _, _) = reported(h).await;
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET lease_id=gen_random_uuid(),lease_until=now()+interval '1 minute' WHERE delivery='PENDING'",
    )
    .await;
    assert_eq!(
        f.relay().await,
        RelayReport::default(),
        "a live lease is respected"
    );
    assert_eq!(f.relay().await.acked, 0);
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET lease_until=now()-interval '1 second' WHERE delivery='PENDING'",
    )
    .await;
    assert_eq!(f.relay().await.acked, 1);
    let posted = f.posts();
    assert_event(&posted[1], other, "OPEN", 1);
    // Re-post of an acknowledged event (as after a crash between post and settle).
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET delivery='PENDING',outcome=NULL,completed_at=NULL WHERE subject_id IN \
          (SELECT subject_id FROM communication_admin_bridge_events ORDER BY occurred_at LIMIT 1)",
    )
    .await;
    assert_eq!(f.relay().await.acked, 1);
    let posted = f.posts();
    assert_eq!(posted[2], first, "byte-identical re-post");
    assert_eq!(stored(h).await[0]["outcome"], "DUPLICATE");
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn an_unreachable_bridge_ends_the_pass_and_keeps_order() {
    let f = Fixture::start().await;
    let h = &f.h;
    reported(h).await;
    reported(h).await;
    f.script(&[503]);
    let r = f.relay().await;
    assert_eq!((r.acked, r.retried), (0, 1), "{r:?}");
    let rows = stored(h).await;
    // The second was not tried: no attempt counted, and it waits with the first.
    assert_eq!(
        (rows[0]["attempts"].as_i64(), rows[1]["attempts"].as_i64()),
        (Some(1), Some(0))
    );
    assert!(rows
        .iter()
        .all(|r| r["delivery"] == "PENDING" && r["lease_id"].is_null()));
    due_now(h).await;
    assert_eq!(f.relay().await.acked, 2);
    f.close().await;
}

#[tokio::test]
async fn contract_refusals_dead_letter() {
    let f = Fixture::start().await;
    let h = &f.h;
    reported(h).await;
    f.script(&[400]);
    let r = f.relay().await;
    assert_eq!(r.dead, 1, "{r:?}");
    let row = &stored(h).await[0];
    assert_eq!(row["delivery"], "DEAD");
    assert_eq!(row["last_error"], "CONTRACT_REJECTED");
    due_now(h).await;
    assert_eq!(f.relay().await, RelayReport::default(), "never sent again");
    f.close().await;
}

#[tokio::test]
async fn replicas_never_post_the_same_event_at_once() {
    let f = Fixture::start().await;
    let h = &f.h;
    for _ in 0..6 {
        reported(h).await;
    }
    f.bridge.lock().unwrap().delay_ms = 50;
    // Two replicas of the Hub relaying at the same time: the lease is taken in the shared
    // database, so each takes rows the other does not hold.
    let (a, b) = tokio::join!(
        relay_due(&h.state, &f.sink, 3),
        relay_due(&h.state, &f.sink, 3)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.acked + b.acked, 6, "{a:?} {b:?}");
    let posts = f.posts();
    let ids: BTreeSet<String> = posts
        .iter()
        .map(|p| p["event_id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!((posts.len(), ids.len()), (6, 6), "each event posted once");
    assert!(stored(h).await.iter().all(|r| r["delivery"] == "ACKED"));
    f.close().await;
}

#[tokio::test]
async fn expired_rows_are_never_sent_and_settled_rows_are_purged() {
    let f = Fixture::start().await;
    let h = &f.h;
    reported(h).await;
    reported(h).await;
    // The bridge was unreachable for longer than an event may wait.
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET expires_at=now()-interval '1 second' WHERE event_id=\
          (SELECT event_id FROM communication_admin_bridge_events ORDER BY occurred_at LIMIT 1)",
    )
    .await;
    let r = f.relay().await;
    assert_eq!((r.expired, r.acked), (1, 1), "{r:?}");
    let rows = stored(h).await;
    assert_eq!(rows[0]["delivery"], "EXPIRED");
    assert_eq!(f.posts().len(), 1);

    // Retention: settled rows older than 30 days go; recent and PENDING rows stay.
    reported(h).await;
    sql(
        h,
        "UPDATE communication_admin_bridge_events SET completed_at=now()-interval '31 days' WHERE delivery='EXPIRED'",
    )
    .await;
    assert_eq!(maintain(&h.state, 1000).await.unwrap(), (0, 1));
    let rows = stored(h).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["delivery"] == "PENDING"));
    // The Hub's own retention sweep runs it as well.
    h.state.maintain().await.unwrap();
    assert_eq!(stored(h).await.len(), 2);
    f.close().await;
}

#[tokio::test]
async fn nothing_is_stored_or_sent_without_the_bridge() {
    let h = Harness::new().await;
    assert!(!h.state.config.admin_bridge_events);
    let (id, _, _) = reported(&h).await;
    let (status, v) = h
        .http(
            &h.operator,
            CONSOLE_REVIEW,
            json!({"reportId": id, "decision": "ACTIONED", "reason": "ABUSE", "holdTarget": false}),
            h.scope.clone(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert!(stored(&h).await.is_empty());
    // A relay pass (were it started) finds nothing and calls nobody: the harness client
    // has no admin bridge target.
    let sink = AdminBridgeEvents::new(h.state.config.client.clone());
    assert_eq!(
        relay_due(&h.state, &sink, 20).await.unwrap(),
        RelayReport::default()
    );
    h.close().await;
}
