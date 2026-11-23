//! ADR 0068 phase A7: Calling's owner events to the HQ admin bridge —
//! `abuse.report.changed.v1` and `incident.changed.v1` — through the transactional outbox
//! `calling_admin_bridge_events` and its relay to a fake bridge event listener
//! (`POST /internal/v1/hq-bridge/events`) that verifies the REAL service token (issuer
//! `cfmeet.calling`, audience `cfmeet.admin-bridge`, `hq-bridge-events.publish`) and the
//! closed body. Off unless `CF_ADMIN_BRIDGE_BASE_URL` is configured; ids, states, versions
//! and times only; a platform-wide incident is never published. Real PostgreSQL.

mod common;

use std::sync::{Arc, Mutex};

use axum::{
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use calling::{
    admin_bridge_events::{
        maintain, relay_due, set_enabled, AdminBridgeEvents, RelayReport, EVENTS_PATH,
    },
    calls::app::{incidents, AbuseControls},
    domain::{EnvironmentKind, ManualClock, TenantClassification},
    platform::operator::OperatorContext,
};
use cf_service_auth::{
    InternalPermission, PublicJwk, RuntimeEnvironment, ServiceId, ServiceTokenVerifier, TrustPolicy,
};
use common::{
    app::AppWorld,
    bridge::{self, Attribution},
    calls::CallWorld,
    fake_media::FakeMediaProvider,
    service_auth_fixture_targeting, t0, Harness, TestDb,
};
use sea_orm::ConnectionTrait;
use serde_json::{json, Value};
use uuid::Uuid;

/// The switch is process-wide: the tests of this binary run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ------------------------------------------------------------------- fake bridge

#[derive(Default)]
struct Listener {
    /// Every authenticated, accepted body.
    events: Vec<Value>,
    /// Scripted HTTP statuses for the next requests.
    script: Vec<u16>,
    seen: std::collections::BTreeSet<String>,
    unauthenticated: usize,
    /// Bodies that are not the closed shape (must stay empty).
    invalid: Vec<String>,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    inner: Arc<Mutex<Listener>>,
}

fn closed_body_ok(b: &Value) -> bool {
    let Some(o) = b.as_object() else {
        return false;
    };
    let mut keys: Vec<&str> = o.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let subject_ok = b["subject"]
        .as_object()
        .is_some_and(|s| s.len() == 2 && s["kind"].is_string() && s["id"].is_string());
    keys == [
        "event_id",
        "occurred_at",
        "state",
        "subject",
        "type",
        "version",
    ] && b["event_id"]
        .as_str()
        .is_some_and(|e| Uuid::parse_str(e).is_ok())
        && subject_ok
        && b["version"].as_i64().is_some_and(|v| v >= 1)
        && b["occurred_at"]
            .as_str()
            .is_some_and(|t| chrono::DateTime::parse_from_rfc3339(t).is_ok())
}

async fn ingest(
    AxumState(ctx): AxumState<Arc<Ctx>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let refused = |status: u16, code: &str| {
        (
            StatusCode::from_u16(status).unwrap(),
            Json(json!({ "error": { "code": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        ctx.inner.lock().unwrap().unauthenticated += 1;
        return refused(401, "UNAUTHENTICATED");
    };
    if !caller.has_permission(InternalPermission::HqBridgeEventsPublish)
        || caller.authenticated_service().as_str() != cf_service_auth::services::CALLING
    {
        return refused(403, "FORBIDDEN");
    }
    let mut inner = ctx.inner.lock().unwrap();
    if !inner.script.is_empty() {
        let status = inner.script.remove(0);
        return refused(status, "SCRIPTED");
    }
    if !closed_body_ok(&body) {
        inner.invalid.push(body.to_string());
        return refused(400, "EVENT_INVALID");
    }
    let id = body["event_id"].as_str().unwrap().to_string();
    let status = if inner.seen.insert(id) {
        inner.events.push(body);
        "APPENDED"
    } else {
        "DUPLICATE"
    };
    (
        StatusCode::ACCEPTED,
        Json(json!({ "status": status, "sequence": null })),
    )
}

fn verifier(calling: &PublicJwk, clock: cf_service_auth::ManualClock) -> ServiceTokenVerifier {
    let bundle = json!({
        "bundle_version": 1,
        "receiver": ServiceId::admin_bridge(),
        "runtime_environment": "test",
        "trusted_issuers": [{
            "issuer": ServiceId::calling(),
            "audience": ServiceId::admin_bridge(),
            "runtime_environment": "test",
            "keys": [calling],
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
        Arc::new(clock),
    )
}

// ------------------------------------------------------------------------- world

/// A Calling deployment whose `CF_ADMIN_BRIDGE_BASE_URL` points at a fake bridge event
/// listener (when `bridge`), raising an ABUSE_REPORTS_BURST incident from the first report
/// against a principal.
struct World {
    a: AppWorld,
    listener: Arc<Mutex<Listener>>,
    sink: AdminBridgeEvents,
}

impl World {
    async fn start(bridge: bool) -> Option<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let db = Arc::new(TestDb::new().await?);
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = common::test_runtime(Some(media.clone()), bus.clone());
        runtime.abuse_controls = AbuseControls {
            abuse_reports_incident_threshold: 1,
            ..AbuseControls::default()
        };
        let push = Arc::new(common::RecordingPush::default());
        runtime.push = push.clone();
        let targets: Vec<(ServiceId, &str)> = if bridge {
            vec![(ServiceId::admin_bridge(), url.as_str())]
        } else {
            Vec::new()
        };
        let (service_auth, peers) = service_auth_fixture_targeting(&targets);
        let state = Arc::new(Mutex::new(Listener::default()));
        let ctx = Arc::new(Ctx {
            verifier: verifier(&service_auth.signer().public_jwk(), peers.clock.clone()),
            inner: state.clone(),
        });
        let app = Router::new()
            .route(EVENTS_PATH, post(ingest))
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
        let sink = AdminBridgeEvents::new(h.state.service_auth.client_arc());
        let consumer = h.consumer("acme", TenantClassification::External).await;
        let w = CallWorld::for_consumer(h, consumer, EnvironmentKind::Production).await;
        // The deployment's switch follows its configuration (as `main` sets it).
        set_enabled(bridge);
        Some(Self {
            a: AppWorld { w },
            listener: state,
            sink,
        })
    }

    fn h(&self) -> &Harness {
        self.a.h()
    }

    fn tenant(&self) -> Uuid {
        self.a.w.consumer.tenant.id.as_uuid()
    }

    async fn outbox(&self) -> i64 {
        self.a
            .w
            .count("SELECT count(*) FROM calling_admin_bridge_events")
            .await
    }

    /// A call from the world's initiator to a new principal `n`, reported by that
    /// principal (the initiator is reported); returns the report id.
    async fn report(&self, n: usize) -> Uuid {
        let target = &self.a.w.initiator;
        let phone = self.a.device(target, "target-main").await;
        let reporter = self.a.principal(&format!("reporter-{n}")).await;
        self.a
            .device(&reporter, &format!("reporter-{n}-main"))
            .await;
        let r = self
            .a
            .create_call(
                "VOICE",
                target,
                &phone,
                &[&reporter],
                &format!("report-{n}"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let r = self
            .a
            .post(
                &format!("/v1/calls/{call}/abuse-reports"),
                json!({ "principalId": reporter.id(), "category": "SPAM" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["abuseReportId"].as_str().unwrap().parse().unwrap()
    }

    async fn incident_of(&self, principal: Uuid) -> Uuid {
        let rows = self
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT id FROM calling_incidents WHERE category = 'ABUSE_REPORTS_BURST' \
                 AND resource_id = $1 AND status = 'OPEN'",
                vec![principal.into()],
            ))
            .await
            .unwrap()
            .expect("the burst incident is open");
        rows.try_get_by_index(0).unwrap()
    }

    async fn relay(&self) -> RelayReport {
        relay_due(self.h().conn(), &self.sink, 100).await.unwrap()
    }

    fn events(&self) -> Vec<Value> {
        self.listener.lock().unwrap().events.clone()
    }
}

fn assert_ids_only(events: &[Value]) {
    let text = serde_json::to_string(events).unwrap().to_lowercase();
    for banned in [
        "sip:",
        "tel:",
        "+234",
        "spam",
        "principal",
        "tenant",
        "evidence",
    ] {
        assert!(
            !text.contains(banned),
            "{banned} must never be sent: {text}"
        );
    }
}

// ------------------------------------------------------------------------- tests

#[tokio::test]
async fn nothing_is_stored_or_sent_without_the_bridge_base_url() {
    let _serial = SERIAL.lock().await;
    let Some(w) = World::start(false).await else {
        return;
    };
    let report = w.report(1).await;
    let incident = w.incident_of(w.a.w.initiator.id().as_uuid()).await;
    let r = bridge::command(
        w.h(),
        &format!("/internal/v1/operator/abuse-reports/{report}/resolve"),
        json!({ "tenantId": w.tenant(), "expectedVersion": 1, "status": "REVIEWED" }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = bridge::command(
        w.h(),
        &format!("/internal/v1/operator/incidents/{incident}/resolve"),
        json!({ "tenantId": w.tenant() }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(w.outbox().await, 0, "the outbox stays empty when off");
    // A relay without a bridge target sends nothing (nothing is due anyway).
    assert_eq!(w.relay().await, RelayReport::default());
    assert!(w.events().is_empty());
}

#[tokio::test]
async fn abuse_reports_and_tenant_incidents_reach_the_bridge_ids_only() {
    let _serial = SERIAL.lock().await;
    let Some(w) = World::start(true).await else {
        return;
    };
    let report = w.report(1).await;
    let reported = w.a.w.initiator.id().as_uuid();
    let incident = w.incident_of(reported).await;
    // Filed (OPEN v1) and the new burst incident (OPEN v1), in the report's transaction.
    assert_eq!(w.outbox().await, 2);
    // A repeat occurrence of the open incident changes no state: no event.
    let second = w.report(2).await;
    assert_eq!(w.incident_of(reported).await, incident);
    assert_eq!(w.outbox().await, 3, "only the second report's own event");

    // The bridge reviews the first report (v2) and resolves the incident (RESOLVED v2).
    let r = bridge::command(
        w.h(),
        &format!("/internal/v1/operator/abuse-reports/{report}/resolve"),
        json!({ "tenantId": w.tenant(), "expectedVersion": 1, "status": "DISMISSED" }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = bridge::command(
        w.h(),
        &format!("/internal/v1/operator/incidents/{incident}/resolve"),
        json!({ "tenantId": w.tenant() }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Resolving it again changes nothing: no event.
    let r = bridge::command(
        w.h(),
        &format!("/internal/v1/operator/incidents/{incident}/resolve"),
        json!({ "tenantId": w.tenant() }),
        Attribution::Operator,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(w.outbox().await, 5);

    // A platform-wide incident is never published (resolved by `platform_admin`).
    let platform = Uuid::new_v4();
    common::exec(
        w.h().conn(),
        &format!(
            "INSERT INTO calling_incidents (id, tenant_id, application_id, environment_id, dedupe_key, \
               category, severity, status, resource_type, resource_id, occurrences, restricted_evidence, \
               first_seen_at, last_seen_at) VALUES \
             ('{platform}', NULL, NULL, NULL, 'worker_backlog:00000000000000000000000000000000:outbox', \
              'WORKER_BACKLOG', 'HIGH', 'OPEN', NULL, NULL, 1, '{{\"queue\": \"outbox\"}}', now(), now())"
        ),
    )
    .await
    .unwrap();
    incidents::operator_resolve(
        w.h().conn(),
        &OperatorContext::new("ops-0001").unwrap(),
        platform,
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    assert_eq!(w.outbox().await, 5);

    let report_out = w.relay().await;
    assert_eq!(report_out.acked, 5, "{report_out:?}");
    assert_eq!(report_out.dead + report_out.retried, 0);
    {
        let listener = w.listener.lock().unwrap();
        assert_eq!(listener.unauthenticated, 0);
        assert!(listener.invalid.is_empty(), "{:?}", listener.invalid);
    }
    let events = w.events();
    assert_ids_only(&events);
    let seen: Vec<(String, String, String, String, i64)> = events
        .iter()
        .map(|e| {
            (
                e["type"].as_str().unwrap().to_string(),
                e["subject"]["kind"].as_str().unwrap().to_string(),
                e["subject"]["id"].as_str().unwrap().to_string(),
                e["state"].as_str().unwrap().to_string(),
                e["version"].as_i64().unwrap(),
            )
        })
        .collect();
    let abuse = |id: Uuid, state: &str, v: i64| {
        (
            "abuse.report.changed.v1".to_string(),
            "abuse_report".to_string(),
            id.to_string(),
            state.to_string(),
            v,
        )
    };
    let inc = |state: &str, v: i64| {
        (
            "incident.changed.v1".to_string(),
            "incident".to_string(),
            incident.to_string(),
            state.to_string(),
            v,
        )
    };
    for expected in [
        abuse(report, "OPEN", 1),
        inc("OPEN", 1),
        abuse(second, "OPEN", 1),
        abuse(report, "DISMISSED", 2),
        inc("RESOLVED", 2),
    ] {
        assert!(seen.contains(&expected), "{expected:?} in {seen:?}");
    }
    assert!(!seen.iter().any(|s| s.2 == platform.to_string()));
    assert_eq!(
        w.a.w
            .count(
                "SELECT count(*) FROM calling_admin_bridge_events \
                 WHERE delivery_state = 'ACKED' AND outcome = 'APPENDED'"
            )
            .await,
        5
    );
    // Nothing is due any more; settled rows are kept for diagnostics until retention.
    assert_eq!(w.relay().await, RelayReport::default());
    assert_eq!(maintain(w.h().conn(), 100).await.unwrap(), (0, 0));
}

#[tokio::test]
async fn an_unavailable_bridge_is_retried_and_a_refused_event_dead_lettered() {
    let _serial = SERIAL.lock().await;
    let Some(w) = World::start(true).await else {
        return;
    };
    let report = w.report(1).await;
    assert_eq!(w.outbox().await, 2);
    // 503: retried with backoff, the pass stops (the other row waits, no attempt counted).
    w.listener.lock().unwrap().script = vec![503];
    let r = w.relay().await;
    assert_eq!((r.retried, r.acked, r.dead), (1, 0, 0), "{r:?}");
    assert_eq!(
        w.a.w
            .count(
                "SELECT count(*) FROM calling_admin_bridge_events WHERE delivery_state = 'PENDING' \
                 AND attempts = 1 AND last_error = 'UNAVAILABLE' AND next_attempt_at > now()"
            )
            .await,
        1
    );
    // Due again: one refused for good (400), the other appended.
    common::exec(
        w.h().conn(),
        "UPDATE calling_admin_bridge_events SET next_attempt_at = now() - interval '1 second'",
    )
    .await
    .unwrap();
    w.listener.lock().unwrap().script = vec![400];
    let r = w.relay().await;
    assert_eq!((r.dead, r.acked), (1, 1), "{r:?}");
    assert_eq!(
        w.a.w
            .count(
                "SELECT count(*) FROM calling_admin_bridge_events WHERE delivery_state = 'DEAD' \
                 AND outcome = 'REJECTED' AND last_error = 'CONTRACT_REJECTED'"
            )
            .await,
        1
    );
    // A re-post of an acknowledged event (a crash before the settle) is a DUPLICATE.
    common::exec(
        w.h().conn(),
        "UPDATE calling_admin_bridge_events SET delivery_state = 'PENDING', outcome = NULL, \
           completed_at = NULL WHERE delivery_state = 'ACKED'",
    )
    .await
    .unwrap();
    let r = w.relay().await;
    assert_eq!(r.acked, 1, "{r:?}");
    assert_eq!(
        w.a.w
            .count("SELECT count(*) FROM calling_admin_bridge_events WHERE outcome = 'DUPLICATE'")
            .await,
        1
    );
    // An overdue event expires unsent; settled rows past retention are purged.
    let n = w.outbox().await;
    common::exec(
        w.h().conn(),
        &format!(
            "INSERT INTO calling_admin_bridge_events (event_id, event_type, subject_kind, subject_id, \
               tenant_id, subject_state, subject_version, occurred_at, expires_at) VALUES \
             ('{}', 'abuse.report.changed.v1', 'abuse_report', '{report}', '{}', 'REVIEWED', 2, now(), \
              now() - interval '1 second')",
            Uuid::new_v4(),
            w.tenant()
        ),
    )
    .await
    .unwrap();
    common::exec(
        w.h().conn(),
        "UPDATE calling_admin_bridge_events SET completed_at = now() - interval '31 days' \
         WHERE delivery_state <> 'PENDING'",
    )
    .await
    .unwrap();
    assert_eq!(maintain(w.h().conn(), 100).await.unwrap(), (1, n as u64));
    assert_eq!(
        w.outbox().await,
        1,
        "the just-expired row is kept until retention"
    );
}
