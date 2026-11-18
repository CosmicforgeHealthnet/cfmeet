//! C14 (CAL-019): attended (consultative) transfer. The transferring participant holds the
//! original Call, consults the target on a separate consultation Call, then completes
//! (the canonical transfer of the original Call: the original party and the target end up
//! together, the transferring participant drops, the consultation Call ends) or cancels
//! (the consultation Call ends and the original Call resumes). A failed completion leaves
//! the consultation going on and the original Call held.

mod common;

use axum::http::StatusCode;
use calling::{calls::CallState, signaling::RealtimeSignalBus};
use common::app::{AppWorld, Device};
use serde_json::{json, Value};
use uuid::Uuid;

fn drain(subscription: &mut calling::signaling::bus::SignalSubscription) -> Vec<Value> {
    let mut out = Vec::new();
    while let Some(signal) = subscription.try_recv() {
        out.push(signal);
    }
    out
}

async fn scalar(a: &AppWorld, sql: &str) -> i64 {
    common::scalar_i64(a.h().conn(), sql).await
}

async fn consult(
    a: &AppWorld,
    call: Uuid,
    transferring: Uuid,
    target: Uuid,
    from: &Device,
    version: i64,
    key: &str,
) -> common::TestResponse {
    a.post_with(
        &format!("/v1/calls/{call}/consultations"),
        json!({ "transferringParticipantId": transferring, "targetPrincipalId": target,
                "initiatorEndpointId": from.endpoint, "expectedVersion": version }),
        &[("idempotency-key", key)],
    )
    .await
}

fn id(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}

/// The target acknowledges, answers and both parties join the consultation Call.
async fn answer_consultation(a: &AppWorld, consultation_call: Uuid, from: &Device, to: &Device) {
    let offer = a
        .invitation_for(consultation_call, to.endpoint)
        .await
        .expect("the target is rung on the consultation call");
    let ack = a.acknowledge_for(consultation_call, offer, to).await;
    assert_eq!(ack.status, StatusCode::OK, "{}", ack.text);
    let (_, v) = a.state(consultation_call).await;
    let r = a.accept(consultation_call, offer, to, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    for device in [from, to] {
        let session = a
            .session_on(consultation_call, device.endpoint)
            .await
            .unwrap();
        a.connect(consultation_call, session).await;
    }
    assert_eq!(a.state(consultation_call).await.0, CallState::Connected);
}

async fn consultation_state(a: &AppWorld, id: Uuid) -> String {
    common::enterprise::strings(
        a.h().conn(),
        &format!("SELECT state FROM call_consultations WHERE id = '{id}'"),
    )
    .await
    .pop()
    .unwrap_or_default()
}

#[tokio::test]
async fn consult_then_complete_joins_the_original_party_and_the_target() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("attended-carol").await;
    let carol_d = a.device(&carol, "attended-carol").await;
    let mut carol_signals = a.h().bus.subscribe(carol_d.endpoint);
    let (call, caller, callee) = a.connected_call("VOICE").await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;
    let calls_before = scalar(&a, "SELECT count(*) FROM calls").await;

    // Bob consults Carol: the original Call is held, a consultation Call rings Carol.
    let (_, version) = a.state(call).await;
    let r = consult(
        &a,
        call,
        bob,
        carol.id().as_uuid(),
        &callee,
        version,
        "consult-1",
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "HELD", "{}", r.text);
    assert_eq!(r.json["consultation"]["state"], "CONSULTING");
    assert_eq!(r.json["consultation"]["heldOriginal"], true);
    assert_eq!(
        r.json["consultation"]["targetPrincipalId"],
        carol.id().to_string()
    );
    let consultation = id(&r.json["consultation"]["consultationId"]);
    let consultation_call = id(&r.json["consultationCall"]["call"]["callId"]);
    assert_eq!(
        r.json["consultation"]["consultationCallId"],
        consultation_call.to_string()
    );
    assert_ne!(consultation_call, call);
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM calls").await,
        calls_before + 1,
        "the consultation is its own canonical Call"
    );
    // A replay returns the same consultation, never a second one.
    let replay = consult(
        &a,
        call,
        bob,
        carol.id().as_uuid(),
        &callee,
        version,
        "consult-1",
    )
    .await;
    assert_eq!(replay.status, StatusCode::CREATED, "{}", replay.text);
    assert_eq!(
        replay
            .headers
            .get("idempotent-replayed")
            .map(|v| v.to_str().unwrap()),
        Some("true")
    );
    assert_eq!(
        replay.json["consultation"]["consultationId"],
        consultation.to_string()
    );
    assert_eq!(
        scalar(&a, "SELECT count(*) FROM calls").await,
        calls_before + 1
    );
    // One consultation at a time per Call.
    let dave = a.principal("attended-dave").await;
    a.device(&dave, "attended-dave").await;
    let (_, v) = a.state(call).await;
    let r = consult(&a, call, bob, dave.id().as_uuid(), &callee, v, "consult-2").await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // Completing before the target answered is refused.
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/complete"),
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);

    // Carol answers; Bob and Carol talk; Bob completes.
    answer_consultation(&a, consultation_call, &callee, &carol_d).await;
    drain(&mut carol_signals);
    let (_, v) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/complete"),
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["consultation"]["state"], "COMPLETING", "{}", r.text);
    assert_eq!(r.json["call"]["state"], "TRANSFERRING");
    assert_eq!(r.json["transfer"]["preTransferState"], "HELD");
    let transfer = r.json["transfer"]["transferId"].clone();
    assert_eq!(r.json["consultation"]["transferId"], transfer);
    // A repeat is idempotent.
    let again = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/complete"),
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(
        scalar(
            &a,
            &format!("SELECT count(*) FROM call_transfers WHERE call_id = '{call}'")
        )
        .await,
        1
    );
    // Carol's client is told, on the consultation Call, which offer is the transfer.
    let signals = drain(&mut carol_signals);
    let notice = signals
        .iter()
        .find(|s| s["type"] == "call.consultation_transfer")
        .expect("call.consultation_transfer delivered");
    assert_eq!(notice["callId"], consultation_call.to_string());
    assert_eq!(notice["data"]["originalCallId"], call.to_string());
    assert_eq!(notice["data"]["consultationId"], consultation.to_string());
    assert_eq!(notice["data"]["transferId"], transfer);
    assert!(signals
        .iter()
        .any(|s| s["type"] == "call.incoming" && s["callId"] == call.to_string()));

    // Carol answers the transfer offer and joins the original Call.
    let offer = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    let (_, v) = a.state(call).await;
    let r = a.accept(call, offer, &carol_d, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let session = a.session_on(call, carol_d.endpoint).await.unwrap();
    let carol_media = a.connect(call, session).await;

    // Alice and Carol are on the original Call; Bob dropped; the consultation ended.
    let view = a.call_view(call).await;
    assert_eq!(view["state"], "CONNECTED", "{view:#}");
    let active: Vec<String> = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["endedAt"].is_null())
        .map(|p| p["principalId"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(active.len(), 2, "{view:#}");
    assert!(active.contains(&a.w.initiator.id().to_string()));
    assert!(active.contains(&carol.id().to_string()));
    assert_eq!(consultation_state(&a, consultation).await, "COMPLETED");
    let (b_state, _) = a.state(consultation_call).await;
    assert!(
        matches!(b_state, CallState::Ending | CallState::Ended),
        "consultation call {b_state:?}"
    );
    // The hold is over at the provider: Carol (and Alice) may publish again.
    let room = AppWorld::room(call);
    assert!(a
        .media()
        .publish(&room, &carol_media.identity, false)
        .is_ok());
    let alice_identity = a
        .media()
        .participants(&room)
        .into_iter()
        .map(|p| p.identity)
        .find(|i| *i != carol_media.identity)
        .unwrap();
    assert!(a.media().publish(&room, &alice_identity, false).is_ok());
    assert_eq!(a.media().participants(&room).len(), 2, "Bob was removed");
    // Terminal: neither cancelled nor completed again.
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let _ = caller;
}

#[tokio::test]
async fn cancel_ends_the_consultation_and_resumes_the_original_call() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("attended-cancel-carol").await;
    a.device(&carol, "attended-cancel-carol").await;
    let (call, _caller, callee) = a.connected_call("VOICE").await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;

    // Validation: key required, target already on the Call, unknown participant, stale
    // version.
    let (_, version) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations"),
            json!({ "transferringParticipantId": bob, "targetPrincipalId": carol.id(),
                    "initiatorEndpointId": callee.endpoint, "expectedVersion": version }),
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "{}", r.text);
    let r = consult(
        &a,
        call,
        bob,
        a.w.initiator.id().as_uuid(),
        &callee,
        version,
        "cancel-0",
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = consult(
        &a,
        call,
        Uuid::new_v4(),
        carol.id().as_uuid(),
        &callee,
        version,
        "cancel-00",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let r = consult(
        &a,
        call,
        bob,
        carol.id().as_uuid(),
        &callee,
        version + 7,
        "cancel-000",
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Connected, "nothing held");

    let r = consult(
        &a,
        call,
        bob,
        carol.id().as_uuid(),
        &callee,
        version,
        "cancel-1",
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let consultation = id(&r.json["consultation"]["consultationId"]);
    let consultation_call = id(&r.json["consultationCall"]["call"]["callId"]);
    assert_eq!(a.state(call).await.0, CallState::Held);

    // Unknown consultation: 404.
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{}/cancel", Uuid::new_v4()),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    // Bob changes his mind before Carol answers.
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["consultation"]["state"], "CANCELLED");
    assert_eq!(r.json["resumed"], true);
    assert_eq!(r.json["call"]["state"], "CONNECTED");
    assert!(r.json["consultation"]["endedAt"].is_string());
    assert_eq!(a.state(call).await.0, CallState::Connected);
    assert_eq!(a.state(consultation_call).await.0, CallState::Cancelled);
    // Idempotent; completing a cancelled consultation is refused.
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, v) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/complete"),
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    // A new consultation may start afterwards.
    let r = consult(&a, call, bob, carol.id().as_uuid(), &callee, v, "cancel-2").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

#[tokio::test]
async fn a_declined_completion_keeps_consulting_with_the_original_call_held() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("attended-decline-carol").await;
    let carol_d = a.device(&carol, "attended-decline-carol").await;
    let (call, _caller, callee) = a.connected_call("VOICE").await;
    let bob = a.participant_of(call, a.w.recipient.id().as_uuid()).await;

    // The original Call was already held: it stays held, and is not resumed on cancel.
    let (_, version) = a.state(call).await;
    let r = a.command(call, "hold", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, version) = a.state(call).await;
    let r = consult(
        &a,
        call,
        bob,
        carol.id().as_uuid(),
        &callee,
        version,
        "decline-1",
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["consultation"]["heldOriginal"], false);
    let consultation = id(&r.json["consultation"]["consultationId"]);
    let consultation_call = id(&r.json["consultationCall"]["call"]["callId"]);
    answer_consultation(&a, consultation_call, &callee, &carol_d).await;

    let (_, v) = a.state(call).await;
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/complete"),
            json!({ "expectedVersion": v }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    // Carol declines the transfer offer: the original Call goes back to HELD, the
    // consultation goes on.
    let offer = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    let (_, v) = a.state(call).await;
    let r = a.decline(call, offer, &carol_d, v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Held);
    assert_eq!(consultation_state(&a, consultation).await, "CONSULTING");
    assert_eq!(a.state(consultation_call).await.0, CallState::Connected);

    // Cancel: the consultation Call ends; the original Call stays held (it was held
    // before the consultation).
    let r = a
        .post(
            &format!("/v1/calls/{call}/consultations/{consultation}/cancel"),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["resumed"], false);
    assert_eq!(a.state(call).await.0, CallState::Held);
    let (b_state, _) = a.state(consultation_call).await;
    assert!(matches!(b_state, CallState::Ending | CallState::Ended));
    assert_eq!(
        scalar(
            &a,
            &format!(
                "SELECT count(*) FROM call_consultations WHERE call_id = '{call}' \
                   AND state = 'CANCELLED' AND completion_attempts = 1 AND ended_at IS NOT NULL"
            )
        )
        .await,
        1
    );
}
