//! Notification Service N7 / N11 — Meeting's routed STANDARD categories
//! (`MEETING_INVITATION`, `MEETING_NOTIFICATION`, `ORGANIZATION_INVITATION`; Document 4
//! §9.3, §9.5, §5.1) on the real code paths (invitation emails, reschedule / cancellation
//! notices, the reminder scan), and the relay to a fake I1 that verifies the REAL service
//! token (`cfmeet.meeting-service` → `cfmeet.notifications`, `notifications-event.publish`)
//! and validates every envelope with packages/notification-client. Real PostgreSQL; the
//! legacy rows go through the real email ledger, so the guard triggers are exercised.

mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use backend::{
    config::MeetingV2Config,
    domain::Role,
    services::{
        notification_fence::MeetingCategory,
        notification_publications::{
            maintain, relay_due, NotificationServiceEvents, PublicationReport, INGEST_PATH,
        },
        notifications::{self as notify, NoPreferenceStore, NotificationContext, Notifier},
    },
};
use cf_notification_client::{validate, Envelope, SealingKey};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{DateTime, Duration, Utc};
use common::*;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;
const MEETING: &str = "cfmeet.meeting-service";

macro_rules! some {
    ($e:expr) => {
        match $e {
            Some(v) => v,
            None => return,
        }
    };
}

// ------------------------------------------------------------------------- fixtures

struct Harness {
    app: TestApp,
}

fn sealing_key() -> SealingKey {
    let (public, _private) =
        cf_notification_client::seal::generate_keypair("notif-hpke-2026-11").unwrap();
    SealingKey::parse(&public).unwrap()
}

impl Harness {
    /// Notification email on, the real email ledger as transport, no Hub preferences;
    /// `sealing`: the service's public key is configured.
    async fn new(sealing: bool) -> Option<Self> {
        Self::with_config(sealing, TestApp::notifications_config()).await
    }

    async fn with_config(sealing: bool, config: MeetingV2Config) -> Option<Self> {
        let app = TestApp::with(TestOptions {
            config: Some(config),
            ..TestOptions::default()
        })
        .await?;
        let notifier = Notifier::new(app.state.email_service.clone(), Arc::new(NoPreferenceStore))
            .with_sealing(sealing.then(sealing_key));
        let app = app.with_notifier(Arc::new(notifier));
        Some(Self { app })
    }

    fn ctx(&self) -> NotificationContext {
        NotificationContext::from_state(&self.app.state)
    }

    async fn set_mode(&self, category: MeetingCategory, mode: &str, canary_percent: i16) {
        self.app
            .db
            .conn
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE notification_routing_state
                    SET mode = $2, canary_percent = $3, routing_version = routing_version + 1,
                        changed_by_run = 'test', changed_at = now()
                  WHERE category = $1",
                [category.as_str().into(), mode.into(), canary_percent.into()],
            ))
            .await
            .unwrap();
    }

    async fn set_all(&self, mode: &str, canary_percent: i16) {
        for c in [
            MeetingCategory::MeetingInvitation,
            MeetingCategory::MeetingNotification,
            MeetingCategory::OrganizationInvitation,
        ] {
            self.set_mode(c, mode, canary_percent).await;
        }
    }

    async fn count(&self, sql: &str) -> i64 {
        self.app.scalar(sql).await
    }

    async fn text(&self, sql: &str) -> Option<String> {
        self.app
            .db
            .conn
            .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
            .await
            .unwrap()
            .and_then(|r| r.try_get_by_index::<Option<String>>(0).unwrap())
    }

    /// Every publication envelope, oldest first.
    async fn envelopes(&self) -> Vec<(bool, Value)> {
        self.app
            .db
            .conn
            .query_all(Statement::from_string(
                DbBackend::Postgres,
                "SELECT shadow, envelope FROM meeting_notification_publications ORDER BY created_at, id"
                    .to_string(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r.try_get::<bool>("", "shadow").unwrap(),
                    r.try_get::<Value>("", "envelope").unwrap(),
                )
            })
            .collect()
    }

    async fn relay(&self, sink: &NotificationServiceEvents) -> PublicationReport {
        relay_due(&self.app.db.conn, sink, 500, Utc::now())
            .await
            .unwrap()
    }

    async fn due_now(&self) {
        self.app
            .db
            .conn
            .execute(Statement::from_string(
                DbBackend::Postgres,
                "UPDATE meeting_notification_publications SET next_attempt_at = created_at WHERE state = 'PENDING'"
                    .to_string(),
            ))
            .await
            .unwrap();
    }
}

/// Validates a stored envelope exactly as I1 does (producer = authenticated issuer).
fn valid(envelope: &Value) -> Envelope {
    let env: Envelope = serde_json::from_value(envelope.clone()).expect("envelope shape");
    validate(&env, Some(MEETING)).expect("envelope valid for I1");
    env
}

fn rfc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

async fn scheduled(app: &TestApp, owner: &User, start: DateTime<Utc>) -> (Uuid, Uuid) {
    let space = app
        .create_space(
            owner,
            json!({ "title": "Quarterly review", "timezone": "Europe/London" }),
        )
        .await;
    let sid = uuid(&space["id"]);
    let (s, c) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/conferences"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(start), "scheduledEndAt": rfc(start + Duration::minutes(30)) }),
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    (sid, uuid(&c["id"]))
}

async fn invite(app: &TestApp, owner: &User, sid: Uuid, body: Value) -> Value {
    let (s, v) = app
        .post(
            format!("/api/v2/meeting-spaces/{sid}/invitations"),
            Auth::Bearer(&owner.token),
            body,
        )
        .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn reschedule(app: &TestApp, owner: &User, cid: Uuid, to: DateTime<Utc>) {
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/reschedule"),
            Auth::Bearer(&owner.token),
            json!({ "scheduledStartAt": rfc(to) }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// A space owner, one account member and a conference in three days.
async fn meeting(h: &Harness) -> (User, User, Uuid, Uuid) {
    let app = &h.app;
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let (sid, cid) = scheduled(app, &owner, Utc::now() + Duration::days(3)).await;
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    (owner, member, sid, cid)
}

async fn org_invitation(h: &Harness, invitation: Uuid) -> notify::EmailDelivery {
    notify::send_organization_invitation(
        &h.ctx(),
        "Ada Admin",
        &format!(
            "invitee-{}@example.com",
            &invitation.simple().to_string()[..8]
        ),
        Uuid::new_v4(),
        Some(invitation),
        "MEMBER",
        "tok_0123456789abcdefghijKLMNOP",
        Some(Utc::now() + Duration::days(7)),
    )
    .await
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
    /// The Notification Service's I1, trusting Meeting's `key` with
    /// `notifications-event.publish` only.
    async fn start(key: &ServiceSigningKey) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": cf_service_auth::services::NOTIFICATIONS,
            "runtime_environment": ENV,
            "trusted_issuers": [{
                "issuer": cf_service_auth::services::MEETING_SERVICE,
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

    fn invalid(&self) -> Vec<String> {
        self.inner.lock().unwrap().invalid.clone()
    }
}

fn signing_key() -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse("meeting-service-test-1").unwrap())
}

fn copy(k: &ServiceSigningKey) -> ServiceSigningKey {
    ServiceSigningKey::from_pkcs8_pem(k.kid().clone(), &k.to_pkcs8_pem()).unwrap()
}

/// Meeting's outbound I1 sink, signing as `cfmeet.meeting-service` with `key`, with a
/// Notification Service target at `url` (or none).
fn sink(key: ServiceSigningKey, url: Option<&str>) -> NotificationServiceEvents {
    let signer = Arc::new(ServiceTokenSigner::new(
        ServiceId::meeting_service(),
        ENV,
        key,
        TokenTtl::default(),
        Arc::new(SystemClock),
    ));
    let mut targets = InternalTargets::new(ENV);
    if let Some(url) = url {
        targets.insert(ServiceId::notifications(), url).unwrap();
    }
    NotificationServiceEvents::new(Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    ))
}

// --------------------------------------------------------------------------- tests

/// LEGACY_ONLY (the production state): exactly today's rows — no decision, no
/// publication, invitation emails keyed by the ledger's random `msg:` key.
#[tokio::test]
async fn legacy_only_writes_todays_rows_and_no_fence_rows() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    let (owner, _member, sid, cid) = meeting(&h).await;
    let invitee = app.user("invitee").await;
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    reschedule(app, &owner, cid, Utc::now() + Duration::days(4)).await;
    assert_eq!(
        org_invitation(&h, Uuid::new_v4()).await,
        notify::EmailDelivery::Accepted
    );
    assert_eq!(
        h.count("SELECT count(*) FROM email_deliveries WHERE category IN ('MEETING_INVITATION', 'ORGANIZATION_INVITATION') AND idempotency_key LIKE 'msg:%'")
            .await,
        2
    );
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}' AND kind = 'CONFERENCE_RESCHEDULED'"))
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        0
    );
}

/// SHADOW: every legacy row is written (keyed by its decision) AND one shadow publication
/// per decision; the relay posts them with `shadow: true`; the service's acknowledgement
/// of a shadow copy never marks the OLD decision acknowledged; the job's per-recipient
/// emails pass the guard trigger.
#[tokio::test]
async fn shadow_writes_legacy_rows_and_one_shadow_publication() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    h.set_all("SHADOW", 0).await;
    let (owner, member, sid, cid) = meeting(&h).await;
    reschedule(app, &owner, cid, Utc::now() + Duration::days(4)).await;
    let invitee = app.user("invitee").await;
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    let invitation = uuid(&v["invitation"]["id"]);
    let org = Uuid::new_v4();
    assert_eq!(
        org_invitation(&h, org).await,
        notify::EmailDelivery::Accepted
    );
    // A second email for the same invitation (a replay re-issues the token): its own key.
    assert_eq!(
        org_invitation(&h, org).await,
        notify::EmailDelivery::Accepted
    );

    // Legacy rows, keyed by the decision.
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM email_deliveries WHERE category = 'MEETING_INVITATION' AND idempotency_key = 'meeting-invitation:{invitation}'"))
            .await,
        1
    );
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM email_deliveries WHERE category = 'ORGANIZATION_INVITATION' AND idempotency_key IN ('org-invitation:{org}:1', 'org-invitation:{org}:2')"))
            .await,
        2
    );
    let job_key = h
        .text(&format!("SELECT dedupe_key FROM meeting_notifications WHERE conference_id = '{cid}' AND kind = 'CONFERENCE_RESCHEDULED'"))
        .await
        .expect("the reschedule job is written");
    // Four OLD decisions (ROUTED), four shadow publications.
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE route = 'OLD' AND reason = 'ROUTED'")
            .await,
        4
    );
    let envelopes = h.envelopes().await;
    assert_eq!(envelopes.len(), 4);
    assert!(envelopes.iter().all(|(shadow, _)| *shadow));
    let by_type = |t: &str| -> Envelope {
        valid(
            &envelopes
                .iter()
                .find(|(_, e)| e["eventType"] == t)
                .unwrap_or_else(|| panic!("no {t}"))
                .1,
        )
    };
    let inv = by_type("meeting-service.invitation.email-requested");
    assert_eq!(
        inv.delivery.idempotency_key.as_deref(),
        Some(format!("meeting-invitation:{invitation}").as_str())
    );
    assert_eq!(
        inv.recipients[0].reference,
        format!("cfmeet-user:{}", invitee.id)
    );
    assert_eq!(inv.data["conferenceId"], json!(cid.to_string()));
    assert_eq!(inv.data["invitationId"], json!(invitation.to_string()));
    assert!(inv.sealed.is_some(), "the token is sealed");
    let notice = by_type("meeting-service.conference.rescheduled");
    assert_eq!(notice.delivery.dedupe_key, job_key);
    assert_eq!(
        notice.delivery.idempotency_key.as_deref(),
        Some(format!("meeting-notification:{job_key}").as_str())
    );
    // The actor (owner) is not told; the member is, by account reference only.
    let refs: Vec<_> = notice
        .recipients
        .iter()
        .map(|r| r.reference.clone())
        .collect();
    assert_eq!(refs, vec![format!("cfmeet-user:{}", member.id)]);
    assert!(notice.sealed.is_none());
    let o = by_type("meeting-service.organization-invitation.email-requested");
    assert_eq!(
        o.recipients[0].reference,
        format!("meeting-email-principal:{org}")
    );
    assert!(o.sealed.is_some(), "token and address are sealed");
    // Ids only: no title, name, address or token in clear anywhere.
    let all = serde_json::to_string(&envelopes).unwrap();
    for secret in [
        "Quarterly review",
        "tok_0123456789abcdefghijKLMNOP",
        invitee.email.as_str(),
        "invitee-",
        "Ada Admin",
    ] {
        assert!(!all.contains(secret), "{secret} leaked");
    }

    // The worker still delivers the job (its per-recipient emails pass the guard). Its
    // recipients are resolved at delivery: the member and, since then, the invitee.
    let t = notify::tick(&h.ctx(), Utc::now()).await.unwrap();
    assert_eq!(t.sent, 1, "{t:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM email_deliveries WHERE category = 'MEETING_NOTIFICATION' AND idempotency_key LIKE 'meeting-notification:%'")
            .await,
        2
    );

    // Relay: shadow batch flag, real token, valid envelopes, acknowledged.
    let key = signing_key();
    let i1 = FakeI1::start(&key).await;
    let s = sink(copy(&key), Some(&i1.url));
    let r = h.relay(&s).await;
    assert_eq!(r.acked, 4, "{r:?}");
    let batches = i1.batches();
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0]["shadow"], json!(true));
    assert!(i1.invalid().is_empty(), "{:?}", i1.invalid());
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications WHERE state = 'ACKED' AND outcome = 'ACCEPTED' AND NOT (envelope ? 'sealed')")
            .await,
        4,
        "acknowledged, sealed parts erased"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE ns_acked_at IS NOT NULL")
            .await,
        0,
        "a shadow copy never acknowledges an OLD decision"
    );
    // Nothing more to post.
    assert_eq!(h.relay(&s).await, PublicationReport::default());
}

/// FENCED_OLD: the legacy rows and their OLD decisions only, no publication.
#[tokio::test]
async fn fenced_old_writes_legacy_rows_only() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    h.set_all("FENCED_OLD", 100).await;
    let (owner, _member, sid, cid) = meeting(&h).await;
    let invitee = app.user("invitee").await;
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        org_invitation(&h, Uuid::new_v4()).await,
        notify::EmailDelivery::Accepted
    );
    assert_eq!(
        h.count("SELECT count(*) FROM email_deliveries WHERE category IN ('MEETING_INVITATION', 'ORGANIZATION_INVITATION')")
            .await,
        2
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE dedupe_key = 'cancelled:{cid}'"
        ))
        .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE route = 'OLD' AND reason = 'ROUTED'")
            .await,
        3
    );
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        0
    );
}

/// FENCED_NEW (canary 100 %): the publication only, no legacy row; the relay posts it
/// without the shadow flag, the acknowledgement records `ns_acked_at`, a re-post is a
/// DUPLICATE (acknowledged), and the sticky decision makes retries idempotent.
#[tokio::test]
async fn fenced_new_publishes_only_and_acknowledges() {
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    h.set_all("FENCED_NEW", 100).await;
    let (owner, member, sid, cid) = meeting(&h).await;
    reschedule(app, &owner, cid, Utc::now() + Duration::days(5)).await;
    let invitee = app.user("invitee").await;
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    let org = Uuid::new_v4();
    assert_eq!(
        org_invitation(&h, org).await,
        notify::EmailDelivery::Accepted
    );

    assert_eq!(
        h.count("SELECT count(*) FROM email_deliveries").await,
        0,
        "no legacy email"
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}'"
        ))
        .await,
        0,
        "no legacy job"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE route = 'NEW' AND reason = 'ROUTED' AND ns_acked_at IS NULL")
            .await,
        3
    );
    let envelopes = h.envelopes().await;
    assert_eq!(envelopes.len(), 3);
    for (shadow, e) in &envelopes {
        assert!(!shadow);
        let env = valid(e);
        // The envelope names the recorded decision.
        assert_eq!(
            h.count(&format!(
                "SELECT count(*) FROM notification_route_decisions WHERE decision_id = '{}' AND route = 'NEW'",
                env.route.decision_id
            ))
            .await,
            1
        );
    }
    let notice = envelopes
        .iter()
        .map(|(_, e)| valid(e))
        .find(|e| e.event_type == "meeting-service.conference.rescheduled")
        .unwrap();
    assert_eq!(
        notice.recipients[0].reference,
        format!("cfmeet-user:{}", member.id)
    );
    // The worker has nothing to deliver.
    assert_eq!(notify::tick(&h.ctx(), Utc::now()).await.unwrap().sent, 0);

    // A retry of the same notice (same job key): the sticky decision writes nothing new.
    let c = <backend::models::conference_sessions::Entity as sea_orm::EntityTrait>::find_by_id(cid)
        .one(&app.db.conn)
        .await
        .unwrap()
        .unwrap();
    notify::record_rescheduled(&app.db.conn, &h.ctx().config, &c, &c, Some(owner.id))
        .await
        .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        3,
        "sticky decision: no second publication"
    );

    // Relay: transport failure first (retry), then accepted.
    let key = signing_key();
    let i1 = FakeI1::start(&key).await;
    let s = sink(copy(&key), Some(&i1.url));
    i1.script(&[Scripted::Status(503)]);
    let r = h.relay(&s).await;
    assert_eq!((r.retried, r.acked), (3, 0), "{r:?}");
    h.due_now().await;
    let r = h.relay(&s).await;
    assert_eq!(r.acked, 3, "{r:?}");
    let batches = i1.batches();
    assert!(batches.iter().all(|b| b.get("shadow").is_none()));
    assert!(i1.invalid().is_empty(), "{:?}", i1.invalid());
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE route = 'NEW' AND ns_acked_at IS NOT NULL")
            .await,
        3,
        "responsibility moved to the service"
    );
    // A crash between the service commit and the ACK: the re-post is a DUPLICATE (acked).
    app.db
        .conn
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE meeting_notification_publications SET state = 'PENDING', outcome = NULL, completed_at = NULL"
                .to_string(),
        ))
        .await
        .unwrap();
    h.due_now().await;
    let r = h.relay(&s).await;
    assert_eq!(r.acked, 3, "{r:?}");
    assert_eq!(
        h.count(
            "SELECT count(*) FROM meeting_notification_publications WHERE outcome = 'DUPLICATE'"
        )
        .await,
        3
    );
}

/// The reminder scan outside LEGACY_ONLY: each reminder is decided once under its
/// `reminder:<conference>:<start>` key; in FENCED_NEW it is published (scheduled event
/// with the reminder dedupe key, its cancel key and expiry), never found again, and no
/// legacy job is written.
#[tokio::test]
async fn reminders_are_routed_once_per_start() {
    let h = some!(Harness::new(false).await);
    let app = &h.app;
    h.set_mode(MeetingCategory::MeetingNotification, "FENCED_NEW", 100)
        .await;
    let owner = app.user("owner").await;
    let member = app.user("member").await;
    let start = Utc::now() + Duration::minutes(10);
    let (sid, cid) = scheduled(app, &owner, start).await;
    app.add_member(sid, &owner, &member, Role::Participant)
        .await;
    let ctx = h.ctx();
    let t = notify::tick(&ctx, Utc::now()).await.unwrap();
    assert_eq!(t.reminders_created, 0, "{t:?}");
    let now = Utc::now();
    let (a, b) = tokio::join!(notify::tick(&ctx, now), notify::tick(&ctx, now));
    assert_eq!(
        a.unwrap().reminders_created + b.unwrap().reminders_created,
        0
    );
    let envelopes = h.envelopes().await;
    assert_eq!(envelopes.len(), 1, "one decision, one publication");
    let env = valid(&envelopes[0].1);
    assert_eq!(env.event_type, "meeting-service.conference.scheduled");
    assert!(env
        .delivery
        .dedupe_key
        .starts_with(&format!("reminder:{cid}:")));
    assert_eq!(
        env.delivery.cancel_key.as_deref(),
        Some(format!("conference-reminder:{cid}").as_str())
    );
    assert!(env.delivery.expires_at.is_some());
    assert_eq!(env.recipients.len(), 2, "owner and member");
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE conference_id = '{cid}'"
        ))
        .await,
        0
    );

    // FENCED_OLD for the next start: a legacy job and its OLD decision.
    h.set_mode(MeetingCategory::MeetingNotification, "FENCED_OLD", 0)
        .await;
    let other_start = Utc::now() + Duration::minutes(12);
    let (_, other) = scheduled(app, &owner, other_start).await;
    let t = notify::tick(&ctx, Utc::now()).await.unwrap();
    assert_eq!(t.reminders_created, 1, "{t:?}");
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM notification_route_decisions d JOIN meeting_notifications n ON n.dedupe_key = d.idempotency_key WHERE n.conference_id = '{other}' AND d.route = 'OLD'"))
            .await,
        1
    );
    assert_eq!(
        notify::tick(&ctx, Utc::now())
            .await
            .unwrap()
            .reminders_created,
        0
    );
}

/// Events that cannot travel in one envelope stay OLD and keep the legacy path, whatever
/// the canary says: an audience larger than the configured maximum (`RECIPIENT_LIMIT`),
/// an email-only participant or invitee the service cannot address from an envelope, and
/// an invitation email without a sealing key (`UNPUBLISHABLE`). Never dropped.
#[tokio::test]
async fn events_that_cannot_be_published_stay_old() {
    let mut config = TestApp::notifications_config();
    config.notifications.max_recipients = 1;
    let h = some!(Harness::with_config(false, config).await);
    let app = &h.app;
    h.set_all("FENCED_NEW", 100).await;
    let (owner, _member, sid, cid) = meeting(&h).await;
    let second = app.user("second").await;
    app.add_member(sid, &owner, &second, Role::Participant)
        .await;
    // Two recipients for a maximum of one.
    reschedule(app, &owner, cid, Utc::now() + Duration::days(4)).await;
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM notification_route_decisions d JOIN meeting_notifications n ON n.dedupe_key = d.idempotency_key WHERE n.conference_id = '{cid}' AND d.route = 'OLD' AND d.reason = 'RECIPIENT_LIMIT'"))
            .await,
        1
    );
    // Invitation email without a sealing key: OLD, the email is still sent.
    let invitee = app.user("invitee").await;
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": invitee.email, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    let invitation = uuid(&v["invitation"]["id"]);
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM email_deliveries e JOIN notification_route_decisions d ON d.idempotency_key = e.idempotency_key WHERE e.idempotency_key = 'meeting-invitation:{invitation}' AND d.route = 'OLD' AND d.reason = 'UNPUBLISHABLE'"))
            .await,
        1
    );
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        0
    );

    // With the sealing key: an email-only invitee is still UNPUBLISHABLE (OLD); an
    // email-only participant keeps the conference notice OLD too.
    let h = some!(Harness::new(true).await);
    let app = &h.app;
    h.set_all("FENCED_NEW", 100).await;
    let (owner, _member, sid, cid) = meeting(&h).await;
    let external = format!(
        "external-{}@example.com",
        &Uuid::new_v4().simple().to_string()[..8]
    );
    let v = invite(
        app,
        &owner,
        sid,
        json!({ "inviteeEmail": external, "conferenceId": cid, "sendEmail": true }),
    )
    .await;
    assert_eq!(v["emailDelivery"], "ACCEPTED", "{v}");
    let invitation = uuid(&v["invitation"]["id"]);
    assert_eq!(
        h.text(&format!("SELECT reason FROM notification_route_decisions WHERE idempotency_key = 'meeting-invitation:{invitation}'"))
            .await
            .as_deref(),
        Some("UNPUBLISHABLE")
    );
    assert_eq!(
        h.count(&format!("SELECT count(*) FROM email_deliveries WHERE idempotency_key = 'meeting-invitation:{invitation}'"))
            .await,
        1
    );
    let (s, v) = app
        .post(
            format!("/api/v2/conferences/{cid}/cancel"),
            Auth::Bearer(&owner.token),
            json!({}),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(
        h.text(&format!("SELECT reason FROM notification_route_decisions WHERE idempotency_key = 'cancelled:{cid}'"))
            .await
            .as_deref(),
        Some("UNPUBLISHABLE")
    );
    assert_eq!(
        h.count(&format!(
            "SELECT count(*) FROM meeting_notifications WHERE dedupe_key = 'cancelled:{cid}'"
        ))
        .await,
        1
    );
    // ... and the worker emails everyone, the external participant included.
    let t = notify::tick(&h.ctx(), Utc::now()).await.unwrap();
    assert_eq!(t.sent, 1, "{t:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM email_deliveries WHERE category = 'MEETING_NOTIFICATION'")
            .await,
        2,
        "member and external participant"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        0
    );
}

/// Relay outcomes: HELD acknowledges; a per-event REJECTED and a contract 4xx
/// dead-letter; 401 (an issuer the service does not trust) and 429 retry; without a target
/// nothing is posted (rows wait); a row past its expiry is EXPIRED and never posted;
/// settled rows are purged after the retention.
#[tokio::test]
async fn relay_outcomes_dead_letters_retries_expiry_and_purge() {
    let h = some!(Harness::new(true).await);
    h.set_all("FENCED_NEW", 100).await;
    for _ in 0..4 {
        assert_eq!(
            org_invitation(&h, Uuid::new_v4()).await,
            notify::EmailDelivery::Accepted
        );
    }
    let key = signing_key();
    let i1 = FakeI1::start(&key).await;

    // Not configured: rows wait.
    let r = h.relay(&sink(copy(&key), None)).await;
    assert_eq!(r.not_configured, 4, "{r:?}");
    assert!(i1.batches().is_empty());
    // An untrusted key: 401, retried.
    h.due_now().await;
    let r = h.relay(&sink(signing_key(), Some(&i1.url))).await;
    assert_eq!(r.retried, 4, "{r:?}");
    assert_eq!(i1.inner.lock().unwrap().unauthenticated, 1);
    // 429: retried.
    let s = sink(copy(&key), Some(&i1.url));
    i1.script(&[Scripted::Status(429)]);
    h.due_now().await;
    assert_eq!(h.relay(&s).await.retried, 4);
    // A contract refusal of the batch: dead letter.
    i1.script(&[Scripted::Status(400)]);
    h.due_now().await;
    let r = h.relay(&s).await;
    assert_eq!(r.dead, 4, "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications WHERE state = 'DEAD' AND last_error = 'CONTRACT_REJECTED' AND NOT (envelope ? 'sealed')")
            .await,
        4
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE ns_acked_at IS NOT NULL")
            .await,
        0
    );

    // Two more: REJECTED per event, then HELD.
    for _ in 0..2 {
        org_invitation(&h, Uuid::new_v4()).await;
    }
    i1.script(&[Scripted::Outcome("REJECTED", "ROUTE_NOT_NEW")]);
    let r = h.relay(&s).await;
    assert_eq!(r.dead, 2, "{r:?}");
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications WHERE state = 'DEAD' AND outcome = 'REJECTED' AND last_error = 'ROUTE_NOT_NEW'")
            .await,
        2
    );
    org_invitation(&h, Uuid::new_v4()).await;
    i1.script(&[Scripted::Outcome("HELD", "CATEGORY_OFF")]);
    assert_eq!(h.relay(&s).await.acked, 1);
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions WHERE ns_acked_at IS NOT NULL")
            .await,
        1,
        "HELD: stored by the service"
    );

    // Expiry: a row past its deadline is never posted.
    org_invitation(&h, Uuid::new_v4()).await;
    h.app
        .db
        .conn
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE meeting_notification_publications SET expires_at = created_at + interval '1 millisecond' WHERE state = 'PENDING'"
                .to_string(),
        ))
        .await
        .unwrap();
    let before = i1.batches().len();
    let r = h.relay(&s).await;
    assert_eq!(r.expired, 1, "{r:?}");
    assert_eq!(i1.batches().len(), before);
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications WHERE state = 'EXPIRED' AND NOT (envelope ? 'sealed')")
            .await,
        1
    );
    assert!(i1.invalid().is_empty(), "{:?}", i1.invalid());

    // Purge after the retention (settled rows only).
    let (expired, purged, _) = maintain(&h.app.db.conn, 500, Utc::now() + Duration::days(31))
        .await
        .unwrap();
    assert_eq!((expired, purged), (0, 8));
    assert_eq!(
        h.count("SELECT count(*) FROM meeting_notification_publications")
            .await,
        0
    );
}

/// The CRITICAL categories are untouched: their legacy rows are written without any
/// decision whatever the STANDARD categories' modes are, and the guard triggers stay
/// inert for them (they are LEGACY_ONLY).
#[tokio::test]
async fn critical_categories_keep_the_old_path() {
    let h = some!(Harness::new(true).await);
    h.set_all("FENCED_NEW", 100).await;
    backend::services::email_delivery::enqueue(
        &h.app.db.conn,
        backend::services::email_delivery::NewEmail {
            category: backend::services::email_delivery::category::AUTH_VERIFICATION,
            idempotency_key: Some("otp:verify:critical-untouched".into()),
            to: "someone@example.com".into(),
            to_name: None,
            subject: "Your code".into(),
            html: "<p>code</p>".into(),
            text: "code".into(),
            expires_at: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM notification_routing_state WHERE category IN ('AUTH_VERIFICATION', 'AUTH_PASSWORD_RESET', 'SECURITY_DEVICE_STEP_UP', 'SECURITY_PASSWORD_CHANGED', 'SECURITY_DEVICE_LINKED') AND mode = 'LEGACY_ONLY'")
            .await,
        5
    );
    assert_eq!(
        h.count("SELECT count(*) FROM notification_route_decisions")
            .await,
        0
    );
}
