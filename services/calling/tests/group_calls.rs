//! Group call governance: the call host (creator by default, transferable; host-only
//! end / hold for everyone, removal and mute of participants) and rejoining an ongoing
//! call after leaving it.

mod common;

use axum::http::StatusCode;
use calling::{calls::CallState, signaling::RealtimeSignalBus};
use common::app::AppWorld;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::{json, Value};
use uuid::Uuid;

fn drain(subscription: &mut calling::signaling::bus::SignalSubscription) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(signal) = subscription.try_recv() {
        out.push(signal);
    }
    out
}

async fn host_of(a: &AppWorld, call: Uuid) -> Value {
    let view = a.call_view(call).await;
    common::contracts::check_openapi_component("Call", &view);
    view["hostParticipantId"].clone()
}

async fn everyone(a: &AppWorld, call: Uuid, action: &str, acting: Uuid) -> common::TestResponse {
    let (_, version) = a.state(call).await;
    a.post(
        &format!("/v1/calls/{call}/{action}"),
        json!({ "expectedVersion": version, "actingParticipantId": acting }),
    )
    .await
}

async fn mute(
    a: &AppWorld,
    call: Uuid,
    acting: Uuid,
    target: Uuid,
    muted: bool,
) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/participants/{target}/mute"),
        json!({ "actingParticipantId": acting, "muted": muted }),
    )
    .await
}

async fn remove(a: &AppWorld, call: Uuid, acting: Uuid, target: Uuid) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/participants/{target}/remove"),
        json!({ "actingParticipantId": acting }),
    )
    .await
}

async fn transfer_host(a: &AppWorld, call: Uuid, acting: Uuid, to: Uuid) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/host"),
        json!({ "actingParticipantId": acting, "hostParticipantId": to }),
    )
    .await
}

fn can_publish_audio(a: &AppWorld, call: Uuid, identity: &str) -> bool {
    a.media()
        .participant(&AppWorld::room(call), identity)
        .expect("provider participant")
        .permissions
        .can_publish_audio
}

/// The provider identity of the live session on `endpoint` in `call`.
async fn identity_of(a: &AppWorld, call: Uuid, endpoint: Uuid) -> String {
    let session = a.session_on(call, endpoint).await.unwrap();
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

#[tokio::test]
async fn the_creator_hosts_a_group_call_and_only_the_host_acts_for_everyone() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let carol = a.participant_of(call, third.principal).await;
    assert_eq!(host_of(&a, call).await, alice.to_string());

    // A participant who is not the host can neither end nor hold the call for everyone.
    for action in ["end", "hold"] {
        let r = everyone(&a, call, action, bob).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{action}: {}", r.text);
        assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED", "{}", r.text);
    }
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // ... nor remove or mute anyone, nor take the host role.
    assert_eq!(
        remove(&a, call, bob, carol).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        mute(&a, call, bob, carol, true).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        transfer_host(&a, call, bob, bob).await.status,
        StatusCode::FORBIDDEN
    );
    // Cancel never names an acting participant.
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/cancel"),
            json!({ "expectedVersion": version, "actingParticipantId": alice }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // The host holds and resumes for everyone.
    let r = everyone(&a, call, "hold", alice).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Held);
    let r = everyone(&a, call, "resume", alice).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Connected);

    // The host hands the role over; the former host lost its powers.
    let mut bob_signals = a.h().bus.subscribe(callee.endpoint);
    let r = transfer_host(&a, call, alice, carol).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/HostControlResult",
        &r.json,
    );
    assert_eq!(r.json["hostParticipantId"], carol.to_string());
    assert_eq!(host_of(&a, call).await, carol.to_string());
    assert!(drain(&mut bob_signals).iter().any(|s| {
        s["type"] == "call.participants_changed"
            && s["data"]["hostParticipantId"] == carol.to_string()
    }));
    assert_eq!(
        everyone(&a, call, "end", alice).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        remove(&a, call, alice, bob).await.status,
        StatusCode::FORBIDDEN
    );
    // A host cannot remove itself (it leaves instead).
    assert_eq!(
        remove(&a, call, carol, carol).await.status,
        StatusCode::BAD_REQUEST
    );

    // The new host ends the call for everyone.
    let r = everyone(&a, call, "end", carol).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(matches!(
        a.state(call).await.0,
        CallState::Ending | CallState::Ended
    ));
}

#[tokio::test]
async fn the_host_mutes_at_the_provider_and_removes_for_good() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let carol = a.participant_of(call, third.principal).await;
    let bob_identity = identity_of(&a, call, callee.endpoint).await;
    assert!(can_publish_audio(&a, call, &bob_identity));

    // Mute: enforced at the provider, shown on the Call, survives hold / resume.
    let r = mute(&a, call, alice, bob, true).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/HostControlResult",
        &r.json,
    );
    assert_eq!(r.json["operation"], "MUTE_PARTICIPANT");
    assert!(!can_publish_audio(&a, call, &bob_identity));
    let view = a.call_view(call).await;
    assert_eq!(view["mutedParticipantIds"], json!([bob.to_string()]));
    assert_eq!(
        everyone(&a, call, "hold", alice).await.status,
        StatusCode::OK
    );
    assert_eq!(
        everyone(&a, call, "resume", alice).await.status,
        StatusCode::OK
    );
    assert!(!can_publish_audio(&a, call, &bob_identity));
    // Muting again changes nothing; unmuting restores audio publishing.
    assert_eq!(
        mute(&a, call, alice, bob, true).await.status,
        StatusCode::OK
    );
    let r = mute(&a, call, alice, bob, false).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["operation"], "UNMUTE_PARTICIPANT");
    assert!(can_publish_audio(&a, call, &bob_identity));
    assert_eq!(a.call_view(call).await["mutedParticipantIds"], json!([]));

    // Removal: the participant leaves for good, the call goes on with two parties.
    let r = remove(&a, call, alice, bob).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["removed"], true);
    let view = a.call_view(call).await;
    let bob_row = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["participantId"] == bob.to_string())
        .cloned()
        .unwrap();
    assert!(!bob_row["endedAt"].is_null(), "{bob_row}");
    assert!(a.session_on(call, callee.endpoint).await.is_none());
    assert_eq!(a.state(call).await.0, CallState::Connected);
    // A removed participant cannot be muted, and removing it again is a no-op.
    assert_eq!(
        mute(&a, call, alice, bob, true).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(remove(&a, call, alice, bob).await.status, StatusCode::OK);
    // Removing the last other party ends the call (fewer than two parties).
    assert_eq!(remove(&a, call, alice, carol).await.status, StatusCode::OK);
    assert!(matches!(
        a.state(call).await.0,
        CallState::Ending | CallState::Ended
    ));
}

#[tokio::test]
async fn in_a_one_to_one_call_either_party_still_ends_it() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    assert_eq!(host_of(&a, call).await, alice.to_string());
    let r = everyone(&a, call, "end", bob).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn a_host_who_leaves_hands_the_role_to_someone_present() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let carol = a.participant_of(call, third.principal).await;
    let session = a.session_on(call, caller.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // The creator is no longer present: a present participant (the longest-standing) hosts.
    let host: Uuid = host_of(&a, call).await.as_str().unwrap().parse().unwrap();
    assert!(host == bob || host == carol, "{host}");
    assert_eq!(
        everyone(&a, call, "hold", alice).await.status,
        StatusCode::FORBIDDEN
    );
    let r = everyone(&a, call, "hold", host).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

// ---------------------------------------------------------------------------------------
// Rejoining an ongoing call
// ---------------------------------------------------------------------------------------

async fn rejoin(
    a: &AppWorld,
    call: Uuid,
    participant: Uuid,
    endpoint: Uuid,
) -> common::TestResponse {
    a.post(
        &format!("/v1/calls/{call}/participants/{participant}/rejoin"),
        json!({ "endpointId": endpoint }),
    )
    .await
}

async fn leave(a: &AppWorld, call: Uuid, endpoint: Uuid) {
    let session = a.session_on(call, endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

#[tokio::test]
async fn a_participant_who_left_rejoins_the_ongoing_group_call() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, _third) = a.connected_group_call("VOICE").await;
    let bob = a.participant_of(call, callee.principal).await;
    leave(&a, call, callee.endpoint).await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    assert!(a.session_on(call, callee.endpoint).await.is_none());
    assert_eq!(a.media().participants(&AppWorld::room(call)).len(), 2);

    let mut alice_signals = a.h().bus.subscribe(caller.endpoint);
    let r = rejoin(&a, call, bob, callee.endpoint).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    common::contracts::check(
        "calling/control/v1.schema.json#/$defs/RejoinResult",
        &r.json,
    );
    assert_eq!(r.json["rejoined"], true);
    assert_eq!(r.json["participantId"], bob.to_string());
    let session: Uuid = r.json["sessionId"].as_str().unwrap().parse().unwrap();
    assert_eq!(a.session_on(call, callee.endpoint).await, Some(session));
    assert!(drain(&mut alice_signals).iter().any(|s| {
        s["type"] == "call.participants_changed"
            && s["data"]["rejoinedParticipantId"] == bob.to_string()
    }));
    // The new session joins the media resource like after an answer.
    let joined = a.connect(call, session).await;
    assert!(joined.permissions.can_publish_audio);
    assert_eq!(a.media().participants(&AppWorld::room(call)).len(), 3);
    // Asking again while that session is live returns it.
    let again = rejoin(&a, call, bob, callee.endpoint).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["rejoined"], false);
    assert_eq!(again.json["sessionId"], session.to_string());
    assert_eq!(a.state(call).await.0, CallState::Connected);

    // The creator leaves and comes back: it is the host again once present.
    let alice = a.participant_of(call, caller.principal).await;
    leave(&a, call, caller.endpoint).await;
    assert_ne!(host_of(&a, call).await, alice.to_string());
    let r = rejoin(&a, call, alice, caller.endpoint).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let session: Uuid = r.json["sessionId"].as_str().unwrap().parse().unwrap();
    a.connect(call, session).await;
    assert_eq!(host_of(&a, call).await, alice.to_string());
}

#[tokio::test]
async fn rejoining_is_refused_after_removal_before_joining_and_on_foreign_endpoints() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee, third) = a.connected_group_call("VOICE").await;
    let alice = a.participant_of(call, caller.principal).await;
    let bob = a.participant_of(call, callee.principal).await;
    let carol = a.participant_of(call, third.principal).await;

    // A participant the host removed never comes back.
    let r = a
        .post(
            &format!("/v1/calls/{call}/participants/{carol}/remove"),
            json!({ "actingParticipantId": alice }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = rejoin(&a, call, carol, third.endpoint).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED");
    // Another principal's endpoint is not the participant's own: 404.
    assert_eq!(
        rejoin(&a, call, bob, caller.endpoint).await.status,
        StatusCode::NOT_FOUND
    );
    // Still in the call on its device: nothing to rejoin there (the live session is
    // returned), and a second device is refused while the first is live.
    let still = rejoin(&a, call, bob, callee.endpoint).await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.text);
    assert_eq!(still.json["rejoined"], false);
    let bob_tablet = a.device(&a.w.recipient, "bob-tablet").await;
    let r = rejoin(&a, call, bob, bob_tablet.endpoint).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // Someone invited who never answered answers its invitation instead.
    let dave = a.principal("rejoin-dave").await;
    let dave_d = a.device(&dave, "rejoin-dave").await;
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/participants"),
            json!({ "invitingParticipantId": alice, "targetPrincipalIds": [dave.id()],
                    "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let dave_p = a.participant_of(call, dave.id().as_uuid()).await;
    let r = rejoin(&a, call, dave_p, dave_d.endpoint).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_RESOURCE_STATE_CONFLICT");

    // Once the call is over nobody rejoins.
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/end"),
            json!({ "expectedVersion": version, "actingParticipantId": alice }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        rejoin(&a, call, bob, callee.endpoint).await.status,
        StatusCode::CONFLICT
    );
}
