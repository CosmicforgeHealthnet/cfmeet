//! The Telephony reconciler (Part E §50, §134–§140): every drift case, safe repairs only
//! when the outcome is unambiguous, incidents (restricted, number-free) otherwise,
//! results reaching Calling only as normalized, authenticated events, and two
//! reconcilers on the same object producing exactly one effective repair.

mod common;

use axum::http::Method;
use common::Harness;
use serde_json::json;
use telephony::{
    domain::{EndReason, NormalizedEvent},
    providers::ControlOutcome,
};
use uuid::Uuid;

/// An executed outbound leg: (telephony leg, correlation key).
async fn live(h: &Harness) -> (Uuid, String) {
    let (leg, _) = h.originate().await;
    h.run().await;
    (leg, h.correlation_of(leg).await)
}

async fn apply(h: &Harness, correlation: &str, event: NormalizedEvent) {
    let e = h.provider_a.progress(correlation, event, h.now());
    h.svc.ingest_provider_event("dp-a", e).await.unwrap();
}

async fn runs(h: &Harness, leg: Uuid, case: &str, action: &str) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM telephony_reconciliation_runs WHERE telephony_leg_id = '{leg}' \
         AND case_code = '{case}' AND action = '{action}'"
    ))
    .await
}

async fn open_incidents(h: &Harness, category: &str) -> i64 {
    h.scalar(&format!(
        "SELECT count(*) FROM telephony_incidents WHERE category = '{category}' AND status = 'OPEN'"
    ))
    .await
}

async fn sweep(h: &Harness, n: usize) {
    for _ in 0..n {
        h.run().await;
    }
}

#[tokio::test]
async fn a_leg_ringing_beyond_its_timeout_is_ended_at_the_provider() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegRinging).await;
    h.advance(30);
    sweep(&h, 2).await;
    assert_eq!(
        h.leg_status(leg).await,
        "RINGING",
        "still within the ring timeout"
    );
    h.advance(61);
    sweep(&h, 3).await;
    assert_eq!(h.leg_status(leg).await, "FAILED");
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    assert_eq!(runs(&h, leg, "RINGING_TIMEOUT", "MARKED_FAILED").await, 1);
    assert!(h
        .calling
        .event_types_for(leg)
        .contains(&"LEG_NO_ANSWER".to_string()));
}

#[tokio::test]
async fn connected_but_provider_ended_is_repaired_from_read_back() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
    apply(&h, &correlation, NormalizedEvent::LegConnected).await;
    // The provider's hangup callback is lost.
    h.provider_a
        .end_silently(&correlation, EndReason::Completed);
    h.advance(601);
    sweep(&h, 6).await;
    assert_eq!(h.leg_status(leg).await, "ENDED");
    assert_eq!(
        runs(&h, leg, "CONNECTED_PROVIDER_ENDED", "MARKED_ENDED").await,
        1
    );
    let types = h.calling.event_types_for(leg);
    assert!(
        types.ends_with(&["LEG_ENDED".into(), "USAGE_FINALIZED".into()]),
        "{types:?}"
    );
}

#[tokio::test]
async fn connected_but_absent_at_the_provider_is_not_guessed() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
    h.provider_a.forget(&correlation);
    h.advance(601);
    sweep(&h, 3).await;
    // Ambiguous: we believe it is live, the provider has no record. No repair.
    assert_eq!(h.leg_status(leg).await, "ANSWERED");
    assert_eq!(open_incidents(&h, "CONNECTED_BUT_PROVIDER_ENDED").await, 1);
    let evidence = h
        .text("SELECT restricted_evidence::text FROM telephony_incidents WHERE category = 'CONNECTED_BUT_PROVIDER_ENDED'")
        .await
        .unwrap();
    assert!(!evidence.contains("+234"), "{evidence}");
}

#[tokio::test]
async fn provider_calls_without_a_valid_binding_are_found() {
    let Some(h) = Harness::new().await else {
        return;
    };
    // A live provider call bound to a leg Telephony already considers ended: orphan.
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
    apply(&h, &correlation, NormalizedEvent::LegEnded).await;
    assert_eq!(h.leg_status(leg).await, "ENDED");
    // ...but the provider resurrected it (a late success), with no callback.
    let late = h
        .provider_a
        .progress(&correlation, NormalizedEvent::LegConnected, h.now());
    drop(late);
    assert!(!h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    // A provider call nobody asked for.
    h.provider_a.create_unsolicited("dp-ghost-1");
    h.advance(61);
    sweep(&h, 1).await;
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    assert_eq!(
        runs(&h, leg, "TERMINAL_CALL_ACTIVE_LEG", "TERMINATED_ORPHAN").await,
        1
    );
    // The unknown one is never terminated on a guess: an incident tracks it.
    assert!(!h
        .provider_a
        .terminated()
        .contains(&"dp-ghost-1".to_string()));
    assert_eq!(
        open_incidents(&h, "PROVIDER_ACTIVE_WITHOUT_BINDING").await,
        1
    );
    // Rate-limited: an immediate sweep does not list again.
    sweep(&h, 1).await;
    assert_eq!(
        h.scalar("SELECT occurrences::bigint FROM telephony_incidents WHERE category = 'PROVIDER_ACTIVE_WITHOUT_BINDING'")
            .await,
        1
    );
    // Once it is gone, the incident resolves itself.
    h.provider_a
        .end_silently("unsolicited-dp-ghost-1", EndReason::Completed);
    h.advance(61);
    sweep(&h, 1).await;
    assert_eq!(
        open_incidents(&h, "PROVIDER_ACTIVE_WITHOUT_BINDING").await,
        0
    );
}

#[tokio::test]
async fn an_unresolved_termination_is_never_reported_ended_until_the_provider_confirms() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
    sweep(&h, 2).await;
    // The provider refuses the hangup: the paid call may still be up.
    h.provider_a
        .set_terminate_outcome(Some(ControlOutcome::DefiniteFailure));
    let r = h
        .calling(
            Method::POST,
            &format!("/internal/v1/legs/{leg}/terminate"),
            Some(json!({ "schemaVersion": "1", "commandId": Uuid::new_v4(), "idempotencyKey": format!("term-{leg}"),
                         "correlationId": cf_service_auth::CorrelationId::generate().as_str(), "reason": "CALL_ENDED" })),
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    sweep(&h, 2).await;
    assert_eq!(h.leg_status(leg).await, "ENDING");
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'TERMINATE'"
        ))
        .await
        .unwrap(),
        "UNKNOWN_OUTCOME"
    );
    assert_eq!(open_incidents(&h, "UNKNOWN_TERMINATION_OUTCOME").await, 1);
    assert!(!h
        .calling
        .event_types_for(leg)
        .contains(&"LEG_ENDED".to_string()));
    // Still refused: retried with backoff, escalated, never assumed.
    h.advance(10);
    sweep(&h, 2).await;
    assert_eq!(h.leg_status(leg).await, "ENDING");
    assert!(runs(&h, leg, "UNKNOWN_TERMINATION", "RETRY_SCHEDULED").await >= 1);
    // The provider recovers: the reconciler terminates it, then (and only then) ENDED.
    h.provider_a.set_terminate_outcome(None);
    h.advance(120);
    sweep(&h, 3).await;
    assert_eq!(h.leg_status(leg).await, "ENDED");
    assert!(h
        .provider_a
        .call(&correlation)
        .unwrap()
        .status
        .is_terminal());
    assert_eq!(open_incidents(&h, "UNKNOWN_TERMINATION_OUTCOME").await, 0);
    assert_eq!(
        h.text(&format!(
            "SELECT status FROM telephony_commands WHERE telephony_leg_id = '{leg}' AND kind = 'TERMINATE'"
        ))
        .await
        .unwrap(),
        "SUCCEEDED"
    );
    assert!(h
        .calling
        .event_types_for(leg)
        .contains(&"LEG_ENDED".to_string()));
}

#[tokio::test]
async fn missing_usage_finalization_is_completed_exactly_once() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let (leg, correlation) = live(&h).await;
    apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
    h.advance(42);
    apply(&h, &correlation, NormalizedEvent::LegEnded).await;
    assert_eq!(
        h.text(&format!(
            "SELECT usage_status FROM telephony_legs WHERE id = '{leg}'"
        ))
        .await
        .unwrap(),
        "PENDING"
    );
    // Two finalizers at once (two instances): one usage fact, one event.
    let other = h.svc.clone().as_instance("w-other");
    let (a, b) = tokio::join!(h.svc.finalize_usage_once(10), other.finalize_usage_once(10));
    a.unwrap();
    b.unwrap();
    sweep(&h, 3).await;
    assert_eq!(
        h.scalar(&format!(
            "SELECT count(*) FROM telephony_usage_records WHERE telephony_leg_id = '{leg}'"
        ))
        .await,
        1
    );
    let usage: Vec<_> = h
        .calling
        .events()
        .into_iter()
        .filter(|e| e["telephonyLegId"] == json!(leg) && e["eventType"] == "USAGE_FINALIZED")
        .collect();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["usage"]["billableSeconds"], 42);
}

#[tokio::test]
async fn two_reconcilers_on_the_same_object_make_exactly_one_repair() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let mut legs = Vec::new();
    for _ in 0..6 {
        let (leg, correlation) = live(&h).await;
        apply(&h, &correlation, NormalizedEvent::LegAnswered).await;
        h.provider_a
            .end_silently(&correlation, EndReason::Completed);
        legs.push(leg);
    }
    sweep(&h, 2).await;
    h.advance(601);
    let other = h.svc.clone().as_instance("w-second");
    for _ in 0..3 {
        let (a, b) = tokio::join!(h.svc.reconcile_once(50), other.reconcile_once(50));
        a.unwrap();
        b.unwrap();
    }
    sweep(&h, 4).await;
    for leg in &legs {
        assert_eq!(h.leg_status(*leg).await, "ENDED");
        assert_eq!(
            runs(&h, *leg, "CONNECTED_PROVIDER_ENDED", "MARKED_ENDED").await,
            1,
            "exactly one effective repair of {leg}"
        );
        let ended = h
            .calling
            .event_types_for(*leg)
            .iter()
            .filter(|t| *t == "LEG_ENDED")
            .count();
        assert_eq!(ended, 1);
    }
    // Both instances took part (leases split the work), neither repaired twice.
    assert_eq!(
        h.scalar(
            "SELECT count(*) FROM telephony_reconciliation_runs WHERE action = 'MARKED_ENDED'"
        )
        .await,
        legs.len() as i64
    );
}

#[tokio::test]
async fn an_orphan_bridge_is_ended() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let call = Uuid::new_v4();
    let mut members = Vec::new();
    for _ in 0..2 {
        let contact = h.privacy.add_contact("+2348031234567", "NG");
        let mut body = h.originate_body(contact, Uuid::new_v4(), 1);
        body["call"]["callId"] = json!(call);
        body["call"]["routeClass"] = json!("PSTN_RELAY");
        let r = h
            .calling(Method::POST, "/internal/v1/legs/originate", Some(body))
            .await;
        assert_eq!(r.status, axum::http::StatusCode::ACCEPTED, "{}", r.text);
        members.push(
            r.json["leg"]["telephonyLegId"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
    }
    sweep(&h, 1).await;
    for leg in &members {
        apply(
            &h,
            &h.correlation_of(*leg).await,
            NormalizedEvent::LegAnswered,
        )
        .await;
    }
    let r = h
        .calling(
            Method::POST,
            "/internal/v1/bridges",
            Some(json!({
                "schemaVersion": "1", "commandId": Uuid::new_v4(),
                "idempotencyKey": format!("bridge-{}", Uuid::new_v4().simple()),
                "correlationId": cf_service_auth::CorrelationId::generate().as_str(),
                "callId": call, "kind": "PUBLIC_LEGS", "telephonyLegIds": members,
            })),
        )
        .await;
    assert_eq!(r.status, axum::http::StatusCode::ACCEPTED, "{}", r.text);
    sweep(&h, 1).await;
    assert_eq!(
        h.text("SELECT status FROM telephony_bridges")
            .await
            .unwrap(),
        "PENDING"
    );
    // A member ended but the bridge bookkeeping was lost (the worker died between
    // commits): the reconciler's orphan scan ends the bridge.
    h.sql(&format!(
        "UPDATE telephony_legs SET status = 'ENDED', ended_at = updated_at, end_reason = 'COMPLETED', version = version + 1 \
         WHERE id = '{}'",
        members[0]
    ))
    .await;
    sweep(&h, 2).await;
    let (status, reason) = (
        h.text("SELECT status FROM telephony_bridges")
            .await
            .unwrap(),
        h.text("SELECT coalesce(end_reason, '') FROM telephony_bridges")
            .await
            .unwrap(),
    );
    // (never established: FAILED, with the orphan reason)
    assert_eq!((status.as_str(), reason.as_str()), ("FAILED", "ORPHANED"));
}
