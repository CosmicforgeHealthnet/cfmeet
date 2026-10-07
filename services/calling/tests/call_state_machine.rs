//! Part C §17–§18: the canonical state machine and its transition rules, against a real
//! PostgreSQL database.
//!
//! * The Rust graph and the database graph (`calling_call_transition_allowed`) agree on
//!   all 17 × 17 pairs.
//! * Every allowed edge succeeds with exactly one version increment, one CALL_STATE_CHANGED
//!   event and one outbox row; every disallowed edge and every action outside its source
//!   states is refused without side effects.
//! * ANSWERED is not CONNECTED; CONNECTED needs positive media evidence; ENDING needs
//!   completion evidence; terminal Calls never move again (Rust and direct SQL); late
//!   events are refused; first-entry timestamps are never rewritten.

mod common;

use calling::calls::{
    ids::{CallLegId, CallSessionId},
    kinds::{
        DisconnectReason, EndpointCategory, FailureCategory, LegDirection, LegKind, LegState,
        SessionState, TerminalReason,
    },
    legs::NewLeg,
    service::NewCall,
    transitions::ConnectedEvidence,
    CallAction, CallError, CallState, EndedEvidence,
};
use chrono::Duration;
use common::calls::{actions_into, canonical_path, is_refusal, sql, CallWorld};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

#[tokio::test]
async fn database_graph_equals_the_rust_graph_for_every_pair() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    for from in CallState::ALL {
        for to in CallState::ALL {
            let row =
                w.h.conn()
                    .query_one(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "SELECT calling_call_transition_allowed($1, $2)",
                        [from.as_str().into(), to.as_str().into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap();
            let db: bool = row.try_get_by_index(0).unwrap();
            assert_eq!(db, from.can_transition_to(*to), "{from} -> {to}");
        }
    }
}

#[tokio::test]
async fn every_allowed_transition_succeeds_once_with_one_event_and_one_outbox_row() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let mut edges = 0;
    for from in CallState::ALL {
        for to in from.allowed_targets() {
            let call = w.call_in(*from).await;
            // Evidence preparation (decisions, sessions) may record its own facts first.
            let action = w.action_for(call.id, *from, *to).await;
            let events_before = w.event_count(call.id).await;
            let outbox_before = w.outbox_count(call.id).await;
            let outcome = w
                .act(call.id, action.clone())
                .await
                .unwrap_or_else(|e| panic!("{from} -> {to} via {}: {e:?}", action.name()));
            assert_eq!(outcome.call.state, *to);
            assert_eq!(outcome.call.version, call.version + 1, "{from} -> {to}");
            assert_eq!(w.event_count(call.id).await, events_before + 1);
            assert_eq!(w.outbox_count(call.id).await, outbox_before + 1);
            assert_eq!(outcome.event.from_state, Some(*from));
            assert_eq!(outcome.event.to_state, Some(*to));
            assert_eq!(outcome.event.call_version, outcome.call.version);
            assert_eq!(outcome.call.is_terminal(), to.is_terminal());
            assert_eq!(outcome.call.terminal_reason.is_some(), to.is_terminal());
            assert_eq!(
                outcome.call.failure_category.is_some(),
                *to == CallState::Failed
            );
            edges += 1;
        }
    }
    assert_eq!(edges, 43, "the transition graph changed");
}

#[tokio::test]
async fn disallowed_transitions_and_out_of_source_actions_are_refused_without_side_effects() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    for from in CallState::ALL {
        let call = w.call_in(*from).await;
        let events = w.event_count(call.id).await;
        for to in CallState::ALL {
            for action in actions_into(&w, call.id, *to).await {
                if from.can_transition_to(*to) && action.sources().contains(from) {
                    continue; // allowed (covered above)
                }
                let err = w
                    .act(call.id, action.clone())
                    .await
                    .expect_err(&format!("{from} accepted {}", action.name()));
                assert!(is_refusal(&err), "{from} {}: {err:?}", action.name());
                if from.is_terminal() {
                    assert!(matches!(err, CallError::Terminal { .. }), "{err:?}");
                }
            }
        }
        let after = w.call(call.id).await;
        assert_eq!(after.state, *from);
        assert_eq!(after.version, call.version, "{from} version moved");
        assert_eq!(
            w.event_count(call.id).await,
            events,
            "{from} recorded events"
        );
    }
}

#[tokio::test]
async fn named_forbidden_jumps_are_refused() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let requested = w.create().await.call;
    for action in [
        CallAction::MarkRinging,
        CallAction::Accept,
        CallAction::ConfirmConnected {
            evidence: ConnectedEvidence::RtcMediaPathEstablished {
                session_ids: w.connect_media(requested.id).await,
            },
        },
    ] {
        assert!(matches!(
            w.act(requested.id, action).await.unwrap_err(),
            CallError::InvalidTransition {
                state: CallState::Requested,
                ..
            }
        ));
    }
    let ringing = w.call_in(CallState::Ringing).await;
    assert!(w.act(ringing.id, CallAction::BeginRouting).await.is_err());
    let connected = w.call_in(CallState::Connected).await;
    assert!(matches!(
        w.act(connected.id, CallAction::Cancel).await.unwrap_err(),
        CallError::InvalidTransition {
            state: CallState::Connected,
            ..
        }
    ));
    assert_eq!(w.call(connected.id).await.state, CallState::Connected);
}

#[tokio::test]
async fn answered_is_not_connected_and_can_fail_when_media_fails() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let answered = w.call_in(CallState::Answered).await;
    assert_eq!(answered.state, CallState::Answered);
    assert!(answered.timestamps.answered_at.is_some());
    assert!(answered.timestamps.connected_at.is_none());
    let failed = w
        .act(
            answered.id,
            CallAction::Fail {
                category: FailureCategory::Media,
            },
        )
        .await
        .unwrap()
        .call;
    assert_eq!(failed.state, CallState::Failed);
    assert_eq!(failed.failure_category, Some(FailureCategory::Media));
    assert_eq!(failed.terminal_reason, Some(TerminalReason::Failed));
    assert!(failed.timestamps.connected_at.is_none());
}

#[tokio::test]
async fn connected_requires_positive_bidirectional_media_evidence() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Answered).await;
    let refused = |r: Result<_, CallError>| {
        assert!(
            matches!(r, Err(CallError::InsufficientEvidence(_))),
            "{r:?}"
        )
    };
    let rtc = |ids: Vec<CallSessionId>| CallAction::ConfirmConnected {
        evidence: ConnectedEvidence::RtcMediaPathEstablished { session_ids: ids },
    };
    // Unknown sessions.
    refused(
        w.act(
            call.id,
            rtc(vec![
                CallSessionId::from_uuid(uuid::Uuid::new_v4()),
                CallSessionId::from_uuid(uuid::Uuid::new_v4()),
            ]),
        )
        .await
        .map(|_| ()),
    );
    // Sessions exist but never connected.
    let participants = w.calls().participants(w.scope(), call.id).await.unwrap();
    let (recipient, _) = w
        .calls()
        .add_participant(&w.cmd, call.id, &w.recipient, None)
        .await
        .unwrap();
    let a = w
        .calls()
        .create_session(&w.cmd, call.id, participants[0].id, EndpointCategory::Web)
        .await
        .unwrap();
    let b = w
        .calls()
        .create_session(&w.cmd, call.id, recipient.id, EndpointCategory::Mobile)
        .await
        .unwrap();
    refused(w.act(call.id, rtc(vec![a.id, b.id])).await.map(|_| ()));
    // One side connected only.
    w.calls()
        .connect_session(&w.cmd, call.id, a.id)
        .await
        .unwrap();
    refused(w.act(call.id, rtc(vec![a.id, b.id])).await.map(|_| ()));
    // Two connected devices of the same participant are not a path between parties.
    let a2 = w
        .calls()
        .create_session(
            &w.cmd,
            call.id,
            participants[0].id,
            EndpointCategory::Desktop,
        )
        .await
        .unwrap();
    w.calls()
        .connect_session(&w.cmd, call.id, a2.id)
        .await
        .unwrap();
    refused(w.act(call.id, rtc(vec![a.id, a2.id])).await.map(|_| ()));
    // A session of another call is not evidence for this one.
    let other = w.call_in(CallState::Connected).await;
    let foreign: Vec<_> = w
        .calls()
        .sessions(w.scope(), other.id)
        .await
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    refused(
        w.act(call.id, rtc(vec![a.id, foreign[0]]))
            .await
            .map(|_| ()),
    );

    // "The provider accepted the request" is not connected evidence: dialing / answered
    // legs are refused.
    let mut legs = Vec::new();
    for _ in 0..2 {
        let leg = w
            .calls()
            .record_leg(
                &w.cmd,
                call.id,
                NewLeg {
                    route_id: None,
                    participant_id: None,
                    direction: LegDirection::Outbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await
            .unwrap();
        w.calls()
            .update_leg_state(&w.cmd, call.id, leg.id, LegState::Dialing, None)
            .await
            .unwrap();
        legs.push(leg.id);
    }
    let network = |ids: Vec<CallLegId>| CallAction::ConfirmConnected {
        evidence: ConnectedEvidence::NetworkLegEstablished { leg_ids: ids },
    };
    refused(w.act(call.id, network(legs.clone())).await.map(|_| ()));
    w.calls()
        .update_leg_state(&w.cmd, call.id, legs[0], LegState::Answered, None)
        .await
        .unwrap();
    refused(w.act(call.id, network(legs.clone())).await.map(|_| ()));
    assert_eq!(w.call(call.id).await.state, CallState::Answered);
    assert_eq!(w.state_event_count(call.id).await, 6);

    // Positive evidence: two connected sessions of two participants.
    w.calls()
        .connect_session(&w.cmd, call.id, b.id)
        .await
        .unwrap();
    let connected = w.act(call.id, rtc(vec![a.id, b.id])).await.unwrap();
    assert_eq!(connected.call.state, CallState::Connected);
    assert!(connected.call.timestamps.connected_at.is_some());
    assert_eq!(
        connected.event.data["evidence"],
        "RTC_MEDIA_PATH_ESTABLISHED"
    );

    // Network and mixed bridges are accepted only with connected legs.
    let net = w.call_in(CallState::Answered).await;
    let mut net_legs = Vec::new();
    for _ in 0..2 {
        let leg = w
            .calls()
            .record_leg(
                &w.cmd,
                net.id,
                NewLeg {
                    route_id: None,
                    participant_id: None,
                    direction: LegDirection::Outbound,
                    kind: LegKind::PublicNetwork,
                },
            )
            .await
            .unwrap();
        for state in [LegState::Dialing, LegState::Answered, LegState::Connected] {
            w.calls()
                .update_leg_state(&w.cmd, net.id, leg.id, state, None)
                .await
                .unwrap();
        }
        net_legs.push(leg.id);
    }
    assert_eq!(
        w.act(net.id, network(net_legs.clone()))
            .await
            .unwrap()
            .call
            .state,
        CallState::Connected
    );
    let mixed = w.call_in(CallState::Answered).await;
    let sessions = w.connect_media(mixed.id).await;
    let leg = w
        .calls()
        .record_leg(
            &w.cmd,
            mixed.id,
            NewLeg {
                route_id: None,
                participant_id: None,
                direction: LegDirection::Outbound,
                kind: LegKind::PublicNetwork,
            },
        )
        .await
        .unwrap();
    let bridge = CallAction::ConfirmConnected {
        evidence: ConnectedEvidence::MixedBridgeEstablished {
            session_ids: vec![sessions[0]],
            leg_ids: vec![leg.id],
        },
    };
    refused(w.act(mixed.id, bridge.clone()).await.map(|_| ()));
    for state in [LegState::Dialing, LegState::Answered, LegState::Connected] {
        w.calls()
            .update_leg_state(&w.cmd, mixed.id, leg.id, state, None)
            .await
            .unwrap();
    }
    assert_eq!(
        w.act(mixed.id, bridge).await.unwrap().call.state,
        CallState::Connected
    );
}

#[tokio::test]
async fn every_entry_into_connected_needs_evidence() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let held = w.call_in(CallState::Held).await;
    let no_evidence = ConnectedEvidence::RtcMediaPathEstablished {
        session_ids: vec![
            CallSessionId::from_uuid(uuid::Uuid::new_v4()),
            CallSessionId::from_uuid(uuid::Uuid::new_v4()),
        ],
    };
    assert!(matches!(
        w.act(
            held.id,
            CallAction::Resume {
                evidence: no_evidence.clone()
            }
        )
        .await
        .unwrap_err(),
        CallError::InsufficientEvidence(_)
    ));
    let transferring = w.call_in(CallState::Transferring).await;
    assert!(matches!(
        w.act(
            transferring.id,
            CallAction::CompleteTransfer {
                evidence: no_evidence
            }
        )
        .await
        .unwrap_err(),
        CallError::InsufficientEvidence(_)
    ));
}

#[tokio::test]
async fn ending_becomes_ended_only_with_completion_evidence() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let confirm = |evidence| CallAction::ConfirmEnded { evidence };

    // Sessions still connected: "all participation ended" does not hold.
    let call = w.call_in(CallState::Ending).await;
    assert!(matches!(
        w.act(call.id, confirm(EndedEvidence::AllParticipationEnded))
            .await
            .unwrap_err(),
        CallError::InsufficientEvidence(_)
    ));
    // Reconciliation timeout only after the documented timeout.
    assert!(matches!(
        w.act(call.id, confirm(EndedEvidence::ReconciliationTimeout))
            .await
            .unwrap_err(),
        CallError::InsufficientEvidence(_)
    ));
    w.h.clock.advance(Duration::minutes(5));
    let ended = w
        .act(call.id, confirm(EndedEvidence::ReconciliationTimeout))
        .await
        .unwrap();
    assert_eq!(ended.call.state, CallState::Ended);
    assert_eq!(
        ended.call.terminal_reason,
        Some(TerminalReason::ReconciliationTimeout)
    );
    assert_eq!(ended.event.data["evidence"], "RECONCILIATION_TIMEOUT");
    // The disposition closes what was still open, auditable in the event.
    assert_eq!(ended.event.data["intervals_closed"], 2);
    let intervals = w.calls().intervals(w.scope(), call.id).await.unwrap();
    assert!(intervals
        .iter()
        .all(|i| i.disconnect_reason == Some(DisconnectReason::CallTerminated)));

    // Verified end of participation.
    let call = w.call_in(CallState::Ending).await;
    for s in w.calls().sessions(w.scope(), call.id).await.unwrap() {
        w.calls().end_session(&w.cmd, call.id, s.id).await.unwrap();
    }
    let ended = w
        .act(call.id, confirm(EndedEvidence::AllParticipationEnded))
        .await
        .unwrap();
    assert_eq!(
        ended.call.terminal_reason,
        Some(TerminalReason::ParticipationEnded)
    );
    assert_eq!(ended.event.data["intervals_closed"], 0);

    // Confirmed cleanup.
    let call = w.call_in(CallState::Ending).await;
    let ended = w
        .act(call.id, confirm(EndedEvidence::CleanupConfirmed))
        .await
        .unwrap();
    assert_eq!(
        ended.call.terminal_reason,
        Some(TerminalReason::CleanupConfirmed)
    );
    assert!(ended.call.timestamps.ending_at.is_some());
    assert!(ended.call.timestamps.ended_at >= ended.call.timestamps.ending_at);
    for s in w.calls().sessions(w.scope(), call.id).await.unwrap() {
        assert_eq!(s.state, SessionState::Ended);
    }
    for p in w.calls().participants(w.scope(), call.id).await.unwrap() {
        assert!(p.ended_at.is_some());
    }
}

#[tokio::test]
async fn terminal_calls_can_never_be_resurrected_in_rust_or_by_direct_sql() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    for terminal in CallState::TERMINAL {
        let call = w.call_in(*terminal).await;
        for to in CallState::ALL.iter().filter(|s| !s.is_terminal()) {
            for action in actions_into(&w, call.id, *to).await {
                assert!(matches!(
                    w.act(call.id, action).await.unwrap_err(),
                    CallError::Terminal { .. }
                ));
            }
            // Defence in depth: the database refuses the write even with a "correct"
            // version increment, and with no increment at all.
            for version in ["version + 1", "version"] {
                let err = sql(
                    &w.h,
                    &format!(
                        "UPDATE calls SET current_state = '{to}', version = {version} WHERE id = '{}'",
                        call.id
                    ),
                )
                .await
                .unwrap_err();
                assert!(
                    err.contains("terminal") || err.contains("version"),
                    "{terminal} -> {to}: {err}"
                );
            }
        }
        let after = w.call(call.id).await;
        assert_eq!((after.state, after.version), (*terminal, call.version));
    }
}

#[tokio::test]
async fn database_refuses_graph_skips_version_jumps_and_silent_version_changes() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.create().await.call;
    for (update, needle) in [
        (
            "current_state = 'CONNECTED', version = 2, connected_at = now()",
            "not allowed",
        ),
        (
            "current_state = 'AUTHORIZED', version = 3, authorized_at = now()",
            "version",
        ),
        ("version = 2", "version"),
        ("version = 0", "version"),
    ] {
        let err = sql(
            &w.h,
            &format!("UPDATE calls SET {update} WHERE id = '{}'", call.id),
        )
        .await
        .unwrap_err();
        assert!(err.contains(needle), "{update}: {err}");
    }
    // A legal edge without its state event cannot commit (deferred constraint trigger).
    let txn = w.h.conn().begin().await.unwrap();
    txn.execute_unprepared(&format!(
        "UPDATE calls SET current_state = 'CANCELLED', version = 2, ended_at = requested_at, \
         terminal_reason = 'CANCELLED', updated_at = updated_at WHERE id = '{}'",
        call.id
    ))
    .await
    .unwrap();
    let err = txn.commit().await.unwrap_err().to_string();
    assert!(err.contains("has no state event"), "{err}");
    let after = w.call(call.id).await;
    assert_eq!((after.state, after.version), (CallState::Requested, 1));
}

#[tokio::test]
async fn late_events_never_change_a_terminal_call() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let leg = w
        .calls()
        .record_leg(
            &w.cmd,
            call.id,
            NewLeg {
                route_id: None,
                participant_id: None,
                direction: LegDirection::Outbound,
                kind: LegKind::PublicNetwork,
            },
        )
        .await
        .unwrap();
    w.calls()
        .update_leg_state(&w.cmd, call.id, leg.id, LegState::Dialing, None)
        .await
        .unwrap();
    let sessions: Vec<_> = w
        .calls()
        .sessions(w.scope(), call.id)
        .await
        .unwrap()
        .iter()
        .map(|s| s.id)
        .collect();
    let ended = w
        .act(call.id, CallAction::RequestEnd)
        .await
        .and(
            w.act(
                call.id,
                CallAction::ConfirmEnded {
                    evidence: EndedEvidence::CleanupConfirmed,
                },
            )
            .await,
        )
        .unwrap()
        .call;
    assert_eq!(ended.state, CallState::Ended);
    let state_events = w.state_event_count(call.id).await;

    // Late lifecycle commands.
    for action in [
        CallAction::MarkRinging,
        CallAction::Accept,
        CallAction::ConfirmConnected {
            evidence: ConnectedEvidence::RtcMediaPathEstablished {
                session_ids: sessions.clone(),
            },
        },
        CallAction::Fail {
            category: FailureCategory::Network,
        },
    ] {
        assert!(matches!(
            w.calls()
                .transition(&w.cmd, call.id, ended.version, action)
                .await
                .unwrap_err(),
            CallError::Terminal { .. }
        ));
    }
    // Late network / media callback equivalents.
    for state in [LegState::Ringing, LegState::Answered, LegState::Connected] {
        assert!(matches!(
            w.calls()
                .update_leg_state(&w.cmd, call.id, leg.id, state, None)
                .await
                .unwrap_err(),
            CallError::Terminal { .. }
        ));
    }
    assert!(w
        .calls()
        .reconnect_session(&w.cmd, call.id, sessions[0])
        .await
        .is_err());
    assert!(w
        .calls()
        .connect_session(&w.cmd, call.id, sessions[1])
        .await
        .is_err());
    let participant = w.calls().participants(w.scope(), call.id).await.unwrap()[0].id;
    assert!(matches!(
        w.calls()
            .create_session(&w.cmd, call.id, participant, EndpointCategory::Web)
            .await
            .unwrap_err(),
        CallError::Terminal { .. }
    ));
    // Closing evidence of the execution side is still accepted, and changes no state.
    let closed = w
        .calls()
        .update_leg_state(&w.cmd, call.id, leg.id, LegState::Cancelled, None)
        .await
        .unwrap();
    assert_eq!(closed.state, LegState::Cancelled);

    let after = w.call(call.id).await;
    assert_eq!(
        (after.state, after.version),
        (CallState::Ended, ended.version)
    );
    assert_eq!(w.state_event_count(call.id).await, state_events);
}

#[tokio::test]
async fn lifecycle_timestamps_are_written_on_first_entry_only() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let call = w.call_in(CallState::Connected).await;
    let first_connected = call.timestamps.connected_at.unwrap();
    w.h.clock.advance(Duration::seconds(30));
    let held = w.act(call.id, CallAction::Hold).await.unwrap().call;
    assert!(held.updated_at > call.updated_at);
    w.h.clock.advance(Duration::seconds(30));
    let action = w
        .action_for(call.id, CallState::Held, CallState::Connected)
        .await;
    let resumed = w.act(call.id, action).await.unwrap().call;
    assert_eq!(resumed.state, CallState::Connected);
    assert_eq!(resumed.timestamps.connected_at, Some(first_connected));
    assert_eq!(
        resumed.timestamps.requested_at,
        call.timestamps.requested_at
    );
    assert_eq!(
        resumed.timestamps.authorized_at,
        call.timestamps.authorized_at
    );
    assert!(resumed.updated_at > held.updated_at);
    assert_eq!(resumed.version, call.version + 2);
    // The database refuses rewriting a first-entry timestamp.
    let err = sql(
        &w.h,
        &format!(
            "UPDATE calls SET connected_at = connected_at + interval '1 second' WHERE id = '{}'",
            call.id
        ),
    )
    .await
    .unwrap_err();
    assert!(err.contains("cannot be reassigned"), "{err}");
}

#[tokio::test]
async fn canonical_paths_cover_every_state() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    for state in CallState::ALL {
        let call = w.call_in(*state).await;
        assert_eq!(call.state, *state);
        assert_eq!(call.version, 1 + canonical_path(*state).len() as i64);
    }
    // A video call is the same canonical object.
    let video = w
        .calls()
        .create_requested_call(
            &w.ctx,
            &w.initiator,
            NewCall::of_kind(calling::calls::kinds::CallKind::Video),
        )
        .await
        .unwrap();
    assert_eq!(video.call.state, CallState::Requested);
}
