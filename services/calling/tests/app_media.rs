//! Calling's media boundary on the test provider (Part C §36–§54, §80–§84, §108–§111):
//! the provider contract, media-credential security, provider callbacks as evidence only,
//! reconciliation and privacy of the stored media evidence.

mod common;

use axum::http::{Method, StatusCode};
use calling::{
    calls::{ids::CallId, kinds::CallKind, CallState},
    media::{provider::ProviderParticipant, MediaPermissions},
};
use common::{
    app::AppWorld,
    fake_media::{FakeMediaProvider, FakeOp},
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

async fn text(a: &AppWorld, sql: &str) -> String {
    a.h()
        .conn()
        .query_all(Statement::from_string(DbBackend::Postgres, sql.to_string()))
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.try_get_by_index::<String>(0).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_test_provider_satisfies_the_provider_contract() {
    common::provider_contract::run(&FakeMediaProvider::default()).await;
}

#[tokio::test]
async fn media_credentials_are_issued_only_to_authorized_sessions() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let bob = a.w.recipient.clone();
    let caller = a.device(&a.w.initiator, "cred-caller").await;
    let bob_1 = a.device(&bob, "cred-bob-1").await;
    let bob_2 = a.device(&bob, "cred-bob-2").await;
    let r = a
        .create_call("VOICE", &a.w.initiator, &caller, &[&bob], "cred-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let room = AppWorld::room(call);

    // Before answering, the recipient has no session and nothing to ask for.
    let r = a.media_credential(call, Uuid::new_v4()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let bob_2_token = a.signaling_token(&bob_2).await;

    // bob_1 wins; bob_2 (the losing device) can never obtain media for bob_1's session.
    let invitation = a.invitation_for(call, bob_1.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, invitation, &bob_1, version).await.status,
        StatusCode::OK
    );
    let bob_session = a.session_on(call, bob_1.endpoint).await.unwrap();
    assert!(
        a.session_on(call, bob_2.endpoint).await.is_none(),
        "the loser got no session"
    );
    let r = a
        .as_endpoint(
            &bob_2_token,
            Method::POST,
            &format!("/v1/calling/sessions/{bob_session}/media-credentials"),
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Another Call's id with this session: not found.
    let r = a.media_credential(Uuid::new_v4(), bob_session).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // No self-granted permission.
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{bob_session}/media-credentials"),
            json!({ "canPublishVideo": true }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);

    // The credential is room-bound, identity-bound, generation-bound, voice-only.
    let r = a.media_credential(call, bob_session).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let token = r.json["token"].as_str().unwrap().to_string();
    assert_eq!(
        r.json["capabilities"],
        json!({
            "canPublishAudio": true, "canPublishVideo": false, "canPublishData": false, "canSubscribe": true
        })
    );
    let (granted_room, identity, generation) = a.media().grant_of(&token).unwrap();
    assert_eq!(granted_room, room);
    assert!(identity.starts_with("cfmid_"));
    assert!(generation >= 1);
    assert!(
        !r.text.contains(&identity),
        "the media identity is never returned"
    );
    assert!(!r.text.contains(&room), "the room name is never returned");
    let view = a.call_view(call).await.to_string();
    assert!(!view.contains(&identity) && !view.contains(&room));

    // Removal: the session's credentials stop working and re-entry is evicted.
    let joined = a.join(call, &token).await;
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{bob_session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(a.media().participant(&room, &joined.identity).is_none());
    let r = a.media_credential(call, bob_session).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let stale = a
        .media()
        .join(&token)
        .expect("the provider still accepts the old token");
    let (auth, body) = a
        .media()
        .signed_event("participant_joined", &room, Some(&stale));
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    assert!(
        a.media().participant(&room, &stale.identity).is_none(),
        "a removed session re-entering with a still-valid token is evicted"
    );
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_media_provider_events WHERE outcome = 'EVICTED'"
        )
        .await,
        1
    );

    // A terminal Call issues nothing.
    let caller_session = a.session_on(call, caller.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "cancel", version).await.status,
        StatusCode::OK
    );
    let r = a.media_credential(call, caller_session).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
}

#[tokio::test]
async fn provider_callbacks_are_evidence_only() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let room = AppWorld::room(call);
    let participants_before = a.call_view(call).await["participants"]
        .as_array()
        .unwrap()
        .len();

    // A valid callback is recorded once; a duplicate is a no-op.
    let caller_p = a.media().participants(&room)[0].clone();
    let (auth, body) =
        a.media()
            .signed_event_with_id("EV_dup", "participant_joined", &room, Some(&caller_p));
    for _ in 0..3 {
        let r = a.callback(&auth, body.clone()).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    }
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_media_provider_events WHERE provider_event_id = 'EV_dup'"
        )
        .await,
        1
    );
    // Forged, unsigned and malformed callbacks are refused and leave no trace.
    let (_, forged_body) =
        a.media()
            .signed_event_with_id("EV_forged", "participant_left", &room, Some(&caller_p));
    for auth in ["", "Bearer forged", "00ff"] {
        let r = a.callback(auth, forged_body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{auth:?}: {}", r.text);
    }
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_media_provider_events WHERE provider_event_id = 'EV_forged'"
        )
        .await,
        0
    );
    assert!(a.media().participant(&room, &caller_p.identity).is_some());

    // An unknown connection is quarantined and removed — never attached to anyone.
    let intruder = ProviderParticipant {
        identity: format!("cfmid_{}", "f".repeat(64)),
        generation: Some(1),
        permissions: MediaPermissions::NONE,
        active: true,
        published_audio: false,
        published_video: false,
        joined_at: None,
    };
    a.media().inject(&room, intruder.clone());
    let (auth, body) = a
        .media()
        .signed_event("participant_joined", &room, Some(&intruder));
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    assert!(a.media().participant(&room, &intruder.identity).is_none());
    let view = a.call_view(call).await;
    assert_eq!(
        view["participants"].as_array().unwrap().len(),
        participants_before
    );
    assert_eq!(a.state(call).await.0, CallState::Connected);

    // After the Call ended, late callbacks neither resurrect it nor its resource.
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    let version = a.state(call).await.1;
    a.media().inject_room(&room);
    let (auth, body) = a.media().signed_event("room_started", &room, None);
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    let (auth, body) = a
        .media()
        .signed_event("participant_joined", &room, Some(&caller_p));
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await, (CallState::Ended, version));
    assert!(
        !a.media().room_exists(&room),
        "a lingering resource of an ended Call is torn down"
    );
    // A callback for a resource Calling does not own is ignored.
    let (auth, body) = a
        .media()
        .signed_event("room_started", "not-a-call-room", None);
    assert_eq!(a.callback(&auth, body).await.status, StatusCode::OK);
    let _ = (caller, callee);
}

#[tokio::test]
async fn reconciliation_repairs_drift_without_creating_authority() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, _caller, callee) = a.connected_call("VOICE").await;
    let room = AppWorld::room(call);
    let voice = MediaPermissions::default_for(CallKind::Voice);

    // Permission drift (camera enabled at the provider) is reverted.
    let mut drift = voice;
    drift.can_publish_video = true;
    for p in a.media().participants(&room) {
        a.media().force_permissions(&room, &p.identity, drift);
    }
    // An unknown connection that never produced a callback is found and removed.
    let ghost = ProviderParticipant {
        identity: "ghost".into(),
        generation: None,
        permissions: MediaPermissions::NONE,
        active: true,
        published_audio: false,
        published_video: false,
        joined_at: None,
    };
    a.media().inject(&room, ghost);
    // A removal the provider did not confirm stays pending, then is enforced.
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    a.media().fail_next(
        FakeOp::Remove,
        calling::media::provider::MediaProviderError::Timeout,
        1,
    );
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{session}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        a.media().participants(&room).len(),
        3,
        "removal not yet enforced"
    );
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_sessions WHERE media_enforcement_pending"
        )
        .await,
        1
    );

    a.h().clock.advance(chrono::Duration::seconds(30));
    let report = a.h().state.app.reconcile_due(100).await.unwrap();
    let _ = report;
    let left = a.media().participants(&room);
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].permissions, voice);
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_sessions WHERE media_enforcement_pending"
        )
        .await,
        0
    );
    // Nothing the provider reported became a participant or an authorization.
    assert_eq!(
        a.call_view(call).await["participants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        scalar(
            &a,
            "SELECT count(*) FROM call_sessions WHERE media_access = 'AUTHORIZED'"
        )
        .await,
        1
    );
    assert!(
        scalar(
            &a,
            "SELECT count(*) FROM call_events WHERE event_type = 'CALL_MEDIA_RECONCILED'"
        )
        .await
            >= 1
    );

    // A resource left behind for a terminal Call is terminated by reconciliation.
    let (other, _, _) = a.ringing_call("VOICE").await;
    let (_, version) = a.state(other).await;
    a.media().fail_next(
        FakeOp::Terminate,
        calling::media::provider::MediaProviderError::Unavailable,
        1,
    );
    assert_eq!(
        a.command(other, "cancel", version).await.status,
        StatusCode::OK
    );
    assert_eq!(a.state(other).await.0, CallState::Cancelled);
    assert!(
        a.media().room_exists(&AppWorld::room(other)),
        "termination failed once"
    );
    a.h().clock.advance(chrono::Duration::seconds(60));
    a.h().state.app.reconcile_due(100).await.unwrap();
    assert!(!a.media().room_exists(&AppWorld::room(other)));
    assert_eq!(a.state(other).await.0, CallState::Cancelled);
}

#[tokio::test]
async fn media_evidence_and_events_hold_no_secrets_or_provider_identifiers() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VIDEO").await;
    let room = AppWorld::room(call);
    let session = a.session_on(call, callee.endpoint).await.unwrap();
    let media_token = a.media_credential(call, session).await.json["token"]
        .as_str()
        .unwrap()
        .to_string();
    let signaling = a.signaling_token(&caller).await;
    let (_, version) = a.state(call).await;
    a.command(call, "hold", version).await;
    let (_, version) = a.state(call).await;
    a.command(call, "resume", version).await;
    let (_, version) = a.state(call).await;
    a.command(call, "end", version).await;

    let durable = text(
        &a,
        "SELECT row_to_json(t)::text FROM call_events t \
         UNION ALL SELECT envelope::text FROM calling_outbox_events \
         UNION ALL SELECT row_to_json(t)::text FROM call_commands t \
         UNION ALL SELECT row_to_json(t)::text FROM call_invitations t \
         UNION ALL SELECT row_to_json(t)::text FROM call_transfers t \
         UNION ALL SELECT row_to_json(t)::text FROM call_endpoints t \
         UNION ALL SELECT row_to_json(t)::text FROM call_participant_media_permissions t",
    )
    .await;
    for secret in [
        media_token.as_str(),
        signaling.as_str(),
        a.secret(),
        &room,
        "EV_fake_",
    ] {
        assert!(
            !durable.contains(secret),
            "durable records contain {secret:?}"
        );
    }
    // Media identities never leave the private tables either.
    let identities = text(&a, "SELECT media_identity FROM call_sessions").await;
    for identity in identities.lines() {
        assert!(
            !durable.contains(identity),
            "durable records contain a media identity"
        );
    }
    // Provider evidence stays in the two private media tables, bounded and redacted.
    let evidence = text(
        &a,
        "SELECT row_to_json(t)::text FROM call_media_provider_events t",
    )
    .await;
    for secret in [media_token.as_str(), signaling.as_str(), a.secret()] {
        assert!(!evidence.contains(secret));
    }
    let view: Value = a.call_view(call).await;
    let text_view = view.to_string();
    for forbidden in [room.as_str(), "fake", "provider", "roomSid", "resourceRef"] {
        assert!(
            !text_view.contains(forbidden),
            "public view contains {forbidden:?}"
        );
    }
    let _ = CallId::from_uuid(call);
}
