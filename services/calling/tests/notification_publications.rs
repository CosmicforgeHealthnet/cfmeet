//! Notification Service roadmap N7 (D-10): Calling's publications of
//! `calling.call.participant-missed`, `calling.voicemail.ready` and
//! `calling.recording.ready` — the transactional outbox `calling_notification_publications`
//! (its own migration history) and its relay to a fake I1 that verifies the REAL service
//! token (issuer `cfmeet.calling`, audience `cfmeet.notifications`,
//! `notifications-event.publish`) and validates every envelope with
//! packages/notification-client. Real PostgreSQL; calls, voicemail and recordings go
//! through Calling's own API and capture paths.
mod common;

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State as AxumState,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Json, Router,
};
use calling::{
    calls::{app::enterprise::CapturedMedia, CallServiceConfig, CallState},
    domain::{EnvironmentKind, ManualClock, PrincipalKind, TenantClassification},
    enterprise::policy::AdminCapability,
    notification_publications::{
        enqueue_missed, event_id, maintain, relay_due, CallNotice, NotificationServiceEvents,
        PublicationReport, INGEST_PATH,
    },
    platform::{
        principals::{PrincipalReference, ResolvedPrincipal},
        scopes::names,
    },
};
use calling_migration::{MigratorTrait, NotificationPublicationsMigrator};
use cf_notification_client::{validate, Envelope};
use cf_object_storage::MemoryObjectStore;
use cf_service_auth::{
    InternalPermission, PublicJwk, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, TrustPolicy,
};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    enterprise::{credential, id_of, EnterpriseWorld, RecordingCapture},
    fake_media::FakeMediaProvider,
    service_auth_fixture_targeting, t0, Harness, NewPrincipal, RecordingPush, TestDb,
};
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement, TransactionTrait};
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
    // The contract body: `events` (1..=100); Calling never sends shadow copies.
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

fn verifier(calling: &PublicJwk, clock: cf_service_auth::ManualClock) -> ServiceTokenVerifier {
    let bundle = json!({
        "bundle_version": 1,
        "receiver": ServiceId::notifications(),
        "runtime_environment": "test",
        "trusted_issuers": [{
            "issuer": ServiceId::calling(),
            "audience": ServiceId::notifications(),
            "runtime_environment": "test",
            "keys": [calling],
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
        Arc::new(clock),
    )
}

// --------------------------------------------------------------------------- world

/// An enterprise-capable Calling deployment in a tenant of `classification` whose
/// `CF_NOTIFICATIONS_BASE_URL` points at a fake I1 (when `publications`), with memory
/// artifact storage and a recording RTC capture adapter.
struct World {
    e: EnterpriseWorld,
    capture: Arc<RecordingCapture>,
    i1: Arc<Mutex<I1>>,
    sink: NotificationServiceEvents,
}

impl World {
    async fn start(classification: TenantClassification, publications: bool) -> Option<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let db = Arc::new(TestDb::new().await?);
        NotificationPublicationsMigrator::up(&db.conn, None)
            .await
            .unwrap();
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = common::test_runtime(Some(media.clone()), bus.clone());
        runtime.call_config = CallServiceConfig {
            notification_publications: publications,
            ..CallServiceConfig::default()
        };
        let capture = Arc::new(RecordingCapture::default());
        runtime.artifacts = Arc::new(MemoryObjectStore::new());
        runtime.capture = capture.clone();
        let push = Arc::new(RecordingPush::default());
        runtime.push = push.clone();
        let targets: Vec<(ServiceId, &str)> = if publications {
            vec![(ServiceId::notifications(), url.as_str())]
        } else {
            Vec::new()
        };
        let (service_auth, peers) = service_auth_fixture_targeting(&targets);
        let i1 = Arc::new(Mutex::new(I1::default()));
        let ctx = Arc::new(Ctx {
            verifier: verifier(&service_auth.signer().public_jwk(), peers.clock.clone()),
            inner: i1.clone(),
        });
        let app = Router::new()
            .route(INGEST_PATH, post(ingest))
            .with_state(ctx);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let h = Harness::with_service_auth(
            db,
            ManualClock::new(t0()),
            media,
            bus,
            runtime,
            push,
            service_auth,
            peers,
        );
        let sink = NotificationServiceEvents::new(h.state.service_auth.client_arc());
        let consumer = h.consumer("acme", classification).await;
        let w = CallWorld::for_consumer(h, consumer, EnvironmentKind::Production).await;
        let e = EnterpriseWorld::around(AppWorld { w }).await;
        Some(Self {
            e,
            capture,
            i1,
            sink,
        })
    }

    fn h(&self) -> &Harness {
        self.e.h()
    }

    fn a(&self) -> &AppWorld {
        &self.e.a
    }

    /// A principal of this world's environment with one device.
    async fn person(&self, kind: PrincipalKind, reference: &str) -> (ResolvedPrincipal, Device) {
        let w = &self.e.a.w;
        let (view, _) = self
            .h()
            .platform
            .register_principal(
                w.consumer.env(w.kind).id,
                NewPrincipal::External {
                    kind,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
        let principal = self
            .h()
            .state
            .principals
            .resolve(&w.ctx, &PrincipalReference::Id(view.id))
            .await
            .unwrap();
        let device = self
            .a()
            .device(&principal, &format!("device-{}", Uuid::new_v4().simple()))
            .await;
        (principal, device)
    }

    /// A CF Meet account (NATIVE_CF_USER, `cfmeet-user:<uuid>`) with one device.
    async fn account(&self) -> (ResolvedPrincipal, Device, String) {
        let reference = format!("cfmeet-user:{}", Uuid::new_v4());
        let (p, d) = self.person(PrincipalKind::NativeCfUser, &reference).await;
        (p, d, reference)
    }

    /// A ringing one-to-one call from `from` to `to` (offer acknowledged by `to`'s device).
    async fn ringing(
        &self,
        from: (&ResolvedPrincipal, &Device),
        to: (&ResolvedPrincipal, &Device),
    ) -> Uuid {
        let a = self.a();
        let r = a
            .create_call(
                "VOICE",
                from.0,
                from.1,
                &[to.0],
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let invitation = a.invitation_for(call, to.1.endpoint).await.unwrap();
        let ack = a.acknowledge_for(call, invitation, to.1).await;
        assert_eq!(ack.status, StatusCode::OK, "{}", ack.text);
        call
    }

    async fn cancel(&self, call: Uuid) {
        let (_, version) = self.a().state(call).await;
        let r = self
            .a()
            .post_with(
                &format!("/v1/calls/{call}/cancel"),
                json!({ "expectedVersion": version }),
                &[("idempotency-key", &Uuid::new_v4().to_string())],
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        assert_eq!(self.a().state(call).await.0, CallState::Cancelled);
    }

    /// A ringing call answered by `to`, both connected, then ended.
    async fn answered(
        &self,
        from: (&ResolvedPrincipal, &Device),
        to: (&ResolvedPrincipal, &Device),
    ) -> Uuid {
        let a = self.a();
        let call = self.ringing(from, to).await;
        let invitation = a.invitation_for(call, to.1.endpoint).await.unwrap();
        let (_, v) = a.state(call).await;
        let r = a.accept(call, invitation, to.1, v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        for d in [from.1, to.1] {
            let s = a.session_on(call, d.endpoint).await.unwrap();
            a.connect(call, s).await;
        }
        assert_eq!(a.state(call).await.0, CallState::Connected);
        call
    }

    async fn end(&self, call: Uuid) {
        let (_, v) = self.a().state(call).await;
        let r = self.a().command(call, "end", v).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }

    async fn relay(&self) -> PublicationReport {
        relay_due(self.h().conn(), &self.sink, 500).await.unwrap()
    }

    fn batches(&self) -> Vec<Value> {
        self.i1.lock().unwrap().batches.clone()
    }

    fn posted(&self) -> Vec<Value> {
        self.batches()
            .iter()
            .flat_map(|b| b["events"].as_array().unwrap().clone())
            .collect()
    }

    fn script(&self, s: &[Scripted]) {
        self.i1.lock().unwrap().script.extend_from_slice(s);
    }

    fn assert_valid(&self) {
        let inner = self.i1.lock().unwrap();
        assert!(inner.invalid.is_empty(), "{:?}", inner.invalid);
        assert_eq!(inner.unauthenticated, 0);
    }

    async fn rows(&self) -> Vec<Value> {
        self.h()
            .conn()
            .query_all(Statement::from_string(
                DbBackend::Postgres,
                "SELECT to_jsonb(p) AS doc FROM calling_notification_publications p \
                 ORDER BY created_at, event_type, event_id",
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.try_get::<Value>("", "doc").unwrap())
            .collect()
    }

    async fn sql(&self, statement: &str) -> u64 {
        self.h()
            .conn()
            .execute_unprepared(statement)
            .await
            .unwrap()
            .rows_affected()
    }

    /// Makes every waiting publication due now (instead of waiting for the backoff).
    async fn due_now(&self) {
        self.sql(
            "UPDATE calling_notification_publications SET next_attempt_at = now() - interval '1 second' \
              WHERE state = 'PENDING'",
        )
        .await;
    }

    async fn participant(&self, call: Uuid, principal: &ResolvedPrincipal) -> Uuid {
        self.h()
            .conn()
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT id FROM call_participants WHERE call_id = '{call}' AND principal_id = '{}'",
                    principal.id()
                ),
            ))
            .await
            .unwrap()
            .expect("participant")
            .try_get_by_index(0)
            .unwrap()
    }
}

/// Ids only: an envelope never carries a name, a number, a label, an object key or a URL;
/// its data keys are exactly the catalogue's.
fn assert_ids_only(envelope: &Value, keys: &[&str]) {
    let data: BTreeSet<&str> = envelope["data"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(data, keys.iter().copied().collect(), "{envelope}");
    assert!(envelope.get("sealed").is_none(), "{envelope}");
    let text = envelope.to_string();
    for forbidden in [
        "Box ",
        "Extension",
        "voicemail/",
        "recordings/",
        "http",
        "+234",
        "tel:",
        "sip:",
        "displayName",
        "display_name",
        "label",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}

fn wav(ms: u32) -> Vec<u8> {
    let samples = 8 * ms;
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..samples {
        let v = if (i / 10) % 2 == 0 { 800i16 } else { -800i16 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn media(bytes: &[u8]) -> CapturedMedia {
    CapturedMedia {
        content_type: "audio/wav".into(),
        duration_ms: 1500,
        sha256: cf_object_storage::sha256_hex(bytes),
        bytes: bytes.to_vec(),
    }
}

fn by_type<'a>(rows: &'a [Value], event_type: &str) -> Vec<&'a Value> {
    rows.iter()
        .filter(|r| r["event_type"] == event_type)
        .collect()
}

// --------------------------------------------------------------------------- tests

#[tokio::test]
async fn a_missed_call_is_published_once_to_the_cf_meet_recipient() {
    let Some(w) = World::start(TenantClassification::FirstParty, true).await else {
        return;
    };
    let (caller, caller_phone, _) = w.account().await;
    let (callee, callee_phone, callee_ref) = w.account().await;

    // An answered call and a declined call are never missed.
    let answered = w
        .answered((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    w.end(answered).await;
    let declined = w
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    let invitation = w
        .a()
        .invitation_for(declined, callee_phone.endpoint)
        .await
        .unwrap();
    let (_, v) = w.a().state(declined).await;
    let r = w.a().decline(declined, invitation, &callee_phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(w.rows().await.is_empty(), "{:?}", w.rows().await);

    // The caller cancels while it rings: the recipient missed it.
    let call = w
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    w.cancel(call).await;
    let rows = w.rows().await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    let row = &rows[0];
    let participant = w.participant(call, &callee).await;
    assert_eq!(row["event_type"], "calling.call.participant-missed");
    assert_eq!(row["state"], "PENDING");
    assert_eq!(row["call_id"], call.to_string());
    assert_eq!(row["source_id"], participant.to_string());
    assert_eq!(row["recipient_principal_id"], callee.id().to_string());
    // Deterministic: the id is derived from the fact, its type and the recipient.
    assert_eq!(
        row["event_id"],
        event_id(
            CallNotice::ParticipantMissed,
            participant,
            callee.id().as_uuid()
        )
    );
    let envelope = &row["envelope"];
    assert_eq!(envelope["producer"], "cfmeet.calling");
    assert_eq!(envelope["recipients"], json!([{ "ref": callee_ref }]));
    assert_eq!(envelope["subject"], json!({ "kind": "CALL", "id": call }));
    assert_eq!(
        envelope["data"],
        json!({ "callId": call, "callKind": "VOICE" })
    );
    assert_eq!(envelope["delivery"]["priority"], "HIGH");
    assert_eq!(envelope["delivery"]["idempotencyKey"], row["event_id"]);
    let scope = w.e.a.w.scope();
    assert_eq!(
        envelope["scope"],
        json!({
            "tenantId": scope.tenant_id().as_uuid(),
            "applicationId": scope.application_id().as_uuid(),
            "environmentId": scope.environment_id().as_uuid(),
        })
    );
    // Repeated calls from one caller coalesce into one inbox group.
    let coalesce = envelope["delivery"]["coalesceKey"].as_str().unwrap();
    assert!(
        coalesce.starts_with(&format!(
            "calling-missed:{}:",
            caller.id().as_uuid().simple()
        )),
        "{coalesce}"
    );
    assert_ids_only(envelope, &["callId", "callKind"]);

    // Relayed once, validated, acknowledged.
    let report = w.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");
    w.assert_valid();
    let posted = w.posted();
    assert_eq!(posted, vec![envelope.clone()]);
    let rows = w.rows().await;
    assert_eq!(
        (&rows[0]["state"], &rows[0]["outcome"]),
        (&json!("ACKED"), &json!("ACCEPTED"))
    );
    assert_eq!(w.relay().await, PublicationReport::default());
    assert_eq!(w.batches().len(), 1);

    // Idempotent: enqueueing the same fact again writes nothing.
    let txn = w.h().conn().begin().await.unwrap();
    let model = calling::models::calls::Entity::find_by_id(call)
        .one(&txn)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(enqueue_missed(&txn, &model).await.unwrap(), 0);
    txn.commit().await.unwrap();
    assert_eq!(w.rows().await.len(), 1);

    // A crash after the post but before the commit leaves the row PENDING: the
    // byte-identical re-post is answered DUPLICATE and acknowledged.
    w.sql(
        "UPDATE calling_notification_publications SET state = 'PENDING', outcome = NULL, \
           completed_at = NULL",
    )
    .await;
    let report = w.relay().await;
    assert_eq!(report.acked, 1, "{report:?}");
    let posted = w.posted();
    assert_eq!(posted.len(), 2);
    assert_eq!(
        serde_json::to_vec(&posted[0]).unwrap(),
        serde_json::to_vec(&posted[1]).unwrap()
    );
    assert_eq!(w.rows().await[0]["outcome"], "DUPLICATE");
    w.assert_valid();
}

#[tokio::test]
async fn voicemail_ready_reaches_the_mailbox_owner_and_recording_ready_the_participants() {
    let Some(w) = World::start(TenantClassification::FirstParty, true).await else {
        return;
    };
    let e = &w.e;
    // Voicemail: the owner's unanswered calls go to the owner's mailbox.
    let (owner, owner_phone, owner_ref) = w.account().await;
    let (caller, caller_phone, caller_ref) = w.account().await;
    let vm_admin = credential(
        w.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_VOICEMAIL_READ,
            names::ENTERPRISE_VOICEMAIL_MANAGE,
        ],
    )
    .await;
    let r = e
        .send(
            vm_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/voicemail/mailboxes",
            Some(
                json!({ "owner": { "type": "PRINCIPAL", "id": owner.id().to_string() },
                         "displayLabel": "Box of the owner", "maxMessageSeconds": 120,
                         "retentionDays": 30 }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mailbox = id_of(&r.json, "mailboxId");
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/forwarding-rules",
            Some(json!({ "principalId": owner.id(), "condition": "NO_ANSWER",
                         "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    e.extension("4100", "PRINCIPAL", owner.id()).await;
    let r = e
        .call_extension(
            "VOICE",
            &caller,
            &caller_phone,
            "4100",
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let _ = owner_phone;
    w.h().clock.advance(chrono::Duration::seconds(46));
    w.h().state.app.expire_due(100).await.unwrap();
    let message: Uuid = w
        .h()
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT id FROM voicemail_messages WHERE call_id = '{call}'"),
        ))
        .await
        .unwrap()
        .expect("a voicemail message")
        .try_get_by_index(0)
        .unwrap();
    // Nothing is published while the message records.
    assert!(by_type(&w.rows().await, "calling.voicemail.ready").is_empty());
    let audio = wav(1500);
    w.h()
        .state
        .app
        .deliver_capture(message, call, media(&audio))
        .await
        .unwrap();
    // Redelivery of the same capture changes nothing.
    w.h()
        .state
        .app
        .deliver_capture(message, call, media(&audio))
        .await
        .unwrap();
    let rows = w.rows().await;
    let vm = by_type(&rows, "calling.voicemail.ready");
    assert_eq!(vm.len(), 1, "{rows:?}");
    assert_eq!(vm[0]["source_id"], message.to_string());
    assert_eq!(
        vm[0]["envelope"]["recipients"],
        json!([{ "ref": owner_ref }])
    );
    assert_eq!(
        vm[0]["envelope"]["data"],
        json!({ "callId": call, "voiceMessageId": message })
    );
    assert_eq!(vm[0]["envelope"]["delivery"]["priority"], "NORMAL");
    assert_ids_only(&vm[0]["envelope"], &["callId", "voiceMessageId"]);
    // The owner also missed the call itself (the call ended unanswered).
    let missed = by_type(&rows, "calling.call.participant-missed");
    assert_eq!(missed.len(), 1, "{rows:?}");
    assert_eq!(
        missed[0]["envelope"]["recipients"],
        json!([{ "ref": owner_ref }])
    );

    // Recording: both parties of a recorded, connected call.
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            e.a.w.scope(),
            vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageRecording,
            ],
        )
        .await
        .unwrap();
    let rec_admin = credential(
        w.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_CONFIG_READ,
            names::ENTERPRISE_RECORDING_READ,
            names::ENTERPRISE_RECORDING_MANAGE,
        ],
    )
    .await;
    let r = e
        .send(
            rec_admin.secret.expose_once(),
            Method::POST,
            "/v1/enterprise/recording-policies",
            Some(
                json!({ "name": "Recording policy", "mode": "MANUAL_ALLOWED",
                         "noticeRequired": false, "consentParticipants": "NONE",
                         "jurisdictionRef": "NG-NDPA-2023/v2", "retentionDays": 30,
                         "maxRecordingSeconds": 600, "isDefault": true }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let (callee, callee_phone, callee_ref) = w.account().await;
    e.extension("4200", "PRINCIPAL", callee.id()).await;
    let r = e
        .call_extension(
            "VOICE",
            &caller,
            &caller_phone,
            "4200",
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recorded: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let inv =
        e.a.invitation_for(recorded, callee_phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(recorded).await;
    let r = e.a.accept(recorded, inv, &callee_phone, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for d in [&caller_phone, &callee_phone] {
        let s = e.a.session_on(recorded, d.endpoint).await.unwrap();
        e.a.connect(recorded, s).await;
    }
    let r = e
        .send(
            rec_admin.secret.expose_once(),
            Method::POST,
            &format!("/v1/calls/{recorded}/recordings"),
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording = id_of(&r.json["recording"], "recordingId");
    w.end(recorded).await;
    w.h().state.app.expire_due(100).await.unwrap();
    assert!(w.capture.stopped().contains(&recording));
    assert!(by_type(&w.rows().await, "calling.recording.ready").is_empty());
    w.h()
        .state
        .app
        .deliver_capture(recording, recorded, media(&audio))
        .await
        .unwrap();
    let rows = w.rows().await;
    let rec = by_type(&rows, "calling.recording.ready");
    let recipients: BTreeSet<String> = rec
        .iter()
        .map(|r| {
            r["envelope"]["recipients"][0]["ref"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        recipients,
        [caller_ref.clone(), callee_ref.clone()]
            .into_iter()
            .collect()
    );
    for r in &rec {
        assert_eq!(
            r["envelope"]["data"],
            json!({ "callId": recorded, "recordingId": recording })
        );
        assert_ids_only(&r["envelope"], &["callId", "recordingId"]);
    }
    // Nobody missed the recorded call.
    assert!(by_type(&rows, "calling.call.participant-missed")
        .iter()
        .all(|r| r["call_id"] != recorded.to_string()));

    let report = w.relay().await;
    assert_eq!(report.acked, 4, "{report:?}");
    w.assert_valid();
    let types: BTreeSet<String> = w
        .posted()
        .iter()
        .map(|e| e["eventType"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "calling.call.participant-missed",
            "calling.recording.ready",
            "calling.voicemail.ready"
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    );
}

#[tokio::test]
async fn retries_back_off_dead_letter_expire_and_purge() {
    let Some(w) = World::start(TenantClassification::FirstParty, true).await else {
        return;
    };
    let (caller, caller_phone, _) = w.account().await;
    let mut calls = Vec::new();
    for _ in 0..4 {
        let (callee, callee_phone, _) = w.account().await;
        let call = w
            .ringing((&caller, &caller_phone), (&callee, &callee_phone))
            .await;
        w.cancel(call).await;
        calls.push(call);
    }
    assert_eq!(w.rows().await.len(), 4);
    let state_of = |rows: &[Value], call: Uuid| -> Value {
        rows.iter()
            .find(|r| r["call_id"] == call.to_string())
            .cloned()
            .unwrap()
    };

    // One row expires before it is ever posted.
    w.sql(&format!(
        "UPDATE calling_notification_publications SET expires_at = now() - interval '1 second' \
          WHERE call_id = '{}'",
        calls[3]
    ))
    .await;

    // 503, 429, 401 and 408: retried with growing backoff, never dead.
    for (status, attempts) in [(503, 1), (429, 2), (401, 3), (408, 4)] {
        w.script(&[Scripted::Status(status)]);
        let report = w.relay().await;
        assert_eq!(report.retried, 3, "{status}: {report:?}");
        let rows = w.rows().await;
        for call in &calls[..3] {
            let row = state_of(&rows, *call);
            assert_eq!(row["state"], "PENDING", "{row}");
            assert_eq!(row["attempts"], attempts);
            assert_eq!(row["last_error"], "UNAVAILABLE");
        }
        // Not due again before its backoff.
        assert_eq!(w.relay().await.retried, 0);
        w.due_now().await;
    }
    let rows = w.rows().await;
    assert_eq!(state_of(&rows, calls[3])["state"], "EXPIRED");
    assert_eq!(state_of(&rows, calls[3])["last_error"], "EXPIRED");
    let expired_id = state_of(&rows, calls[3])["event_id"].clone();
    assert!(w.posted().iter().all(|e| e["eventId"] != expired_id));

    // A per-event REJECTED dead-letters with the service's machine reason.
    w.script(&[Scripted::Outcome("REJECTED", "DIGEST_MISMATCH")]);
    let report = w.relay().await;
    assert_eq!(report.dead, 3, "{report:?}");
    let rows = w.rows().await;
    for call in &calls[..3] {
        let row = state_of(&rows, *call);
        assert_eq!(
            (&row["state"], &row["outcome"], &row["last_error"]),
            (
                &json!("DEAD"),
                &json!("REJECTED"),
                &json!("DIGEST_MISMATCH")
            )
        );
    }

    // A contract refusal of the batch (400) dead-letters it too.
    let (callee, callee_phone, _) = w.account().await;
    let call = w
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    w.cancel(call).await;
    w.script(&[Scripted::Status(400)]);
    let report = w.relay().await;
    assert_eq!(report.dead, 1, "{report:?}");
    let row = state_of(&w.rows().await, call);
    assert_eq!(
        (&row["state"], &row["last_error"]),
        (&json!("DEAD"), &json!("CONTRACT_REJECTED"))
    );
    w.assert_valid();

    // Retention: settled rows older than 30 days are purged, younger ones kept.
    w.sql(&format!(
        "UPDATE calling_notification_publications SET completed_at = now() - interval '31 days' \
          WHERE call_id IN ('{}', '{}')",
        calls[0], calls[3]
    ))
    .await;
    let (expired, purged) = maintain(w.h().conn(), 1000).await.unwrap();
    assert_eq!((expired, purged), (0, 2));
    assert_eq!(w.rows().await.len(), 3);
}

#[tokio::test]
async fn nothing_is_written_or_sent_without_the_base_url() {
    let Some(w) = World::start(TenantClassification::FirstParty, false).await else {
        return;
    };
    let (caller, caller_phone, _) = w.account().await;
    let (callee, callee_phone, _) = w.account().await;
    let call = w
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    w.cancel(call).await;
    assert!(w.rows().await.is_empty());
    // Even if a row existed, an unconfigured target leaves it waiting (never sent).
    assert_eq!(w.relay().await, PublicationReport::default());
    assert!(w.batches().is_empty());

    // The history is required only with the base URL; readiness is exact.
    assert!(
        calling_migration::notification_publications_ready(w.h().conn())
            .await
            .unwrap()
    );
    let empty = TestDb::empty().await.unwrap();
    assert!(
        !calling_migration::notification_publications_ready(&empty.conn)
            .await
            .unwrap()
    );
    calling_migration::up_all(&empty.conn).await.unwrap();
    assert!(
        calling_migration::notification_publications_ready(&empty.conn)
            .await
            .unwrap()
    );
    assert!(calling_migration::pending_migrations(&empty.conn)
        .await
        .unwrap()
        .is_empty());
    // The previous image's readiness (main history, fail-closed on unknown versions) is
    // unchanged by the separate history.
    let main: i64 = empty
        .conn
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT count(*) FROM calling_seaql_migrations WHERE version LIKE '%notification_publications%'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index(0)
        .unwrap();
    assert_eq!(main, 0);
}

#[tokio::test]
async fn only_cf_meet_accounts_of_first_party_tenants_are_recipients() {
    // An EXTERNAL tenant's principals are customer identities, even with a cfmeet-user
    // reference: nothing is written.
    let Some(x) = World::start(TenantClassification::External, true).await else {
        return;
    };
    let (caller, caller_phone, _) = x.account().await;
    let (callee, callee_phone, _) = x.account().await;
    let call = x
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    x.cancel(call).await;
    assert!(x.rows().await.is_empty());
    assert_eq!(x.relay().await, PublicationReport::default());
    assert!(x.batches().is_empty());

    // A FIRST_PARTY tenant: a principal without an account is recorded SKIPPED.
    let Some(w) = World::start(TenantClassification::FirstParty, true).await else {
        return;
    };
    let (caller, caller_phone, _) = w.account().await;
    let (guest, guest_phone) = w
        .person(PrincipalKind::ExternalApplicationPrincipal, "guest-1")
        .await;
    let (lookalike, lookalike_phone) = w
        .person(
            PrincipalKind::TemporaryExternalParticipant,
            &format!("cfmeet-user:{}", Uuid::new_v4()),
        )
        .await;
    for (p, d) in [(&guest, &guest_phone), (&lookalike, &lookalike_phone)] {
        let call = w.ringing((&caller, &caller_phone), (p, d)).await;
        w.cancel(call).await;
    }
    let rows = w.rows().await;
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(row["state"], "SKIPPED");
        assert_eq!(row["last_error"], "RECIPIENT_NOT_CF_MEET_ACCOUNT");
        assert!(row["envelope"].is_null());
    }

    // A recipient deactivated before the relay is skipped there, never sent.
    let (callee, callee_phone, _) = w.account().await;
    let call = w
        .ringing((&caller, &caller_phone), (&callee, &callee_phone))
        .await;
    w.cancel(call).await;
    w.sql(&format!(
        "UPDATE platform_principals SET status = 'DEACTIVATED' WHERE id = '{}'",
        callee.id()
    ))
    .await;
    let report = w.relay().await;
    assert_eq!(report.skipped, 1, "{report:?}");
    assert!(w.batches().is_empty());
    let row = w
        .rows()
        .await
        .into_iter()
        .find(|r| r["call_id"] == call.to_string())
        .unwrap();
    assert_eq!(
        (&row["state"], &row["last_error"]),
        (&json!("SKIPPED"), &json!("RECIPIENT_INACTIVE"))
    );

    // The scope of a row is the Call's: a row naming a principal of another environment
    // is refused by the database.
    let other = w.e.a.w.consumer.env(EnvironmentKind::Test).id;
    let (stranger, _) = w
        .h()
        .platform
        .register_principal(
            other,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: format!("cfmeet-user:{}", Uuid::new_v4()),
            },
        )
        .await
        .unwrap();
    let refused = w
        .h()
        .conn()
        .execute_unprepared(&format!(
            "INSERT INTO calling_notification_publications (id, event_id, event_type, tenant_id, \
               application_id, environment_id, call_id, source_id, recipient_principal_id, state, \
               last_error, completed_at, expires_at) \
             SELECT gen_random_uuid(), 'evt_' || md5(random()::text), 'calling.call.participant-missed', \
               c.tenant_id, c.application_id, c.environment_id, c.id, gen_random_uuid(), '{}', \
               'SKIPPED', 'TEST', now(), now() FROM calls c WHERE c.id = '{call}'",
            stranger.id
        ))
        .await;
    assert!(refused.is_err());
}
