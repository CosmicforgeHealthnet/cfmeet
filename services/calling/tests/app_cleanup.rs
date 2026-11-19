//! C1 — cancel and end answer as soon as their transaction committed; the media
//! provider clean-up (strip / remove every participant, delete the room) runs in the
//! background, and runs once: a provider that already confirmed the teardown is not asked
//! again when its own "room finished" callback arrives.

mod common;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::{app::AppWorld, fake_media::FakeOp};
use serde_json::json;

#[tokio::test]
async fn end_and_cancel_answer_before_the_provider_cleanup_finishes() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let (_, version) = a.state(call).await;
    // A slow provider: room deletion does not finish until released.
    a.media().hold_terminations();
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        a.h().request_unsettled(
            Method::POST,
            &format!("/v1/calls/{call}/end"),
            Some(a.secret()),
            Some(json!({ "expectedVersion": version })),
            &[],
        ),
    )
    .await
    .expect("END answered without waiting for the provider");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "ENDING", "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ending);
    a.media().release_terminations();
    a.h().state.app.settle_background().await;
    assert_eq!(a.state(call).await.0, CallState::Ended);

    // CANCEL of a ringing call: the same.
    let (call, _caller, _callee) = a.ringing_call("VOICE").await;
    let (_, version) = a.state(call).await;
    a.media().hold_terminations();
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        a.h().request_unsettled(
            Method::POST,
            &format!("/v1/calls/{call}/cancel"),
            Some(a.secret()),
            Some(json!({ "expectedVersion": version })),
            &[],
        ),
    )
    .await
    .expect("CANCEL answered without waiting for the provider");
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "CANCELLED", "{}", r.text);
    assert!(a.media().room_exists(&AppWorld::room(call)));
    a.media().release_terminations();
    a.h().state.app.settle_background().await;
    assert!(!a.media().room_exists(&AppWorld::room(call)));
}

#[tokio::test]
async fn a_confirmed_teardown_is_not_repeated_when_the_room_finished_callback_arrives() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, _callee) = a.connected_call("VIDEO").await;
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    let before = (
        a.media().calls_of(FakeOp::Terminate),
        a.media().calls_of(FakeOp::Remove),
        a.media().calls_of(FakeOp::UpdatePermissions),
    );
    // LiveKit sends room_finished after the room was deleted.
    let (auth, body) = a
        .media()
        .signed_event("room_finished", &AppWorld::room(call), None);
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    a.h().state.app.settle_background().await;
    // Reconciliation of the finished Call finds nothing to do either.
    a.h().clock.advance(chrono::Duration::seconds(30));
    a.h().state.app.reconcile_due(100).await.unwrap();
    let after = (
        a.media().calls_of(FakeOp::Terminate),
        a.media().calls_of(FakeOp::Remove),
        a.media().calls_of(FakeOp::UpdatePermissions),
    );
    assert_eq!(
        before, after,
        "no second RemoveParticipant / UpdateParticipant / DeleteRoom"
    );
    assert_eq!(a.state(call).await.0, CallState::Ended);
}
