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
    // CF2: switching to video while it still rings offers the Call as a video call; the
    // callee then answers a video call (its media permissions include video).
    let (call, _caller, callee) = a.ringing_call("VOICE").await;
    let (_, version) = a.state(call).await;
    let r = upgrade(&a, call, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["kind"], "VIDEO", "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ringing);
    let token = a.signaling_token(&callee).await;
    let offers = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(offers.status, StatusCode::OK, "{}", offers.text);
    assert_eq!(
        offers.json["data"][0]["callKind"], "VIDEO",
        "{}",
        offers.text
    );
    let invitation = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, invitation, &callee, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let credential = a.media_credential(call, session).await;
    assert_eq!(credential.status, StatusCode::OK, "{}", credential.text);
    assert_eq!(
        credential.json["capabilities"]["canPublishVideo"], true,
        "{}",
        credential.text
    );
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    // A DIALING Call (no device rang yet) may switch too.
    let caller = a.device(&a.w.initiator, "caller-main").await;
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.json["call"]["state"], "DIALING", "{}", r.text);
    let dialing: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let (_, version) = a.state(dialing).await;
    let r = upgrade(&a, dialing, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.call_view(dialing).await["kind"], "VIDEO");
    let (_, version) = a.state(dialing).await;
    assert_eq!(
        a.command(dialing, "cancel", version).await.status,
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

// ---------------------------------------------------------------------------------------
// C7 — inviting participants into an ongoing call
// ---------------------------------------------------------------------------------------

async fn invite(
    a: &AppWorld,
    call: Uuid,
    inviter: Uuid,
    targets: &[Uuid],
    version: i64,
) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/participants"),
        json!({
            "invitingParticipantId": inviter,
            "targetPrincipalIds": targets,
            "expectedVersion": version,
        }),
    )
    .await
}

/// The history entry of `principal` for `call` (`GET /v1/calls?principalId=`).
async fn history_of(a: &AppWorld, principal: Uuid, call: Uuid) -> Value {
    let r = a.get(&format!("/v1/calls?principalId={principal}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .cloned()
        .unwrap_or_else(|| panic!("no history entry: {}", r.text))
}

/// A world whose group limit is `limit` participants (initiator included).
async fn world_with_limit(limit: usize) -> Option<AppWorld> {
    use std::sync::Arc;
    let db = common::TestDb::new().await?;
    let media = Arc::new(common::fake_media::FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let mut runtime = common::test_runtime(Some(media.clone()), bus.clone());
    runtime.app_config.max_group_participants = limit;
    let h = common::Harness::with_runtime(
        Arc::new(db),
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    Some(AppWorld {
        w: common::calls::CallWorld::in_harness(h, "invite", EnvironmentKind::Production).await,
    })
}

#[tokio::test]
async fn a_one_to_one_call_gains_a_third_party_and_then_follows_group_rules() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let carol = a.principal("invite-carol").await;
    let carol_d = a.device(&carol, "invite-carol").await;
    let mut carol_signals = a.h().bus.subscribe(carol_d.endpoint);
    let mut caller_signals = a.h().bus.subscribe(caller.endpoint);
    let alice = a.participant_of(call, caller.principal).await;
    let (_, version) = a.state(call).await;

    let r = invite(&a, call, alice, &[carol.id().as_uuid()], version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/AddParticipantsResult",
        &r.json,
    );
    assert_eq!(
        r.json["participants"][0]["principalId"],
        carol.id().to_string()
    );
    assert_eq!(r.json["participants"][0]["role"], "RECIPIENT");
    // The Call is unchanged (state and version); the new offer is PENDING until receipt.
    assert_eq!(a.state(call).await, (CallState::Connected, version));
    let view = a.call_view(call).await;
    let offer = view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["endpointId"] == carol_d.endpoint.to_string())
        .cloned()
        .unwrap();
    assert_eq!(offer["status"], "PENDING");
    assert_eq!(offer["purpose"], "CALL");
    let incoming: Vec<_> = drain(&mut carol_signals)
        .into_iter()
        .filter(|s| s["type"] == "call.incoming")
        .collect();
    assert_eq!(incoming.len(), 1);
    assert!(drain(&mut caller_signals)
        .iter()
        .any(|s| s["type"] == "call.participants_changed" && s["data"]["activeParticipants"] == 3));

    // Carol's device acknowledges (RINGING), she accepts and joins the media resource.
    let invitation: Uuid = offer["invitationId"].as_str().unwrap().parse().unwrap();
    let ack = a.acknowledge_for(call, invitation, &carol_d).await;
    assert_eq!(ack.status, StatusCode::OK, "{}", ack.text);
    assert_eq!(ack.json["status"], "RINGING");
    assert_eq!(ack.json["callState"], "CONNECTED");
    let r = a.accept(call, invitation, &carol_d, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["callAnswered"], false);
    let session = a.session_on(call, carol_d.endpoint).await.unwrap();
    let joined = a.connect(call, session).await;
    assert!(joined.permissions.can_publish_audio);
    assert_eq!(a.media().participants(&AppWorld::room(call)).len(), 3);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    assert_eq!(
        history_of(&a, carol.id().as_uuid(), call).await["participation"]["direction"],
        "INBOUND"
    );

    // Group rules now: the original callee leaving does not end the Call.
    let bob_session = a.session_on(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{bob_session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // The last other party leaving ends it (CAL-001 counts the parties now).
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ended);
}

#[tokio::test]
async fn an_added_participants_expired_offer_leaves_the_call_connected_and_is_missed() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, _callee) = a.connected_call("VIDEO").await;
    let dave = a.principal("invite-dave").await;
    let dave_d = a.device(&dave, "invite-dave").await;
    let alice = a.participant_of(call, caller.principal).await;
    let (_, version) = a.state(call).await;
    let r = invite(&a, call, alice, &[dave.id().as_uuid()], version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(a.invitation_for(call, dave_d.endpoint).await.is_some());
    a.h().clock.advance(chrono::Duration::seconds(120));
    a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(a.state(call).await, (CallState::Connected, version));
    assert!(a.invitation_for(call, dave_d.endpoint).await.is_none());
    let view = a.call_view(call).await;
    assert!(view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["endpointId"] == dave_d.endpoint.to_string() && i["status"] == "EXPIRED"));
    let entry = history_of(&a, dave.id().as_uuid(), call).await;
    assert_eq!(entry["participation"]["direction"], "INBOUND");
    assert_eq!(entry["participation"]["outcome"], "MISSED");
    assert_eq!(entry["participation"]["missed"], true);
    // The reaper leaves the Call alone: two parties still participate.
    a.h().state.app.reap_abandoned_calls(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::Connected);
}

#[tokio::test]
async fn invitations_are_authorized_version_checked_and_limited() {
    let Some(a) = world_with_limit(4).await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let erin = a.principal("invite-erin").await;
    a.device(&erin, "invite-erin").await;
    let (_, version) = a.state(call).await;
    // An existing participant is refused; so is a stale version and an unknown inviter.
    let r = invite(&a, call, alice, &[callee.principal], version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CONFLICT");
    let r = invite(&a, call, alice, &[erin.id().as_uuid()], version - 1).await;
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION", "{}", r.text);
    let r = invite(&a, call, Uuid::new_v4(), &[erin.id().as_uuid()], version).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // A target without an eligible endpoint fails the whole request.
    let nobody = a.principal("invite-nobody").await;
    let r = invite(
        &a,
        call,
        alice,
        &[erin.id().as_uuid(), nobody.id().as_uuid()],
        version,
    )
    .await;
    assert_eq!(r.json["error"]["code"], "TARGET_UNAVAILABLE", "{}", r.text);
    assert_eq!(
        a.call_view(call).await["participants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // calls.read cannot invite; another environment cannot see the Call.
    let read_only = a
        .h()
        .issue(
            a.w.consumer.sa(EnvironmentKind::Production),
            &[names::CALLS_READ],
        )
        .await;
    let body = json!({
        "invitingParticipantId": alice,
        "targetPrincipalIds": [erin.id()],
        "expectedVersion": version,
    });
    let r = a
        .h()
        .request(
            Method::POST,
            &format!("/v1/calls/{call}/participants"),
            Some(read_only.secret.expose_once()),
            Some(body.clone()),
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
            &format!("/v1/calls/{call}/participants"),
            Some(foreign.secret.expose_once()),
            Some(body),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // Concurrent invitations serialize on the Call: the limit (4) is never exceeded.
    let mut groups = Vec::new();
    for i in 0..4 {
        let p = a.principal(&format!("invite-race-{i}")).await;
        a.device(&p, &format!("invite-race-{i}")).await;
        groups.push(p.id().as_uuid());
    }
    let (x, y) = tokio::join!(
        invite(&a, call, alice, &groups[0..2], version),
        invite(&a, call, alice, &groups[2..4], version),
    );
    let mut codes = [x.status, y.status];
    codes.sort();
    assert_eq!(
        codes,
        [StatusCode::OK, StatusCode::UNPROCESSABLE_ENTITY],
        "{} / {}",
        x.text,
        y.text
    );
    let loser = if x.status == StatusCode::OK { &y } else { &x };
    assert_eq!(loser.json["error"]["code"], "PARTICIPANT_LIMIT_EXCEEDED");
    assert_eq!(
        a.call_view(call).await["participants"]
            .as_array()
            .unwrap()
            .len(),
        4
    );

    // An Idempotency-Key replays the recorded answer.
    let (call, caller, _callee) = a.connected_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let (_, version) = a.state(call).await;
    let key = [("idempotency-key", "invite-erin-once")];
    let body = json!({
        "invitingParticipantId": alice,
        "targetPrincipalIds": [erin.id()],
        "expectedVersion": version,
    });
    let first = a
        .post_with(
            &format!("/v1/calls/{call}/participants"),
            body.clone(),
            &key,
        )
        .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.text);
    let again = a
        .post_with(&format!("/v1/calls/{call}/participants"), body, &key)
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.headers.get("idempotent-replayed").unwrap(), "true");
    assert_eq!(again.json, first.json);

    // Pre-answer and finished Calls are refused.
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    let (_, version) = a.state(call).await;
    let r = invite(&a, call, alice, &[groups[3]], version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let (call, caller, _callee) = a.ringing_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let (_, version) = a.state(call).await;
    let r = invite(&a, call, alice, &[groups[3]], version).await;
    assert_eq!(r.json["error"]["code"], "CALL_STATE_CONFLICT", "{}", r.text);
}

// ---------------------------------------------------------------------------------------
// C8 — per-participant call-quality samples
// ---------------------------------------------------------------------------------------

async fn sample(a: &AppWorld, call: Uuid, session: Uuid, body: Value) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/sessions/{session}/quality-samples"),
        body,
    )
    .await
}

#[tokio::test]
async fn quality_samples_are_validated_bounded_and_summarized_per_participant() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VIDEO").await;
    let caller_session = a.session_on(call, caller.endpoint).await.unwrap();
    let callee_session = a.session_on(call, callee.endpoint).await.unwrap();

    let r = sample(
        &a,
        call,
        caller_session,
        json!({ "jitterMs": 4.25, "packetLossPercent": 0.5, "rttMs": 60 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/QualitySampleResult",
        &r.json,
    );
    let mos = r.json["mosEstimate"].as_f64().unwrap();
    assert!(mos > 4.0 && mos <= 4.5, "{mos}");
    assert_eq!(
        r.json["participantId"],
        a.participant_of(call, caller.principal).await.to_string()
    );
    // A poor path scores lower.
    let r = sample(
        &a,
        call,
        callee_session,
        json!({ "jitterMs": 80, "packetLossPercent": 8, "rttMs": 600, "intervalSeconds": 30 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert!(r.json["mosEstimate"].as_f64().unwrap() < mos);
    let r = sample(
        &a,
        call,
        callee_session,
        json!({ "jitterMs": 20, "packetLossPercent": 2, "rttMs": 200 }),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);

    // Validation: bounds, finiteness, types and unknown fields; nothing is stored.
    let before = scalar(&a, "SELECT count(*) FROM call_quality_records").await;
    for body in [
        json!({ "jitterMs": -1, "packetLossPercent": 0, "rttMs": 10 }),
        json!({ "jitterMs": 1, "packetLossPercent": 100.5, "rttMs": 10 }),
        json!({ "jitterMs": 1, "packetLossPercent": 0, "rttMs": 60001 }),
        json!({ "jitterMs": 10001, "packetLossPercent": 0, "rttMs": 10 }),
        json!({ "jitterMs": "1", "packetLossPercent": 0, "rttMs": 10 }),
        json!({ "jitterMs": 1, "packetLossPercent": 0 }),
        json!({ "jitterMs": 1, "packetLossPercent": 0, "rttMs": 10, "intervalSeconds": 0 }),
        json!({ "jitterMs": 1, "packetLossPercent": 0, "rttMs": 10, "mosEstimate": 4.4 }),
        json!({ "jitterMs": 1, "packetLossPercent": 0, "rttMs": 10, "provider": "x" }),
    ] {
        let r = sample(&a, call, caller_session, body.clone()).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text);
    }
    // Unknown session / call: 404.
    let ok = json!({ "jitterMs": 1, "packetLossPercent": 0, "rttMs": 10 });
    let r = sample(&a, call, Uuid::new_v4(), ok.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = sample(&a, Uuid::new_v4(), caller_session, ok.clone()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM call_quality_records").await,
        before
    );

    // The summary: per participant, aggregates of its samples only.
    let r = a.get(&format!("/v1/calls/{call}/quality")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/CallQualitySummary",
        &r.json,
    );
    let participants = r.json["participants"].as_array().unwrap();
    assert_eq!(participants.len(), 2, "{}", r.text);
    let callee_participant = a.participant_of(call, callee.principal).await.to_string();
    let callee_q = participants
        .iter()
        .find(|p| p["participantId"] == callee_participant)
        .unwrap();
    assert_eq!(callee_q["sampleCount"], 2);
    assert_eq!(callee_q["jitterMs"]["avg"], 50.0);
    assert_eq!(callee_q["jitterMs"]["max"], 80.0);
    assert_eq!(callee_q["packetLossPercent"]["max"], 8.0);
    assert_eq!(callee_q["rttMs"]["avg"], 400.0);
    assert!(
        callee_q["mosEstimate"]["min"].as_f64().unwrap()
            <= callee_q["mosEstimate"]["avg"].as_f64().unwrap()
    );

    // Bounded: a session holds at most MAX_SAMPLES_PER_SESSION samples (409 beyond).
    let max = calling::calls::app::quality::MAX_SAMPLES_PER_SESSION;
    a.h()
        .conn()
        .execute(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "INSERT INTO call_quality_records (id, call_id, session_id, tenant_id, \
                   application_id, environment_id, observed_from, observed_to, metrics, created_at) \
                 SELECT gen_random_uuid(), r.call_id, r.session_id, r.tenant_id, r.application_id, \
                        r.environment_id, r.observed_from, r.observed_to, r.metrics, r.created_at \
                   FROM call_quality_records r, generate_series(1, {max}) \
                  WHERE r.session_id = '{caller_session}' LIMIT {max}"
            ),
        ))
        .await
        .unwrap();
    let r = sample(&a, call, caller_session, ok.clone()).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // An ended session (here: the Call ended) cannot report.
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = sample(&a, call, callee_session, ok).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}
