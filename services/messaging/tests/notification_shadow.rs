//! Notification Service N7b: Messaging's SHADOW (and FENCED_NEW) publications of
//! MESSAGE_WAKEUPS (Document 4 §9.3, §9.5, §5.1) on the real push fan-out path, and the
//! relay to a fake I1 that verifies the REAL service token
//! (`cfmeet.notifications`, `notifications-event.publish`) and validates every envelope
//! with packages/notification-client. Real PostgreSQL.
mod common;

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use cf_notification_client::{validate, Envelope};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, ServiceId, ServiceSigningKey, ServiceTokenSigner, ServiceTokenVerifier,
    SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use common::{flows::*, realtime::cid, *};
use messaging::{
    events::outbox::OutboxEvent,
    identity::{AuthorityUnavailable, OwnerScope, PrincipalDirectory, PrincipalStatus},
    notification_fence::push_key,
    notification_publications::{
        maintain, relay_due, NotificationServiceEvents, PublicationReport, INGEST_PATH, METRIC,
    },
    realtime::push,
};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectionTrait, FromQueryResult, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

// ------------------------------------------------------------------------- fixtures

async fn set_mode(h: &Harness, mode: &str, canary_percent: i16) {
    h.sql(&format!(
        "UPDATE messaging_notification_routing_state \
            SET mode = '{mode}', canary_percent = {canary_percent}, routing_version = routing_version + 1, \
                changed_by_run = 'test', changed_at = now() \
          WHERE category = 'MESSAGE_WAKEUPS'"
    ))
    .await
    .unwrap();
}

/// A principal directory that knows the CF Meet account references of some principals.
struct Accounts {
    inner: Arc<FakePrincipals>,
    references: HashMap<Uuid, String>,
}

#[async_trait]
impl PrincipalDirectory for Accounts {
    async fn resolve(
        &self,
        scope: &OwnerScope,
        principal_id: Uuid,
    ) -> Result<PrincipalStatus, AuthorityUnavailable> {
        self.inner.resolve(scope, principal_id).await
    }

    async fn native_user_references(
        &self,
        _scope: &OwnerScope,
        principals: &[Uuid],
    ) -> Result<HashMap<Uuid, String>, AuthorityUnavailable> {
        if *self.inner.down.lock().unwrap() {
            return Err(AuthorityUnavailable("down".into()));
        }
        Ok(principals
            .iter()
            .filter_map(|p| self.references.get(p).map(|r| (*p, r.clone())))
            .collect())
    }
}

struct Fixture {
    c: Customer,
    conversation: Value,
    /// The sender first; every member has one push-registered device.
    people: Vec<Member>,
    /// The CF Meet account of each recipient (the sender has one too).
    accounts: HashMap<Uuid, String>,
    /// CF Meet's personal scope, governed by the Communications Hub. Set only while the
    /// relay runs (sending would ask the Hub's policy, which these tests do not run).
    governed: cf_communications_policy::PolicyClient,
}

impl Fixture {
    fn sender(&self) -> Uuid {
        self.people[0].principal
    }

    /// The account references the envelope must carry (recipients, sorted).
    fn recipient_refs(&self) -> Vec<String> {
        let mut refs: Vec<String> = self.people[1..]
            .iter()
            .map(|m| self.accounts[&m.principal].clone())
            .collect();
        refs.sort();
        refs
    }
}

/// A MANAGED GROUP of three members with push-registered devices, in CF Meet's governed
/// personal scope (where principals map to `cfmeet-user:` accounts, as for X3).
async fn fixture(h: &mut Harness) -> Fixture {
    let c = h.customer(ALL_SCOPES);
    h.allow_managed(&c, "GROUP").await;
    let mut people = Vec::new();
    for _ in 0..3 {
        let m = h.member(&c, 0).await;
        let r = h
            .put(
                &c,
                Some(m.principal),
                &format!("/v1/messaging/devices/{}/push-registration", m.device_id()),
                json!({ "platform": "LOCAL_TEST", "handle": format!("shadow-test-{}", m.principal) }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        people.push(m);
    }
    let r = h
        .create_conversation(
            &c,
            people[0].principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    for p in &people[1..] {
        let a = h
            .post(
                &c,
                Some(people[0].principal),
                &format!("/v1/conversations/{}/members", cid(&conversation)),
                json!({ "principalId": p.principal }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    let accounts: HashMap<Uuid, String> = people
        .iter()
        .map(|m| (m.principal, format!("cfmeet-user:{}", Uuid::new_v4())))
        .collect();
    h.state.principals = Arc::new(Accounts {
        inner: h.principals.clone(),
        references: accounts.clone(),
    });
    let governed = cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    );
    Fixture {
        c,
        conversation,
        people,
        accounts,
        governed,
    }
}

/// The committed `messaging.message.created` event of the latest message.
async fn latest_message_event(h: &Harness) -> OutboxEvent {
    OutboxEvent::find_by_statement(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        "SELECT id, sequence, event_type, schema_version, aggregate_type, aggregate_id, tenant_id, \
                application_id, environment_id, correlation_id, causation_id, payload, occurred_at, \
                attempts, conversation_id, conversation_event_sequence \
           FROM messaging_outbox_events WHERE event_type = 'messaging.message.created' \
          ORDER BY sequence DESC LIMIT 1"
            .to_owned(),
    ))
    .one(h.conn())
    .await
    .unwrap()
    .unwrap()
}

async fn send(h: &Harness, f: &Fixture, text: &str) -> OutboxEvent {
    let r = h
        .send_managed(&f.c, &f.conversation, f.sender(), text)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    latest_message_event(h).await
}

async fn count(h: &Harness, sql: &str) -> i64 {
    h.scalar(sql).await
}

async fn jobs_of(h: &Harness, e: &OutboxEvent) -> i64 {
    count(
        h,
        &format!(
            "SELECT count(*) FROM messaging_push_jobs WHERE event_id = '{}'",
            e.id
        ),
    )
    .await
}

async fn decisions_of(h: &Harness, e: &OutboxEvent) -> i64 {
    count(
        h,
        &format!(
            "SELECT count(*) FROM messaging_notification_route_decisions WHERE idempotency_key = '{}'",
            push_key(e.id)
        ),
    )
    .await
}

#[derive(Debug, FromQueryResult)]
struct Publication {
    event_id: String,
    decision_id: Uuid,
    shadow: bool,
    state: String,
    outcome: Option<String>,
    last_error: Option<String>,
    attempts: i32,
    envelope: Option<Value>,
}

async fn publications_of(h: &Harness, e: &OutboxEvent) -> Vec<Publication> {
    Publication::find_by_statement(Statement::from_string(
        sea_orm::DbBackend::Postgres,
        format!(
            "SELECT event_id, decision_id, shadow, state, outcome, last_error, attempts, envelope \
               FROM messaging_notification_publications WHERE outbox_event_id = '{}'",
            e.id
        ),
    ))
    .all(h.conn())
    .await
    .unwrap()
}

async fn publication(h: &Harness, e: &OutboxEvent) -> Publication {
    let mut all = publications_of(h, e).await;
    assert_eq!(all.len(), 1, "{all:?}");
    all.remove(0)
}

async fn decision(h: &Harness, e: &OutboxEvent) -> (Uuid, String, Option<String>) {
    let row = h
        .conn()
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT decision_id, route, ns_acked_at::text AS acked FROM messaging_notification_route_decisions \
                  WHERE idempotency_key = '{}'",
                push_key(e.id)
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    (
        row.try_get("", "decision_id").unwrap(),
        row.try_get("", "route").unwrap(),
        row.try_get("", "acked").unwrap(),
    )
}

/// Makes every waiting publication due now (instead of moving the clock).
async fn due_now(h: &Harness) {
    h.sql("UPDATE messaging_notification_publications SET next_attempt_at = created_at WHERE state = 'PENDING'")
        .await
        .unwrap();
}

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

#[derive(Clone)]
struct FakeI1 {
    url: String,
    inner: Arc<Mutex<I1>>,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<I1>>,
}

async fn ingest(
    State(ctx): State<Arc<Ctx>>,
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
    // The contract body: `events` (1..=100) and the optional `shadow` (true only).
    let keys: BTreeSet<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let events = body["events"].as_array().cloned().unwrap_or_default();
    if !keys.is_subset(&BTreeSet::from(["events", "shadow"]))
        || body.get("shadow").is_some_and(|s| s != &json!(true))
        || !(1..=100).contains(&events.len())
    {
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

impl FakeI1 {
    /// The Notification Service's I1, trusting Messaging's `key` with
    /// `notifications-event.publish` only.
    async fn start(key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::NOTIFICATIONS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::MESSAGING,
                "audience": cf_service_auth::services::NOTIFICATIONS,
                "runtime_environment": ENV,
                "keys": [key.public_jwk()],
                "permissions": ["notifications-event.publish"],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(&bundle, &ServiceId::notifications(), ENV).unwrap();
        let clock: SharedClock = Arc::new(SystemClock);
        let inner = Arc::new(Mutex::new(I1::default()));
        let ctx = Arc::new(Ctx {
            verifier: ServiceTokenVerifier::new(policy, clock),
            inner: inner.clone(),
        });
        let app = Router::new()
            .route(INGEST_PATH, post(ingest))
            .with_state(ctx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url, inner }
    }

    fn batches(&self) -> Vec<Value> {
        self.inner.lock().unwrap().batches.clone()
    }

    fn script(&self, s: &[Scripted]) {
        self.inner.lock().unwrap().script.extend_from_slice(s);
    }
}

/// Messaging's outbound client signing as `cfmeet.messaging` with `key`, with a
/// Notification Service target at `url` (or none).
fn client(key: ServiceSigningKey, url: Option<&str>) -> Arc<InternalServiceClient> {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::messaging(),
        ENV,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(ENV);
    if let Some(url) = url {
        targets.insert(ServiceId::notifications(), url).unwrap();
    }
    Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    )
}

fn copy(k: &ServiceSigningKey) -> ServiceSigningKey {
    ServiceSigningKey::from_pkcs8_pem(k.kid().clone(), &k.to_pkcs8_pem()).unwrap()
}

/// One relay pass; `governed`: inside CF Meet's governed personal scope (account
/// mapping available), as for X3.
async fn relay(
    h: &mut Harness,
    governed: Option<&Fixture>,
    sink: &NotificationServiceEvents,
) -> PublicationReport {
    h.state.communications = governed.map(|f| f.governed.clone());
    let report = relay_due(&h.state, sink, 500).await.unwrap();
    h.state.communications = None;
    report
}

// --------------------------------------------------------------------------- tests

/// LEGACY_ONLY (the production state): no decision, no publication, and the push jobs
/// are exactly today's — the same rows as on a database without the fence history.
#[tokio::test]
async fn legacy_only_writes_no_fence_rows_and_identical_push_jobs() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    let e = send(&h, &f, "legacy").await;
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 2);
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 0);
    assert_eq!(decisions_of(&h, &e).await, 0);
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM messaging_notification_route_decisions"
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM messaging_notification_publications"
        )
        .await,
        0
    );
    let payloads = |h: &Harness, e: &OutboxEvent| {
        let sql = format!(
            "SELECT payload FROM messaging_push_jobs WHERE event_id = '{}' ORDER BY device_id",
            e.id
        );
        let conn = h.conn().clone();
        async move {
            conn.query_all(Statement::from_string(sea_orm::DbBackend::Postgres, sql))
                .await
                .unwrap()
                .iter()
                .map(|r| r.try_get::<Value>("", "payload").unwrap())
                .collect::<Vec<_>>()
        }
    };
    let fenced = payloads(&h, &e).await;
    assert_eq!(fenced.len(), 2);
    for p in &fenced {
        assert_eq!(
            p,
            &json!({
                "v": 1, "type": "message", "conversationId": cid(&f.conversation),
                "eventSequence": e.conversation_event_sequence.unwrap(),
                "sequence": e.payload["sequence"],
            })
        );
    }

    // A database without the routing fence history (pre-fence): the same jobs.
    let db = Arc::new(TestDb::empty().await.unwrap());
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    let mut plain = Harness::on(db, |_| {});
    let g = fixture(&mut plain).await;
    let e2 = send(&plain, &g, "legacy").await;
    assert_eq!(push::fanout_jobs(&plain.state, &e2).await.unwrap(), 2);
    let unfenced = payloads(&plain, &e2).await;
    assert_eq!(unfenced.len(), 2);
    assert_eq!(
        unfenced[0].as_object().unwrap().keys().collect::<Vec<_>>(),
        fenced[0].as_object().unwrap().keys().collect::<Vec<_>>()
    );
}

/// SHADOW: an OLD decision, today's push jobs AND one shadow publication per event (not
/// per device); a retry follows the decision and writes nothing twice.
#[tokio::test]
async fn shadow_writes_old_decision_push_jobs_and_one_shadow_publication() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    set_mode(&h, "SHADOW", 0).await;
    let e = send(&h, &f, "shadowed").await;
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 2);
    let (decision_id, route, acked) = decision(&h, &e).await;
    assert_eq!((route.as_str(), acked), ("OLD", None));
    let p = publication(&h, &e).await;
    assert_eq!(
        (
            p.event_id.as_str(),
            p.decision_id,
            p.shadow,
            p.state.as_str(),
            p.attempts
        ),
        (push_key(e.id).as_str(), decision_id, true, "PENDING", 0)
    );
    assert!(p.envelope.is_none());

    // Retries (outbox redelivery), also after the mode moved on: sticky, nothing twice.
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 0);
    set_mode(&h, "FENCED_OLD", 0).await;
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 0);
    assert_eq!((jobs_of(&h, &e).await, decisions_of(&h, &e).await), (2, 1));
    assert_eq!(publications_of(&h, &e).await.len(), 1);

    // An event decided under FENCED_OLD gets no shadow copy when retried in SHADOW.
    let e2 = send(&h, &f, "fenced first").await;
    assert_eq!(push::fanout_jobs(&h.state, &e2).await.unwrap(), 2);
    set_mode(&h, "SHADOW", 0).await;
    assert_eq!(push::fanout_jobs(&h.state, &e2).await.unwrap(), 0);
    assert!(publications_of(&h, &e2).await.is_empty());
}

/// FENCED_OLD: OLD decision and push jobs, never a publication.
#[tokio::test]
async fn fenced_old_publishes_nothing() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    set_mode(&h, "FENCED_OLD", 100).await;
    let e = send(&h, &f, "fenced old").await;
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 2);
    assert_eq!(decision(&h, &e).await.1, "OLD");
    assert!(publications_of(&h, &e).await.is_empty());
    assert_eq!(
        count(
            &h,
            "SELECT count(*) FROM messaging_notification_publications"
        )
        .await,
        0
    );
}

/// FENCED_NEW (not reachable in production yet): inside the canary a NEW decision, no
/// push jobs and one non-shadow publication; outside it, the legacy path.
#[tokio::test]
async fn fenced_new_routes_to_the_service_without_push_jobs() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    set_mode(&h, "FENCED_NEW", 100).await;
    let e = send(&h, &f, "new route").await;
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 0);
    let (decision_id, route, _) = decision(&h, &e).await;
    assert_eq!(route, "NEW");
    let p = publication(&h, &e).await;
    assert_eq!(
        (p.decision_id, p.shadow, p.state.as_str()),
        (decision_id, false, "PENDING")
    );
    // Retried: still no jobs, one publication.
    assert_eq!(push::fanout_jobs(&h.state, &e).await.unwrap(), 0);
    assert_eq!(
        (jobs_of(&h, &e).await, publications_of(&h, &e).await.len()),
        (0, 1)
    );
    // Outside the canary: OLD, jobs, no publication.
    set_mode(&h, "FENCED_NEW", 0).await;
    let e2 = send(&h, &f, "old route").await;
    assert_eq!(push::fanout_jobs(&h.state, &e2).await.unwrap(), 2);
    assert_eq!(decision(&h, &e2).await.1, "OLD");
    assert!(publications_of(&h, &e2).await.is_empty());
}

/// The relay posts the right envelope with a real signed token, acknowledges ACCEPTED and
/// DUPLICATE, retries 503, dead-letters 400 and REJECTED, and records the
/// acknowledgement of a NEW decision (§9.5). Envelopes are ids only.
#[tokio::test]
async fn relay_posts_signed_envelopes_and_settles_every_outcome() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    let key = cf_service_auth::KeyId::parse("messaging-n7b-1").unwrap();
    let key = ServiceSigningKey::generate(key);
    let i1 = FakeI1::start(&key).await;
    let sink = NotificationServiceEvents::new(client(copy(&key), Some(&i1.url)));
    let dead_before =
        messaging::metrics::counter(METRIC, &[("outcome", "dead"), ("kind", "shadow")]);

    // SHADOW: posted as a shadow batch, acknowledged; the decision stays OLD / unacked.
    set_mode(&h, "SHADOW", 0).await;
    const TEXT: &str = "the-message-text-never-leaves-messaging";
    let e = send(&h, &f, TEXT).await;
    push::fanout_jobs(&h.state, &e).await.unwrap();
    let report = relay(&mut h, Some(&f), &sink).await;
    assert_eq!((report.acked, report.retried, report.dead), (1, 0, 0));
    let batches = i1.batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0]["shadow"], true);
    let env = &batches[0]["events"][0];
    let (decision_id, _, acked) = decision(&h, &e).await;
    assert_eq!(acked, None);
    let routing_version = h
        .scalar("SELECT routing_version::bigint FROM messaging_notification_routing_state")
        .await;
    assert_eq!(env["eventId"], json!(push_key(e.id)));
    assert_eq!(env["eventType"], "messaging.message.created");
    assert_eq!(env["producer"], "cfmeet.messaging");
    assert_eq!(env["correlationId"], json!(e.correlation_id));
    assert_eq!(
        env["route"],
        json!({ "decisionId": decision_id, "routingVersion": routing_version, "protocol": 1 })
    );
    assert_eq!(
        env["scope"],
        json!({
            "tenantId": f.c.context.scope.tenant_id,
            "applicationId": f.c.context.scope.application_id,
            "environmentId": f.c.context.scope.environment_id,
        })
    );
    assert_eq!(
        env["subject"],
        json!({ "kind": "CONVERSATION", "id": cid(&f.conversation) })
    );
    assert_eq!(
        env["data"],
        json!({
            "conversationId": cid(&f.conversation),
            "conversationKind": "GROUP",
            "eventSequence": e.conversation_event_sequence.unwrap(),
            "securityProfile": "MANAGED",
        })
    );
    let refs: Vec<String> = env["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["ref"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(refs, f.recipient_refs());
    assert!(!refs.contains(&f.accounts[&f.sender()]));
    assert_eq!(env["delivery"]["priority"], "HIGH");
    assert!(env.get("sealed").is_none());
    let text = env.to_string();
    assert!(!text.contains(TEXT), "{text}");
    for word in ["ciphertext", "plaintext", "\"text\"", "body", "content"] {
        assert!(!text.contains(word), "{word}: {text}");
    }
    let p = publication(&h, &e).await;
    assert_eq!(
        (p.state.as_str(), p.outcome.as_deref(), p.attempts),
        ("ACKED", Some("ACCEPTED"), 1)
    );
    assert_eq!(p.envelope.as_ref(), Some(env));

    // A relay crash after the service committed: the re-post is byte-identical and the
    // DUPLICATE answer acknowledges it.
    h.sql(&format!(
        "UPDATE messaging_notification_publications SET state = 'PENDING', outcome = NULL, completed_at = NULL \
          WHERE event_id = '{}'",
        push_key(e.id)
    ))
    .await
    .unwrap();
    assert_eq!(relay(&mut h, Some(&f), &sink).await.acked, 1);
    let batches = i1.batches();
    assert_eq!(batches[1]["events"][0], batches[0]["events"][0]);
    assert_eq!(
        publication(&h, &e).await.outcome.as_deref(),
        Some("DUPLICATE")
    );

    // 503: retried with backoff (not due again at once), then acknowledged.
    let e2 = send(&h, &f, "two").await;
    push::fanout_jobs(&h.state, &e2).await.unwrap();
    i1.script(&[Scripted::Status(503)]);
    let report = relay(&mut h, Some(&f), &sink).await;
    assert_eq!((report.acked, report.retried), (0, 1));
    let p = publication(&h, &e2).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref(), p.attempts),
        ("PENDING", Some("UNAVAILABLE"), 1)
    );
    assert_eq!(
        relay(&mut h, Some(&f), &sink).await,
        PublicationReport::default()
    );
    due_now(&h).await;
    assert_eq!(relay(&mut h, Some(&f), &sink).await.acked, 1);
    let p = publication(&h, &e2).await;
    assert_eq!((p.state.as_str(), p.attempts), ("ACKED", 2));

    // 400 (a contract error): dead-lettered, counted, never retried.
    let e3 = send(&h, &f, "three").await;
    push::fanout_jobs(&h.state, &e3).await.unwrap();
    i1.script(&[Scripted::Status(400)]);
    assert_eq!(relay(&mut h, Some(&f), &sink).await.dead, 1);
    let p = publication(&h, &e3).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref()),
        ("DEAD", Some("CONTRACT_REJECTED"))
    );
    due_now(&h).await;
    let sent = i1.batches().len();
    assert_eq!(
        relay(&mut h, Some(&f), &sink).await,
        PublicationReport::default()
    );
    assert_eq!(i1.batches().len(), sent);
    // A per-event refusal: dead-lettered with the service's reason.
    let e4 = send(&h, &f, "four").await;
    push::fanout_jobs(&h.state, &e4).await.unwrap();
    i1.script(&[Scripted::Outcome("REJECTED", "TYPE_NOT_ALLOWED")]);
    assert_eq!(relay(&mut h, Some(&f), &sink).await.dead, 1);
    let p = publication(&h, &e4).await;
    assert_eq!(
        (
            p.state.as_str(),
            p.outcome.as_deref(),
            p.last_error.as_deref()
        ),
        ("DEAD", Some("REJECTED"), Some("TYPE_NOT_ALLOWED"))
    );
    assert_eq!(
        messaging::metrics::counter(METRIC, &[("outcome", "dead"), ("kind", "shadow")])
            - dead_before,
        2
    );

    // FENCED_NEW: a normal batch (no shadow flag); ACCEPTED records the responsibility
    // boundary on the NEW decision.
    set_mode(&h, "FENCED_NEW", 100).await;
    let e5 = send(&h, &f, "five").await;
    assert_eq!(push::fanout_jobs(&h.state, &e5).await.unwrap(), 0);
    assert_eq!(relay(&mut h, Some(&f), &sink).await.acked, 1);
    let last = i1.batches().pop().unwrap();
    assert!(last.get("shadow").is_none(), "{last}");
    assert_eq!(last["events"][0]["eventId"], json!(push_key(e5.id)));
    let (_, route, acked) = decision(&h, &e5).await;
    assert_eq!(route, "NEW");
    assert!(acked.is_some());
    // HELD is stored by the service: acknowledged too.
    let e6 = send(&h, &f, "six").await;
    push::fanout_jobs(&h.state, &e6).await.unwrap();
    i1.script(&[Scripted::Outcome("HELD", "CATEGORY_NOT_ACTIVE")]);
    assert_eq!(relay(&mut h, Some(&f), &sink).await.acked, 1);
    assert_eq!(publication(&h, &e6).await.outcome.as_deref(), Some("HELD"));

    // Every envelope passed the client validator with the authenticated issuer, and no
    // request was unauthenticated.
    {
        let inner = i1.inner.lock().unwrap();
        assert!(inner.invalid.is_empty(), "{:?}", inner.invalid);
        assert_eq!(inner.unauthenticated, 0);
    }

    // The backlog is visible on the metrics surface.
    let metrics = messaging::metrics::render(&h.state).await;
    assert!(
        metrics.contains("messaging_notification_publications{state=\"ACKED\"} 4"),
        "{metrics}"
    );
    assert!(metrics.contains("messaging_notification_publications{state=\"DEAD\"} 2"));
    assert!(metrics.contains("messaging_notification_publications_oldest_pending_seconds 0"));
}

/// A Messaging key the service does not trust is refused (401) and retried; nothing is
/// acknowledged.
#[tokio::test]
async fn relay_retries_when_the_service_refuses_the_token() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    let trusted =
        ServiceSigningKey::generate(cf_service_auth::KeyId::parse("messaging-n7b-2").unwrap());
    let rogue =
        ServiceSigningKey::generate(cf_service_auth::KeyId::parse("messaging-rogue-1").unwrap());
    let i1 = FakeI1::start(&trusted).await;
    let sink = NotificationServiceEvents::new(client(rogue, Some(&i1.url)));
    set_mode(&h, "SHADOW", 0).await;
    let e = send(&h, &f, "rogue").await;
    push::fanout_jobs(&h.state, &e).await.unwrap();
    let report = relay(&mut h, Some(&f), &sink).await;
    assert_eq!((report.acked, report.retried), (0, 1));
    assert_eq!(i1.inner.lock().unwrap().unauthenticated, 1);
    assert!(i1.batches().is_empty());
    let p = publication(&h, &e).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref()),
        ("PENDING", Some("UNAVAILABLE"))
    );
}

/// Unconfigured (no `CF_NOTIFICATIONS_BASE_URL`): the mode alone decides what is
/// enqueued; nothing is sent and rows wait without counting attempts. The setting maps
/// to the Notification Service target only when present.
#[tokio::test]
async fn relay_is_off_when_the_service_is_not_configured() {
    use messaging::config::service_auth::ServiceAuthSettings;
    let base = |extra: Option<(&'static str, &'static str)>| {
        move |k: &str| -> Option<String> {
            let mut pairs = vec![
                ("CF_RUNTIME_ENVIRONMENT", "test"),
                ("CF_SERVICE_AUTH_KEY_ID", "messaging-test-1"),
                (
                    "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                    "/run/secrets/messaging.pem",
                ),
                ("CF_SERVICE_TRUST_BUNDLE_FILE", "/etc/cfmeet/trust.json"),
            ];
            pairs.extend(extra);
            pairs
                .into_iter()
                .find(|(var, _)| *var == k)
                .map(|(_, v)| v.to_string())
        }
    };
    let targets = |s: ServiceAuthSettings| -> Vec<String> {
        s.targets
            .iter()
            .map(|(id, _)| id.as_str().to_owned())
            .collect()
    };
    assert!(
        !targets(ServiceAuthSettings::from_lookup(&base(None)).unwrap())
            .contains(&"cfmeet.notifications".to_owned())
    );
    assert_eq!(
        targets(
            ServiceAuthSettings::from_lookup(&base(Some((
                "CF_NOTIFICATIONS_BASE_URL",
                "http://notifications:8080"
            ))))
            .unwrap()
        ),
        vec!["cfmeet.notifications".to_owned()]
    );

    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    set_mode(&h, "SHADOW", 0).await;
    let e = send(&h, &f, "unconfigured").await;
    push::fanout_jobs(&h.state, &e).await.unwrap();
    let key =
        ServiceSigningKey::generate(cf_service_auth::KeyId::parse("messaging-n7b-3").unwrap());
    let sink = NotificationServiceEvents::new(client(key, None));
    let report = relay(&mut h, Some(&f), &sink).await;
    assert_eq!(
        (report.acked, report.retried, report.not_configured),
        (0, 0, 1)
    );
    let p = publication(&h, &e).await;
    assert_eq!(
        (p.state.as_str(), p.attempts, p.last_error.as_deref()),
        ("PENDING", 0, Some("NOT_CONFIGURED"))
    );
    // LEGACY_ONLY with the relay unconfigured: nothing enqueued at all.
    set_mode(&h, "LEGACY_ONLY", 0).await;
    let e2 = send(&h, &f, "legacy").await;
    assert_eq!(push::fanout_jobs(&h.state, &e2).await.unwrap(), 2);
    assert!(publications_of(&h, &e2).await.is_empty());
}

/// Rows the relay cannot or must not post: recipients without a CF Meet account
/// (outside the governed scope) are SKIPPED, a directory outage retries, an expired row
/// is never posted. The database refuses an envelope carrying content or a sealed part.
#[tokio::test]
async fn unpublishable_rows_are_skipped_retried_or_expired_and_content_is_refused() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = fixture(&mut h).await;
    let key =
        ServiceSigningKey::generate(cf_service_auth::KeyId::parse("messaging-n7b-4").unwrap());
    let i1 = FakeI1::start(&key).await;
    let sink = NotificationServiceEvents::new(client(copy(&key), Some(&i1.url)));
    set_mode(&h, "SHADOW", 0).await;

    // The Platform Access directory is down: retried, nothing posted.
    let e1 = send(&h, &f, "one").await;
    push::fanout_jobs(&h.state, &e1).await.unwrap();
    *h.principals.down.lock().unwrap() = true;
    assert_eq!(relay(&mut h, Some(&f), &sink).await.retried, 1);
    let p = publication(&h, &e1).await;
    assert_eq!(
        (
            p.state.as_str(),
            p.last_error.as_deref(),
            p.envelope.is_none()
        ),
        ("PENDING", Some("PRINCIPALS_UNAVAILABLE"), true)
    );
    *h.principals.down.lock().unwrap() = false;

    // Outside CF Meet's governed scope no principal maps to an account: SKIPPED.
    due_now(&h).await;
    assert_eq!(relay(&mut h, None, &sink).await.skipped, 1);
    let p = publication(&h, &e1).await;
    assert_eq!(
        (p.state.as_str(), p.last_error.as_deref()),
        ("SKIPPED", Some("NO_ACCOUNT_RECIPIENTS"))
    );

    // Expired before the relay ran: EXPIRED, never posted.
    let e2 = send(&h, &f, "two").await;
    push::fanout_jobs(&h.state, &e2).await.unwrap();
    h.sql("UPDATE messaging_notification_publications SET expires_at = created_at WHERE state = 'PENDING'")
        .await
        .unwrap();
    assert_eq!(relay(&mut h, Some(&f), &sink).await.expired, 1);
    assert_eq!(publication(&h, &e2).await.state, "EXPIRED");
    assert!(i1.batches().is_empty());
    // Maintenance (with or without the relay): settled rows go after the retention
    // period, pending ones never; a database without the fence history is a no-op.
    h.sql("UPDATE messaging_notification_publications SET completed_at = completed_at - interval '31 days'")
        .await
        .unwrap();
    let e2b = send(&h, &f, "two-b").await;
    push::fanout_jobs(&h.state, &e2b).await.unwrap();
    assert_eq!(maintain(&h.state, 100).await.unwrap(), (0, 2));
    assert!(publications_of(&h, &e2).await.is_empty());
    assert_eq!(publication(&h, &e2b).await.state, "PENDING");
    let db = Arc::new(TestDb::empty().await.unwrap());
    messaging_migration::Migrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::GapClosureMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::MlsJoinMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::MemberNotificationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::ChatControlsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    messaging_migration::AdminOperationsMigrator::up(&db.conn, None)
        .await
        .unwrap();
    let plain = Harness::on(db, |_| {});
    assert_eq!(maintain(&plain.state, 100).await.unwrap(), (0, 0));

    // The database refuses an envelope with any other data key or a sealed part.
    let e3 = send(&h, &f, "three").await;
    push::fanout_jobs(&h.state, &e3).await.unwrap();
    let event_id = push_key(e3.id);
    for bad in [
        json!({ "eventId": event_id, "data": { "conversationId": "x", "text": "hello" } }),
        json!({ "eventId": event_id, "data": { "ciphertext": "AAAA" } }),
        json!({ "eventId": event_id, "data": {}, "sealed": { "kid": "k" } }),
        json!({ "eventId": "evt_00000000000000000000000000000000", "data": {} }),
    ] {
        let r = h
            .sql(&format!(
                "UPDATE messaging_notification_publications SET envelope = '{bad}'::jsonb WHERE event_id = '{event_id}'"
            ))
            .await;
        assert!(
            r.as_ref()
                .is_err_and(|e| e.contains("ck_messaging_notification_publications_envelope")),
            "{bad}: {r:?}"
        );
    }
}

// ------------------------------------------------------- gap audit S1: organization chats

/// A MANAGED GROUP of three members in an ORGANIZATION's own first-party scope (as the
/// Meeting Service's organization relay creates them). Like real organization members,
/// nobody has a push registration in that scope: their devices register once, in CF
/// Meet's personal scope, which `governed` names (a different first-party scope).
async fn organization_fixture(h: &mut Harness) -> Fixture {
    let c = h.first_party(true);
    h.allow_managed(&c, "GROUP").await;
    let mut people = Vec::new();
    for _ in 0..3 {
        people.push(h.member(&c, 0).await);
    }
    let r = h
        .create_conversation(
            &c,
            people[0].principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    for p in &people[1..] {
        let a = h
            .post(
                &c,
                Some(people[0].principal),
                &format!("/v1/conversations/{}/members", cid(&conversation)),
                json!({ "principalId": p.principal }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    let accounts: HashMap<Uuid, String> = people
        .iter()
        .map(|m| (m.principal, format!("cfmeet-user:{}", Uuid::new_v4())))
        .collect();
    h.state.principals = Arc::new(Accounts {
        inner: h.principals.clone(),
        references: accounts.clone(),
    });
    let personal = h.first_party(true);
    let governed = cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: personal.context.scope.tenant_id,
            application_id: personal.context.scope.application_id,
            environment_id: personal.context.scope.environment_id,
        },
    );
    // CF Meet's personal scope is configured (it does not govern the organization's).
    h.state.communications = Some(governed.clone());
    Fixture {
        c,
        conversation,
        people,
        accounts,
        governed,
    }
}

/// Gap audit S1: an organization chat's wake-up reaches nobody on the legacy path (no
/// registration in the organization's scope). With `MESSAGING_ORGANIZATION_WAKEUPS` on it
/// is routed NEW in every fence mode — even LEGACY_ONLY — and published to the
/// Notification Service addressed to the members' CF Meet accounts (ids only, never the
/// text); never both. Off, nothing changes.
#[tokio::test]
async fn organization_chats_wake_members_through_the_service_only_when_enabled() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let f = organization_fixture(&mut h).await;
    const TEXT: &str = "organization-text-never-leaves-messaging";

    // The event records the authenticated classification for delayed workers; customer
    // webhook envelopes never carry it.
    let off = send(&h, &f, TEXT).await;
    assert_eq!(off.payload["first_party"], true);
    assert!(off.envelope("test")["payload"].get("first_party").is_none());

    // Switch off (default), LEGACY_ONLY: today's behaviour — no decision, no publication,
    // and (nobody registered here) no push job.
    assert_eq!(push::fanout_jobs(&h.state, &off).await.unwrap(), 0);
    assert_eq!(decisions_of(&h, &off).await, 0);
    assert!(publications_of(&h, &off).await.is_empty());

    // Switch on: routed NEW even in LEGACY_ONLY, one publication, no legacy jobs.
    h.state.organization_wakeups = true;
    let on = send(&h, &f, TEXT).await;
    assert_eq!(push::fanout_jobs(&h.state, &on).await.unwrap(), 0);
    let (decision_id, route, _) = decision(&h, &on).await;
    assert_eq!(route, "NEW");
    let p = publication(&h, &on).await;
    assert_eq!((p.decision_id, p.shadow), (decision_id, false));
    assert_eq!(jobs_of(&h, &on).await, 0);
    // Sticky and idempotent.
    assert_eq!(push::fanout_jobs(&h.state, &on).await.unwrap(), 0);
    assert_eq!(publications_of(&h, &on).await.len(), 1);

    // Also in FENCED_OLD (where personal chats stay legacy).
    set_mode(&h, "FENCED_OLD", 0).await;
    let fenced = send(&h, &f, TEXT).await;
    push::fanout_jobs(&h.state, &fenced).await.unwrap();
    assert_eq!(decision(&h, &fenced).await.1, "NEW");

    // The relay addresses the members' accounts (never the sender), ids only.
    let key = cf_service_auth::KeyId::parse("messaging-s1-1").unwrap();
    let key = ServiceSigningKey::generate(key);
    let i1 = FakeI1::start(&key).await;
    let sink = NotificationServiceEvents::new(client(copy(&key), Some(&i1.url)));
    let report = relay(&mut h, Some(&f), &sink).await;
    assert_eq!((report.acked, report.dead), (2, 0), "{report:?}");
    let batches = i1.batches();
    let events: Vec<&Value> = batches
        .iter()
        .flat_map(|b| b["events"].as_array().unwrap())
        .collect();
    assert_eq!(events.len(), 2);
    for env in events {
        assert!(batches.iter().all(|b| b.get("shadow").is_none()));
        let recipients: Vec<String> = env["recipients"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["ref"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(recipients, f.recipient_refs());
        assert_eq!(env["data"]["conversationKind"], "GROUP");
        assert_eq!(env["data"]["securityProfile"], "MANAGED");
        assert_eq!(
            env["scope"]["environmentId"],
            json!(f.c.context.scope.environment_id)
        );
        assert!(!env.to_string().contains(TEXT));
    }
    assert!(i1.inner.lock().unwrap().invalid.is_empty());
    h.state.communications = Some(f.governed.clone());

    // X3 answers with the same account mapping for the organization's conversation.
    let answer = h
        .call_internal(
            &peers().notifications,
            axum::http::Method::POST,
            "/internal/v1/notifications/wakeups/authorize",
            Some(json!({
                "conversationId": cid(&f.conversation),
                "eventSequence": on.conversation_event_sequence.unwrap(),
                "senderPrincipal": f.sender().to_string(),
            })),
            None,
            &[],
        )
        .await;
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.text);
    let mapped: BTreeSet<String> = answer.json["entitled"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["accountRef"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        mapped,
        f.recipient_refs().into_iter().collect::<BTreeSet<_>>()
    );
}

/// Gap audit S1: a customer's (EXTERNAL tenant) conversation is never an organization
/// chat: its principals are customer identities, even with `cfmeet-user:`-looking
/// references, and its wake-ups keep the legacy path.
#[tokio::test]
async fn external_conversations_never_take_the_organization_route() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let mut f = organization_fixture(&mut h).await;
    // The same shape in an EXTERNAL tenant.
    let external = h.first_party(false);
    h.allow_managed(&external, "GROUP").await;
    let a = h.member(&external, 0).await;
    let b = h.member(&external, 0).await;
    let r = h
        .create_conversation(
            &external,
            a.principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    let add = h
        .post(
            &external,
            Some(a.principal),
            &format!("/v1/conversations/{}/members", cid(&conversation)),
            json!({ "principalId": b.principal }),
        )
        .await;
    assert_eq!(add.status, StatusCode::CREATED, "{}", add.text);
    f.c = external;
    f.conversation = conversation;
    f.people = vec![a, b];
    h.state.organization_wakeups = true;
    let e = send(&h, &f, "external").await;
    assert_eq!(e.payload["first_party"], false);
    push::fanout_jobs(&h.state, &e).await.unwrap();
    assert_eq!(decisions_of(&h, &e).await, 0);
    assert!(publications_of(&h, &e).await.is_empty());
}

// ------------------------------------------- owner request 2026-11-19: "added to a group"

/// A person someone else adds to a group gets one `messaging.member.invited` notice
/// (GROUP_INVITATIONS, the bell's "Added to a group"), addressed to their CF Meet account,
/// ids only; the person who created the group gets none. Nothing is written while the
/// Notification Service is not configured. A member who left before the relay is skipped.
#[tokio::test]
async fn people_added_to_a_group_get_a_member_notice_only_when_configured() {
    let Some(mut h) = Harness::new().await else {
        return;
    };
    let rows = "SELECT count(*) FROM messaging_member_notifications";
    // Not configured: nothing is written.
    let _off = first_party_fixture(&mut h).await;
    assert_eq!(h.scalar(rows).await, 0);
    // A customer's conversation (neither first party nor CF Meet's governed scope): nothing.
    h.state.member_notices = true;
    let _customer = fixture(&mut h).await;
    assert_eq!(h.scalar(rows).await, 0);

    let f = first_party_fixture(&mut h).await;
    assert_eq!(h.scalar(rows).await, 2);
    // Adding an ACTIVE member again announces nothing new.
    let again = h
        .post(
            &f.c,
            Some(f.people[0].principal),
            &format!("/v1/conversations/{}/members", cid(&f.conversation)),
            json!({ "principalId": f.people[1].principal }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(h.scalar(rows).await, 2);

    let key = cf_service_auth::KeyId::parse("messaging-members-1").unwrap();
    let key = ServiceSigningKey::generate(key);
    let i1 = FakeI1::start(&key).await;
    let sink = NotificationServiceEvents::new(client(copy(&key), Some(&i1.url)));
    h.state.communications = Some(f.governed.clone());
    let report = messaging::member_notifications::relay_due(&h.state, &sink, 500)
        .await
        .unwrap();
    assert_eq!(
        (report.acked, report.dead, report.skipped),
        (2, 0, 0),
        "{report:?}"
    );
    let batches = i1.batches();
    let events: Vec<&Value> = batches
        .iter()
        .flat_map(|b| b["events"].as_array().unwrap())
        .collect();
    assert!(batches.iter().all(|b| b.get("shadow").is_none()));
    let mut recipients: Vec<String> = events
        .iter()
        .map(|e| {
            assert_eq!(e["eventType"], "messaging.member.invited");
            assert_eq!(e["data"]["conversationId"], json!(cid(&f.conversation)));
            assert_eq!(e["data"].as_object().unwrap().len(), 2);
            let r = e["recipients"].as_array().unwrap();
            assert_eq!(r.len(), 1);
            r[0]["ref"].as_str().unwrap().to_string()
        })
        .collect();
    recipients.sort();
    assert_eq!(recipients, f.recipient_refs());
    assert!(i1.inner.lock().unwrap().invalid.is_empty());
    // Settled once.
    let next = messaging::member_notifications::relay_due(&h.state, &sink, 500)
        .await
        .unwrap();
    assert_eq!(next.acked, 0);

    // Someone added and removed before the relay ran is not told. (Adding asks the Hub's
    // policy when the scope is governed, which these tests do not run.)
    h.state.communications = None;
    let late = h.member(&f.c, 0).await;
    let add = h
        .post(
            &f.c,
            Some(f.people[0].principal),
            &format!("/v1/conversations/{}/members", cid(&f.conversation)),
            json!({ "principalId": late.principal }),
        )
        .await;
    assert_eq!(add.status, StatusCode::CREATED, "{}", add.text);
    h.sql(&format!(
        "UPDATE messaging_conversation_members SET state = 'REMOVED', removed_at = now(), removed_reason = 'REMOVED' WHERE principal_id = '{}'",
        late.principal
    ))
    .await
    .unwrap();
    h.state.communications = Some(f.governed.clone());
    let skipped = messaging::member_notifications::relay_due(&h.state, &sink, 500)
        .await
        .unwrap();
    assert_eq!((skipped.acked, skipped.skipped), (0, 1), "{skipped:?}");
    h.state.communications = None;
}

/// A MANAGED GROUP of three members (the first added the others) in a first-party scope,
/// with each member's CF Meet account; `governed` names that scope (CF Meet's personal one).
async fn first_party_fixture(h: &mut Harness) -> Fixture {
    let c = h.first_party(true);
    h.allow_managed(&c, "GROUP").await;
    let mut people = Vec::new();
    for _ in 0..3 {
        people.push(h.member(&c, 0).await);
    }
    let r = h
        .create_conversation(
            &c,
            people[0].principal,
            json!({ "contextClass": "GROUP", "securityProfile": "MANAGED" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let conversation = r.json;
    for p in &people[1..] {
        let a = h
            .post(
                &c,
                Some(people[0].principal),
                &format!("/v1/conversations/{}/members", cid(&conversation)),
                json!({ "principalId": p.principal }),
            )
            .await;
        assert_eq!(a.status, StatusCode::CREATED, "{}", a.text);
    }
    let accounts: HashMap<Uuid, String> = people
        .iter()
        .map(|m| (m.principal, format!("cfmeet-user:{}", Uuid::new_v4())))
        .collect();
    h.state.principals = Arc::new(Accounts {
        inner: h.principals.clone(),
        references: accounts.clone(),
    });
    let governed = cf_communications_policy::PolicyClient::new(
        h.state.service_auth.client(),
        cf_communications_policy::Scope {
            tenant_id: c.context.scope.tenant_id,
            application_id: c.context.scope.application_id,
            environment_id: c.context.scope.environment_id,
        },
    );
    Fixture {
        c,
        conversation,
        people,
        accounts,
        governed,
    }
}
