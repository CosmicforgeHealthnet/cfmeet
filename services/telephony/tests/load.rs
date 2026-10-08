//! Load / contention measurement (Part E §212) against PostgreSQL with the
//! deterministic interconnect: command throughput (durable intake + execution), provider
//! event ingestion (incl. a full duplicate replay), and reconciliation batches with two
//! instances. Sizes are modest so the suite stays fast; the numbers are printed
//! (`LOAD:` lines) and the invariants — one paid effect per command, one record per
//! provider event, one repair per leg — are asserted.

mod common;

use std::time::Instant;

use axum::http::{Method, StatusCode};
use common::Harness;
use telephony::domain::{EndReason, NormalizedEvent};
use uuid::Uuid;

const LEGS: usize = 200;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_event_and_reconciliation_throughput() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Raise the routes' configured capacity for the run.
    h.sql("UPDATE telecom_routes SET max_concurrent_legs = 1000")
        .await;
    h.sql("UPDATE telecom_providers SET max_concurrent_legs = 1000")
        .await;

    // 1. Durable intake: LEGS originate commands, 50 in flight at a time.
    let bodies: Vec<_> = (0..LEGS)
        .map(|_| {
            let contact = h.privacy.add_contact("+2348031234567", "NG");
            h.originate_body(contact, Uuid::new_v4(), 1)
        })
        .collect();
    let started = Instant::now();
    let mut legs = Vec::new();
    for chunk in bodies.chunks(50) {
        let rs = futures::future::join_all(
            chunk
                .iter()
                .map(|b| h.calling(Method::POST, "/internal/v1/legs/originate", Some(b.clone()))),
        )
        .await;
        for r in rs {
            assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
            legs.push(
                r.json["leg"]["telephonyLegId"]
                    .as_str()
                    .unwrap()
                    .parse::<Uuid>()
                    .unwrap(),
            );
        }
    }
    let intake = started.elapsed();
    // Execution by two worker instances.
    let other = h.svc.clone().as_instance("w-load-2");
    let started = Instant::now();
    while h.provider_a.requests().len() + h.provider_b.requests().len() < LEGS {
        let (a, b) = tokio::join!(h.svc.run_commands_once(100), other.run_commands_once(100));
        a.unwrap();
        b.unwrap();
        assert!(started.elapsed().as_secs() < 120, "execution stalled");
    }
    let execution = started.elapsed();
    let effects = h.provider_a.effects().len() + h.provider_b.effects().len();
    assert_eq!(effects, LEGS, "exactly one paid effect per command");
    eprintln!(
        "LOAD: intake {LEGS} commands in {intake:?} ({:.0}/s); execution in {execution:?} ({:.0}/s, 2 workers)",
        LEGS as f64 / intake.as_secs_f64(),
        LEGS as f64 / execution.as_secs_f64()
    );

    // 2. Provider event ingestion: RINGING / ANSWERED / CONNECTED for every leg, then a
    // full duplicate replay of all of them.
    let mut events = Vec::new();
    for leg in &legs {
        let correlation = h.correlation_of(*leg).await;
        let provider = if h.provider_a.call(&correlation).is_some() {
            "dp-a"
        } else {
            "dp-b"
        };
        let p = if provider == "dp-a" {
            &h.provider_a
        } else {
            &h.provider_b
        };
        for e in [
            NormalizedEvent::LegRinging,
            NormalizedEvent::LegAnswered,
            NormalizedEvent::LegConnected,
        ] {
            events.push((provider, p.progress(&correlation, e, h.now())));
        }
    }
    let started = Instant::now();
    for chunk in events.chunks(60) {
        let rs = futures::future::join_all(
            chunk
                .iter()
                .map(|(p, e)| h.svc.ingest_provider_event(p, e.clone())),
        )
        .await;
        for r in rs {
            r.unwrap();
        }
    }
    let ingest = started.elapsed();
    let started = Instant::now();
    for chunk in events.chunks(60) {
        let rs = futures::future::join_all(
            chunk
                .iter()
                .map(|(p, e)| h.svc.ingest_provider_event(p, e.clone())),
        )
        .await;
        for r in rs {
            assert_eq!(r.unwrap().as_str(), "DUPLICATE");
        }
    }
    let replay = started.elapsed();
    assert_eq!(
        h.scalar("SELECT count(*) FROM provider_events").await,
        events.len() as i64,
        "one record per provider event"
    );
    eprintln!(
        "LOAD: ingested {} provider events in {ingest:?} ({:.0}/s); duplicate replay in {replay:?} ({:.0}/s)",
        events.len(),
        events.len() as f64 / ingest.as_secs_f64(),
        events.len() as f64 / replay.as_secs_f64()
    );

    // 3. Reconciliation: every provider call ends silently; two reconcilers repair.
    for leg in &legs {
        let correlation = h.correlation_of(*leg).await;
        h.provider_a
            .end_silently(&correlation, EndReason::Completed);
        h.provider_b
            .end_silently(&correlation, EndReason::Completed);
    }
    h.advance(601);
    let started = Instant::now();
    let mut rounds = 0;
    loop {
        let (a, b) = tokio::join!(h.svc.reconcile_once(50), other.reconcile_once(50));
        let n = a.unwrap() + b.unwrap();
        rounds += 1;
        if n == 0 {
            break;
        }
        assert!(rounds < 100, "reconciliation did not converge");
    }
    let reconcile = started.elapsed();
    assert_eq!(
        h.scalar("SELECT count(*) FROM telephony_legs WHERE status = 'ENDED'")
            .await,
        LEGS as i64
    );
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_reconciliation_runs WHERE action = 'MARKED_ENDED'"
        )
        .await,
        LEGS as i64,
        "exactly one effective repair per leg"
    );
    eprintln!(
        "LOAD: reconciled {LEGS} legs in {reconcile:?} ({rounds} paired batches of <=50, {:.0} legs/s)",
        LEGS as f64 / reconcile.as_secs_f64()
    );
}
