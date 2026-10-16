//! Part K §108: control-plane load evidence for Calling, measured with the shared
//! reporter (`tests/common/load.rs`). Each scenario prints exactly one line
//!
//! `LOAD <scenario> env=… duration_ms=… concurrency=… ops=… throughput_per_s=… p50_ms=…
//!  p95_ms=… p99_ms=… errors=… error_rate=… max_db_connections=… lock_waits=… worker_lag_ms=…`
//!
//! and asserts CORRECTNESS ONLY (error rate 0, invariants per operation, conservative
//! resource upper bounds that hold on any machine) — never a latency or throughput target,
//! so the suite cannot flake on a slow runner. Sizes scale with `CALLING_LOAD_SCALE`
//! (1–50, default 1: a CI-sized run).
//!
//! Documented targets — for a RELEASE build on a dedicated 4-CPU host with a local
//! PostgreSQL (evidence to compare runs against, never assertions; a debug build on a
//! shared CI runner is expected to measure well below them):
//! * protected call creation + Privacy Relay authorization: ≥ 20/s sequential and
//!   ≥ 50/s at concurrency 16;
//! * multi-device ringing fan-out (3 recipients × 5 devices): every Call offers 15
//!   invitations, ≥ 20 Calls/s at concurrency 8;
//! * callback relay requests: one Call and exactly one paid first leg per request,
//!   ≥ 20/s at concurrency 8;
//! * outbox backlog drain by the real publication pass (`publish_outbox_once`, batch 200):
//!   worker lag (backlog complete → last event published) ≤ 5 s for 1 000 events;
//! * Privacy Relay ingress (Session reads through Calling's port): ≥ 100/s at
//!   concurrency 16, no request lost or answered for the wrong tenant;
//! * the database pool never exceeds its configured size (20 per instance).

mod common;

use std::{sync::Arc, time::Instant};

use axum::http::{Method, StatusCode};
use calling::{domain::EnvironmentKind, platform::scopes::names};
use common::{
    app::AppWorld,
    deployment::{Deployment, Receiver},
    enterprise::credential,
    load::{run_bounded, LoadRun},
    privacy::FakePrivacyRelay,
    scalar_i64, TestResponse,
};
use serde_json::json;
use uuid::Uuid;

/// The test database pool size (`TestDb::connect`): the hard upper bound of one instance.
const POOL: i64 = 20;

fn scale() -> usize {
    std::env::var("CALLING_LOAD_SCALE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|n| (1..=50).contains(n))
        .unwrap_or(1)
}

fn created(r: &TestResponse) -> bool {
    r.status == StatusCode::CREATED
}

fn call_of(r: &TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

#[test]
fn percentiles_use_the_nearest_rank() {
    let samples: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(common::load::percentile(&samples, 50.0), 50.0);
    assert_eq!(common::load::percentile(&samples, 95.0), 95.0);
    assert_eq!(common::load::percentile(&samples, 99.0), 99.0);
    assert_eq!(common::load::percentile(&[7.0], 99.0), 7.0);
    assert_eq!(common::load::percentile(&[], 50.0), 0.0);
}

/// Protected call creation: Calling authorizes every Call with Privacy Relay (through
/// its port) before anything rings — sequential, then concurrent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn call_creation_and_authorization_throughput() {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let Some(a) = AppWorld::with_privacy_relay(privacy.clone()).await else {
        return;
    };
    privacy.allow(a.w.initiator.id().as_uuid());
    privacy.allow(a.w.recipient.id().as_uuid());
    let caller = a.device(&a.w.initiator, "load-caller").await;
    a.device(&a.w.recipient, "load-callee").await;
    let session = Uuid::new_v4();
    let url = a.h().db.url.clone();

    let n = 40 * scale();
    let run = LoadRun::start("call_create_authorize_sequential", 1, &url).await;
    for i in 0..n {
        run.op(
            a.create_protected_call(
                "VOICE",
                &a.w.initiator,
                &caller,
                &[&a.w.recipient],
                session,
                &format!("seq-{i}"),
            ),
            created,
        )
        .await;
    }
    let report = run.finish(None).await;
    assert_eq!(report.errors, 0);
    assert!(report.max_db_connections <= POOL);

    let n2 = 80 * scale();
    let concurrency = 16;
    let run = LoadRun::start("call_create_authorize_concurrent", concurrency, &url).await;
    let responses = run_bounded(
        &run,
        n2,
        concurrency,
        |i| {
            let (a, caller) = (&a, &caller);
            async move {
                a.create_protected_call(
                    "VOICE",
                    &a.w.initiator,
                    caller,
                    &[&a.w.recipient],
                    session,
                    &format!("con-{i}"),
                )
                .await
            }
        },
        created,
    )
    .await;
    let report = run.finish(None).await;
    assert_eq!(report.errors, 0, "error_rate must be 0");
    assert_eq!(report.error_rate, 0.0);
    assert!(report.max_db_connections <= POOL);
    // Invariants: one Call per request, each authorized exactly once by Privacy Relay
    // with its decision recorded, every Call ringing.
    assert!(responses
        .iter()
        .all(|r| r.json["call"]["state"] == "RINGING"));
    let total = (n + n2) as i64;
    assert_eq!(
        scalar_i64(a.h().conn(), "SELECT count(*) FROM calls").await,
        total
    );
    assert_eq!(
        scalar_i64(
            a.h().conn(),
            "SELECT count(*) FROM (SELECT call_id FROM call_privacy_authorizations \
               GROUP BY call_id HAVING count(*) = 1) one"
        )
        .await,
        total
    );
    assert_eq!(privacy.requests().len() as i64, total);
}

/// Concurrent multi-device ringing: every Call offers every device of every target
/// exactly once (3 recipients × 5 devices).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_multi_device_ringing_fanout() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "fanout-caller").await;
    let mut targets = Vec::new();
    for i in 0..3 {
        let p = a.principal(&format!("fanout-{i}")).await;
        for k in 0..5 {
            a.device(&p, &format!("fanout-{i}-{k}")).await;
        }
        targets.push(p);
    }
    let refs: Vec<_> = targets.iter().collect();
    let n = 30 * scale();
    let concurrency = 8;
    let run = LoadRun::start("ringing_fanout_concurrent", concurrency, &a.h().db.url).await;
    let responses = run_bounded(
        &run,
        n,
        concurrency,
        |i| {
            let (a, caller, refs) = (&a, &caller, &refs);
            async move {
                a.create_call("VIDEO", &a.w.initiator, caller, refs, &format!("fan-{i}"))
                    .await
            }
        },
        |r: &TestResponse| created(r) && r.json["liveInvitationCount"] == 15,
    )
    .await;
    let report = run.finish(None).await;
    assert_eq!(report.errors, 0);
    assert!(report.max_db_connections <= POOL);
    for r in &responses {
        let call = call_of(r);
        assert_eq!(
            scalar_i64(
                a.h().conn(),
                &format!(
                    "SELECT count(DISTINCT endpoint_id) FROM call_invitations \
                      WHERE call_id = '{call}' AND status = 'RINGING'"
                )
            )
            .await,
            15,
            "each device offered exactly once"
        );
    }
}

/// Concurrent callback relay requests (Part E §47): one Call per request, exactly one
/// paid first leg (the requester), never the callee before the first leg answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn callback_relay_concurrency() {
    let Some(d) = Deployment::new().await else {
        return;
    };
    let n = 40 * scale();
    // Requesters' and callees' verified contacts (≤ 100 requests per party).
    let parties = n.div_ceil(100);
    let mines: Vec<Uuid> = (0..parties)
        .map(|_| d.x.privacy.add_contact("NG"))
        .collect();
    let theirs: Vec<Uuid> = (0..parties)
        .map(|_| d.x.privacy.add_contact("NG"))
        .collect();
    // Spend headroom for the whole burst (a callback reserves both legs): limits are not
    // what this scenario measures (public_spend.rs certifies them).
    let scope = d.e.a.w.scope();
    calling::telephony::spend::store_policy(
        d.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &calling::telephony::spend::SpendPolicy {
            max_concurrent_legs: 10_000,
            max_originations_per_minute: 10_000,
            max_per_party_per_hour: 1_000,
            daily_limit_minor: 1_000_000_000,
            monthly_limit_minor: 10_000_000_000,
            ..common::public::spend_policy()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let concurrency = 8;
    let run = LoadRun::start("callback_relay_concurrent", concurrency, &d.h().db.url).await;
    let responses = run_bounded(
        &run,
        n,
        concurrency,
        |i| {
            let (d, mines, theirs) = (&d, &mines, &theirs);
            async move {
                d.a()
                    .post_with(
                        "/v1/calls",
                        json!({
                            "kind": "VOICE",
                            "callbackRequested": true,
                            "initiatorPrincipalId": d.e.a.w.initiator.id(),
                            "initiatorContactReference": mines[i % parties],
                            "targetContactReference": theirs[i % parties],
                            "privacyRelaySessionId": d.session,
                        }),
                        &[("idempotency-key", &format!("cb-{i}"))],
                    )
                    .await
            }
        },
        created,
    )
    .await;
    let report = run.finish(None).await;
    assert_eq!(
        report.errors,
        0,
        "{:?}",
        responses.iter().find(|r| !created(r)).map(|r| &r.text)
    );
    assert!(report.max_db_connections <= POOL);
    let executions = d.x.telephony.executions();
    assert_eq!(executions.len(), n, "exactly one paid leg per request");
    for (i, r) in responses.iter().enumerate() {
        let call = call_of(r);
        let legs: Vec<_> = executions
            .iter()
            .filter(|(_, _, v)| v["call"]["callId"] == json!(call))
            .collect();
        assert_eq!(legs.len(), 1, "{call}: one first leg");
        // The requester is called first; never the callee before that leg answered.
        assert_eq!(
            legs[0].2["destination"]["contactReference"],
            json!(mines[i % parties])
        );
    }
}

/// An outbox backlog (no worker running) drained by the real publication pass with a
/// webhook subscription fanning every public event out: worker lag and drain rate.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_backlog_drain_by_the_worker() {
    let Some(d) = Deployment::new().await else {
        return;
    };
    let a = d.a();
    let rx = Receiver::start().await;
    let manager = d
        .credential(&[
            names::WEBHOOKS_MANAGE,
            names::WEBHOOKS_READ,
            names::CALLS_READ,
        ])
        .await;
    let r =
        d.e.send(
            manager.secret.expose_once(),
            Method::POST,
            "/v1/webhooks",
            Some(json!({ "url": rx.url, "eventTypes": ["call.ringing", "call.cancelled"] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    a.device(&a.w.recipient, "backlog-callee").await;
    // Backlog: Calls created and cancelled while no publication worker runs.
    let calls = 60 * scale();
    let caller = d.caller.clone();
    let build = LoadRun::start("outbox_backlog_build", 8, &d.h().db.url).await;
    run_bounded(
        &build,
        calls,
        8,
        |i| {
            let caller = caller.clone();
            async move {
                let r = a
                    .create_call(
                        "VOICE",
                        &a.w.initiator,
                        &caller,
                        &[&a.w.recipient],
                        &format!("bk-{i}"),
                    )
                    .await;
                assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
                let call = call_of(&r);
                let (_, v) = a.state(call).await;
                a.command(call, "cancel", v).await
            }
        },
        |r: &TestResponse| r.status == StatusCode::OK,
    )
    .await;
    let built = build.finish(None).await;
    assert_eq!(built.errors, 0);
    let backlog = scalar_i64(
        d.h().conn(),
        "SELECT count(*) FROM calling_outbox_events WHERE status = 'PENDING'",
    )
    .await;
    assert!(backlog >= (calls * 3) as i64, "a real backlog: {backlog}");
    // Drain with the worker loop body of `main` (publication pass, batch 200), back to back.
    let webhooks = d.h().state.webhooks.clone();
    let transport = calling::webhooks::worker::LocalWebhookTransport::new((*webhooks).clone());
    let run = LoadRun::start("outbox_backlog_drain", 1, &d.h().db.url).await;
    let started = Instant::now();
    let mut published = 0usize;
    let mut passes = 0;
    loop {
        let pass = Instant::now();
        let report = webhooks.publish_outbox_once(&transport, 200).await.unwrap();
        let took = pass.elapsed();
        passes += 1;
        // Per-event latency: the pass time shared by the events it published.
        for _ in 0..report.published {
            run.record(took / report.published as u32, true);
        }
        for _ in 0..(report.retried + report.dead_lettered) {
            run.record(took, false);
        }
        published += report.published;
        if report.published + report.retried == 0 {
            break;
        }
        assert!(passes < 10_000, "the backlog drains");
    }
    let lag = started.elapsed();
    let report = run.finish(Some(lag)).await;
    assert_eq!(report.errors, 0);
    assert_eq!(
        published as i64, backlog,
        "every event published exactly once"
    );
    assert_eq!(
        scalar_i64(
            d.h().conn(),
            "SELECT count(*) FROM calling_outbox_events WHERE status <> 'PUBLISHED' OR published_at IS NULL"
        )
        .await,
        0
    );
    // Fan-out: one public webhook event per source event, one delivery each.
    assert_eq!(
        scalar_i64(
            d.h().conn(),
            "SELECT count(*) FROM (SELECT source_event_id FROM webhook_events \
               GROUP BY producer, source_event_id HAVING count(*) > 1) d"
        )
        .await,
        0
    );
    assert!(report.max_db_connections <= POOL);
}

/// Privacy Relay access volume through Calling's port: Session reads proxied for the
/// caller's tenant only (another tenant's Session answers 404 exactly like an unknown one).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn privacy_relay_access_volume_through_the_port() {
    let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let Some(a) = AppWorld::with_privacy_relay(privacy.clone()).await else {
        return;
    };
    let token = credential(
        a.h(),
        &a.w.consumer,
        EnvironmentKind::Production,
        &[names::PRIVACY_SESSIONS_READ],
    )
    .await;
    let tenant = a.w.scope().tenant_id().as_uuid();
    let owned: Vec<Uuid> = (0..8).map(|_| Uuid::new_v4()).collect();
    for s in &owned {
        privacy.own_session(*s, tenant);
    }
    let foreign = Uuid::new_v4();
    privacy.own_session(foreign, Uuid::new_v4());
    let n = 200 * scale();
    let concurrency = 16;
    let secret = token.secret.expose_once().to_string();
    let run = LoadRun::start("privacy_relay_session_reads", concurrency, &a.h().db.url).await;
    let responses = run_bounded(
        &run,
        n,
        concurrency,
        |i| {
            let session = if i % 5 == 4 {
                foreign
            } else {
                owned[i % owned.len()]
            };
            let secret = secret.clone();
            let h = a.h();
            async move {
                let r = h
                    .request(
                        Method::GET,
                        &format!("/v1/privacy-relay/sessions/{session}"),
                        Some(&secret),
                        None,
                    )
                    .await;
                (session == foreign, r)
            }
        },
        |(is_foreign, r)| {
            if *is_foreign {
                r.status == StatusCode::NOT_FOUND
            } else {
                r.status == StatusCode::OK
            }
        },
    )
    .await;
    let report = run.finish(None).await;
    assert_eq!(report.errors, 0);
    assert!(report.max_db_connections <= POOL);
    assert_eq!(privacy.proxied.lock().unwrap().len(), n, "no request lost");
    assert!(responses
        .iter()
        .filter(|(f, _)| !f)
        .all(|(_, r)| r.json["relaySessionId"].is_string()));
}
