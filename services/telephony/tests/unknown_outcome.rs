//! Unknown provider outcome (Part E §48, §113–§121).
//!
//! A paid origination is SUCCESS, DEFINITE_FAILURE or UNKNOWN_OUTCOME, and UNKNOWN is
//! never treated as an ordinary failure: once the transmit fence is committed, nothing
//! blindly retries; read-back or a correlated provider event resolves it. Every case
//! below is injected through the deterministic interconnect and the real executor,
//! reconciler and event ingestion against PostgreSQL.

mod common;

use axum::http::Method;
use common::Harness;
use telephony::{
    domain::NormalizedEvent,
    providers::{deterministic::Script, UnknownReason},
};
use uuid::Uuid;

async fn command_status(h: &Harness, leg: Uuid) -> String {
    h.text(&format!(
        "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
    ))
    .await
    .unwrap()
}

async fn result_code(h: &Harness, leg: Uuid) -> String {
    h.text(&format!(
        "SELECT result_code FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
    ))
    .await
    .unwrap_or_default()
}

/// Lets the reconciliation delay pass and runs the workers (several times: idempotent).
async fn reconcile(h: &Harness) {
    h.advance(10);
    for _ in 0..3 {
        h.run().await;
    }
}

#[tokio::test]
async fn timeout_before_send_is_a_safe_retry() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A worker claimed the command and died BEFORE the transmit fence: nothing left
    // this process, so the command is simply executed again — exactly one paid effect.
    let (leg, _) = h.originate().await;
    let past = h.now() - chrono::Duration::seconds(1);
    h.sql(&format!(
        "UPDATE telephony_commands SET status = 'CLAIMED', lease_owner = 'crashed-worker', lease_until = '{}', \
           attempts = attempts + 1 WHERE telephony_leg_id = '{leg}'",
        past.to_rfc3339()
    ))
    .await;
    h.run().await;
    assert_eq!(command_status(&h, leg).await, "SUCCEEDED");
    assert_eq!(h.provider_a.effects().len(), 1);
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");

    // A connection that could not even be opened is a definite, untransmitted failure:
    // nothing was created, and the leg says so (definite = a retry or failover is safe).
    h.provider_a.script(&[Script::Fail {
        transmitted: false,
        reason: telephony::domain::EndReason::Failed,
    }]);
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(command_status(&h, leg).await, "FAILED");
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert_eq!(h.provider_a.effects().len(), 1, "no second paid effect");
    let failed = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == serde_json::json!(leg) && e["eventType"] == "LEG_FAILED")
        .expect("LEG_FAILED delivered");
    assert_eq!(failed["definite"], true);
}

#[tokio::test]
async fn timeout_after_send_is_unknown_and_never_blindly_retried() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // The provider created the call but did not answer within the executor's deadline.
    h.provider_a.script(&[Script::Stall { create: true }]);
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(command_status(&h, leg).await, "UNKNOWN_OUTCOME");
    assert_eq!(result_code(&h, leg).await, "TIMEOUT_AFTER_SEND");
    assert_eq!(h.leg_status(leg).await, "UNKNOWN");
    // Calling hears an indefinite status — never a failure.
    let types = h.calling.event_types_for(leg);
    assert_eq!(types, vec!["LEG_STATUS_UNKNOWN"], "{types:?}");
    assert!(!types.iter().any(|t| t == "LEG_FAILED"));
    // Sweeps before the reconciliation delay never re-send.
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(h.provider_a.requests().len(), 1);
    // Read-back finds the call: the leg continues, bound to it — one paid effect.
    reconcile(&h).await;
    assert_eq!(h.leg_status(leg).await, "RINGING");
    assert_eq!(command_status(&h, leg).await, "SUCCEEDED");
    assert_eq!(h.provider_a.effects().len(), 1);
    assert_eq!(h.provider_a.requests().len(), 1);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM provider_call_bindings WHERE telephony_leg_id = '{leg}' AND status = 'BOUND'"
        ))
        .await,
        1
    );
}

#[tokio::test]
async fn connection_reset_and_malformed_success_are_unknown() {
    let Some(h) = Harness::new().await else {
        return;
    };
    for reason in [
        UnknownReason::ConnectionResetAfterSend,
        UnknownReason::MalformedSuccess,
    ] {
        // The provider never created anything, but the caller cannot know that.
        h.provider_a.script(&[Script::LoseRequest(reason)]);
        let (leg, _) = h.originate().await;
        h.run().await;
        assert_eq!(command_status(&h, leg).await, "UNKNOWN_OUTCOME");
        assert_eq!(result_code(&h, leg).await, reason.as_str());
        assert_eq!(h.leg_status(leg).await, "UNKNOWN");
        // Read-back with evidence covering the transmission proves absence: only now is
        // it a (definite) failure.
        reconcile(&h).await;
        assert_eq!(h.leg_status(leg).await, "FAILED");
        assert_eq!(command_status(&h, leg).await, "FAILED");
        // (followed by the zero usage fact of a leg that never connected)
        assert_eq!(
            h.calling.event_types_for(leg)[..2],
            ["LEG_STATUS_UNKNOWN", "LEG_FAILED"]
        );
    }
    assert!(h.provider_a.effects().is_empty());
    assert_eq!(h.provider_a.requests().len(), 2, "each sent exactly once");
}

#[tokio::test]
async fn unknown_stays_unknown_while_read_back_is_inconclusive() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.provider_a.script(&[Script::AcceptButLoseResponse(
        UnknownReason::TimeoutAfterSend,
    )]);
    h.provider_a.set_query_unknown(true);
    let (leg, _) = h.originate().await;
    h.run().await;
    for _ in 0..4 {
        reconcile(&h).await;
        h.advance(60);
    }
    // Never guessed: still UNKNOWN, an incident tracks it, nothing re-sent.
    assert_eq!(h.leg_status(leg).await, "UNKNOWN");
    assert_eq!(command_status(&h, leg).await, "UNKNOWN_OUTCOME");
    assert_eq!(h.provider_a.requests().len(), 1);
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE dedupe_key = 'unknown-origination:{leg}' AND status = 'OPEN'"
        ))
        .await,
        1
    );
    // A database trigger refuses a blind retry of a transmitted command.
    assert!(h
        .try_sql(&format!(
            "UPDATE telephony_commands SET status = 'PENDING' WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .is_err());
    // Once read-back works again it resolves, and the incident closes.
    h.provider_a.set_query_unknown(false);
    h.advance(600);
    reconcile(&h).await;
    assert_eq!(h.leg_status(leg).await, "RINGING");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE dedupe_key = 'unknown-origination:{leg}' AND status = 'OPEN'"
        ))
        .await,
        0
    );
}

#[tokio::test]
async fn worker_crash_after_the_provider_accepted_goes_to_reconciliation() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.provider_a.script(&[Script::AcceptThenCrash]);
    let (leg, _) = h.originate().await;
    // The executing task dies after the provider created the call.
    let svc = h.svc.clone();
    let crashed = tokio::spawn(async move { svc.run_commands_once(10).await }).await;
    assert!(crashed.unwrap_err().is_panic());
    assert_eq!(command_status(&h, leg).await, "RUNNING");
    assert_eq!(h.provider_a.effects().len(), 1);
    // Another instance recovers the expired lease: the fence was committed, so the
    // outcome is UNKNOWN (never an immediate retry)...
    h.advance(31);
    h.svc.run_commands_once(10).await.unwrap();
    assert_eq!(command_status(&h, leg).await, "UNKNOWN_OUTCOME");
    assert_eq!(result_code(&h, leg).await, "WORKER_INTERRUPTED");
    assert_eq!(h.leg_status(leg).await, "UNKNOWN");
    assert_eq!(h.provider_a.requests().len(), 1);
    // ...and reconciliation finds and binds the call the provider created.
    reconcile(&h).await;
    assert_eq!(h.leg_status(leg).await, "RINGING");
    assert_eq!(command_status(&h, leg).await, "SUCCEEDED");
    assert_eq!(h.provider_a.effects().len(), 1);
}

#[tokio::test]
async fn a_late_success_event_while_unknown_binds_and_continues() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.provider_a.script(&[Script::AcceptButLoseResponse(
        UnknownReason::TimeoutAfterSend,
    )]);
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(h.leg_status(leg).await, "UNKNOWN");
    // The provider's callback carries the correlation key persisted before the request.
    let correlation = h.correlation_of(leg).await;
    let ringing = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegRinging, h.now());
    let d = h.svc.ingest_provider_event("dp-a", ringing).await.unwrap();
    assert_eq!(d.as_str(), "APPLIED");
    assert_eq!(h.leg_status(leg).await, "RINGING");
    assert_eq!(command_status(&h, leg).await, "SUCCEEDED");
    let answered = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegAnswered, h.now());
    assert_eq!(
        h.svc
            .ingest_provider_event("dp-a", answered)
            .await
            .unwrap()
            .as_str(),
        "APPLIED"
    );
    assert_eq!(h.leg_status(leg).await, "ANSWERED");
    // (the outbox delivers per leg in order: one event per leg per sweep)
    h.run().await;
    h.run().await;
    assert_eq!(
        h.calling.event_types_for(leg),
        vec!["LEG_STATUS_UNKNOWN", "LEG_RINGING", "LEG_ANSWERED"]
    );
    // Nothing was re-sent and the leg view says nothing provider-specific.
    assert_eq!(h.provider_a.requests().len(), 1);
    let view = h
        .calling(Method::GET, &format!("/internal/v1/legs/{leg}"), None)
        .await;
    assert!(!view.text.contains("dp-"), "{}", view.text);
}

#[tokio::test]
async fn native_provider_idempotency_is_used_and_its_reference_persisted() {
    let Some(h) = Harness::new().await else {
        return;
    };
    h.provider_a.set_native_idempotency(true);
    let (leg, _) = h.originate().await;
    h.run().await;
    assert_eq!(command_status(&h, leg).await, "SUCCEEDED");
    // The TelephonyCommand idempotency key went to the provider; its reference is kept.
    let command: String = h
        .text(&format!(
            "SELECT id::text FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await
        .unwrap();
    assert_eq!(
        h.text(&format!(
            "SELECT provider_idempotency_ref FROM provider_call_bindings WHERE telephony_leg_id = '{leg}'"
        ))
        .await
        .unwrap(),
        format!("idem-{command}")
    );
}

#[tokio::test]
async fn the_database_failing_at_the_transmit_fence_means_nothing_is_sent() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Part E §206: the database refuses the transmit-fence commit (as if it went down
    // between the command becoming durable and the provider request).
    h.sql(
        "CREATE FUNCTION test_fence_down() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN \
           IF NEW.transmitted_at IS NOT NULL AND OLD.transmitted_at IS NULL THEN \
             RAISE EXCEPTION 'database unavailable (injected)'; END IF; RETURN NEW; END $$",
    )
    .await;
    h.sql(
        "CREATE TRIGGER test_fence_down BEFORE UPDATE ON telephony_commands \
         FOR EACH ROW EXECUTE FUNCTION test_fence_down()",
    )
    .await;
    let (leg, _) = h.originate().await;
    for _ in 0..3 {
        h.run().await;
    }
    // No provider request left Telephony; the leg never moved; nothing is marked done.
    assert!(h.provider_a.requests().is_empty());
    assert_eq!(h.leg_status(leg).await, "PLANNED");
    assert!(h.calling.event_types_for(leg).is_empty());
    // §207: the database is back; the expired lease is recovered (nothing was
    // transmitted, so a retry is safe) and the leg executes exactly once.
    h.sql("DROP TRIGGER test_fence_down ON telephony_commands")
        .await;
    h.advance(31);
    for _ in 0..3 {
        h.run().await;
    }
    assert_eq!(h.provider_a.requests().len(), 1);
    assert_eq!(h.provider_a.effects().len(), 1);
    assert_eq!(h.leg_status(leg).await, "ORIGINATING");
}
