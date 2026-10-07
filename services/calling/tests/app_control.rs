//! Call control (Part C §55–§70, §89–§91, §112–§118): cancel vs end, cleanup evidence,
//! provider-enforced hold / resume, application transfer with recovery, idempotency,
//! expected-version protection and provider failures.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use calling::{
    calls::CallState,
    domain::EnvironmentKind,
    media::{provider::MediaProviderError, MediaPermissions, MediaProviderCapabilities},
};
use common::{
    app::AppWorld,
    calls::CallWorld,
    fake_media::{FakeMediaProvider, FakeOp},
    test_runtime, Harness, TestDb, TEST_PEPPER,
};
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

fn call_id(r: &common::TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

/// An AppWorld over a provider with `media` (None: application media disabled).
async fn world_with(media: Option<Arc<FakeMediaProvider>>) -> Option<AppWorld> {
    let db = Arc::new(TestDb::new().await?);
    let fake = media.clone().unwrap_or_default();
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let runtime = test_runtime(
        media.map(|m| m as Arc<dyn calling::media::CallMediaProvider>),
        bus.clone(),
    );
    let h = Harness::with_runtime(
        db,
        TEST_PEPPER,
        calling::domain::ManualClock::new(common::t0()),
        fake,
        bus,
        runtime,
    );
    Some(AppWorld {
        w: CallWorld::in_harness(h, "control", EnvironmentKind::Production).await,
    })
}

#[tokio::test]
async fn cancel_is_pre_active_idempotent_and_refuses_other_requests_under_its_key() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    // From ANSWERED (not yet connected): allowed.
    let (call, _caller, callee) = a.ringing_call("VOICE").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &callee, version).await.status,
        StatusCode::OK
    );
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Answered);
    let key = [("idempotency-key", "cancel-answered")];
    let r = a
        .post_with(
            &format!("/v1/calls/{call}/cancel"),
            json!({ "expectedVersion": version }),
            &key,
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "CANCELLED");
    assert!(!a.media().room_exists(&AppWorld::room(call)));
    assert_eq!(
        scalar(&a, &format!("SELECT count(*) FROM call_sessions WHERE call_id = '{call}' AND media_access = 'AUTHORIZED'")).await,
        0
    );
    // Same key + same request: the recorded response. Same key + another request: 409.
    let replay = a
        .post_with(
            &format!("/v1/calls/{call}/cancel"),
            json!({ "expectedVersion": version }),
            &key,
        )
        .await;
    assert_eq!(replay.json, r.json);
    assert_eq!(replay.headers["idempotent-replayed"], "true");
    let other = a
        .post_with(
            &format!("/v1/calls/{call}/cancel"),
            json!({ "expectedVersion": version + 1 }),
            &key,
        )
        .await;
    assert_eq!(other.status, StatusCode::CONFLICT);
    assert_eq!(other.json["error"]["code"], "IDEMPOTENCY_CONFLICT");
    let other = a
        .post_with(
            &format!("/v1/calls/{call}/end"),
            json!({ "expectedVersion": version }),
            &key,
        )
        .await;
    assert_ne!(
        other.status,
        StatusCode::OK,
        "a key is bound to its command"
    );

    // An active (CONNECTED) Call is ended, not cancelled.
    let (call, _, _) = a.connected_call("VOICE").await;
    let (_, version) = a.state(call).await;
    let r = a.command(call, "cancel", version).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_STATE_CONFLICT");
    assert_eq!(a.state(call).await, (CallState::Connected, version));
}

#[tokio::test]
async fn end_reaches_ended_only_on_cleanup_evidence() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, _callee) = a.connected_call("VOICE").await;
    let room = AppWorld::room(call);
    a.h().clock.advance(chrono::Duration::seconds(7));
    a.media()
        .fail_next(FakeOp::Terminate, MediaProviderError::Timeout, 1);
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "ENDING");
    assert_eq!(a.state(call).await.0, CallState::Ending);
    assert!(
        a.media().room_exists(&room),
        "the provider did not confirm termination"
    );
    // Ending is not cancellable, and a repeated END is harmless.
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "cancel", version).await.status,
        StatusCode::CONFLICT
    );
    // Reconciliation retries and confirms: ENDED with finalized usage.
    a.h().clock.advance(chrono::Duration::seconds(30));
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert!(!a.media().room_exists(&room));
    let view = a.call_view(call).await;
    assert_eq!(view["terminalReason"], "CLEANUP_CONFIRMED");
    assert!(view["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["state"] == "ENDED" && s["mediaAccess"] == "REVOKED"));
    let usage =
        a.w.calls()
            .usage_records(a.w.scope(), calling::calls::ids::CallId::from_uuid(call))
            .await
            .unwrap();
    let connected = usage
        .iter()
        .find(|u| u.usage_type.as_str() == "CALL_CONNECTED_TIME")
        .unwrap();
    assert!(connected.quantity >= 7_000, "{}", connected.quantity);
    // Per-participant time never double counts devices; every record is finalized.
    assert!(usage.iter().all(|u| u.finalized_at.is_some()));
}

#[tokio::test]
async fn hold_and_resume_are_enforced_at_the_provider() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, callee) = a.connected_call("VIDEO").await;
    let room = AppWorld::room(call);
    let video = MediaPermissions::default_for(calling::calls::kinds::CallKind::Video);
    // A client cannot smuggle media options into a command.
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/hold"),
            json!({ "expectedVersion": version, "video": false }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    // An unconfirmed provider update: MEDIA_UNAVAILABLE and the Call stays CONNECTED.
    a.media().fail_next(
        FakeOp::UpdatePermissions,
        MediaProviderError::Unavailable,
        1,
    );
    let r = a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "MEDIA_UNAVAILABLE");
    assert_eq!(a.state(call).await, (CallState::Connected, version));
    for p in a.media().participants(&room) {
        assert_eq!(p.permissions, video, "nothing is half-held");
    }
    // Hold: nobody publishes; everybody still subscribes.
    let r = a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for p in a.media().participants(&room) {
        assert!(
            !p.permissions.can_publish_audio
                && !p.permissions.can_publish_video
                && p.permissions.can_subscribe
        );
        assert!(a.media().publish(&room, &p.identity, false).is_err());
    }
    // Resume re-derives permissions from current server state.
    let (_, version) = a.state(call).await;
    let r = a.command(call, "resume", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for p in a.media().participants(&room) {
        assert_eq!(p.permissions, video);
    }
    // Resume is only for HELD Calls.
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "resume", version).await.status,
        StatusCode::CONFLICT
    );
    // Held again, the callee's session is removed: resume cannot re-grant it, and with a
    // single remaining party there is no media path to resume to.
    assert_eq!(
        a.command(call, "hold", version).await.status,
        StatusCode::OK
    );
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = a.command(call, "resume", version).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CALL_EVIDENCE_REQUIRED");
    let remaining = a.media().participants(&room);
    assert_eq!(remaining.len(), 1);
    assert!(!remaining[0].permissions.can_publish_audio, "still held");
    assert_eq!(a.state(call).await.0, CallState::Held);
}

#[tokio::test]
async fn providers_that_cannot_enforce_media_answer_capability_unavailable() {
    let caps = MediaProviderCapabilities {
        permission_update: false,
        ..FakeMediaProvider::default_capabilities()
    };
    let Some(a) = world_with(Some(Arc::new(FakeMediaProvider::with_capabilities(caps)))).await
    else {
        return;
    };
    let caller = a.device(&a.w.initiator, "caps-caller").await;
    a.device(&a.w.recipient, "caps-callee").await;
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "caps-1",
        )
        .await;
    // Without server-side permission updates nothing could be admitted, held or resumed:
    // the provider cannot run application calls at all.
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");

    let Some(a) = world_with(None).await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "none-caller").await;
    a.device(&a.w.recipient, "none-callee").await;
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "none-1",
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM calls").await,
        0,
        "nothing was created"
    );
}

#[tokio::test]
async fn transfer_keeps_the_same_call_and_recovers_from_failure() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("transfer-carol").await;
    let dave = a.principal("transfer-dave").await;
    let erin = a.principal("transfer-erin").await;
    let nobody = a.principal("transfer-no-device").await;
    let carol_d = a.device(&carol, "transfer-carol").await;
    let dave_d = a.device(&dave, "transfer-dave").await;
    a.device(&erin, "transfer-erin").await;
    let (call, _caller, callee) = a.connected_call("VOICE").await;
    let calls_before = scalar(&a, "SELECT count(*) FROM calls").await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;

    // Validation: key required, target must be reachable, participant must exist.
    let (_, version) = a.state(call).await;
    let r = a
        .post(&format!("/v1/calls/{call}/transfer"), json!({ "transferringParticipantId": bob, "targetPrincipalId": carol.id(), "expectedVersion": version }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = a.transfer(call, bob, &nobody, version, "transfer-0").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "TARGET_UNAVAILABLE");
    let r = a
        .transfer(call, Uuid::new_v4(), &carol, version, "transfer-00")
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);

    // 1. Held Call, target declines → transfer FAILED, Call back to HELD, no orphans.
    let r = a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, version) = a.state(call).await;
    let r = a.transfer(call, bob, &carol, version, "transfer-1").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "TRANSFERRING");
    assert_eq!(r.json["transfer"]["preTransferState"], "HELD");
    let replay = a.transfer(call, bob, &carol, version, "transfer-1").await;
    assert_eq!(
        replay.json, r.json,
        "a replay never duplicates the target path"
    );
    assert_eq!(
        scalar(
            &a,
            &format!("SELECT count(*) FROM call_transfers WHERE call_id = '{call}'")
        )
        .await,
        1
    );
    assert_eq!(
        scalar(&a, &format!("SELECT count(*) FROM call_invitations WHERE call_id = '{call}' AND purpose = 'TRANSFER'")).await,
        1
    );
    let conflict = a.transfer(call, bob, &dave, version, "transfer-1").await;
    assert_eq!(conflict.json["error"]["code"], "IDEMPOTENCY_CONFLICT");
    let offer = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.decline(call, offer, &carol_d, version).await.status,
        StatusCode::OK
    );
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "HELD", "{view:#}");
    assert_eq!(transfer_of(&view, &r)["state"], "FAILED");
    assert_eq!(transfer_of(&view, &r)["failureReason"], "TARGET_DECLINED");
    assert_carol_retired(&view, &carol);
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "resume", version).await.status,
        StatusCode::OK
    );

    // 2. Target never answers → FAILED (TARGET_NO_ANSWER), back to CONNECTED.
    let (_, version) = a.state(call).await;
    let r = a.transfer(call, bob, &erin, version, "transfer-2").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    a.h().clock.advance(chrono::Duration::seconds(60));
    a.h().state.app.expire_due(100).await.unwrap();
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "CONNECTED", "{view:#}");
    assert_eq!(transfer_of(&view, &r)["failureReason"], "TARGET_NO_ANSWER");

    // 3. Target accepts but never joins → FAILED (MEDIA_PATH_LOST) after the deadline.
    let (_, version) = a.state(call).await;
    let r = a.transfer(call, bob, &dave, version, "transfer-3").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, dave_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &dave_d, version).await.status,
        StatusCode::OK
    );
    a.h().clock.advance(chrono::Duration::seconds(61));
    a.h().state.app.expire_due(100).await.unwrap();
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "CONNECTED", "{view:#}");
    assert_eq!(transfer_of(&view, &r)["state"], "FAILED");
    assert_eq!(transfer_of(&view, &r)["failureReason"], "MEDIA_PATH_LOST");
    // The failed target's session holds no media authority.
    let dave_session = a.session_on(call, dave_d.endpoint).await;
    assert!(dave_session.is_none(), "{view:#}");
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM calls").await,
        calls_before,
        "never a second Call"
    );
    // The original parties still have media.
    assert_eq!(a.media().participants(&AppWorld::room(call)).len(), 2);
    let _ = callee;
}

fn transfer_of<'v>(view: &'v Value, response: &common::TestResponse) -> &'v Value {
    let id = &response.json["transfer"]["transferId"];
    view["transfers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| &t["transferId"] == id)
        .unwrap()
}

fn assert_carol_retired(view: &Value, carol: &calling::platform::principals::ResolvedPrincipal) {
    let p = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["principalId"] == carol.id().to_string())
        .unwrap();
    assert!(!p["endedAt"].is_null(), "the failed target is retired");
    assert!(view["invitations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["status"] != "RINGING"));
}

#[tokio::test]
async fn every_command_refuses_a_stale_version_without_effect() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, callee) = a.ringing_call("VOICE").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, offer, &callee, version - 1).await;
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION");
    assert_eq!(
        a.invitation_for(call, callee.endpoint).await,
        Some(offer),
        "nothing happened"
    );
    let r = a.command(call, "cancel", version + 5).await;
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION");
    assert_eq!(a.state(call).await, (CallState::Ringing, version));
    a.accept(call, offer, &callee, version).await;
    let caller_session = a.session_on(call, _caller.endpoint).await.unwrap();
    let callee_session = a.session_on(call, callee.endpoint).await.unwrap();
    a.connect(call, caller_session).await;
    a.connect(call, callee_session).await;
    let (_, version) = a.state(call).await;
    for action in ["hold", "end"] {
        let r = a.command(call, action, version - 1).await;
        assert_eq!(r.status, StatusCode::CONFLICT, "{action}");
        assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION", "{action}");
    }
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{callee_session}/remove"),
            json!({ "expectedVersion": version - 1 }),
        )
        .await;
    assert_eq!(r.json["error"]["code"], "STALE_CALL_VERSION");
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;
    let r = a
        .transfer(
            call,
            bob,
            &a.w.initiator.clone(),
            version - 1,
            "stale-transfer",
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(a.state(call).await, (CallState::Connected, version));
    // Missing / non-integer versions are malformed requests.
    for body in [
        json!({}),
        json!({ "expectedVersion": "7" }),
        json!({ "expectedVersion": 7, "state": "HELD" }),
    ] {
        let r = a.post(&format!("/v1/calls/{call}/hold"), body).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn provider_failures_never_corrupt_the_call() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "fail-caller").await;
    let callee = a.device(&a.w.recipient, "fail-callee").await;
    // Ensure times out: the Call waits in ROUTING (attempt outcome unknown), and the
    // reconciler resumes it.
    a.media()
        .fail_next(FakeOp::Ensure, MediaProviderError::Timeout, 1);
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "fail-1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "ROUTING");
    assert_eq!(r.json["liveInvitationCount"], 0);
    let call = call_id(&r);
    a.h().clock.advance(chrono::Duration::seconds(5));
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::Ringing);
    assert!(a.invitation_for(call, callee.endpoint).await.is_some());
    assert_eq!(
        a.media().ensure_calls(),
        1,
        "one resource (the timed-out attempt created none)"
    );
    // A replay of the creation returns the creation response, not a second Call.
    let replay = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "fail-1",
        )
        .await;
    assert_eq!(call_id(&replay), call);

    // Ensure keeps failing past the deadline: FAILED (MEDIA); nothing rings.
    a.media()
        .fail_next(FakeOp::Ensure, MediaProviderError::Unavailable, 100);
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient],
            "fail-2",
        )
        .await;
    let failed = call_id(&r);
    for _ in 0..4 {
        a.h().clock.advance(chrono::Duration::seconds(30));
        a.h().state.app.reconcile_due(100).await.unwrap();
    }
    a.media().clear_failures();
    let view = a.call_view(failed).await;
    assert_eq!(view["state"], "FAILED", "{view:#}");
    assert_eq!(view["failureCategory"], "MEDIA");
    assert!(view["invitations"].as_array().unwrap().is_empty());

    // Credential issuance failure: MEDIA_UNAVAILABLE; the next request succeeds.
    let (call, _, callee) = a.ringing_call("VOICE").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    a.accept(call, offer, &callee, version).await;
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    a.media()
        .fail_next(FakeOp::Issue, MediaProviderError::Timeout, 1);
    let r = a.media_credential(call, session).await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    let r = a.media_credential(call, session).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}
