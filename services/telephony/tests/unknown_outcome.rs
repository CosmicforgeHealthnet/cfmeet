//! Unknown provider outcome (Part E §48, §113–§121).
//!
//! A paid origination is SUCCESS, DEFINITE_FAILURE or UNKNOWN_OUTCOME, and UNKNOWN is
//! never treated as an ordinary failure: once the transmit fence is committed, nothing
//! blindly retries; read-back or a correlated provider event resolves it. Every case
//! below is injected through the deterministic interconnect and the real executor,
//! reconciler and event ingestion against PostgreSQL. Attempt caps (Part I §81): an
//! untransmitted command at its cap is DEAD_LETTER and never retried, a transmitted one
//! is UNKNOWN (reconciliation, never re-sent), and a TERMINATE is never abandoned.

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

/// Puts a leg's ORIGINATE in the state a worker that died holding it leaves behind,
/// already at its attempt cap (`transmitted`: the fence was committed).
async fn crashed_at_cap(h: &Harness, leg: Uuid, transmitted: bool) {
    let past = (h.now() - chrono::Duration::seconds(1)).to_rfc3339();
    h.sql(&format!(
        "UPDATE telephony_commands SET status = 'CLAIMED', lease_owner = 'crashed-worker', lease_until = '{past}', \
           attempts = max_attempts WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
    ))
    .await;
    if transmitted {
        // The fence commit: RUNNING with `transmitted_at`.
        h.sql(&format!(
            "UPDATE telephony_commands SET status = 'RUNNING', transmitted_at = '{past}' \
             WHERE telephony_leg_id = '{leg}' AND kind = 'ORIGINATE'"
        ))
        .await;
    }
}

#[tokio::test]
async fn a_command_at_its_attempt_cap_is_dead_lettered_and_a_transmitted_one_is_reconciled() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // Its worker died on every attempt before the fence (a poison command): at the cap
    // it is DEAD_LETTER — final — and its leg fails definitely as NOT_SENT.
    let (poison, _) = h.originate().await;
    crashed_at_cap(&h, poison, false).await;
    h.run().await;
    assert_eq!(command_status(&h, poison).await, "DEAD_LETTER");
    assert_eq!(result_code(&h, poison).await, "ATTEMPTS_EXHAUSTED");
    assert_eq!(h.leg_status(poison).await, "FAILED");
    assert_eq!(
        h.text(&format!(
            "SELECT end_reason FROM telephony_legs WHERE id = '{poison}'"
        ))
        .await
        .unwrap(),
        "NOT_SENT"
    );
    let failed = h
        .calling
        .events()
        .into_iter()
        .find(|e| e["telephonyLegId"] == serde_json::json!(poison))
        .expect("LEG_FAILED delivered");
    assert_eq!(failed["eventType"], "LEG_FAILED");
    assert_eq!(failed["definite"], true);

    // A command already past its cap when claimed (e.g. restored from a backup) is
    // dead-lettered before anything runs.
    let (restored, _) = h.originate().await;
    h.sql(&format!(
        "UPDATE telephony_commands SET attempts = max_attempts WHERE telephony_leg_id = '{restored}'"
    ))
    .await;
    h.run().await;
    assert_eq!(command_status(&h, restored).await, "DEAD_LETTER");
    assert_eq!(h.leg_status(restored).await, "FAILED");

    // The same cap AFTER the fence: the request may have reached the provider, so the
    // outcome is UNKNOWN and goes to reconciliation — never dead-lettered, never re-sent.
    let (sent, _) = h.originate().await;
    crashed_at_cap(&h, sent, true).await;
    h.run().await;
    assert_eq!(command_status(&h, sent).await, "UNKNOWN_OUTCOME");
    assert_eq!(result_code(&h, sent).await, "WORKER_INTERRUPTED");
    assert_eq!(h.leg_status(sent).await, "UNKNOWN");

    // Nothing ever reached the interconnect, and later sweeps change nothing.
    for _ in 0..3 {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
    }
    assert!(h.provider_a.requests().is_empty());
    assert!(h.provider_b.requests().is_empty());
    assert_eq!(command_status(&h, poison).await, "DEAD_LETTER");
    assert_eq!(command_status(&h, restored).await, "DEAD_LETTER");
    assert_ne!(command_status(&h, sent).await, "DEAD_LETTER");
    assert!(h
        .try_sql(&format!(
            "UPDATE telephony_commands SET status = 'PENDING' WHERE telephony_leg_id = '{sent}'"
        ))
        .await
        .is_err());
}

async fn control(h: &Harness, leg: Uuid, kind: &str) -> Uuid {
    let command = Uuid::new_v4();
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/{kind}"),
            Some(serde_json::json!({
                "schemaVersion": "1",
                "commandId": command,
                "idempotencyKey": format!("{kind}-{command}"),
                "correlationId": format!("corr-{kind}-{}", command.simple()),
            })),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::ACCEPTED, "{}", r.text);
    command
}

async fn status_of(h: &Harness, command: Uuid) -> (String, String, i64) {
    (
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE id = '{command}'"
        ))
        .await
        .unwrap(),
        h.text(&format!(
            "SELECT coalesce(result_code, '') FROM telephony_commands WHERE id = '{command}'"
        ))
        .await
        .unwrap(),
        h.scalar(&format!(
            "SELECT attempts::bigint FROM telephony_commands WHERE id = '{command}'"
        ))
        .await,
    )
}

#[tokio::test]
async fn a_control_command_that_can_never_run_stops_at_its_cap_and_a_terminate_is_never_abandoned()
{
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, _) = h.originate().await;
    h.run().await;
    let correlation = h.correlation_of(leg).await;
    for event in [NormalizedEvent::LegRinging, NormalizedEvent::LegAnswered] {
        let e = h.provider_a.progress(&correlation, event, h.now());
        h.svc.ingest_provider_event("dp-a", e).await.unwrap();
    }
    // The leg's provider binding is gone (released): a HOLD can never reach the call.
    h.sql(&format!(
        "UPDATE provider_call_bindings SET status = 'RELEASED' WHERE telephony_leg_id = '{leg}'"
    ))
    .await;
    let hold = control(&h, leg, "hold").await;
    let mut rounds = 0;
    while status_of(&h, hold).await.0 != "DEAD_LETTER" {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
        rounds += 1;
        assert!(rounds <= 12, "{:?}", status_of(&h, hold).await);
    }
    let max: i64 = 10;
    assert_eq!(
        status_of(&h, hold).await,
        (
            "DEAD_LETTER".to_string(),
            "ATTEMPTS_EXHAUSTED".to_string(),
            max
        )
    );
    // Stopped: no further attempt.
    for _ in 0..3 {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
    }
    assert_eq!(status_of(&h, hold).await.2, max);
    assert_eq!(h.leg_status(leg).await, "ANSWERED");

    // A TERMINATE in the same situation is never dead-lettered: at its cap the leg is
    // held in ENDING (the provider call may still be up), the outcome is UNKNOWN and
    // reconciliation owns it.
    let terminate = control(&h, leg, "terminate").await;
    let mut rounds = 0;
    while status_of(&h, terminate).await.0 == "PENDING" {
        h.advance(61);
        h.svc.run_commands_once(10).await.unwrap();
        rounds += 1;
        assert!(rounds <= 12, "{:?}", status_of(&h, terminate).await);
    }
    let (status, code, attempts) = status_of(&h, terminate).await;
    assert_eq!(
        (status.as_str(), code.as_str()),
        ("UNKNOWN_OUTCOME", "ATTEMPTS_EXHAUSTED")
    );
    assert_eq!(attempts, max);
    assert_eq!(h.leg_status(leg).await, "ENDING");
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_incidents WHERE dedupe_key = 'unknown-termination:{leg}' AND status = 'OPEN'"
        ))
        .await,
        1
    );
}
