//! Application calling end-to-end on the test media provider (Part C §22, §23, §25):
//! create → authorize → resolve → route → ring → single-winner accept → credentials →
//! verified joins → CONNECTED → hold / resume → end → ENDED with usage; video grants;
//! group calls; cancel; session removal.

mod common;

use axum::http::StatusCode;
use calling::{
    calls::{ids::CallId, CallState},
    media::MediaPermissions,
};
use common::app::AppWorld;
use serde_json::json;
use uuid::Uuid;

#[tokio::test]
async fn voice_call_end_to_end_with_verified_media_evidence() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.ringing_call("VOICE").await;
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "RINGING");
    // One APPLICATION route, one RTC leg, attempt 1, one invitation per endpoint.
    let routes =
        a.w.calls()
            .routes(a.w.scope(), CallId::from_uuid(call))
            .await
            .unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].route_class.as_str(), "APPLICATION");
    let legs =
        a.w.calls()
            .legs(a.w.scope(), CallId::from_uuid(call))
            .await
            .unwrap();
    assert_eq!(legs.len(), 1);
    assert_eq!(legs[0].state.as_str(), "RINGING");
    let attempts =
        a.w.calls()
            .attempts(a.w.scope(), CallId::from_uuid(call))
            .await
            .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].attempt_number, 1);
    assert!(a.media().room_exists(&AppWorld::room(call)));
    assert_ne!(AppWorld::room(call), call.to_string());

    // The callee answers; the caller's and callee's sessions get credentials.
    let invitation = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, invitation, &callee, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "ANSWERED");
    let caller_session = a.session_on(call, caller.endpoint).await.unwrap();
    let callee_session = a.session_on(call, callee.endpoint).await.unwrap();
    // A credential alone, or one joined party, never makes the call CONNECTED.
    let credential = a.media_credential(call, caller_session).await;
    assert_eq!(credential.status, StatusCode::OK, "{}", credential.text);
    assert_eq!(credential.json["capabilities"]["canPublishVideo"], false);
    assert_eq!(credential.json["capabilities"]["canPublishAudio"], true);
    for forbidden in ["apiKey", "secret", "roomSid", "resourceRef", "room"] {
        assert!(credential.json.get(forbidden).is_none(), "{forbidden}");
    }
    let caller_p = a
        .join(call, credential.json["token"].as_str().unwrap())
        .await;
    assert_eq!(a.state(call).await.0, CallState::Answered);
    // Admission applied the server-derived VOICE permissions; camera stays denied.
    assert_eq!(
        caller_p.permissions,
        MediaPermissions::default_for(calling::calls::kinds::CallKind::Voice)
    );
    a.media()
        .publish(&AppWorld::room(call), &caller_p.identity, false)
        .unwrap();
    assert!(a
        .media()
        .publish(&AppWorld::room(call), &caller_p.identity, true)
        .is_err());
    a.connect(call, callee_session).await;
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Connected);

    // Hold → provider stops publishing; resume restores (voice still no video).
    let r = a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let held = a
        .media()
        .participant(&AppWorld::room(call), &caller_p.identity)
        .unwrap();
    assert!(!held.permissions.can_publish_audio && held.permissions.can_subscribe);
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Held);
    let r = a.command(call, "resume", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let resumed = a
        .media()
        .participant(&AppWorld::room(call), &caller_p.identity)
        .unwrap();
    assert!(resumed.permissions.can_publish_audio && !resumed.permissions.can_publish_video);
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Connected);

    // End: ENDING → provider cleanup → ENDED with usage records.
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert!(!a.media().room_exists(&AppWorld::room(call)));
    let usage =
        a.w.calls()
            .usage_records(a.w.scope(), CallId::from_uuid(call))
            .await
            .unwrap();
    let types: Vec<_> = usage.iter().map(|u| u.usage_type.as_str()).collect();
    for t in [
        "CALL_CONNECTED_TIME",
        "PARTICIPANT_CONNECTED_TIME",
        "SESSION_CONNECTED_TIME",
    ] {
        assert!(types.contains(&t), "{t}: {types:?}");
    }
    assert!(usage.iter().all(|u| u.finalized_at.is_some()));
    let view = a.call_view(call).await;
    assert_eq!(view["terminalReason"], "CLEANUP_CONFIRMED");
    let _ = json!(caller_session);
}

#[tokio::test]
async fn video_calls_grant_camera_and_removed_sessions_lose_access() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, callee) = a.connected_call("VIDEO").await;
    let room = AppWorld::room(call);
    for p in a.media().participants(&room) {
        assert!(
            p.permissions.can_publish_video
                && p.permissions.can_publish_audio
                && p.permissions.can_subscribe
        );
        a.media().publish(&room, &p.identity, true).unwrap();
    }
    // Remove the callee's session: provider disconnects it, credentials are refused.
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.media().participants(&room).len(), 1);
    let r = a.media_credential(call, session).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}

#[tokio::test]
async fn cancel_during_ringing_stops_everything_and_late_accept_fails() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, callee) = a.ringing_call("VOICE").await;
    let invitation = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let mut cancel = Vec::new();
    for _ in 0..2 {
        cancel.push(
            a.post_with(
                &format!("/v1/calls/{call}/cancel"),
                json!({ "expectedVersion": version }),
                &[("idempotency-key", "cancel-1")],
            )
            .await,
        );
    }
    assert_eq!(cancel[0].status, StatusCode::OK, "{}", cancel[0].text);
    assert_eq!(cancel[1].json, cancel[0].json);
    assert_eq!(
        cancel[1].headers.get("idempotent-replayed").unwrap(),
        "true"
    );
    assert_eq!(a.state(call).await.0, CallState::Cancelled);
    let view = a.call_view(call).await;
    assert!(view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] == "CANCELLED"));
    assert!(!a.media().room_exists(&AppWorld::room(call)));
    let (_, version) = a.state(call).await;
    let r = a.accept(call, invitation, &callee, version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INVITATION_NOT_ACTIONABLE");
    let _ = Uuid::nil();
}
