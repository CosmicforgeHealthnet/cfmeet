//! Blueprint v1.0 §12 — the calling lifecycle (CAL-001, CAL-002).
//!
//! * A CONNECTED / HELD Call ends automatically — through the END path, as a system
//!   command whose CALL_STATE_CHANGED carries `end_trigger` — when fewer than two parties
//!   keep live participation: a party leaving (session removal) ends a one-to-one Call at
//!   once; a dropped connection counts during the participation grace (reconnects), then
//!   the reaper ends the Call. Group Calls continue while two parties remain; a completed
//!   transfer never ends the live transferred Call; single-human enterprise phases (queue
//!   wait, IVR, voicemail) run while the Call is still ringing and are never touched.
//! * The idle reaper and the maximum call duration bound every answered Call.
//! * The media resource of an abandoned Call is never re-created.
//! * A client END racing an automatic end yields one ENDED and one usage set.
//! * CAL-002: every party (caller, callee, transferred party) can leave; END / HOLD /
//!   RESUME are not reserved to the initiator (application-scope commands), so a Call
//!   whose initiator transferred away can still be held, resumed and ended.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::{app::AppCallingConfig, CallState},
    domain::EnvironmentKind,
    media::{
        provider::{MediaResourceName, MediaSessionRef},
        CallMediaProvider,
    },
};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    enterprise::{id_of, EnterpriseWorld},
    fake_media::FakeMediaProvider,
    test_runtime, Harness, TestDb,
};
use serde_json::json;
use uuid::Uuid;

/// An AppWorld whose application-calling tunables are adjusted by `configure`.
async fn world(configure: impl FnOnce(&mut AppCallingConfig)) -> Option<AppWorld> {
    let db = Arc::new(TestDb::new().await?);
    let media = Arc::new(FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let mut runtime = test_runtime(Some(media.clone()), bus.clone());
    configure(&mut runtime.app_config);
    let h = Harness::with_runtime(
        db,
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    Some(AppWorld {
        w: CallWorld::in_harness(h, "lifecycle", EnvironmentKind::Production).await,
    })
}

async fn scalar(a: &AppWorld, sql: &str) -> i64 {
    common::scalar_i64(a.h().conn(), sql).await
}

async fn text(a: &AppWorld, sql: &str) -> Vec<String> {
    common::enterprise::strings(a.h().conn(), sql).await
}

/// The `end_trigger` of the Call's ENDING transition (None: ended by a client END).
async fn end_trigger(a: &AppWorld, call: Uuid) -> Option<String> {
    text(
        a,
        &format!(
            "SELECT COALESCE(payload->>'end_trigger', '-') FROM call_events \
              WHERE call_id = '{call}' AND event_type = 'CALL_STATE_CHANGED' AND to_state = 'ENDING'"
        ),
    )
    .await
    .pop()
    .filter(|t| t != "-")
}

async fn remove(a: &AppWorld, call: Uuid, session: Uuid) -> common::TestResponse {
    let (_, version) = a.state(call).await;
    a.post(
        &format!("/v1/calls/{call}/sessions/{session}/remove"),
        json!({ "expectedVersion": version }),
    )
    .await
}

/// The provider reports the session's connection gone (participant_left).
async fn drop_connection(a: &AppWorld, call: Uuid, session: Uuid) {
    let identity = text(
        a,
        &format!("SELECT media_identity FROM call_sessions WHERE id = '{session}'"),
    )
    .await
    .pop()
    .unwrap();
    let room = AppWorld::room(call);
    let left = a
        .media()
        .participant(&room, &identity)
        .expect("connected at the provider");
    a.media().leave(&room, &identity);
    let (auth, body) = a
        .media()
        .signed_event("participant_left", &room, Some(&left));
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
}

async fn reap(a: &AppWorld) -> usize {
    a.h().state.app.reap_abandoned_calls(100).await.unwrap()
}

fn advance(a: &AppWorld, seconds: i64) {
    a.h().clock.advance(chrono::Duration::seconds(seconds));
}

async fn sessions(a: &AppWorld, call: Uuid, devices: &[&Device]) -> Vec<Uuid> {
    let mut out = Vec::new();
    for d in devices {
        out.push(a.session_on(call, d.endpoint).await.unwrap());
    }
    out
}

/// Every usage record of the Call is finalized and none is duplicated.
async fn usage_finalized_once(a: &AppWorld, call: Uuid) {
    let usage =
        a.w.calls()
            .usage_records(a.w.scope(), calling::calls::ids::CallId::from_uuid(call))
            .await
            .unwrap();
    assert!(!usage.is_empty());
    assert!(usage.iter().all(|u| u.finalized_at.is_some()), "{usage:?}");
    assert_eq!(
        scalar(
            a,
            &format!(
                "SELECT count(*) FROM (SELECT usage_type, participant_id, session_id, count(*) \
                   FROM call_usage_records WHERE call_id = '{call}' \
                  GROUP BY usage_type, participant_id, session_id HAVING count(*) > 1) d"
            )
        )
        .await,
        0,
        "no usage record is duplicated"
    );
}

#[tokio::test]
async fn a_one_to_one_call_ends_when_the_callee_leaves() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let auto_ended = calling::metrics::counter(
        "calling_calls_auto_ended_total",
        &[("reason", "PARTICIPATION_ENDED")],
    );
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let room = AppWorld::room(call);
    advance(&a, 9);
    let s = sessions(&a, call, &[&caller, &callee]).await;
    let r = remove(&a, call, s[1]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Same path as END: ENDING → cleanup → ENDED, with the trigger on the evidence.
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
    let view = a.call_view(call).await;
    assert_eq!(view["terminalReason"], "CLEANUP_CONFIRMED");
    assert!(view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["state"] == "ENDED" && s["mediaAccess"] == "REVOKED"));
    assert!(!a.media().room_exists(&room), "the resource is terminated");
    // Usage finalized; nothing counts as active any more (limits, agents, history).
    usage_finalized_once(&a, call).await;
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM calls WHERE current_state NOT IN \
               ('ENDED', 'DECLINED', 'BUSY', 'NO_ANSWER', 'FAILED', 'CANCELLED')"
        )
        .await,
        0
    );
    assert!(
        calling::metrics::counter(
            "calling_calls_auto_ended_total",
            &[("reason", "PARTICIPATION_ENDED")]
        ) > auto_ended
    );
    // The caller's own END afterwards is a harmless replay.
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                   AND event_type = 'CALL_STATE_CHANGED' AND to_state = 'ENDED'"
            )
        )
        .await,
        1
    );
}

#[tokio::test]
async fn dropped_connections_count_during_the_grace_then_the_call_ends() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    // One party drops and comes back within the grace: the Call survives.
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let s = sessions(&a, call, &[&caller, &callee]).await;
    drop_connection(&a, call, s[1]).await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 30);
    assert_eq!(reap(&a).await, 0);
    a.connect(call, s[1]).await;
    advance(&a, 120);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);

    // One party drops for good: the Call ends once the grace has passed.
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let s = sessions(&a, call, &[&caller, &callee]).await;
    drop_connection(&a, call, s[1]).await;
    advance(&a, 59);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 2);
    assert_eq!(reap(&a).await, 1);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
    usage_finalized_once(&a, call).await;

    // Both parties disconnect (tabs closed): ended after the grace, never kept forever.
    let (call, caller, callee) = a.connected_call("VIDEO").await;
    let s = sessions(&a, call, &[&caller, &callee]).await;
    drop_connection(&a, call, s[0]).await;
    drop_connection(&a, call, s[1]).await;
    assert_eq!(
        a.state(call).await.0,
        CallState::Connected,
        "both inside the grace"
    );
    advance(&a, 61);
    assert_eq!(reap(&a).await, 1);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
    // A late provider callback neither resurrects the Call nor its resource.
    let (auth, body) = a
        .media()
        .signed_event("room_finished", &AppWorld::room(call), None);
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
}

#[tokio::test]
async fn a_group_call_continues_while_two_parties_remain() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VIDEO").await;
    let s = sessions(&a, call, &[&caller, &callee, &third]).await;
    assert_eq!(remove(&a, call, s[2]).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 120);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // The second departure leaves the initiator alone: the Call ends.
    assert_eq!(remove(&a, call, s[1]).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
    // A HELD group Call follows the same rule.
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let s = sessions(&a, call, &[&caller, &callee, &third]).await;
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "hold", version).await.status,
        StatusCode::OK
    );
    assert_eq!(remove(&a, call, s[0]).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Held);
    assert_eq!(remove(&a, call, s[2]).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
}

#[tokio::test]
async fn a_completed_transfer_keeps_the_live_transferred_call() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let dave = a.principal("lifecycle-dave").await;
    let dave_d = a.device(&dave, "lifecycle-dave").await;
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, bob, &dave, version, "lifecycle-transfer")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, dave_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &dave_d, version).await.status,
        StatusCode::OK
    );
    // While the target is joining the Call is TRANSFERRING: never evaluated.
    advance(&a, 30);
    assert_eq!(reap(&a).await, 0);
    let dave_session = a.session_on(call, dave_d.endpoint).await.unwrap();
    a.connect(call, dave_session).await;
    // The transferor is retired; the target and the remaining party keep the Call.
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 300);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // CAL-002: the transferred party can leave — and that ends the Call for both.
    assert_eq!(remove(&a, call, dave_session).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
}

#[tokio::test]
async fn every_party_can_leave_and_control_is_not_reserved_to_the_initiator() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    // The caller leaves: the Call ends for the callee too.
    let (call, caller, _callee) = a.connected_call("VOICE").await;
    let session = a.session_on(call, caller.endpoint).await.unwrap();
    assert_eq!(remove(&a, call, session).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);

    // The initiator transfers its own place away: the two remaining parties are both
    // non-initiators — they can still hold, resume and end the Call (application-scope
    // commands; the backend has no initiator-only rule).
    let erin = a.principal("lifecycle-erin").await;
    let erin_d = a.device(&erin, "lifecycle-erin").await;
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let alice = a.participant_of(call, a.w.initiator.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, alice, &erin, version, "lifecycle-initiator-away")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, erin_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &erin_d, version).await.status,
        StatusCode::OK
    );
    let erin_session = a.session_on(call, erin_d.endpoint).await.unwrap();
    a.connect(call, erin_session).await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    let view = a.call_view(call).await;
    assert!(view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["role"] == "INITIATOR" && !p["endedAt"].is_null()));
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await;
    assert_eq!(actions.status, StatusCode::OK, "{}", actions.text);
    for action in ["END", "HOLD", "TRANSFER"] {
        assert!(
            actions.json["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| x["action"] == action && x["available"] == true),
            "{action}: {}",
            actions.text
        );
    }
    for action in ["hold", "resume"] {
        let (_, version) = a.state(call).await;
        let r = a.command(call, action, version).await;
        assert_eq!(r.status, StatusCode::OK, "{action}: {}", r.text);
    }
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(end_trigger(&a, call).await, None, "a client END");
}

#[tokio::test]
async fn idle_calls_are_reaped() {
    // A long grace isolates the idle rule: both parties still "count", nothing is
    // connected.
    let Some(a) = world(|c| {
        c.participation_grace = chrono::Duration::seconds(3_600);
        c.idle_call_timeout = chrono::Duration::seconds(600);
    })
    .await
    else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let s = sessions(&a, call, &[&caller, &callee]).await;
    drop_connection(&a, call, s[0]).await;
    drop_connection(&a, call, s[1]).await;
    advance(&a, 300);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 301);
    assert_eq!(reap(&a).await, 1);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(end_trigger(&a, call).await.as_deref(), Some("IDLE_TIMEOUT"));
    usage_finalized_once(&a, call).await;

    // An answered Call whose media path never establishes is reaped as idle too.
    let (call, _caller, callee) = a.ringing_call("VOICE").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &callee, version).await.status,
        StatusCode::OK
    );
    advance(&a, 599);
    assert_eq!(reap(&a).await, 0);
    assert_eq!(a.state(call).await.0, CallState::Answered);
    advance(&a, 2);
    assert_eq!(reap(&a).await, 1);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(end_trigger(&a, call).await.as_deref(), Some("IDLE_TIMEOUT"));
}

#[tokio::test]
async fn calls_never_outlive_the_maximum_duration() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let max = a.h().state.app.config().max_call_duration.num_seconds();
    assert_eq!(max, 28_800, "safe default: 8 h");
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    advance(&a, max - 1);
    assert_eq!(reap(&a).await, 0, "both parties are still connected");
    assert_eq!(a.state(call).await.0, CallState::Connected);
    advance(&a, 2);
    assert_eq!(reap(&a).await, 1);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(end_trigger(&a, call).await.as_deref(), Some("MAX_DURATION"));
    usage_finalized_once(&a, call).await;
}

#[tokio::test]
async fn an_abandoned_calls_media_resource_is_never_recreated() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let world = &a;
    let lose_room = |call: Uuid| async move {
        let room = MediaSessionRef {
            name: MediaResourceName::for_call(calling::calls::ids::CallId::from_uuid(call)),
        };
        CallMediaProvider::terminate_session(world.media(), &room)
            .await
            .unwrap();
        let (auth, body) = world
            .media()
            .signed_event("room_finished", &AppWorld::room(call), None);
        assert_eq!(world.callback(&auth, body).await.status, StatusCode::OK);
    };
    // Inside the grace (both parties reconnecting) a lost resource is re-ensured.
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let s = sessions(&a, call, &[&caller, &callee]).await;
    drop_connection(&a, call, s[0]).await;
    drop_connection(&a, call, s[1]).await;
    lose_room(call).await;
    let ensured = a.media().ensure_calls();
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert_eq!(a.media().ensure_calls(), ensured + 1);
    assert!(a.media().room_exists(&AppWorld::room(call)));
    assert_eq!(a.state(call).await.0, CallState::Connected);

    // Past the grace nobody is there: the resource is NOT re-created; the Call ends.
    advance(&a, 61);
    lose_room(call).await;
    let ensured = a.media().ensure_calls();
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert_eq!(a.media().ensure_calls(), ensured, "no re-creation");
    assert!(!a.media().room_exists(&AppWorld::room(call)));
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        end_trigger(&a, call).await.as_deref(),
        Some("PARTICIPATION_ENDED")
    );
    // Later reconciliation passes never bring it back.
    advance(&a, 60);
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert!(!a.media().room_exists(&AppWorld::room(call)));
}

#[tokio::test]
async fn a_client_end_racing_an_automatic_end_ends_once() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    for round in 0..3 {
        // Client END against the reaper.
        let (call, caller, callee) = a.connected_call("VOICE").await;
        let s = sessions(&a, call, &[&caller, &callee]).await;
        drop_connection(&a, call, s[0]).await;
        drop_connection(&a, call, s[1]).await;
        advance(&a, 61);
        let (_, version) = a.state(call).await;
        let app = a.h().state.app.clone();
        let (end, reaped) = tokio::join!(a.command(call, "end", version), async move {
            app.reap_abandoned_calls(10).await
        });
        assert_eq!(end.status, StatusCode::OK, "round {round}: {}", end.text);
        assert!(reaped.unwrap() <= 1);
        assert_eq!(a.state(call).await.0, CallState::Ended);
        for to in ["ENDING", "ENDED"] {
            assert_eq!(
                scalar(
                    &a,
                    &format!(
                        "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                           AND event_type = 'CALL_STATE_CHANGED' AND to_state = '{to}'"
                    )
                )
                .await,
                1,
                "round {round}: one {to}"
            );
        }
        usage_finalized_once(&a, call).await;

        // Both parties leave at the same time.
        let (call, caller, callee) = a.connected_call("VOICE").await;
        let s = sessions(&a, call, &[&caller, &callee]).await;
        let (_, version) = a.state(call).await;
        let (p1, p2) = (
            format!("/v1/calls/{call}/sessions/{}/remove", s[0]),
            format!("/v1/calls/{call}/sessions/{}/remove", s[1]),
        );
        let (r1, r2) = tokio::join!(
            a.post(&p1, json!({ "expectedVersion": version })),
            a.post(&p2, json!({ "expectedVersion": version })),
        );
        // A stale version (the other removal or the automatic end won) is a clean 409.
        for r in [&r1, &r2] {
            assert!(
                r.status == StatusCode::OK || r.status == StatusCode::CONFLICT,
                "round {round}: {}",
                r.text
            );
        }
        assert!(r1.status == StatusCode::OK || r2.status == StatusCode::OK);
        assert_eq!(a.state(call).await.0, CallState::Ended);
        assert_eq!(
            scalar(
                &a,
                &format!(
                    "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                       AND event_type = 'CALL_STATE_CHANGED' AND to_state = 'ENDED'"
                )
            )
            .await,
            1
        );
        usage_finalized_once(&a, call).await;
    }
}

/// Queue agents become free again when their Call ends automatically, and a caller
/// waiting in a queue (one human, no second party yet — the Call is still ringing) is
/// never touched by the reaper.
#[tokio::test]
async fn queue_waits_are_never_reaped_and_agents_become_free() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let agent = e.member("lifecycle-agent").await;
    let agent_d = e.a.device(&agent, "lifecycle-agent-desk").await;
    let q = e
        .create(
            "/v1/enterprise/queues",
            json!({ "name": "Lifecycle queue", "maxWaitSeconds": 3600,
                    "offerTimeoutSeconds": 20, "capacity": 10 }),
        )
        .await;
    let queue_id = id_of(&q, "queueId");
    let r = e
        .admin(
            Method::PUT,
            &format!("/v1/enterprise/queues/{queue_id}/members"),
            Some(json!({ "expectedVersion": q["version"], "members": [{ "principalId": agent.id().to_string() }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    e.extension("5100", "QUEUE", queue_id).await;
    let mut callers = Vec::new();
    let mut calls = Vec::new();
    for i in 0..2 {
        let principal = e.member(&format!("lifecycle-caller-{i}")).await;
        let phone =
            e.a.device(&principal, &format!("lifecycle-caller-{i}"))
                .await;
        let r = e
            .call_extension(
                "VOICE",
                &principal,
                &phone,
                "5100",
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        assert_eq!(r.json["enterprise"]["phase"], "QUEUED");
        calls.push(
            r.json["call"]["callId"]
                .as_str()
                .unwrap()
                .parse::<Uuid>()
                .unwrap(),
        );
        callers.push(phone);
    }
    // The agent answers the head; both connect.
    let offer =
        e.a.invitation_for(calls[0], agent_d.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(calls[0]).await;
    assert_eq!(
        e.a.accept(calls[0], offer, &agent_d, v).await.status,
        StatusCode::OK
    );
    for d in [&callers[0], &agent_d] {
        let s = e.a.session_on(calls[0], d.endpoint).await.unwrap();
        e.a.connect(calls[0], s).await;
    }
    assert_eq!(e.a.state(calls[0]).await.0, CallState::Connected);
    // The second caller waits: one human plus the queue, the Call still RINGING. Past
    // the grace and the idle timeout the reaper leaves it alone.
    e.h().clock.advance(chrono::Duration::seconds(700));
    assert_eq!(e.h().state.app.reap_abandoned_calls(100).await.unwrap(), 0);
    assert_eq!(e.a.state(calls[1]).await.0, CallState::Ringing);
    assert_eq!(e.a.state(calls[0]).await.0, CallState::Connected);
    let entry_state = |call: Uuid| {
        let conn = e.h().conn().clone();
        async move {
            common::enterprise::strings(
                &conn,
                &format!("SELECT state FROM enterprise_queue_entries WHERE call_id = '{call}' ORDER BY seq DESC LIMIT 1"),
            )
            .await
            .pop()
            .unwrap_or_default()
        }
    };
    assert_eq!(entry_state(calls[1]).await, "WAITING");
    // The first caller hangs up (leaves): the Call ends automatically and the agent is
    // free — the waiting caller is offered to the agent.
    let s = e.a.session_on(calls[0], callers[0].endpoint).await.unwrap();
    let (_, v) = e.a.state(calls[0]).await;
    let r = e
        .h()
        .request_with(
            Method::POST,
            &format!("/v1/calls/{}/sessions/{s}/remove", calls[0]),
            Some(e.caller_token()),
            Some(json!({ "expectedVersion": v })),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(e.a.state(calls[0]).await.0, CallState::Ended);
    e.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(entry_state(calls[1]).await, "OFFERING");
    assert!(e
        .a
        .invitation_for(calls[1], agent_d.endpoint)
        .await
        .is_some());
}

fn lookup(pairs: Vec<(&'static str, &'static str)>) -> impl Fn(&str) -> Option<String> {
    move |key| {
        pairs
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    }
}

#[test]
fn automatic_end_configuration_is_validated() {
    let defaults = AppCallingConfig::from_lookup(&lookup(vec![])).unwrap();
    assert_eq!(defaults.participation_grace.num_seconds(), 60);
    assert_eq!(defaults.idle_call_timeout.num_seconds(), 600);
    assert_eq!(defaults.max_call_duration.num_seconds(), 28_800);
    let tuned = AppCallingConfig::from_lookup(&lookup(vec![
        ("CALLING_PARTICIPATION_GRACE_SECONDS", "30"),
        ("CALLING_IDLE_CALL_TIMEOUT_SECONDS", "300"),
        ("CALLING_MAX_CALL_DURATION_SECONDS", "7200"),
    ]))
    .unwrap();
    assert_eq!(tuned.participation_grace.num_seconds(), 30);
    assert_eq!(tuned.idle_call_timeout.num_seconds(), 300);
    assert_eq!(tuned.max_call_duration.num_seconds(), 7_200);
    for bad in [
        vec![("CALLING_PARTICIPATION_GRACE_SECONDS", "5")],
        vec![("CALLING_PARTICIPATION_GRACE_SECONDS", "soon")],
        vec![("CALLING_IDLE_CALL_TIMEOUT_SECONDS", "60")],
        vec![("CALLING_MAX_CALL_DURATION_SECONDS", "999999")],
        // The idle timeout must exceed the grace, the maximum duration the idle timeout.
        vec![
            ("CALLING_PARTICIPATION_GRACE_SECONDS", "600"),
            ("CALLING_IDLE_CALL_TIMEOUT_SECONDS", "600"),
        ],
        vec![
            ("CALLING_IDLE_CALL_TIMEOUT_SECONDS", "3600"),
            ("CALLING_MAX_CALL_DURATION_SECONDS", "1800"),
        ],
    ] {
        let shown = format!("{bad:?}");
        assert!(
            AppCallingConfig::from_lookup(&lookup(bad)).is_err(),
            "{shown}"
        );
    }
}
