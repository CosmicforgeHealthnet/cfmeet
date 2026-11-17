//! Notification Service roadmap N7: the Hub's publications of
//! `communications.connection.requested` (to the addressee) and
//! `communications.connection.connected` (to the requester) — the transactional outbox
//! `communication_notification_publications` and its relay to a fake I1 that verifies the
//! REAL service token (issuer `cfmeet.communications`, audience `cfmeet.notifications`,
//! `notifications-event.publish`) and validates every envelope with
//! packages/notification-client. Real PostgreSQL.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use cf_notification_client::{validate, Envelope};
use cf_service_auth::{
    InternalPermission, PublicJwk, RuntimeEnvironment, ServiceId, ServiceTokenVerifier,
    SystemClock, TrustPolicy,
};
use common::*;
use communications::{
    model::Profile,
    notification_publications::{
        decision_id, enqueue, event_id, maintain, relay_due, ConnectionNotice, Notice,
        NotificationServiceEvents, PublicationReport, INGEST_PATH,
    },
    store::{execute, rows},
};
use serde_json::{json, Value};
use uuid::Uuid;

// --------------------------------------------------------------------------- fake I1

#[derive(Clone, Copy)]
enum Scripted {
    Status(u16),
    /// Every event of the next batch gets this outcome and reason.
    Outcome(&'static str, &'static str),
}

#[derive(Default)]
struct I1 {
    /// Every authenticated request body.
    batches: Vec<Value>,
    script: Vec<Scripted>,
    committed: BTreeSet<String>,
    unauthenticated: usize,
    /// Envelopes refused by the client validator (must stay empty).
    invalid: Vec<String>,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<I1>>,
}

async fn ingest(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let error = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({ "error": { "code": code, "message": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        ctx.inner.lock().unwrap().unauthenticated += 1;
        return error(401, "INTERNAL_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(InternalPermission::NotificationsEventPublish) {
        return error(403, "INTERNAL_PERMISSION_DENIED");
    }
    let issuer = caller.authenticated_service().as_str().to_owned();
    let mut inner = ctx.inner.lock().unwrap();
    inner.batches.push(body.clone());
    // The contract body: `events` (1..=100); the Hub never sends shadow copies.
    let keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    let events = body["events"].as_array().cloned().unwrap_or_default();
    if keys != vec!["events"] || !(1..=100).contains(&events.len()) {
        return error(400, "VALIDATION_ERROR");
    }
    let scripted = (!inner.script.is_empty()).then(|| inner.script.remove(0));
    if let Some(Scripted::Status(status)) = scripted {
        return error(status, "SCRIPTED");
    }
    let mut results = Vec::new();
    for e in events {
        let id = e["eventId"].as_str().unwrap_or_default().to_owned();
        match serde_json::from_value::<Envelope>(e.clone()) {
            Ok(env) if validate(&env, Some(&issuer)).is_ok() => {}
            other => inner.invalid.push(format!("{id}: {other:?}")),
        }
        let (outcome, reason) = match scripted {
            Some(Scripted::Outcome(o, r)) => (o, Some(r)),
            _ if inner.committed.insert(id.clone()) => ("ACCEPTED", None),
            _ => ("DUPLICATE", None),
        };
        let mut r = json!({ "eventId": id, "outcome": outcome });
        if let Some(reason) = reason {
            r["reason"] = json!(reason);
        }
        results.push(r);
    }
    (StatusCode::OK, Json(json!({ "results": results })))
}

struct Fixture {
    h: Harness,
    i1: Arc<Mutex<I1>>,
    sink: NotificationServiceEvents,
}

impl Fixture {
    /// A Hub with `CF_NOTIFICATIONS_BASE_URL` pointing at a fake I1 that trusts the Hub's
    /// real signing key with `notifications-event.publish` only.
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let h = Harness::with_notifications(Some(&url)).await;
        let i1 = Arc::new(Mutex::new(I1::default()));
        let ctx = Arc::new(Ctx {
            verifier: verifier(&h.hub_key),
            inner: i1.clone(),
        });
        let app = Router::new()
            .route(INGEST_PATH, post(ingest))
            .with_state(ctx);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let sink = NotificationServiceEvents::new(h.state.config.client.clone());
        Self { h, i1, sink }
    }

    async fn relay(&self) -> PublicationReport {
        relay_due(&self.h.state, &self.sink, 500).await.unwrap()
    }

    fn batches(&self) -> Vec<Value> {
        self.i1.lock().unwrap().batches.clone()
    }

    fn script(&self, s: &[Scripted]) {
        self.i1.lock().unwrap().script.extend_from_slice(s);
    }

    async fn close(self) {
        self.h.close().await;
    }

    fn assert_valid(&self) {
        let inner = self.i1.lock().unwrap();
        assert!(inner.invalid.is_empty(), "{:?}", inner.invalid);
        assert_eq!(inner.unauthenticated, 0);
    }
}

fn verifier(hub: &PublicJwk) -> ServiceTokenVerifier {
    let bundle = json!({
        "bundle_version": 1,
        "receiver": ServiceId::notifications(),
        "runtime_environment": "test",
        "trusted_issuers": [{
            "issuer": ServiceId::communications(),
            "audience": ServiceId::notifications(),
            "runtime_environment": "test",
            "keys": [hub],
            "permissions": ["notifications-event.publish"],
        }],
    })
    .to_string();
    ServiceTokenVerifier::new(
        TrustPolicy::from_json(
            &bundle,
            &ServiceId::notifications(),
            RuntimeEnvironment::Test,
        )
        .unwrap(),
        Arc::new(SystemClock),
    )
}

// --------------------------------------------------------------------------- helpers

async fn publications(h: &Harness) -> Vec<Value> {
    rows(
        &h.state.store.db,
        "SELECT to_jsonb(p) doc FROM communication_notification_publications p ORDER BY created_at, event_type DESC",
        vec![],
    )
    .await
    .unwrap()
}

async fn sql(h: &Harness, statement: &str) -> u64 {
    execute(&h.state.store.db, statement, vec![]).await.unwrap()
}

/// Makes every waiting publication due now (instead of moving the clock).
async fn due_now(h: &Harness) {
    sql(
        h,
        "UPDATE communication_notification_publications SET next_attempt_at=now()-interval '1 second' WHERE state='PENDING'",
    )
    .await;
}

async fn request(h: &Harness, from: &Profile, to: &Profile) -> Uuid {
    let r = h
        .state
        .send_request(
            &h.scope,
            from,
            json!({"cfMeetId":h.public_id(to).await}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    Uuid::parse_str(r["requestId"].as_str().unwrap()).unwrap()
}

async fn accept(h: &Harness, by: &Profile, request: Uuid, key: Uuid) -> Value {
    h.state
        .decide_request(
            &h.scope,
            by,
            request,
            "accept",
            json!({"expectedVersion":1}),
            key,
        )
        .await
        .unwrap()
}

async fn pair(h: &Harness) -> (Profile, Profile) {
    let a = h.profile(&h.account()).await;
    let b = h.profile(&h.account()).await;
    (a, b)
}

async fn connection_of(h: &Harness, request: Uuid) -> Uuid {
    let v: Vec<Value> = rows(
        &h.state.store.db,
        "SELECT jsonb_build_object('c',connection_id) doc FROM communication_contact_requests WHERE id=$1",
        vec![request.into()],
    )
    .await
    .unwrap();
    Uuid::parse_str(v[0]["c"].as_str().unwrap()).unwrap()
}

/// The ids-only shape of one posted envelope.
#[track_caller]
fn assert_envelope(
    h: &Harness,
    e: &Value,
    event_type: &str,
    request: Uuid,
    connection: Uuid,
    actor: &Profile,
    recipient: &Profile,
) {
    let notice = if event_type.ends_with("requested") {
        ConnectionNotice::Requested
    } else {
        ConnectionNotice::Connected
    };
    assert_eq!(e["eventType"], event_type, "{e}");
    assert_eq!(e["producer"], "cfmeet.communications");
    assert_eq!(e["eventId"], event_id(notice, request));
    assert_eq!(
        e["route"]["decisionId"],
        json!(decision_id(notice, request))
    );
    assert_eq!(e["route"]["routingVersion"], 1);
    assert_eq!(e["route"]["protocol"], 1);
    assert_eq!(
        e["recipients"],
        json!([{ "ref": recipient.account_reference }])
    );
    assert_eq!(
        e["data"],
        json!({ "actorRef": actor.account_reference, "connectionId": connection })
    );
    assert_eq!(e["subject"]["kind"], "CONNECTION");
    assert_eq!(e["subject"]["id"], json!(connection));
    assert_eq!(
        e["scope"],
        json!({"tenantId": h.scope.tenant_id(), "applicationId": h.scope.application_id(),
               "environmentId": h.scope.application_environment_id()})
    );
    assert_eq!(e["delivery"]["priority"], "NORMAL");
    assert_eq!(
        e["delivery"]["dedupeKey"],
        format!("{event_type}:{request}")
    );
    assert_eq!(e["delivery"]["idempotencyKey"], e["eventId"]);
    assert_eq!(e["correlationId"], format!("corr_{}", request.simple()));
    assert!(e.get("sealed").is_none());
    // Ids only: no display name, public id or principal id.
    let text = e.to_string();
    for p in [actor, recipient] {
        assert!(!text.contains(&p.display_name), "{text}");
        assert!(!text.contains(&p.principal_id.to_string()), "{text}");
        assert!(!text.contains(&p.id.to_string()), "{text}");
    }
    assert!(!text.contains("CF-"), "{text}");
}

// ----------------------------------------------------------------------------- tests

#[tokio::test]
async fn requests_and_acceptances_are_published_ids_only_to_the_right_person() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;

    // A asks B: one publication for the addressee, written with the request.
    let req = request(h, &a, &b).await;
    let connection = connection_of(h, req).await;
    let rows = publications(h).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["state"], "PENDING");
    assert_eq!(rows[0]["tenant_id"], json!(h.scope.tenant_id()));
    assert_eq!(
        rows[0]["environment_id"],
        json!(h.scope.application_environment_id())
    );
    let report = f.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");
    let batches = f.batches();
    assert_eq!(batches.len(), 1);
    let posted = &batches[0]["events"][0];
    assert_envelope(
        h,
        posted,
        "communications.connection.requested",
        req,
        connection,
        &a,
        &b,
    );
    // The posted bytes are the stored envelope.
    assert_eq!(&publications(h).await[0]["envelope"], posted);

    // B accepts: one publication for the requester, actor = B.
    accept(h, &b, req, Uuid::new_v4()).await;
    let report = f.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");
    let batches = f.batches();
    assert_eq!(batches.len(), 2);
    assert_envelope(
        h,
        &batches[1]["events"][0],
        "communications.connection.connected",
        req,
        connection,
        &b,
        &a,
    );
    let rows = publications(h).await;
    assert_eq!(rows.len(), 2);
    for r in &rows {
        assert_eq!(r["state"], "ACKED", "{r}");
        assert_eq!(r["outcome"], "ACCEPTED");
        assert_eq!(r["attempts"], 1);
        assert!(r["completed_at"].is_string());
    }
    // Nothing is left to send.
    assert_eq!(f.relay().await, PublicationReport::default());
    assert_eq!(f.batches().len(), 2);
    f.assert_valid();

    // Declines and cancellations notify nobody.
    let (c, d) = pair(h).await;
    let declined = request(h, &c, &d).await;
    h.state
        .decide_request(
            &h.scope,
            &d,
            declined,
            "decline",
            json!({"expectedVersion":1}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    let rows = publications(h).await;
    assert_eq!(rows.len(), 3, "only the request itself: {rows:?}");
    // ... and its request is no longer pending, so the relay withdraws it.
    let report = f.relay().await;
    assert_eq!(report.skipped, 1, "{report:?}");
    assert_eq!(f.batches().len(), 2);
    f.close().await;
}

#[tokio::test]
async fn redelivery_is_idempotent_end_to_end() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;
    // An idempotent replay of the same API request writes nothing more.
    let key = Uuid::new_v4();
    let body = json!({"cfMeetId":h.public_id(&b).await});
    let first = h
        .state
        .send_request(&h.scope, &a, body.clone(), key)
        .await
        .unwrap();
    let again = h.state.send_request(&h.scope, &a, body, key).await.unwrap();
    assert_eq!(first, again);
    assert_eq!(publications(h).await.len(), 1);
    assert_eq!(f.relay().await.acked, 1);
    // A crash after I1 committed but before the Hub settled: the row is PENDING again.
    let crash = "UPDATE communication_notification_publications SET state='PENDING',outcome=NULL,completed_at=NULL \
                 WHERE id=(SELECT id FROM communication_notification_publications ORDER BY created_at DESC LIMIT 1)";
    sql(h, crash).await;
    let report = f.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");

    let request = Uuid::parse_str(first["requestId"].as_str().unwrap()).unwrap();
    let accept_key = Uuid::new_v4();
    accept(h, &b, request, accept_key).await;
    accept(h, &b, request, accept_key).await;
    assert_eq!(publications(h).await.len(), 2);
    assert_eq!(f.relay().await.acked, 1);
    sql(h, crash).await;
    assert_eq!(f.relay().await.acked, 1);

    let batches = f.batches();
    assert_eq!(batches.len(), 4);
    // Byte-identical re-posts, answered DUPLICATE (ACCEPTED semantics).
    assert_eq!(batches[0], batches[1]);
    assert_eq!(batches[2], batches[3]);
    assert_ne!(batches[0], batches[2]);
    for r in publications(h).await {
        assert_eq!(r["state"], "ACKED");
        assert_eq!(r["outcome"], "DUPLICATE");
    }
    // The relay never writes a second row for the same request and type.
    let n = sql(
        h,
        &format!(
            "INSERT INTO communication_notification_publications(id,event_id,event_type,tenant_id,application_id,environment_id,\
               connection_id,request_id,actor_profile_id,recipient_profile_id,state,last_error,completed_at,expires_at) \
             SELECT '{}','evt_{}',event_type,tenant_id,application_id,environment_id,connection_id,request_id,actor_profile_id,\
               recipient_profile_id,'SKIPPED','TEST',now(),now() FROM communication_notification_publications LIMIT 1 \
             ON CONFLICT DO NOTHING",
            Uuid::new_v4(),
            Uuid::new_v4().simple()
        ),
    )
    .await;
    assert_eq!(n, 0);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn transient_failures_retry_with_backoff_and_then_succeed() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;
    request(h, &a, &b).await;
    let mut attempts = 0;
    for status in [500, 503, 401, 403, 408, 429] {
        f.script(&[Scripted::Status(status)]);
        let report = f.relay().await;
        assert_eq!(report.retried, 1, "{status}: {report:?}");
        attempts += 1;
        let row = &publications(h).await[0];
        assert_eq!(row["state"], "PENDING", "{status}");
        assert_eq!(row["attempts"], attempts);
        assert_eq!(row["last_error"], "UNAVAILABLE");
        // Backed off: not due again yet, nothing is posted.
        let before = f.batches().len();
        assert_eq!(f.relay().await, PublicationReport::default());
        assert_eq!(f.batches().len(), before);
        due_now(h).await;
    }
    // The service answers again.
    let report = f.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");
    let row = &publications(h).await[0];
    assert_eq!(row["state"], "ACKED");
    assert_eq!(row["attempts"], 7);
    // Every attempt posted the same bytes.
    let batches = f.batches();
    assert_eq!(batches.len(), 7);
    assert!(batches.windows(2).all(|w| w[0] == w[1]));
    // An answer without this event is retried, never dropped.
    let (c, d) = pair(h).await;
    request(h, &c, &d).await;
    f.script(&[Scripted::Outcome("UNKNOWN", "X")]);
    assert_eq!(f.relay().await.retried, 1);
    due_now(h).await;
    assert_eq!(f.relay().await.acked, 1);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn backoff_doubles_from_two_seconds() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;
    request(h, &a, &b).await;
    for (attempt, seconds) in [(1, 2.0), (2, 4.0), (3, 8.0)] {
        f.script(&[Scripted::Status(500)]);
        assert_eq!(f.relay().await.retried, 1);
        let v: Vec<Value> = rows(
            &h.state.store.db,
            "SELECT jsonb_build_object('wait',EXTRACT(EPOCH FROM next_attempt_at-now())::float8,'attempts',attempts) doc \
               FROM communication_notification_publications",
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(v[0]["attempts"], attempt);
        let wait = v[0]["wait"].as_f64().unwrap();
        assert!(
            (seconds - 1.0..=seconds).contains(&wait),
            "{attempt}: {wait}"
        );
        due_now(h).await;
    }
    f.close().await;
}

#[tokio::test]
async fn contract_refusals_dead_letter() {
    let f = Fixture::start().await;
    let h = &f.h;
    // A 4xx refusal of the batch (a contract error) is never retried.
    for status in [400, 404, 409, 413, 422] {
        let (a, b) = pair(h).await;
        let r = request(h, &a, &b).await;
        f.script(&[Scripted::Status(status)]);
        let report = f.relay().await;
        assert_eq!(report.dead, 1, "{status}: {report:?}");
        let row: Vec<Value> = rows(
            &h.state.store.db,
            "SELECT to_jsonb(p) doc FROM communication_notification_publications p WHERE request_id=$1",
            vec![r.into()],
        )
        .await
        .unwrap();
        assert_eq!(row[0]["state"], "DEAD", "{status}");
        assert_eq!(row[0]["last_error"], "CONTRACT_REJECTED");
        assert!(row[0]["outcome"].is_null());
    }
    // A per-event REJECTED keeps the service's machine reason.
    let (a, b) = pair(h).await;
    let r = request(h, &a, &b).await;
    f.script(&[Scripted::Outcome("REJECTED", "DIGEST_MISMATCH")]);
    assert_eq!(f.relay().await.dead, 1);
    let row: Vec<Value> = rows(
        &h.state.store.db,
        "SELECT to_jsonb(p) doc FROM communication_notification_publications p WHERE request_id=$1",
        vec![r.into()],
    )
    .await
    .unwrap();
    assert_eq!(row[0]["state"], "DEAD");
    assert_eq!(row[0]["outcome"], "REJECTED");
    assert_eq!(row[0]["last_error"], "DIGEST_MISMATCH");
    // HELD (the category is OFF in the service) is stored there: acknowledged.
    let (c, d) = pair(h).await;
    request(h, &c, &d).await;
    f.script(&[Scripted::Outcome("HELD", "CATEGORY_NOT_ACTIVE")]);
    assert_eq!(f.relay().await.acked, 1);
    // Dead letters are never posted again.
    let before = f.batches().len();
    due_now(h).await;
    assert_eq!(f.relay().await, PublicationReport::default());
    assert_eq!(f.batches().len(), before);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn blocked_or_withdrawn_relationships_are_not_published() {
    let f = Fixture::start().await;
    let h = &f.h;
    // A block before the request: the Hub refuses the request, nothing is written.
    let (a, b) = pair(h).await;
    h.state
        .set_block(&b, &a, true, Uuid::new_v4())
        .await
        .unwrap();
    assert!(h
        .state
        .send_request(
            &h.scope,
            &a,
            json!({"cfMeetId":h.public_id(&b).await}),
            Uuid::new_v4()
        )
        .await
        .is_err());
    assert!(publications(h).await.is_empty());

    // A block after the request but before the relay: withdrawn, never posted.
    let (c, d) = pair(h).await;
    let r = request(h, &c, &d).await;
    h.state
        .set_block(&c, &d, true, Uuid::new_v4())
        .await
        .unwrap();
    let report = f.relay().await;
    assert_eq!(report.skipped, 1, "{report:?}");
    let withdrawn: Vec<Value> = rows(
        &h.state.store.db,
        "SELECT to_jsonb(p) doc FROM communication_notification_publications p WHERE request_id=$1",
        vec![r.into()],
    )
    .await
    .unwrap();
    assert_eq!(withdrawn[0]["state"], "SKIPPED");
    assert_eq!(withdrawn[0]["last_error"], "BLOCKED");

    // Cancelled by the requester before the relay: withdrawn.
    let (e, g) = pair(h).await;
    let r = request(h, &e, &g).await;
    h.state
        .decide_request(
            &h.scope,
            &e,
            r,
            "cancel",
            json!({"expectedVersion":1}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
    assert_eq!(f.relay().await.skipped, 1);

    // Accepted, then removed before the relay: the connected notice is withdrawn.
    let (m, n) = pair(h).await;
    let r = request(h, &m, &n).await;
    assert_eq!(f.relay().await.acked, 1);
    accept(h, &n, r, Uuid::new_v4()).await;
    let aliases: Vec<Value> = rows(
        &h.state.store.db,
        "SELECT jsonb_build_object('id',id) doc FROM communication_contact_aliases WHERE owner_profile_id=$1",
        vec![m.id.into()],
    )
    .await
    .unwrap();
    let contact = Uuid::parse_str(aliases[0]["id"].as_str().unwrap()).unwrap();
    h.state
        .remove_contact(&m, contact, Uuid::new_v4())
        .await
        .unwrap();
    let report = f.relay().await;
    assert_eq!(report.skipped, 1, "{report:?}");
    let last = publications(h).await.pop().unwrap();
    assert_eq!(last["event_type"], "communications.connection.connected");
    assert_eq!(last["last_error"], "NOT_CONNECTED");
    // Only the one acknowledged request was ever posted.
    assert_eq!(f.batches().len(), 1);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn people_without_a_cf_meet_account_are_skipped_with_a_reason() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;
    let r = request(h, &a, &b).await;
    let connection = connection_of(h, r).await;
    // A profile whose account is not a CF Meet user account (defensive: Account maps
    // every Hub profile to `cfmeet-user:<uuid>` today).
    let mut other = a.clone();
    other.account_reference = format!("platform-principal:{}", Uuid::new_v4());
    let txn = h.state.store.begin().await.unwrap();
    let written = enqueue(
        &h.state,
        &txn,
        Notice {
            notice: ConnectionNotice::Connected,
            request_id: r,
            connection_id: connection,
            connection_version: 3,
            actor: &b,
            recipient: &other,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    assert!(written);
    let row = publications(h).await.pop().unwrap();
    assert_eq!(row["event_type"], "communications.connection.connected");
    assert_eq!(row["state"], "SKIPPED");
    assert_eq!(row["last_error"], "RECIPIENT_NOT_CF_MEET_ACCOUNT");
    assert!(row["envelope"].is_null());
    // The actor's mapping is required too (`actorRef` drives the X4 recheck).
    let (c, d) = pair(h).await;
    let r2 = request(h, &c, &d).await;
    let mut actor = d.clone();
    actor.account_reference = "not-an-account".into();
    let txn = h.state.store.begin().await.unwrap();
    enqueue(
        &h.state,
        &txn,
        Notice {
            notice: ConnectionNotice::Connected,
            request_id: r2,
            connection_id: connection_of(h, r2).await,
            connection_version: 3,
            actor: &actor,
            recipient: &c,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let row = publications(h).await.pop().unwrap();
    assert_eq!(row["last_error"], "ACTOR_NOT_CF_MEET_ACCOUNT");
    // Only the two real requests are posted.
    let report = f.relay().await;
    assert_eq!(report.acked, 2, "{report:?}");
    assert_eq!(f.batches()[0]["events"].as_array().unwrap().len(), 2);
    f.assert_valid();
    f.close().await;
}

#[tokio::test]
async fn expired_rows_are_never_sent_and_settled_rows_are_purged() {
    let f = Fixture::start().await;
    let h = &f.h;
    let (a, b) = pair(h).await;
    request(h, &a, &b).await;
    let (c, d) = pair(h).await;
    request(h, &c, &d).await;
    // The service was unreachable for longer than a publication may wait.
    sql(
        h,
        "UPDATE communication_notification_publications SET expires_at=now()-interval '1 second' \
          WHERE id=(SELECT id FROM communication_notification_publications ORDER BY created_at LIMIT 1)",
    )
    .await;
    let report = f.relay().await;
    assert_eq!(report.expired, 1, "{report:?}");
    assert_eq!(report.acked, 1, "{report:?}");
    let rows = publications(h).await;
    assert_eq!(rows[0]["state"], "EXPIRED");
    assert_eq!(rows[0]["last_error"], "EXPIRED");
    assert_eq!(f.batches()[0]["events"].as_array().unwrap().len(), 1);

    // Retention: settled rows older than 30 days go; recent and PENDING rows stay.
    let (e, g) = pair(h).await;
    request(h, &e, &g).await;
    sql(
        h,
        "UPDATE communication_notification_publications SET completed_at=now()-interval '31 days' WHERE state='EXPIRED'",
    )
    .await;
    let (expired, purged) = maintain(&h.state, 1000).await.unwrap();
    assert_eq!((expired, purged), (0, 1));
    let rows = publications(h).await;
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|r| r["state"] == "PENDING"));
    // The Hub's own retention sweep runs it as well.
    h.state.maintain().await.unwrap();
    assert_eq!(publications(h).await.len(), 2);
    f.close().await;
}

#[tokio::test]
async fn nothing_is_written_or_sent_without_the_notification_service() {
    let h = Harness::new().await;
    assert!(!h.state.config.notifications_configured);
    let (a, b) = pair(&h).await;
    let r = request(&h, &a, &b).await;
    accept(&h, &b, r, Uuid::new_v4()).await;
    // Today's behaviour exactly: the relationship and its Calling / Messaging events,
    // no publication.
    assert!(publications(&h).await.is_empty());
    let events: Vec<Value> = rows(
        &h.state.store.db,
        "SELECT jsonb_build_object('t',event_type,'d',destination) doc FROM communication_outbox \
          WHERE event_type LIKE 'communications.connection.%' ORDER BY seq",
        vec![],
    )
    .await
    .unwrap();
    assert_eq!(events.len(), 4, "{events:?}");
    // A relay pass (were it started) finds nothing and calls nobody: the harness client
    // has no Notification Service target.
    let sink = NotificationServiceEvents::new(h.state.config.client.clone());
    assert_eq!(
        relay_due(&h.state, &sink, 500).await.unwrap(),
        PublicationReport::default()
    );
    h.close().await;
}
