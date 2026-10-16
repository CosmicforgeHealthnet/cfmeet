//! The outbox of normalized events to Calling (Part I §81, §85): permanent refusals and
//! exhausted retries are dead-lettered (final, one deduplicated incident per reason)
//! instead of retried forever; transient failures back off with a cap; a delivery whose
//! worker died is reclaimed after its lease and delivered exactly once; and every
//! delivered event continues the correlation id of the command whose flow produced it
//! (also in the worker's log span, with the leg id).

mod common;

use std::{
    io,
    sync::{Arc, Mutex},
};

use axum::http::{Method, StatusCode};
use common::Harness;
use serde_json::{json, Value};
use telephony::{domain::NormalizedEvent, events::outbox};
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

/// An executed outbound leg whose LEG_ACCEPTED is waiting in the outbox (Calling down).
async fn accepted_leg(h: &Harness) -> (Uuid, String) {
    h.calling.set_available(false);
    let (leg, body) = h.originate().await;
    h.run().await;
    h.calling.set_available(true);
    // Past the first (failed) attempt's backoff.
    h.advance(1);
    (leg, body["correlationId"].as_str().unwrap().to_string())
}

async fn row_state(h: &Harness, leg: Uuid) -> (i64, i64, i64) {
    let q = |cond: &str| {
        format!("SELECT count(*) FROM telephony_outbox WHERE telephony_leg_id = '{leg}' AND {cond}")
    };
    (
        h.scalar(&q("delivered_at IS NOT NULL")).await,
        h.scalar(&q("dead_lettered_at IS NOT NULL")).await,
        h.scalar(&q("delivered_at IS NULL AND dead_lettered_at IS NULL"))
            .await,
    )
}

async fn open_dead_letter_incidents(h: &Harness, reason: &str) -> i64 {
    h.scalar(&format!(
        "SELECT coalesce(sum(occurrences), 0)::bigint FROM telephony_incidents WHERE category = 'OUTBOX_DEAD_LETTER' \
         AND status = 'OPEN' AND dedupe_key = 'outbox-dead-letter:{reason}'"
    ))
    .await
}

fn attempts_of(h: &Harness, event_id: &Value) -> usize {
    h.calling
        .event_attempts
        .lock()
        .unwrap()
        .iter()
        .filter(|e| *e == event_id)
        .count()
}

#[tokio::test]
async fn a_permanent_refusal_is_dead_lettered_once_and_never_retried() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let dead_before = telephony::metrics::counter(
        "telephony_outbox_dead_letters_total",
        &[("reason", "CALLING_REFUSED")],
    );
    // Calling refuses the body (4xx): retrying cannot help.
    let (leg, _) = accepted_leg(&h).await;
    h.calling.set_event_mode("DENIED");
    let (delivered, failed) = h.svc.deliver_outbox_once(100).await.unwrap();
    assert_eq!((delivered, failed), (0, 1));
    assert_eq!(row_state(&h, leg).await, (0, 1, 0));
    assert_eq!(
        h.text(&format!(
            "SELECT last_error_code FROM telephony_outbox WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .unwrap(),
        "VALIDATION_ERROR"
    );
    assert_eq!(open_dead_letter_incidents(&h, "CALLING_REFUSED").await, 1);
    assert!(
        telephony::metrics::counter(
            "telephony_outbox_dead_letters_total",
            &[("reason", "CALLING_REFUSED")]
        ) > dead_before
    );
    let event_id = json!(h
        .text(&format!(
            "SELECT id::text FROM telephony_outbox WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .unwrap());
    // The unavailable attempt while the leg executed, then the refused one.
    assert_eq!(attempts_of(&h, &event_id), 2);
    // The dead letter is set once and excludes delivery (database guards).
    let id = event_id.as_str().unwrap();
    assert!(h
        .try_sql(&format!(
            "UPDATE telephony_outbox SET dead_lettered_at = dead_lettered_at + interval '1 second' WHERE id = '{id}'"
        ))
        .await
        .is_err());
    assert!(h
        .try_sql(&format!(
            "UPDATE telephony_outbox SET delivered_at = now() WHERE id = '{id}'"
        ))
        .await
        .is_err());
    h.calling.set_event_mode("OK");

    // A dead-lettered event no longer holds back the leg's later events.
    let correlation = h.correlation_of(leg).await;
    let e = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegRinging, h.now());
    h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    h.run().await;
    assert_eq!(h.calling.event_types_for(leg), vec!["LEG_RINGING"]);

    // NOT_FOUND (an unknown call / leg at Calling) is permanent too; a second dead
    // letter of the same reason is another occurrence of ONE incident.
    let (leg2, _) = accepted_leg(&h).await;
    let (leg3, _) = accepted_leg(&h).await;
    h.calling.set_event_mode("NOT_FOUND");
    h.svc.deliver_outbox_once(100).await.unwrap();
    assert_eq!(row_state(&h, leg2).await, (0, 1, 0));
    assert_eq!(row_state(&h, leg3).await, (0, 1, 0));
    assert_eq!(open_dead_letter_incidents(&h, "CALLING_NOT_FOUND").await, 2);
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_incidents WHERE category = 'OUTBOX_DEAD_LETTER' AND status = 'OPEN'"
        )
        .await,
        2,
        "one incident per reason, not per event"
    );
    // The incident evidence is bounded and number-free.
    let evidence = h
        .text("SELECT restricted_evidence::text FROM telephony_incidents WHERE category = 'OUTBOX_DEAD_LETTER' LIMIT 1")
        .await
        .unwrap();
    assert!(!evidence.contains("+234"), "{evidence}");

    // Final: later sweeps (long after, Calling healthy, the leg reconciled meanwhile)
    // never send the dead-lettered event again.
    h.calling.set_event_mode("OK");
    for _ in 0..3 {
        h.advance(120);
        h.run().await;
    }
    assert_eq!(attempts_of(&h, &event_id), 2);
    assert!(!h.calling.events().iter().any(|e| e["eventId"] == event_id));
}

#[tokio::test]
async fn transient_failures_back_off_with_a_cap_and_dead_letter_only_when_exhausted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, _) = accepted_leg(&h).await;
    // Unreachable Calling and a refusal of the CALLER (a trust-bundle rollout) are
    // transient: retried with exponential backoff capped at 60 s, never dead-lettered
    // early.
    h.calling.set_available(false);
    let mut last_gap = 0;
    for round in 0..12 {
        if round == 6 {
            h.calling.set_available(true);
            h.calling.set_event_mode("AUTH");
        }
        h.svc.deliver_outbox_once(100).await.unwrap();
        let gap = h
            .scalar(&format!(
                "SELECT (EXTRACT(EPOCH FROM next_attempt_at - '{}'::timestamptz) * 1000)::bigint \
                 FROM telephony_outbox WHERE telephony_leg_id = '{leg}'",
                h.now().to_rfc3339()
            ))
            .await;
        assert!(gap >= last_gap && gap <= 60_000, "round {round}: {gap}");
        last_gap = gap;
        h.advance(61);
    }
    assert_eq!(last_gap, 60_000, "the backoff is capped");
    assert_eq!(row_state(&h, leg).await, (0, 0, 1));
    // The failed attempt while the leg executed, then the 12 rounds.
    assert_eq!(
        h.scalar(&format!(
            "SELECT attempts::bigint FROM telephony_outbox WHERE telephony_leg_id = '{leg}'"
        ))
        .await,
        13
    );
    assert_eq!(
        open_dead_letter_incidents(&h, "ATTEMPTS_EXHAUSTED").await,
        0
    );
    // At the cap the event is dead-lettered (ATTEMPTS_EXHAUSTED) and stops.
    h.sql(&format!(
        "UPDATE telephony_outbox SET attempts = {} WHERE telephony_leg_id = '{leg}'",
        outbox::MAX_DELIVERY_ATTEMPTS - 1
    ))
    .await;
    h.svc.deliver_outbox_once(100).await.unwrap();
    assert_eq!(row_state(&h, leg).await, (0, 1, 0));
    assert_eq!(
        open_dead_letter_incidents(&h, "ATTEMPTS_EXHAUSTED").await,
        1
    );
    assert_eq!(
        h.text(&format!(
            "SELECT last_error_code FROM telephony_outbox WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .unwrap(),
        "INTERNAL_AUTHENTICATION_FAILED"
    );
    let event_id = json!(h
        .text(&format!(
            "SELECT id::text FROM telephony_outbox WHERE telephony_leg_id = '{leg}' AND dead_lettered_at IS NOT NULL"
        ))
        .await
        .unwrap());
    let attempted = attempts_of(&h, &event_id);
    h.calling.set_event_mode("OK");
    h.advance(120);
    h.run().await;
    assert_eq!(attempts_of(&h, &event_id), attempted);
    assert!(!h.calling.events().iter().any(|e| e["eventId"] == event_id));
}

#[tokio::test]
async fn an_expired_outbox_lease_is_reclaimed_and_delivered_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, _) = accepted_leg(&h).await;
    // A worker claims the event and dies before delivering it.
    let claimed = outbox::claim_due(h.svc.db(), h.now(), 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].telephony_leg_id, Some(leg));
    let event_id = json!(claimed[0].id);
    let attempted_before = attempts_of(&h, &event_id);
    // While its lease runs, no other worker takes it.
    let other = h.svc.clone().as_instance("w-outbox-2");
    assert_eq!(other.deliver_outbox_once(10).await.unwrap(), (0, 0));
    h.advance(29);
    assert_eq!(other.deliver_outbox_once(10).await.unwrap(), (0, 0));
    assert!(h.calling.event_types_for(leg).is_empty());
    // The lease expires: another worker reclaims and delivers it — once.
    h.advance(2);
    let (a, b) = tokio::join!(other.deliver_outbox_once(10), h.svc.deliver_outbox_once(10));
    assert_eq!(a.unwrap().0 + b.unwrap().0, 1);
    for _ in 0..3 {
        h.advance(60);
        h.svc.deliver_outbox_once(10).await.unwrap();
        other.deliver_outbox_once(10).await.unwrap();
    }
    assert_eq!(h.calling.event_types_for(leg), vec!["LEG_ACCEPTED"]);
    assert_eq!(attempts_of(&h, &event_id), attempted_before + 1);
    assert_eq!(row_state(&h, leg).await, (1, 0, 0));
}

/// Captures every log line of this test binary. A GLOBAL subscriber (installed once):
/// a thread-local one races with tests on other threads over tracing's per-callsite
/// interest cache. Lines are told apart by this test's unique ids.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn global_logs() -> LogCapture {
    static CAPTURE: std::sync::OnceLock<LogCapture> = std::sync::OnceLock::new();
    CAPTURE
        .get_or_init(|| {
            let capture = LogCapture::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(capture.clone())
                .with_max_level(tracing::Level::DEBUG)
                .with_ansi(false)
                .finish();
            tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
            capture
        })
        .clone()
}

#[tokio::test]
async fn delivered_events_continue_the_originating_commands_correlation() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let capture = global_logs();

    // ORIGINATE with Calling's correlation id: LEG_ACCEPTED continues it.
    let (leg, body) = h.originate().await;
    let originate_correlation = body["correlationId"].as_str().unwrap().to_string();
    h.run().await;
    let accepted = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == json!(leg) && e["eventType"] == "LEG_ACCEPTED")
        .unwrap();
    assert_eq!(
        h.calling.correlation_of(&accepted["eventId"]),
        Some(Some(originate_correlation.clone()))
    );
    assert_eq!(
        h.text(&format!(
            "SELECT correlation_id FROM telephony_outbox WHERE id = '{}'",
            accepted["eventId"].as_str().unwrap()
        ))
        .await
        .as_deref(),
        Some(originate_correlation.as_str())
    );
    // Provider progress of the leg stays in that flow.
    let correlation_key = h.correlation_of(leg).await;
    let e = h
        .provider_a
        .progress(&correlation_key, NormalizedEvent::LegRinging, h.now());
    h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    h.run().await;

    // TERMINATE with its own correlation id: the LEG_ENDED it causes continues THAT one.
    let terminate_correlation = "corr-terminate-0123456789";
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/terminate"),
            Some(json!({
                "schemaVersion": "1",
                "commandId": Uuid::new_v4(),
                "idempotencyKey": format!("term-{leg}"),
                "correlationId": terminate_correlation,
                "reason": "CALL_ENDED",
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
    h.run().await;
    h.run().await;
    let by_type = |t: &str| {
        h.calling
            .events()
            .into_iter()
            .find(|e| e["telephonyLegId"] == json!(leg) && e["eventType"] == t)
            .unwrap_or_else(|| panic!("{t} delivered"))
    };
    assert_eq!(
        h.calling.correlation_of(&by_type("LEG_RINGING")["eventId"]),
        Some(Some(originate_correlation.clone()))
    );
    assert_eq!(
        h.calling.correlation_of(&by_type("LEG_ENDED")["eventId"]),
        Some(Some(terminate_correlation.to_string()))
    );

    // A row written before correlations were stored (NULL) is still delivered; the
    // adapter then mints a fresh correlation.
    let (leg2, _) = accepted_leg(&h).await;
    h.sql("ALTER TABLE telephony_outbox DISABLE TRIGGER trg_telephony_outbox_immutable")
        .await;
    h.sql(&format!(
        "UPDATE telephony_outbox SET correlation_id = NULL WHERE telephony_leg_id = '{leg2}'"
    ))
    .await;
    h.sql("ALTER TABLE telephony_outbox ENABLE TRIGGER trg_telephony_outbox_immutable")
        .await;
    h.run().await;
    let accepted2 = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == json!(leg2))
        .unwrap();
    assert_eq!(h.calling.correlation_of(&accepted2["eventId"]), Some(None));
    // The correlation of a stored row is immutable.
    assert!(h
        .try_sql(&format!(
            "UPDATE telephony_outbox SET correlation_id = 'other-correlation' WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .is_err());

    let logs = String::from_utf8_lossy(&capture.0.lock().unwrap()).to_string();
    // The worker spans carry the leg id and the correlation id.
    let delivery = logs
        .lines()
        .find(|l| {
            l.contains("telephony_outbox_delivery")
                && l.contains(&terminate_correlation.to_string())
        })
        .unwrap_or_else(|| panic!("no delivery span with the correlation:\n{logs}"));
    assert!(delivery.contains(&leg.to_string()), "{delivery}");
    let command = logs
        .lines()
        .find(|l| l.contains("telephony_command{") && l.contains(&originate_correlation))
        .unwrap_or_else(|| panic!("no command span with the correlation:\n{logs}"));
    assert!(command.contains(&leg.to_string()), "{command}");
    // Never a number in the logs.
    assert!(!logs.contains("+2348031234567"));
}
