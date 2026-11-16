//! In-call collaboration: upgrading a voice call to video (C5), screen sharing (C6) and
//! inviting participants into an ongoing call (C7) — authorization, version and lock
//! safety, server-derived media permissions pushed to the provider, signals, evidence and
//! contract conformance.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState, domain::EnvironmentKind, media::provider::MediaProviderError,
    platform::scopes::names, signaling::RealtimeSignalBus,
};
use common::{app::AppWorld, fake_media::FakeOp};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

async fn scalar(a: &AppWorld, sql: &str) -> i64 {
    a.h()
        .conn()
        .query_one(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap()
}

async fn upgrade(a: &AppWorld, call: Uuid, version: i64) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/upgrade"),
        json!({ "kind": "VIDEO", "expectedVersion": version }),
    )
    .await
}

/// Signal types delivered to `subscription` so far.
fn drain(subscription: &mut calling::signaling::bus::SignalSubscription) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(signal) = subscription.try_recv() {
        out.push(signal);
    }
    out
}

fn action(actions: &Value, name: &str) -> bool {
    actions["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == name)
        .map(|a| a["available"] == true)
        .unwrap_or_else(|| panic!("{name} missing: {actions}"))
}

// ---------------------------------------------------------------------------------------
// C5 — voice to video
// ---------------------------------------------------------------------------------------

#[tokio::test]
async fn a_party_upgrades_a_voice_call_to_video_once_without_turning_any_camera_on() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let room = AppWorld::room(call);
    let mut caller_signals = a.h().bus.subscribe(caller.endpoint);
    let mut callee_signals = a.h().bus.subscribe(callee.endpoint);
    for p in a.media().participants(&room) {
        assert!(
            !p.permissions.can_publish_video,
            "a voice call publishes no video"
        );
        assert!(a.media().publish(&room, &p.identity, true).is_err());
    }
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await.json;
    assert!(action(&actions, "UPGRADE"), "{actions}");

    let (_, version) = a.state(call).await;
    let r = upgrade(&a, call, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/ControlResult",
        &r.json,
    );
    assert_eq!(r.json["operation"], "UPGRADE");
    assert_eq!(r.json["call"]["kind"], "VIDEO");
    // State and version are unchanged: the version counts state transitions.
    assert_eq!(a.state(call).await, (CallState::Connected, version));
    assert_eq!(a.call_view(call).await["kind"], "VIDEO");

    // Every active participant may now publish video; the provider received it; nobody's
    // camera was published by the server.
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_participant_media_permissions \
                  WHERE call_id = '{call}' AND call_kind = 'VIDEO' AND can_publish_video"
            )
        )
        .await,
        2
    );
    let participants = a.media().participants(&room);
    assert_eq!(participants.len(), 2);
    for p in &participants {
        assert!(p.permissions.can_publish_video && p.permissions.can_publish_audio);
        assert!(!p.published_video, "no camera is turned on by an upgrade");
    }
    // The client may publish its camera when its user chooses.
    assert!(a
        .media()
        .publish(&room, &participants[0].identity, true)
        .is_ok());
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                  AND event_type = 'CALL_KIND_CHANGED'"
            )
        )
        .await,
        1
    );
    for signals in [drain(&mut caller_signals), drain(&mut callee_signals)] {
        let changed: Vec<_> = signals
            .iter()
            .filter(|s| s["type"] == "call.kind_changed")
            .collect();
        assert_eq!(changed.len(), 1, "{signals:?}");
        assert_eq!(changed[0]["callKind"], "VIDEO");
        assert_eq!(changed[0]["data"]["participantsGrantedVideo"], 2);
    }

    // A second upgrade is an idempotent no-op (no second event).
    let r = upgrade(&a, call, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                  AND event_type = 'CALL_KIND_CHANGED'"
            )
        )
        .await,
        1
    );
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await.json;
    assert!(!action(&actions, "UPGRADE"));
    // Never back to VOICE.
    let r = a
        .post(
            &format!("/v1/calls/{call}/upgrade"),
            json!({ "kind": "VOICE", "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let refused = common::calls::sql(
        a.h(),
        &format!("UPDATE calls SET call_kind = 'VOICE' WHERE id = '{call}'"),
    )
    .await;
    assert!(refused.is_err(), "the database refuses VIDEO -> VOICE");

    // A new media credential (and its admission) carries the VIDEO grant.
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let joined = a.connect(call, session).await;
    assert!(joined.permissions.can_publish_video);

    // HOLD still masks video (and audio); RESUME restores the VIDEO grant.
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "hold", version).await.status,
        StatusCode::OK
    );
    for p in a.media().participants(&room) {
        assert!(!p.permissions.can_publish_video && !p.permissions.can_publish_audio);
    }
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "resume", version).await.status,
        StatusCode::OK
    );
    for p in a.media().participants(&room) {
        assert!(p.permissions.can_publish_video);
    }
}

#[tokio::test]
async fn upgrades_are_version_scope_and_state_checked() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    // Pre-answer: the offer keeps the kind it was offered with.
    let (call, _caller, _callee) = a.ringing_call("VOICE").await;
    let (_, version) = a.state(call).await;
    let r = upgrade(&a, call, version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_STATE_CONFLICT");
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "cancel", version).await.status,
        StatusCode::OK
    );

    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let (_, version) = a.state(call).await;
    // Stale version: 409 without effect.
    let r = upgrade(&a, call, version - 1).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION");
    assert_eq!(a.call_view(call).await["kind"], "VOICE");
    // Unknown fields (a client-chosen permission) are refused.
    let r = a
        .post(
            &format!("/v1/calls/{call}/upgrade"),
            json!({ "kind": "VIDEO", "expectedVersion": version, "canPublishVideo": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // calls.read cannot upgrade; another environment's credential cannot see the Call.
    let read_only = a
        .h()
        .issue(
            a.w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_READ],
        )
        .await;
    let r = a
        .h()
        .request(
            Method::POST,
            &format!("/v1/calls/{call}/upgrade"),
            Some(read_only.secret.expose_once()),
            Some(json!({ "kind": "VIDEO", "expectedVersion": version })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    let foreign = a
        .h()
        .issue(
            a.w.consumer.sa(EnvironmentKind::Sandbox),
            &[names::CALLS_WRITE],
        )
        .await;
    let r = a
        .h()
        .request(
            Method::POST,
            &format!("/v1/calls/{call}/upgrade"),
            Some(foreign.secret.expose_once()),
            Some(json!({ "kind": "VIDEO", "expectedVersion": version })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(a.call_view(call).await["kind"], "VOICE");

    // A provider push failure never undoes the committed upgrade: it is recorded as pending
    // enforcement and the reconciler re-applies the server-derived VIDEO grant.
    a.media().fail_next(
        FakeOp::UpdatePermissions,
        MediaProviderError::Unavailable,
        1,
    );
    let key = [("idempotency-key", "upgrade-once")];
    let r = a
        .post_with(
            &format!("/v1/calls/{call}/upgrade"),
            json!({ "kind": "VIDEO", "expectedVersion": version }),
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
                  AND event_type = 'CALL_MEDIA_ENFORCEMENT_PENDING'"
            )
        )
        .await,
        1
    );
    let room = AppWorld::room(call);
    a.h().clock.advance(chrono::Duration::seconds(30));
    a.h().state.app.reconcile_due(100).await.unwrap();
    for p in a.media().participants(&room) {
        assert!(
            p.permissions.can_publish_video,
            "reconciliation repaired the grant"
        );
    }
    // The same key replays the recorded answer.
    let r = a
        .post_with(
            &format!("/v1/calls/{call}/upgrade"),
            json!({ "kind": "VIDEO", "expectedVersion": version }),
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers.get("idempotent-replayed").unwrap(), "true");

    // Terminal Calls are refused.
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Ended);
    let r = upgrade(&a, call, version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(a.call_view(call).await["kind"], "VOICE");
}

// ---------------------------------------------------------------------------------------
// C6 — screen sharing
// ---------------------------------------------------------------------------------------

async fn share(a: &AppWorld, call: Uuid, participant: Uuid, stop: bool) -> common::TestResponse {
    let path = if stop {
        format!("/v1/calls/{call}/screen-share/stop")
    } else {
        format!("/v1/calls/{call}/screen-share")
    };
    a.post(&path, json!({ "participantId": participant })).await
}

/// The provider identity of `device`'s live session in `call`.
async fn identity_of(a: &AppWorld, call: Uuid, device: &common::app::Device) -> String {
    let session = a.session_on(call, device.endpoint).await.unwrap();
    a.h()
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT media_identity FROM call_sessions WHERE id = '{session}'"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

/// Whether the provider grant of `device`'s connection includes the screen sources.
async fn screen_grant(a: &AppWorld, call: Uuid, device: &common::app::Device) -> bool {
    let identity = identity_of(a, call, device).await;
    a.media()
        .participant(&AppWorld::room(call), &identity)
        .unwrap()
        .permissions
        .can_share_screen
}

async fn stop_events(a: &AppWorld, call: Uuid, reason: &str) -> i64 {
    scalar(
        a,
        &format!(
            "SELECT count(*) FROM call_events WHERE call_id = '{call}' \
              AND event_type = 'CALL_SCREEN_SHARE_STOPPED' AND payload->>'stop_reason' = '{reason}'"
        ),
    )
    .await
}

#[tokio::test]
async fn only_the_single_presenter_is_granted_the_screen_sources() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let mut third_signals = a.h().bus.subscribe(third.endpoint);
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await.json;
    assert!(action(&actions, "START_SCREEN_SHARE"));
    assert!(!action(&actions, "STOP_SCREEN_SHARE"));
    for device in [&caller, &callee, &third] {
        assert!(!screen_grant(&a, call, device).await);
    }

    let r = share(&a, call, alice, false).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/ScreenShareResult",
        &r.json,
    );
    assert_eq!(r.json["screenShare"]["participantId"], alice.to_string());
    // Only the presenter's grant carries the screen sources; nobody else's.
    assert!(screen_grant(&a, call, &caller).await);
    assert!(!screen_grant(&a, call, &callee).await);
    assert!(!screen_grant(&a, call, &third).await);
    let view = a.call_view(call).await;
    assert_eq!(view["screenShare"]["participantId"], alice.to_string());
    common::contracts::check_openapi_component("Call", &view);
    let started: Vec<_> = drain(&mut third_signals)
        .into_iter()
        .filter(|s| s["type"] == "call.screen_share_started")
        .collect();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["data"]["participantId"], alice.to_string());
    // A new media credential of the presenter is admitted with the screen grant.
    let session = a.session_on(call, caller.endpoint).await.unwrap();
    assert!(a.connect(call, session).await.permissions.can_share_screen);

    // One presenter per Call: a second presenter is refused; the presenter's repeat is a
    // no-op; another participant cannot stop the presenter.
    let r = share(&a, call, bob, false).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert!(!screen_grant(&a, call, &callee).await);
    let r = share(&a, call, alice, false).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers.get("idempotent-replayed").unwrap(), "true");
    let r = share(&a, call, bob, true).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await.json;
    assert!(!action(&actions, "START_SCREEN_SHARE"));
    assert!(action(&actions, "STOP_SCREEN_SHARE"));

    // HOLD masks the screen sources; RESUME restores them for the (unchanged) presenter.
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "hold", version).await.status,
        StatusCode::OK
    );
    assert!(!screen_grant(&a, call, &caller).await);
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "resume", version).await.status,
        StatusCode::OK
    );
    assert!(screen_grant(&a, call, &caller).await);

    // Stop revokes the source; a repeat is a no-op.
    let r = share(&a, call, alice, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json["screenShare"].is_null());
    assert!(!screen_grant(&a, call, &caller).await);
    assert_eq!(stop_events(&a, call, "STOPPED").await, 1);
    let r = share(&a, call, alice, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.headers.get("idempotent-replayed").unwrap(), "true");
    assert!(a.call_view(call).await["screenShare"].is_null());
    let stopped = drain(&mut third_signals)
        .into_iter()
        .filter(|s| s["type"] == "call.screen_share_stopped")
        .count();
    assert_eq!(stopped, 1);

    // The presenter leaving clears the state (the group Call continues).
    assert_eq!(share(&a, call, bob, false).await.status, StatusCode::OK);
    let bob_session = a.session_on(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{bob_session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(a.call_view(call).await["screenShare"].is_null());
    assert_eq!(stop_events(&a, call, "PRESENTER_LEFT").await, 1);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    assert!(drain(&mut third_signals)
        .iter()
        .any(|s| s["type"] == "call.screen_share_stopped"
            && s["data"]["participantId"] == bob.to_string()));

    // Ending the Call clears it too.
    assert_eq!(share(&a, call, alice, false).await.status, StatusCode::OK);
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert!(a.call_view(call).await["screenShare"].is_null());
    assert_eq!(stop_events(&a, call, "CALL_ENDED").await, 1);
    let r = share(&a, call, alice, false).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}

#[tokio::test]
async fn concurrent_presenters_never_both_win_and_held_calls_refuse() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VIDEO").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let (x, y) = tokio::join!(share(&a, call, alice, false), share(&a, call, bob, false));
    let mut statuses = [x.status, y.status];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{} / {}",
        x.text,
        y.text
    );
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_participant_media_permissions \
                  WHERE call_id = '{call}' AND screen_share_started_at IS NOT NULL"
            )
        )
        .await,
        1
    );
    let presenter = if x.status == StatusCode::OK {
        alice
    } else {
        bob
    };
    assert_eq!(
        share(&a, call, presenter, true).await.status,
        StatusCode::OK
    );
    // A held Call cannot start a presentation; an unknown participant is not found.
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "hold", version).await.status,
        StatusCode::OK
    );
    let r = share(&a, call, alice, false).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = share(&a, call, Uuid::new_v4(), false).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn the_screen_share_policy_switch_refuses_new_presentations() {
    use std::sync::Arc;
    let Some(db) = common::TestDb::new().await else {
        return;
    };
    let media = Arc::new(common::fake_media::FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let mut runtime = common::test_runtime(Some(media.clone()), bus.clone());
    runtime.app_config.screen_share_enabled = false;
    let h = common::Harness::with_runtime(
        Arc::new(db),
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    let a = AppWorld {
        w: common::calls::CallWorld::in_harness(h, "noshare", EnvironmentKind::Production).await,
    };
    let (call, caller, _callee) = a.connected_call("VIDEO").await;
    let alice = a.participant_of(call, caller.principal).await;
    let actions = a.get(&format!("/v1/calls/{call}/actions")).await.json;
    assert!(!action(&actions, "START_SCREEN_SHARE"));
    let r = share(&a, call, alice, false).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert!(!screen_grant(&a, call, &caller).await);
    // Stopping always works (nothing to stop here: an idempotent no-op).
    assert_eq!(share(&a, call, alice, true).await.status, StatusCode::OK);
}
