//! Part C §16 domain foundations: routes, canonical legs (and their boundary with the
//! future execution side), attempts with explicit unknown outcome, bounded quality
//! records and idempotent usage records. No routing algorithm, failover, provider
//! execution, billing or analytics exists or is exercised here.

mod common;

use calling::{
    calls::{
        attempts::NewAttempt,
        attributes::SafeAttributes,
        kinds::{
            AttemptOutcome, DisconnectReason, FailureCategory, LegDirection, LegKind, LegState,
            RouteClass, RouteState, UsageType, UsageUnit,
        },
        legs::NewLeg,
        quality::{NewQualityRecord, QualityMetrics},
        routes::{NewRoute, PolicyVersion},
        usage::{IdempotencyKey, NewUsage},
        CallAction, CallError, CallState,
    },
    domain::Clock,
};
use chrono::Duration;
use common::calls::{sql, CallWorld};
use sea_orm::{ConnectionTrait, DbBackend, Statement};

fn route(class: RouteClass) -> NewRoute {
    NewRoute {
        route_class: class,
        policy_version: PolicyVersion::parse("routing-v0").unwrap(),
        decision_snapshot: SafeAttributes::new()
            .with("decided_by", "operator-policy")
            .unwrap()
            .with("candidate_count", 1)
            .unwrap(),
    }
}

fn leg(route_id: Option<calling::calls::ids::CallRouteId>) -> NewLeg {
    NewLeg {
        route_id,
        participant_id: None,
        direction: LegDirection::Outbound,
        kind: LegKind::PublicNetwork,
    }
}

async fn strings(w: &CallWorld, query: &str) -> Vec<String> {
    w.h.conn()
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            query.to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap())
        .collect()
}

#[tokio::test]
async fn routes_are_provider_independent_decisions_with_bounded_snapshots() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Routing).await;
    let r = w
        .calls()
        .record_route(&w.cmd, call.id, route(RouteClass::Application))
        .await
        .unwrap();
    assert_eq!(r.state, RouteState::Selected);
    assert!(matches!(
        w.calls()
            .update_route_state(&w.cmd, call.id, r.id, RouteState::Selected)
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    let active = w
        .calls()
        .update_route_state(&w.cmd, call.id, r.id, RouteState::Active)
        .await
        .unwrap();
    assert!(active.ended_at.is_none());
    let ended = w
        .calls()
        .update_route_state(&w.cmd, call.id, r.id, RouteState::Superseded)
        .await
        .unwrap();
    assert!(ended.ended_at.is_some());
    assert!(w
        .calls()
        .update_route_state(&w.cmd, call.id, r.id, RouteState::Active)
        .await
        .is_err());
    // Snapshots refuse provider / contact data by construction.
    assert!(SafeAttributes::new().with("carrier", "x").is_err());
    assert!(SafeAttributes::new()
        .with("target", "+2348012345678")
        .is_err());
    // The database refuses oversize or non-object snapshots.
    for snapshot in [
        format!("'{{\"k\": \"{}\"}}'::jsonb", "x".repeat(5000)),
        "'[1, 2]'::jsonb".to_string(),
    ] {
        let err = sql(
            &w.h,
            &format!(
                "INSERT INTO call_routes SELECT gen_random_uuid(), id, tenant_id, application_id, environment_id, \
                 'APPLICATION', 'SELECTED', 'v1', {snapshot}, now(), NULL, now(), now() FROM calls WHERE id = '{}'",
                call.id
            ),
        )
        .await
        .unwrap_err();
        assert!(err.contains("ck_call_routes_snapshot"), "{err}");
    }
    // Terminal calls accept no new routes and only terminal route states.
    let other = w
        .calls()
        .record_route(&w.cmd, call.id, route(RouteClass::PublicNetwork))
        .await
        .unwrap();
    w.act(call.id, CallAction::Cancel).await.unwrap();
    assert!(matches!(
        w.calls()
            .record_route(&w.cmd, call.id, route(RouteClass::Mixed))
            .await
            .unwrap_err(),
        CallError::Terminal { .. }
    ));
    assert!(w
        .calls()
        .update_route_state(&w.cmd, call.id, other.id, RouteState::Active)
        .await
        .is_err());
    assert!(w
        .calls()
        .update_route_state(&w.cmd, call.id, other.id, RouteState::Ended)
        .await
        .is_ok());
}

#[tokio::test]
async fn legs_are_canonical_and_hold_only_an_opaque_execution_binding() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Dialing).await;
    let r = w
        .calls()
        .record_route(&w.cmd, call.id, route(RouteClass::PublicNetwork))
        .await
        .unwrap();
    let participant = w.calls().participants(w.scope(), call.id).await.unwrap()[0].id;
    let mut new = leg(Some(r.id));
    new.participant_id = Some(participant);
    let l = w.calls().record_leg(&w.cmd, call.id, new).await.unwrap();
    assert_eq!(l.state, LegState::Created);
    let mut last = l.clone();
    for state in [
        LegState::Dialing,
        LegState::Ringing,
        LegState::Answered,
        LegState::Connected,
        LegState::Ended,
    ] {
        w.h.clock.advance(Duration::seconds(1));
        last = w
            .calls()
            .update_leg_state(&w.cmd, call.id, l.id, state, None)
            .await
            .unwrap();
        assert_eq!(last.state, state);
    }
    assert!(last.dialing_at < last.ringing_at && last.ringing_at < last.answered_at);
    assert!(last.connected_at <= last.ended_at);
    assert!(w
        .calls()
        .update_leg_state(&w.cmd, call.id, l.id, LegState::Connected, None)
        .await
        .is_err());
    // A leg update never moves the Call.
    assert_eq!(w.call(call.id).await.version, call.version);

    // FAILED requires a normalized category, nothing else accepts one.
    let f = w
        .calls()
        .record_leg(&w.cmd, call.id, leg(None))
        .await
        .unwrap();
    assert!(w
        .calls()
        .update_leg_state(&w.cmd, call.id, f.id, LegState::Failed, None)
        .await
        .is_err());
    assert!(w
        .calls()
        .update_leg_state(
            &w.cmd,
            call.id,
            f.id,
            LegState::Dialing,
            Some(FailureCategory::Network)
        )
        .await
        .is_err());
    let failed = w
        .calls()
        .update_leg_state(
            &w.cmd,
            call.id,
            f.id,
            LegState::Failed,
            Some(FailureCategory::Execution),
        )
        .await
        .unwrap();
    assert_eq!(failed.failure_category, Some(FailureCategory::Execution));

    // The execution binding is an opaque UUID, set once.
    let binding = uuid::Uuid::new_v4();
    let bound = w
        .calls()
        .assign_leg_execution_binding(&w.cmd, call.id, f.id, binding)
        .await
        .unwrap();
    assert_eq!(bound.execution_binding_ref, Some(binding));
    assert!(w
        .calls()
        .assign_leg_execution_binding(&w.cmd, call.id, f.id, binding)
        .await
        .is_ok());
    assert!(matches!(
        w.calls()
            .assign_leg_execution_binding(&w.cmd, call.id, f.id, uuid::Uuid::new_v4())
            .await
            .unwrap_err(),
        CallError::Conflict(_)
    ));
    // A carrier call id or a phone number can never be stored as a binding.
    for bad in [
        "CA0123456789abcdef0123456789abcdef",
        "+2348012345678",
        "sip:alice@example.org",
    ] {
        assert!(sql(
            &w.h,
            &format!(
                "UPDATE call_legs SET execution_binding_ref = '{bad}' WHERE id = '{}'",
                l.id
            )
        )
        .await
        .is_err());
    }
    // The canonical leg has exactly these columns: no provider / carrier ids, provider
    // status, payloads or telephone numbers.
    let columns = strings(
        &w,
        "SELECT column_name::text FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'call_legs' ORDER BY column_name",
    )
    .await;
    assert_eq!(
        columns,
        [
            "answered_at",
            "application_id",
            "call_id",
            "connected_at",
            "created_at",
            "dialing_at",
            "direction",
            "ended_at",
            "environment_id",
            "execution_binding_ref",
            "failure_category",
            "id",
            "leg_kind",
            "participant_id",
            "ringing_at",
            "route_id",
            "state",
            "tenant_id",
            "updated_at",
        ]
    );
    assert_eq!(
        strings(
            &w,
            "SELECT data_type::text FROM information_schema.columns WHERE table_name = 'call_legs' \
             AND column_name = 'execution_binding_ref'"
        )
        .await,
        ["uuid"]
    );
    let json_columns = w
        .count(
            "SELECT count(*) FROM information_schema.columns WHERE table_name = 'call_legs' \
             AND data_type IN ('json', 'jsonb', 'text', 'character varying')",
        )
        .await;
    // Only the CHECK-constrained vocabulary columns are text.
    assert_eq!(
        json_columns, 4,
        "direction, leg_kind, state, failure_category"
    );
    // Legs never cross calls.
    let other = w.create().await.call;
    assert!(matches!(
        w.calls()
            .record_leg(&w.cmd, other.id, leg(Some(r.id)))
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
}

#[tokio::test]
async fn attempts_are_numbered_linked_and_unknown_is_not_failure() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Dialing).await;
    let first = w
        .calls()
        .record_attempt(&w.cmd, call.id, NewAttempt::default())
        .await
        .unwrap();
    assert_eq!(
        (first.attempt_number, first.outcome),
        (1, AttemptOutcome::Pending)
    );
    // A retry needs a finished predecessor.
    assert!(matches!(
        w.calls()
            .record_attempt(
                &w.cmd,
                call.id,
                NewAttempt {
                    previous_attempt_id: Some(first.id),
                    ..Default::default()
                }
            )
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    let unknown = w
        .calls()
        .finish_attempt(&w.cmd, call.id, first.id, AttemptOutcome::Unknown, None)
        .await
        .unwrap();
    assert_eq!(unknown.outcome, AttemptOutcome::Unknown);
    assert!(unknown.unknown_outcome);
    assert!(
        unknown.failure_category.is_none(),
        "unknown is not a failure"
    );
    assert!(unknown.finished_at.is_some());
    let second = w
        .calls()
        .record_attempt(
            &w.cmd,
            call.id,
            NewAttempt {
                previous_attempt_id: Some(first.id),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.attempt_number, 2);
    assert_eq!(second.previous_attempt_id, Some(first.id));
    // Later reconciliation resolves the unknown outcome; the history flag stays.
    let resolved = w
        .calls()
        .finish_attempt(&w.cmd, call.id, first.id, AttemptOutcome::Succeeded, None)
        .await
        .unwrap();
    assert_eq!(resolved.outcome, AttemptOutcome::Succeeded);
    assert!(resolved.unknown_outcome);
    assert_eq!(resolved.finished_at, unknown.finished_at);
    assert!(w
        .calls()
        .finish_attempt(&w.cmd, call.id, second.id, AttemptOutcome::Failed, None)
        .await
        .is_err());
    let failed = w
        .calls()
        .finish_attempt(
            &w.cmd,
            call.id,
            second.id,
            AttemptOutcome::Failed,
            Some(FailureCategory::Timeout),
        )
        .await
        .unwrap();
    assert!(!failed.unknown_outcome);
    assert!(w
        .calls()
        .finish_attempt(&w.cmd, call.id, second.id, AttemptOutcome::Succeeded, None)
        .await
        .is_err());
    // Database: no duplicate or skipped numbers, the history flag is never cleared.
    let insert = |number: i32| {
        format!(
            "INSERT INTO call_attempts SELECT gen_random_uuid(), call_id, NULL, NULL, tenant_id, application_id, \
             environment_id, {number}, NULL, 'PENDING', false, NULL, now(), NULL, now(), now() \
             FROM call_attempts WHERE id = '{}'",
            first.id
        )
    };
    for number in [2, 4] {
        let err = sql(&w.h, &insert(number)).await.unwrap_err();
        assert!(err.contains("increase by one"), "{err}");
    }
    let err = sql(
        &w.h,
        &format!(
            "UPDATE call_attempts SET unknown_outcome = false WHERE id = '{}'",
            first.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("cannot be cleared"), "{err}");
    let attempts = w.calls().attempts(w.scope(), call.id).await.unwrap();
    assert_eq!(
        attempts
            .iter()
            .map(|a| a.attempt_number)
            .collect::<Vec<_>>(),
        [1, 2]
    );
}

#[tokio::test]
async fn quality_records_are_bounded_numeric_and_append_only() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let session = w.calls().sessions(w.scope(), call.id).await.unwrap()[0].id;
    let now = w.h.clock.now();
    let record = w
        .calls()
        .record_quality(
            &w.cmd,
            call.id,
            NewQualityRecord {
                session_id: Some(session),
                leg_id: None,
                observed_from: now - Duration::seconds(10),
                observed_to: now,
                metrics: QualityMetrics::new()
                    .with("round_trip_ms", 82.5)
                    .unwrap()
                    .with("packet_loss_ratio", 0.01)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    assert_eq!(record.metrics.get("round_trip_ms"), Some(82.5));
    for bad in [
        NewQualityRecord {
            session_id: None,
            leg_id: None,
            observed_from: now,
            observed_to: now,
            metrics: QualityMetrics::new(),
        },
        NewQualityRecord {
            session_id: None,
            leg_id: None,
            observed_from: now,
            observed_to: now - Duration::seconds(1),
            metrics: QualityMetrics::new().with("jitter_ms", 1.0).unwrap(),
        },
    ] {
        assert!(matches!(
            w.calls()
                .record_quality(&w.cmd, call.id, bad)
                .await
                .unwrap_err(),
            CallError::Validation(_)
        ));
    }
    let other = w.call_in(CallState::Connected).await;
    assert!(matches!(
        w.calls()
            .record_quality(
                &w.cmd,
                other.id,
                NewQualityRecord {
                    session_id: Some(session),
                    leg_id: None,
                    observed_from: now,
                    observed_to: now,
                    metrics: QualityMetrics::new().with("jitter_ms", 1.0).unwrap(),
                },
            )
            .await
            .unwrap_err(),
        CallError::NotFound
    ));
    for statement in [
        "UPDATE call_quality_records SET metrics = '{}'::jsonb".to_string(),
        "DELETE FROM call_quality_records".to_string(),
    ] {
        assert!(sql(&w.h, &statement)
            .await
            .unwrap_err()
            .contains("immutable evidence"));
    }
    // Observations arriving after the end are still accepted.
    w.act(call.id, CallAction::RequestEnd).await.unwrap();
    assert!(w
        .calls()
        .record_quality(
            &w.cmd,
            call.id,
            NewQualityRecord {
                session_id: None,
                leg_id: None,
                observed_from: now,
                observed_to: now,
                metrics: QualityMetrics::new().with("mos_estimate", 4.1).unwrap(),
            },
        )
        .await
        .is_ok());
}

#[tokio::test]
async fn usage_records_are_idempotent_and_finalization_is_set_once() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let usage = |quantity| NewUsage {
        usage_type: UsageType::CallConnectedTime,
        participant_id: None,
        session_id: None,
        leg_id: None,
        quantity,
        unit: UsageUnit::Milliseconds,
        usage_started_at: None,
        usage_ended_at: None,
        idempotency_key: IdempotencyKey::parse("call-connected-time").unwrap(),
    };
    let (record, created) = w
        .calls()
        .record_usage(&w.cmd, call.id, usage(1_000))
        .await
        .unwrap();
    assert!(created);
    let events = w.event_count(call.id).await;
    let (again, created) = w
        .calls()
        .record_usage(&w.cmd, call.id, usage(1_000))
        .await
        .unwrap();
    assert!(!created);
    assert_eq!(again.id, record.id);
    assert_eq!(
        w.event_count(call.id).await,
        events,
        "idempotent replay records nothing"
    );
    assert!(matches!(
        w.calls()
            .record_usage(&w.cmd, call.id, usage(2_000))
            .await
            .unwrap_err(),
        CallError::Conflict(_)
    ));
    // Subject consistency.
    let mut bad = usage(5);
    bad.usage_type = UsageType::SessionConnectedTime;
    bad.idempotency_key = IdempotencyKey::parse("session-without-id").unwrap();
    assert!(w.calls().record_usage(&w.cmd, call.id, bad).await.is_err());

    let (finalized, newly) = w
        .calls()
        .finalize_usage(&w.cmd, call.id, record.id)
        .await
        .unwrap();
    assert!(newly && finalized.finalized_at.is_some());
    let events = w.event_count(call.id).await;
    let (again, newly) = w
        .calls()
        .finalize_usage(&w.cmd, call.id, record.id)
        .await
        .unwrap();
    assert!(!newly);
    assert_eq!(again.finalized_at, finalized.finalized_at);
    assert_eq!(w.event_count(call.id).await, events);
    for (statement, needle) in [
        (
            "UPDATE call_usage_records SET quantity = 5".to_string(),
            "immutable",
        ),
        (
            "UPDATE call_usage_records SET finalized_at = now() + interval '1 day'".to_string(),
            "cannot be reassigned",
        ),
        (
            "DELETE FROM call_usage_records".to_string(),
            "immutable evidence",
        ),
    ] {
        let err = sql(&w.h, &statement).await.unwrap_err();
        assert!(err.contains(needle), "{statement}: {err}");
    }
    let err = sql(
        &w.h,
        &format!(
            "INSERT INTO call_usage_records SELECT gen_random_uuid(), call_id, NULL, NULL, NULL, tenant_id, \
             application_id, environment_id, usage_type, 1, unit, NULL, NULL, idempotency_key, NULL, now(), now() \
             FROM call_usage_records WHERE id = '{}'",
            record.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("uq_call_usage_records_idempotency"), "{err}");

    // Participation usage: final only for ended calls, union-based, idempotent.
    assert!(matches!(
        w.calls()
            .finalize_participation_usage(&w.cmd, call.id)
            .await
            .unwrap_err(),
        CallError::InvalidState(_)
    ));
    w.h.clock.advance(Duration::seconds(45));
    let sessions = w.calls().sessions(w.scope(), call.id).await.unwrap();
    w.calls()
        .disconnect_session(&w.cmd, call.id, sessions[0].id, DisconnectReason::Left)
        .await
        .unwrap();
    w.h.clock.advance(Duration::seconds(15));
    w.act(call.id, CallAction::RequestEnd).await.unwrap();
    w.act(
        call.id,
        CallAction::ConfirmEnded {
            evidence: calling::calls::EndedEvidence::CleanupConfirmed,
        },
    )
    .await
    .unwrap();
    let records = w
        .calls()
        .finalize_participation_usage(&w.cmd, call.id)
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    let mut quantities: Vec<_> = records.iter().map(|r| r.quantity).collect();
    quantities.sort();
    assert_eq!(quantities, [45_000, 60_000]);
    assert!(records
        .iter()
        .all(|r| r.finalized_at.is_some() && r.usage_type == UsageType::ParticipantConnectedTime));
    let events = w.event_count(call.id).await;
    let again = w
        .calls()
        .finalize_participation_usage(&w.cmd, call.id)
        .await
        .unwrap();
    assert_eq!(
        again.iter().map(|r| r.id).collect::<Vec<_>>(),
        records.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    assert_eq!(w.event_count(call.id).await, events);
}
