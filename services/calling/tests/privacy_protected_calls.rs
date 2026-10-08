//! Part D §29–§30 in Calling: protected calls are authorized by Privacy Relay
//! (PRIVACY_RELAY_V1), fail CLOSED without an answer, never fall back to APP_DIRECT_V1,
//! and a transfer cannot introduce a participant the context does not authorize.
//! Ordinary application calling is independent of Privacy Relay.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use calling::calls::CallState;
use common::{
    app::{AppWorld, Device},
    privacy::{FakePrivacyRelay, Mode},
    scalar_i64,
};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::Value;
use uuid::Uuid;

async fn world(allowed: &[&str]) -> Option<(AppWorld, Arc<FakePrivacyRelay>)> {
    let fake = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let w = AppWorld::with_privacy_relay(fake.clone()).await?;
    for reference in allowed {
        let p = match *reference {
            "initiator" => w.w.initiator.clone(),
            "recipient" => w.w.recipient.clone(),
            other => w.principal(other).await,
        };
        fake.allow(p.id().as_uuid());
    }
    Some((w, fake))
}

async fn text(w: &AppWorld, sql: &str) -> String {
    w.h()
        .conn()
        .query_one(Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<String>(0)
        .unwrap()
}

async fn devices(w: &AppWorld) -> (Device, Device) {
    (
        w.device(&w.w.initiator, "caller-main").await,
        w.device(&w.w.recipient, "callee-main").await,
    )
}

#[tokio::test]
async fn a_protected_call_is_authorized_by_privacy_relay_and_rings_on_the_app_path() {
    let Some((w, fake)) = world(&["initiator", "recipient"]).await else {
        return;
    };
    let (caller, _callee) = devices(&w).await;
    let session = Uuid::new_v4();
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            session,
            "p-1",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "RINGING");
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Not APP_DIRECT_V1: the Privacy Relay decision is the authorization evidence.
    assert_eq!(
        text(
            &w,
            &format!("SELECT policy_version FROM call_authorizations WHERE call_id = '{call}'")
        )
        .await,
        "PRIVACY_RELAY_V1"
    );
    let snapshot = text(
        &w,
        &format!("SELECT policy_snapshot::text FROM call_authorizations WHERE call_id = '{call}'"),
    )
    .await;
    let snapshot: Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(snapshot["privacy_relay_session_id"], session.to_string());
    assert_eq!(
        snapshot["communication_context_id"],
        fake.context_id.to_string()
    );
    assert!(snapshot["privacy_decision_id"].is_string());
    // The opaque cross-service references are on the Call; the decision is recorded.
    assert_eq!(
        text(
            &w,
            &format!("SELECT communication_context_ref::text FROM calls WHERE id = '{call}'")
        )
        .await,
        fake.context_id.to_string()
    );
    assert_eq!(
        text(
            &w,
            &format!("SELECT privacy_relay_ref::text FROM calls WHERE id = '{call}'")
        )
        .await,
        session.to_string()
    );
    assert_eq!(scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM call_privacy_authorizations WHERE call_id = '{call}' AND purpose = 'CALL_CREATE'")).await, 1);
    // Calling asked with the delegated scope and the initiating principal — CF Meet ids
    // only, never a credential.
    let asked = fake.requests();
    assert_eq!(asked.len(), 1);
    let (delegated, correlation, request) = &asked[0];
    assert_eq!(delegated.tenant_id(), w.w.consumer.tenant.id.as_uuid());
    assert_eq!(
        delegated.initiating_principal().unwrap().principal_id,
        w.w.initiator.id().as_uuid()
    );
    assert_eq!(request.source_principal_id, w.w.initiator.id().as_uuid());
    assert_eq!(
        request.target_principal_ids,
        vec![w.w.recipient.id().as_uuid()]
    );
    assert!(correlation.as_str().starts_with("corr_"));
    // A replay does not ask again.
    let again = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            session,
            "p-1",
        )
        .await;
    assert_eq!(again.json["call"]["callId"], r.json["call"]["callId"]);
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn denials_and_outages_fail_closed_without_any_effect() {
    let Some((w, fake)) = world(&["initiator"]).await else {
        return;
    };
    let (caller, _callee) = devices(&w).await;
    let calls_before = scalar_i64(w.h().conn(), "SELECT count(*) FROM calls").await;
    // The recipient is not authorized by the context.
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "d-1",
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("CALL_NOT_AUTHORIZED")),
        "{}",
        r.text
    );
    for (mode, status, code) in [
        (
            Mode::Deny("CONTEXT_REVOKED"),
            StatusCode::FORBIDDEN,
            "CALL_NOT_AUTHORIZED",
        ),
        (
            Mode::Deny("FEATURE_DISABLED"),
            StatusCode::FORBIDDEN,
            "CALL_NOT_AUTHORIZED",
        ),
        (
            Mode::Unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
            "PRIVACY_AUTHORIZATION_UNAVAILABLE",
        ),
        (Mode::NotFound, StatusCode::NOT_FOUND, "NOT_FOUND"),
    ] {
        fake.set_mode(mode);
        let r = w
            .create_protected_call(
                "VOICE",
                &w.w.initiator,
                &caller,
                &[&w.w.recipient],
                Uuid::new_v4(),
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(
            (r.status, r.json["error"]["code"].as_str()),
            (status, Some(code)),
            "{mode:?}: {}",
            r.text
        );
    }
    // Nothing rang, nothing was created, no media resource was ensured.
    assert_eq!(
        scalar_i64(w.h().conn(), "SELECT count(*) FROM calls").await,
        calls_before
    );
    assert_eq!(
        scalar_i64(w.h().conn(), "SELECT count(*) FROM call_invitations").await,
        0
    );
    assert_eq!(
        scalar_i64(w.h().conn(), "SELECT count(*) FROM call_media_sessions").await,
        0
    );
    // VIDEO is not a Privacy Relay channel today.
    fake.set_mode(Mode::Policy);
    fake.allow(w.w.recipient.id().as_uuid());
    let r = w
        .create_protected_call(
            "VIDEO",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "v-1",
        )
        .await;
    assert_eq!(r.json["error"]["code"], "CAPABILITY_UNAVAILABLE");

    // Ordinary application calling never depends on Privacy Relay, even while it is down.
    fake.set_mode(Mode::Unavailable);
    let ordinary = w
        .create_call("VOICE", &w.w.initiator, &caller, &[&w.w.recipient], "o-1")
        .await;
    assert_eq!(ordinary.status, StatusCode::CREATED, "{}", ordinary.text);
    let id = ordinary.json["call"]["callId"].as_str().unwrap();
    assert_eq!(
        text(
            &w,
            &format!("SELECT policy_version FROM call_authorizations WHERE call_id = '{id}'")
        )
        .await,
        "APP_DIRECT_V1"
    );
    assert_eq!(scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM calls WHERE id = '{id}' AND privacy_relay_ref IS NULL AND communication_context_ref IS NULL")).await, 1);
}

#[tokio::test]
async fn without_a_configured_privacy_relay_protected_calls_fail_closed() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (caller, _callee) = devices(&w).await;
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "u-1",
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Some("PRIVACY_AUTHORIZATION_UNAVAILABLE")
        )
    );
    assert!(!r.text.contains("cfmk_"));
    let ordinary = w
        .create_call("VOICE", &w.w.initiator, &caller, &[&w.w.recipient], "u-2")
        .await;
    assert_eq!(ordinary.status, StatusCode::CREATED);
}

async fn connected_protected(w: &AppWorld, session: Uuid) -> (Uuid, Device, Device) {
    let (caller, callee) = devices(w).await;
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            session,
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let invitation = w.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = w.state(call).await;
    assert_eq!(
        w.accept(call, invitation, &callee, version).await.status,
        StatusCode::OK
    );
    let s1 = w.session_on(call, caller.endpoint).await.unwrap();
    let s2 = w.session_on(call, callee.endpoint).await.unwrap();
    w.connect(call, s1).await;
    w.connect(call, s2).await;
    assert_eq!(w.state(call).await.0, CallState::Connected);
    (call, caller, callee)
}

#[tokio::test]
async fn transfer_cannot_bypass_the_context_participants() {
    let Some((w, fake)) = world(&["initiator", "recipient"]).await else {
        return;
    };
    let session = Uuid::new_v4();
    let (call, _caller, _callee) = connected_protected(&w, session).await;
    let outsider = w.principal("outsider-1").await;
    w.device(&outsider, "outsider-main").await;
    let transferring = w.participant_of(call, w.w.recipient.id().as_uuid()).await;
    let (_, version) = w.state(call).await;
    let r = w
        .transfer(call, transferring, &outsider, version, "t-1")
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("CALL_NOT_AUTHORIZED")),
        "{}",
        r.text
    );
    assert_eq!(
        w.state(call).await,
        (CallState::Connected, version),
        "nothing changed"
    );
    let last = fake.requests().pop().unwrap().2;
    assert_eq!(last.relay_session_id, session);
    assert_eq!(last.source_principal_id, w.w.recipient.id().as_uuid());
    assert_eq!(last.target_principal_ids, vec![outsider.id().as_uuid()]);
    // Privacy Relay unavailable: the transfer fails closed too.
    fake.set_mode(Mode::Unavailable);
    let r = w
        .transfer(call, transferring, &outsider, version, "t-2")
        .await;
    assert_eq!(r.json["error"]["code"], "PRIVACY_AUTHORIZATION_UNAVAILABLE");
    // Once the consumer adds the principal to the context, the transfer may proceed.
    fake.set_mode(Mode::Policy);
    fake.allow(outsider.id().as_uuid());
    let r = w
        .transfer(call, transferring, &outsider, version, "t-3")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM call_privacy_authorizations WHERE call_id = '{call}' AND purpose = 'CALL_TRANSFER' AND target_principal_id = '{}'", outsider.id())).await, 1);
}

#[tokio::test]
async fn a_decision_superseded_by_received_enforcement_is_refused() {
    let Some((w, fake)) = world(&["initiator", "recipient"]).await else {
        return;
    };
    let (caller, _callee) = devices(&w).await;
    // Calling already received a revocation at context version 3; a decision made at
    // version 2 (in flight during the revocation) must not create a call.
    let scope = w.w.scope();
    w.h().conn().execute(Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "INSERT INTO call_privacy_enforcements (enforcement_event_id, tenant_id, application_id, environment_id, \
           communication_context_ref, context_version, relay_session_refs, affected_principal_refs, reason, \
           terminate_active_calls, requested_by_service, request_id, correlation_id, status, matched_calls, terminated_calls, received_at, completed_at) \
         VALUES (gen_random_uuid(), $1, $2, $3, $4, 3, '{}', '{}', 'CONTEXT_REVOKED', true, 'cfmeet.privacy-relay', \
           'req_00000000000000000000000000000000', 'corr_00000000000000000000000000000000', 'APPLIED', 0, 0, now(), now())",
        vec![scope.tenant_id().as_uuid().into(), scope.application_id().as_uuid().into(), scope.environment_id().as_uuid().into(), fake.context_id.into()],
    )).await.unwrap();
    fake.set_versions(2, 1);
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "s-1",
        )
        .await;
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED", "{}", r.text);
    // A decision at a later version is not superseded.
    fake.set_versions(4, 1);
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "s-2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}
