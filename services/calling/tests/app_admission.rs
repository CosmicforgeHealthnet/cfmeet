//! The media connection admission authority (ADR 0007): every fresh connection and
//! reconnection of a media credential is decided from the database before the media
//! server is contacted, and the decision fails closed. Real-provider proof (refreshed
//! privileged tokens, adversarial re-entry, legitimate reconnect) is in
//! `rtc_certification.rs`.

mod common;

use axum::http::StatusCode;
use calling::{
    calls::CallState,
    media::{
        gateway::{ConnectionAdmission, ConnectionClaims, ConnectionDenied},
        provider::MediaResourceName,
    },
};
use common::app::AppWorld;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde_json::json;
use uuid::Uuid;

async fn identity_of(a: &AppWorld, session: Uuid) -> (String, i32) {
    let row = a
        .h()
        .conn()
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "SELECT media_identity, media_generation FROM call_sessions WHERE id = '{session}'"
            ),
        ))
        .await
        .unwrap()
        .unwrap();
    (
        row.try_get_by_index(0).unwrap(),
        row.try_get_by_index(1).unwrap(),
    )
}

fn claims(call: Uuid, identity: &str, generation: Option<i32>) -> ConnectionClaims {
    ConnectionClaims {
        resource_name: MediaResourceName::for_call(calling::calls::ids::CallId::from_uuid(call))
            .as_str()
            .to_string(),
        identity: identity.to_string(),
        generation,
    }
}

async fn admit(a: &AppWorld, c: &ConnectionClaims) -> Result<(), ConnectionDenied> {
    a.h().state.app.admit_connection(c).await
}

#[tokio::test]
async fn only_the_current_generation_of_an_authorized_session_is_admitted() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let s_caller = a.session_on(call, caller.endpoint).await.unwrap();
    let s_callee = a.session_on(call, callee.endpoint).await.unwrap();
    let (callee_id, gen) = identity_of(&a, s_callee).await;
    let (caller_id, caller_gen) = identity_of(&a, s_caller).await;

    // Current authorized generation: PASS (legitimate reconnect).
    assert_eq!(
        admit(&a, &claims(call, &callee_id, Some(gen))).await,
        Ok(())
    );
    assert_eq!(
        admit(&a, &claims(call, &caller_id, Some(caller_gen))).await,
        Ok(())
    );
    // Missing / older / newer generation: REJECT.
    for g in [None, Some(gen - 1), Some(gen + 1), Some(0)] {
        assert_eq!(
            admit(&a, &claims(call, &callee_id, g)).await,
            Err(ConnectionDenied::StaleGeneration),
            "{g:?}"
        );
    }
    // A newer credential supersedes generation N: N is refused, N+1 admitted.
    let r = a.media_credential(call, s_callee).await;
    assert_eq!(r.status, StatusCode::OK);
    let (_, next) = identity_of(&a, s_callee).await;
    assert_eq!(next, gen + 1);
    assert_eq!(
        admit(&a, &claims(call, &callee_id, Some(gen))).await,
        Err(ConnectionDenied::StaleGeneration)
    );
    assert_eq!(
        admit(&a, &claims(call, &callee_id, Some(next))).await,
        Ok(())
    );
    // Unknown identities and foreign / malformed resources: REJECT.
    let unknown = format!("cfmid_{}", "a".repeat(64));
    assert_eq!(
        admit(&a, &claims(call, &unknown, Some(1))).await,
        Err(ConnectionDenied::UnknownIdentity)
    );
    let mut wrong = claims(call, &callee_id, Some(next));
    wrong.resource_name = "not-a-call-room".into();
    assert_eq!(
        admit(&a, &wrong).await,
        Err(ConnectionDenied::ForeignResource)
    );
    // Session A's credential against Call B.
    let (other, _, _) = a.connected_call("VOICE").await;
    assert_eq!(
        admit(&a, &claims(other, &callee_id, Some(next))).await,
        Err(ConnectionDenied::ForeignResource)
    );

    // Session removal: A is refused at every generation; B (same Call) still admitted.
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/sessions/{s_callee}/remove"),
            json!({ "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for g in [Some(next), Some(next + 1), None] {
        assert_ne!(
            admit(&a, &claims(call, &callee_id, g)).await,
            Ok(()),
            "{g:?}"
        );
    }
    assert_eq!(
        admit(&a, &claims(call, &callee_id, Some(next))).await,
        Err(ConnectionDenied::SessionRevoked)
    );
    assert_eq!(
        admit(&a, &claims(call, &caller_id, Some(caller_gen))).await,
        Ok(())
    );

    // Terminal Call: every session is refused.
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert_eq!(a.state(call).await.0, CallState::Ended);
    assert_eq!(
        admit(&a, &claims(call, &caller_id, Some(caller_gen))).await,
        Err(ConnectionDenied::SessionRevoked)
    );
}

#[tokio::test]
async fn a_retired_participant_and_a_cancelled_call_are_never_admitted() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    // Transfer completes: the transferring participant's session is refused.
    let dave = a.principal("admission-dave").await;
    let dave_d = a.device(&dave, "admission-dave").await;
    let (call, _caller, callee) = a.connected_call("VOICE").await;
    let s_bob = a.session_on(call, callee.endpoint).await.unwrap();
    let (bob_id, bob_gen) = identity_of(&a, s_bob).await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;
    let (_, version) = a.state(call).await;
    let r = a
        .transfer(call, bob, &dave, version, "admission-transfer")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let offer = a.invitation_for(call, dave_d.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, offer, &dave_d, version).await.status,
        StatusCode::OK
    );
    let dave_session = a.session_on(call, dave_d.endpoint).await.unwrap();
    a.connect(call, dave_session).await;
    assert_eq!(a.state(call).await.0, CallState::Connected);
    assert_eq!(
        admit(&a, &claims(call, &bob_id, Some(bob_gen))).await,
        Err(ConnectionDenied::SessionRevoked)
    );
    let (dave_id, dave_gen) = identity_of(&a, dave_session).await;
    assert_eq!(
        admit(&a, &claims(call, &dave_id, Some(dave_gen))).await,
        Ok(())
    );

    // Cancelled before it was connected: the initiator's credential is refused.
    let (call, caller, _) = a.ringing_call("VIDEO").await;
    let s = a.session_on(call, caller.endpoint).await.unwrap();
    assert_eq!(a.media_credential(call, s).await.status, StatusCode::OK);
    let (id, gen) = identity_of(&a, s).await;
    assert_eq!(admit(&a, &claims(call, &id, Some(gen))).await, Ok(()));
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.command(call, "cancel", version).await.status,
        StatusCode::OK
    );
    assert_eq!(
        admit(&a, &claims(call, &id, Some(gen))).await,
        Err(ConnectionDenied::SessionRevoked)
    );
}

#[tokio::test]
async fn admission_fails_closed_when_authoritative_state_is_unreadable() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let (call, caller, _) = a.connected_call("VOICE").await;
    let s = a.session_on(call, caller.endpoint).await.unwrap();
    let (id, gen) = identity_of(&a, s).await;
    assert_eq!(admit(&a, &claims(call, &id, Some(gen))).await, Ok(()));
    let conn = a.h().conn().clone();
    let exec = |sql: &'static str| {
        let conn = conn.clone();
        async move {
            conn.execute(Statement::from_string(DbBackend::Postgres, sql.to_string()))
                .await
                .unwrap();
        }
    };
    exec("ALTER TABLE call_sessions RENAME TO call_sessions_unavailable").await;
    assert_eq!(
        admit(&a, &claims(call, &id, Some(gen))).await,
        Err(ConnectionDenied::Unavailable),
        "a database failure is a refusal, never an admission"
    );
    exec("ALTER TABLE call_sessions_unavailable RENAME TO call_sessions").await;
    assert_eq!(admit(&a, &claims(call, &id, Some(gen))).await, Ok(()));
}

#[tokio::test]
async fn without_a_configured_gateway_media_routes_do_not_exist() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    for path in [
        "/v1/calling/media/rtc",
        "/v1/calling/media/rtc/v1",
        "/v1/calling/media/rtc/validate",
        "/v1/calling/media/rtc/v1/validate",
    ] {
        let r = a
            .h()
            .request(axum::http::Method::GET, path, Some("anything"), None)
            .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.json["error"]["code"], "NOT_FOUND");
    }
}
