//! Part D §29: Privacy Relay -> Calling active-call enforcement.

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use cf_service_auth::{
    ApplicationEnvironmentKind, DelegatedPlatformContext, OutboundServiceRequest, ServiceId,
    ServiceTokenSigner,
};
use common::{
    app::{AppWorld, Device},
    privacy::FakePrivacyRelay,
    scalar_i64, TestResponse,
};
use serde_json::{json, Value};
use uuid::Uuid;

const PATH: &str = "/internal/v1/privacy-relay/enforcement";

async fn world() -> Option<(AppWorld, Arc<FakePrivacyRelay>)> {
    let fake = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let w = AppWorld::with_privacy_relay(fake.clone()).await?;
    fake.allow(w.w.initiator.id().as_uuid());
    fake.allow(w.w.recipient.id().as_uuid());
    Some((w, fake))
}

fn delegated(w: &AppWorld) -> DelegatedPlatformContext {
    let s = w.w.scope();
    DelegatedPlatformContext::new(
        s.tenant_id().as_uuid(),
        s.application_id().as_uuid(),
        s.environment_id().as_uuid(),
        ApplicationEnvironmentKind::Production,
        w.w.consumer.sa(w.w.kind).as_uuid(),
    )
}

async fn send(
    w: &AppWorld,
    signer: &ServiceTokenSigner,
    delegated: Option<DelegatedPlatformContext>,
    body: Value,
) -> TestResponse {
    let mut outbound = OutboundServiceRequest::new_operation(ServiceId::calling());
    if let Some(d) = delegated {
        outbound = outbound.with_delegated_context(d);
    }
    let signed = signer.sign(&outbound).unwrap();
    let headers: Vec<(String, String)> = signed
        .headers()
        .iter()
        .map(|(n, v)| (n.as_str().to_string(), v.to_str().unwrap().to_string()))
        .collect();
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    w.h()
        .request_with(Method::POST, PATH, None, Some(body), &refs)
        .await
}

async fn enforce(w: &AppWorld, body: Value) -> TestResponse {
    let signer = w.h().peers.privacy_relay.clone();
    send(w, &signer, Some(delegated(w)), body).await
}

fn command(context: Uuid, version: i64, reason: &str) -> Value {
    json!({
        "enforcementEventId": Uuid::new_v4(),
        "contextId": context,
        "contextVersion": version,
        "relaySessionIds": [],
        "reason": reason,
        "affectedPrincipalIds": [],
        "terminateActiveCalls": true,
    })
}

async fn protected(w: &AppWorld, session: Uuid, connect: bool) -> Uuid {
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let callee: Device = w.device(&w.w.recipient, "callee-main").await;
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
    if connect {
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
    }
    call
}

#[tokio::test]
async fn only_privacy_relay_with_the_local_grant_and_a_delegated_scope_may_enforce() {
    let Some((w, fake)) = world().await else {
        return;
    };
    let body = command(fake.context_id, 2, "CONTEXT_REVOKED");
    // No token / customer credential.
    let r = w
        .h()
        .request(Method::POST, PATH, Some(w.secret()), Some(body.clone()))
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (
            StatusCode::UNAUTHORIZED,
            Some("INTERNAL_AUTHENTICATION_FAILED")
        )
    );
    let r = w
        .h()
        .request(Method::POST, PATH, None, Some(body.clone()))
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // Telephony is trusted by Calling but never granted enforcement.
    let telephony = w.h().peers.telephony.clone();
    let r = send(&w, &telephony, Some(delegated(&w)), body.clone()).await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("INTERNAL_PERMISSION_DENIED"))
    );
    // An untrusted signer.
    let untrusted = w.h().peers.untrusted.clone();
    assert_eq!(
        send(&w, &untrusted, Some(delegated(&w)), body.clone())
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    // Privacy Relay without a delegated scope.
    let relay = w.h().peers.privacy_relay.clone();
    let r = send(&w, &relay, None, body.clone()).await;
    assert_eq!(r.json["error"]["code"], "INTERNAL_PERMISSION_DENIED");
    // A command that tries to grant authority or carries extra data is refused.
    for bad in [
        {
            let mut b = body.clone();
            b["reason"] = json!("CONTEXT_RESUMED");
            b
        },
        {
            let mut b = body.clone();
            b["tenantId"] = json!(Uuid::new_v4());
            b
        },
        {
            let mut b = body.clone();
            b["phoneNumber"] = json!("+2348012345678");
            b
        },
        {
            let mut b = body.clone();
            b["relaySessionIds"] = json!([Uuid::new_v4()]);
            b
        },
    ] {
        assert_eq!(
            enforce(&w, bad).await.json["error"]["code"],
            "VALIDATION_ERROR"
        );
    }
    assert_eq!(
        scalar_i64(
            w.h().conn(),
            "SELECT count(*) FROM call_privacy_enforcements"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn revocation_cancels_ringing_and_ends_connected_protected_calls_once() {
    let Some((w, fake)) = world().await else {
        return;
    };
    let session = Uuid::new_v4();
    let connected = protected(&w, session, true).await;
    let ringing = protected(&w, session, false).await;
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let ordinary = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "ordinary-1",
        )
        .await;
    let ordinary: Uuid = ordinary.json["call"]["callId"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(w.state(ringing).await.0, CallState::Ringing);

    let body = command(fake.context_id, 2, "CONTEXT_REVOKED");
    let r = enforce(&w, body.clone()).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json,
        json!({ "status": "APPLIED", "enforcementEventId": body["enforcementEventId"], "matchedCalls": 2, "terminatedCalls": 2 })
    );
    assert_eq!(w.state(ringing).await.0, CallState::Cancelled);
    // END -> ENDING -> provider cleanup -> ENDED through the ordinary call command.
    assert_eq!(w.state(connected).await.0, CallState::Ended);
    // Ordinary app calls are never touched.
    assert_eq!(w.state(ordinary).await.0, CallState::Ringing);

    // Replays: the same answer, no new terminal events, no second provider cleanup.
    let events = scalar_i64(w.h().conn(), "SELECT count(*) FROM call_events").await;
    let provider_ops = w.media().log().len();
    for _ in 0..3 {
        let again = enforce(&w, body.clone()).await;
        assert_eq!(again.json, r.json);
    }
    // Concurrent duplicates too.
    let replays = futures_util::future::join_all((0..4).map(|_| enforce(&w, body.clone()))).await;
    assert!(replays.iter().all(|x| x.json == r.json));
    assert_eq!(
        scalar_i64(w.h().conn(), "SELECT count(*) FROM call_events").await,
        events
    );
    assert_eq!(w.media().log().len(), provider_ops);
    assert_eq!(
        scalar_i64(
            w.h().conn(),
            "SELECT count(*) FROM call_privacy_enforcements WHERE status = 'APPLIED'"
        )
        .await,
        1
    );
}

#[tokio::test]
async fn stale_versions_other_principals_sessions_and_scopes_are_not_affected() {
    let Some((w, fake)) = world().await else {
        return;
    };
    let session = Uuid::new_v4();
    // The call is authorized at context version 5 / session version 3.
    fake.set_versions(5, 3);
    let call = protected(&w, session, true).await;
    // A lower-version (stale) context enforcement never touches newer authority.
    let r = enforce(&w, command(fake.context_id, 4, "PARTICIPANT_REMOVED")).await;
    assert_eq!(r.json["matchedCalls"], 0);
    // Removal of a principal who is not in the call.
    let mut other = command(fake.context_id, 6, "PARTICIPANT_REMOVED");
    other["affectedPrincipalIds"] = json!([Uuid::new_v4()]);
    assert_eq!(enforce(&w, other).await.json["matchedCalls"], 0);
    // Another session of the same context.
    let mut session_level = command(fake.context_id, 5, "SESSION_REVOKED");
    session_level["relaySessionIds"] = json!([Uuid::new_v4()]);
    session_level["relaySessionVersion"] = json!(4);
    assert_eq!(enforce(&w, session_level).await.json["matchedCalls"], 0);
    // A stale session-level command for this session.
    let mut stale_session = command(fake.context_id, 5, "SESSION_SUSPENDED");
    stale_session["relaySessionIds"] = json!([session]);
    stale_session["relaySessionVersion"] = json!(3);
    assert_eq!(enforce(&w, stale_session).await.json["matchedCalls"], 0);
    // terminateActiveCalls = false: recorded, nothing terminated.
    let mut soft = command(fake.context_id, 6, "CONTEXT_SUSPENDED");
    soft["terminateActiveCalls"] = json!(false);
    let r = enforce(&w, soft).await;
    assert_eq!(
        (
            r.json["matchedCalls"].as_i64(),
            r.json["terminatedCalls"].as_i64()
        ),
        (Some(1), Some(0))
    );
    assert_eq!(w.state(call).await.0, CallState::Connected);
    // Another tenant's scope sees nothing of this call.
    let foreign = DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Production,
        Uuid::new_v4(),
    );
    let relay = w.h().peers.privacy_relay.clone();
    let r = send(
        &w,
        &relay,
        Some(foreign),
        command(fake.context_id, 9, "CONTEXT_REVOKED"),
    )
    .await;
    assert_eq!(r.json["matchedCalls"], 0);
    assert_eq!(w.state(call).await.0, CallState::Connected);
    // An enforcement id replayed into another scope is not visible there.
    let body = command(fake.context_id, 9, "CONTEXT_EXPIRED");
    let foreign = DelegatedPlatformContext::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        ApplicationEnvironmentKind::Production,
        Uuid::new_v4(),
    );
    send(&w, &relay, Some(foreign), body.clone()).await;
    assert_eq!(enforce(&w, body).await.status, StatusCode::NOT_FOUND);
    // The removed participant's call ends on a current-version removal.
    let mut removal = command(fake.context_id, 6, "PARTICIPANT_REMOVED");
    removal["affectedPrincipalIds"] = json!([w.w.recipient.id().as_uuid()]);
    let r = enforce(&w, removal).await;
    assert_eq!(r.json["terminatedCalls"], 1);
    assert_eq!(w.state(call).await.0, CallState::Ended);
}
