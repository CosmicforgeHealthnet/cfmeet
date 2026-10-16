//! Concurrency (Part C §18, §19, §44): optimistic versioning under real parallel
//! transactions, race-safe concurrent-call limits, and race-free reconnect / participant
//! insertion. Every race is repeated many times.

mod common;

use std::sync::Arc;

use calling::{
    calls::{
        kinds::{CallKind, DisconnectReason, EndpointCategory, FailureCategory},
        service::NewCall,
        CallAction, CallError, CallState,
    },
    domain::EnvironmentKind,
    platform::{limits::LimitValues, scopes::names},
};
use common::{calls::CallWorld, Harness};
use tokio::sync::Barrier;

const ROUNDS: usize = 25;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_expected_version_commands_succeed_exactly_once() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let w = Arc::new(w);
    for round in 0..ROUNDS {
        // Call at version 5 (DIALING); RINGING, ANSWERED and FAILED all expect version 5.
        let call = w.call_in(CallState::Dialing).await;
        assert_eq!(call.version, 5);
        let actions = [
            CallAction::MarkRinging,
            CallAction::Accept,
            CallAction::Fail {
                category: FailureCategory::Network,
            },
            CallAction::Decline,
        ];
        let barrier = Arc::new(Barrier::new(actions.len()));
        let mut handles = Vec::new();
        for action in actions {
            let (w, barrier) = (w.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                w.calls().transition(&w.cmd, call.id, 5, action).await
            }));
        }
        let mut ok = 0;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(outcome) => {
                    ok += 1;
                    assert_eq!(outcome.call.version, 6);
                }
                Err(CallError::StaleVersion {
                    expected: 5,
                    current: 6,
                }) => {}
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!(ok, 1, "round {round}");
        let after = w.call(call.id).await;
        assert_eq!(after.version, 6);
        // Exactly one transition recorded for version 6, one outbox row for it.
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{}' AND call_version = 6 AND to_state IS NOT NULL",
                call.id
            ))
            .await,
            1
        );
        assert_eq!(w.state_event_count(call.id).await, 5);
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM calling_outbox_events o JOIN call_events e ON e.event_id = o.source_event_id \
                 WHERE e.call_id = '{}' AND e.call_version = 6 AND e.to_state IS NOT NULL",
                call.id
            ))
            .await,
            1
        );
    }
}

#[tokio::test]
async fn replayed_old_commands_never_touch_a_newer_state() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    for target in [CallState::Connected, CallState::Ending, CallState::Ended] {
        let call = w.call_in(target).await;
        // The old MARK_RINGING was built at version 5 (DIALING).
        assert!(matches!(
            w.calls()
                .transition(&w.cmd, call.id, 5, CallAction::MarkRinging)
                .await
                .unwrap_err(),
            CallError::StaleVersion { expected: 5, .. }
        ));
        // Replayed against the current version it is still refused by the graph.
        assert!(w
            .calls()
            .transition(&w.cmd, call.id, call.version, CallAction::MarkRinging)
            .await
            .is_err());
        let after = w.call(call.id).await;
        assert_eq!((after.state, after.version), (target, call.version));
    }
}

async fn limited_world(h: Harness, credential_limit: Option<i32>) -> CallWorld {
    let w = CallWorld::in_harness(h, "limited", EnvironmentKind::Production).await;
    w.h.platform
        .set_credential_limits(
            w.token.credential.id,
            LimitValues {
                max_concurrent_communications: credential_limit,
                requests_per_minute: Some(100_000),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    w
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creations_never_exceed_a_credential_limit_of_one() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let w = Arc::new(limited_world(h, Some(1)).await);
    for round in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let (w, barrier) = (w.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                w.calls()
                    .create_requested_call(&w.ctx, &w.initiator, NewCall::of_kind(CallKind::Voice))
                    .await
            }));
        }
        let mut created = Vec::new();
        for handle in handles {
            match handle.await.unwrap() {
                Ok(c) => created.push(c.call.id),
                Err(CallError::ConcurrentCallLimitExceeded {
                    level: "credential",
                    limit: 1,
                }) => {}
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!(created.len(), 1, "round {round}: {created:?}");
        assert_eq!(w.calls().active_calls_in_scope(w.scope()).await.unwrap(), 1);
        // A terminal call stops counting: the next round can create again.
        w.act(created[0], CallAction::Cancel).await.unwrap();
        assert_eq!(w.calls().active_calls_in_scope(w.scope()).await.unwrap(), 0);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_creations_never_exceed_an_application_limit_across_credentials() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let w = limited_world(h, Some(100)).await;
    w.h.platform
        .set_application_limits(
            w.consumer.application.id,
            LimitValues {
                max_concurrent_communications: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // A second credential of the same application (another environment, too).
    let other_token =
        w.h.issue(
            w.consumer.sa(EnvironmentKind::Sandbox),
            &[names::CALLS_READ, names::CALLS_WRITE],
        )
        .await;
    let other_ctx = w.h.ctx(other_token.secret.expose_once()).await;
    let other_initiator = common::calls::resolve(
        &w.h,
        &w.consumer,
        EnvironmentKind::Sandbox,
        &other_ctx,
        "sandbox-initiator",
    )
    .await;
    let w = Arc::new(w);
    let other = Arc::new((other_ctx, other_initiator));
    for round in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for i in 0..4 {
            let (w, other, barrier) = (w.clone(), other.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                let (ctx, initiator) = if i % 2 == 0 {
                    (&w.ctx, &w.initiator)
                } else {
                    (&other.0, &other.1)
                };
                w.calls()
                    .create_requested_call(ctx, initiator, NewCall::of_kind(CallKind::Video))
                    .await
                    .map(|c| (i, c.call.id))
            }));
        }
        let mut created = Vec::new();
        for handle in handles {
            match handle.await.unwrap() {
                Ok(c) => created.push(c),
                // The application-wide ceiling also caps each credential (stricter wins).
                Err(CallError::ConcurrentCallLimitExceeded { limit: 1, .. }) => {}
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!(created.len(), 1, "round {round}");
        let (i, id) = created[0];
        let cmd = if i % 2 == 0 {
            w.cmd.clone()
        } else {
            calling::calls::CallCommandContext::customer(&other.0)
        };
        let version = w.calls().call(cmd.scope(), id).await.unwrap().version;
        w.calls()
            .transition(&cmd, id, version, CallAction::Cancel)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn a_zero_limit_refuses_every_call() {
    let Some(h) = Harness::new().await else {
        return;
    };
    let w = limited_world(h, Some(0)).await;
    assert!(matches!(
        w.calls()
            .create_requested_call(&w.ctx, &w.initiator, NewCall::of_kind(CallKind::Voice))
            .await
            .unwrap_err(),
        CallError::ConcurrentCallLimitExceeded { limit: 0, .. }
    ));
    assert_eq!(w.count("SELECT count(*) FROM calls").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM call_events").await, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reconnects_open_exactly_one_interval() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let w = Arc::new(w);
    let call = w.call_in(CallState::Connected).await;
    let session = w.calls().sessions(w.scope(), call.id).await.unwrap()[0].id;
    for round in 0..ROUNDS {
        w.calls()
            .disconnect_session(&w.cmd, call.id, session, DisconnectReason::ConnectionLost)
            .await
            .unwrap();
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let (w, barrier) = (w.clone(), barrier.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                w.calls().reconnect_session(&w.cmd, call.id, session).await
            }));
        }
        let mut ok = 0;
        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => ok += 1,
                Err(CallError::InvalidState(_)) => {}
                Err(other) => panic!("round {round}: unexpected {other:?}"),
            }
        }
        assert_eq!(ok, 1, "round {round}");
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM call_session_intervals WHERE session_id = '{session}' AND disconnected_at IS NULL"
            ))
            .await,
            1
        );
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM call_session_intervals WHERE session_id = '{session}'"
            ))
            .await,
            round as i64 + 2
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_participant_insertion_creates_one_logical_participant() {
    let Some(w) = CallWorld::new().await else {
        return;
    };
    let w = Arc::new(w);
    for round in 0..ROUNDS {
        let call = w.create().await.call;
        let principal = Arc::new(w.principal(&format!("racer-{round}")).await);
        let barrier = Arc::new(Barrier::new(6));
        let mut handles = Vec::new();
        for _ in 0..6 {
            let (w, barrier, principal) = (w.clone(), barrier.clone(), principal.clone());
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                w.calls()
                    .add_participant(&w.cmd, call.id, &principal, None)
                    .await
            }));
        }
        let mut created = 0;
        let mut ids = Vec::new();
        for handle in handles {
            let (participant, new) = handle.await.unwrap().unwrap();
            created += usize::from(new);
            ids.push(participant.id);
        }
        ids.dedup();
        assert_eq!(created, 1, "round {round}");
        assert_eq!(ids.len(), 1, "round {round}");
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM call_participants WHERE call_id = '{}' AND principal_id = '{}'",
                call.id,
                principal.id()
            ))
            .await,
            1
        );
        // Sessions for the same participant are concurrent-safe too (multi-device).
        let participant = ids[0];
        let mut handles = Vec::new();
        for _ in 0..3 {
            let w = w.clone();
            handles.push(tokio::spawn(async move {
                w.calls()
                    .create_session(&w.cmd, call.id, participant, EndpointCategory::Web)
                    .await
            }));
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }
        assert_eq!(
            w.count(&format!(
                "SELECT count(*) FROM call_sessions WHERE participant_id = '{participant}'"
            ))
            .await,
            3
        );
    }
}
