//! CF1 — "on another call" (WhatsApp): a call to someone who is already in a live call
//! does not ring them; it ends BUSY at once for the caller, and the busy person finds a
//! missed call in their history. A group call rings only the people who are free (the
//! busy ones get a missed call), and ends BUSY only when everyone called is busy.

mod common;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::app::{AppWorld, Device};
use serde_json::Value;
use uuid::Uuid;

async fn offers(a: &AppWorld, device: &Device) -> Vec<Value> {
    let token = a.signaling_token(device).await;
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["data"].as_array().unwrap().clone()
}

/// Whether `device` has an offer of `call` it could ring for.
async fn rings_for(a: &AppWorld, device: &Device, call: Uuid) -> bool {
    offers(a, device)
        .await
        .iter()
        .any(|o| o["callId"] == call.to_string() && o["actionable"] == true)
}

async fn outcome(a: &AppWorld, principal: Uuid, call: Uuid) -> (String, bool) {
    let r = a.get(&format!("/v1/calls?principalId={principal}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let e = r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .unwrap_or_else(|| panic!("{call} missing from the history: {}", r.text))
        .clone();
    (
        e["participation"]["outcome"].as_str().unwrap().to_string(),
        e["participation"]["missed"].as_bool().unwrap(),
    )
}

#[tokio::test]
async fn a_call_to_someone_on_another_call_ends_busy_without_ringing_them() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let a = a.busy_on_another_call();
    // The recipient is on a call with the initiator.
    let (live, _caller, callee) = a.connected_call("VOICE").await;
    let busy_id = a.w.recipient.id().as_uuid();
    // A third person calls the recipient.
    let third = a.principal("busy-third").await;
    let third_device = a.device(&third, "busy-third-main").await;
    let r = a
        .create_call(
            "VIDEO",
            &third,
            &third_device,
            &[&a.w.recipient],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "BUSY", "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    // Nothing rings on the busy person's device (no live offer to recover or answer).
    assert!(!rings_for(&a, &callee, call).await);
    let view = a.call_view(call).await;
    assert!(
        view["invitations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["status"] == "CANCELLED"),
        "{view}"
    );
    // The caller's history: not answered; the busy person's: a missed call.
    assert_eq!(
        outcome(&a, third.id().as_uuid(), call).await,
        ("NOT_ANSWERED".to_string(), false)
    );
    assert_eq!(
        outcome(&a, busy_id, call).await,
        ("MISSED".to_string(), true)
    );
    // The new Call's media resource is released; the running call is untouched.
    let binding = a
        .w
        .count(&format!(
            "SELECT count(*) FROM call_media_sessions WHERE call_id = '{call}' AND status = 'TERMINATED'"
        ))
        .await;
    assert_eq!(binding, 1);
    assert_eq!(a.state(live).await.0, CallState::Connected);

    // Once the running call is over, the same person rings again.
    let (_, version) = a.state(live).await;
    assert_eq!(a.command(live, "end", version).await.status, StatusCode::OK);
    assert_eq!(a.state(live).await.0, CallState::Ended);
    let r = a
        .create_call(
            "VOICE",
            &third,
            &third_device,
            &[&a.w.recipient],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.json["call"]["state"], "DIALING", "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(rings_for(&a, &callee, call).await);
}

#[tokio::test]
async fn a_call_that_is_still_ringing_does_not_make_anyone_busy() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let a = a.busy_on_another_call();
    // The recipient is being called (RINGING, not answered): not "on a call" yet.
    let (_ringing, _caller, callee) = a.ringing_call("VOICE").await;
    let third = a.principal("ringing-third").await;
    let third_device = a.device(&third, "busy-third-main").await;
    let r = a
        .create_call(
            "VOICE",
            &third,
            &third_device,
            &[&a.w.recipient],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.json["call"]["state"], "DIALING", "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(rings_for(&a, &callee, call).await);
}

#[tokio::test]
async fn a_group_call_rings_only_the_people_who_are_free() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let a = a.busy_on_another_call();
    let (_live, _caller, callee) = a.connected_call("VOICE").await;
    let busy_id = a.w.recipient.id().as_uuid();
    let host = a.principal("group-host").await;
    let host_device = a.device(&host, "host-main").await;
    let free = a.principal("group-free").await;
    let free_device = a.device(&free, "free-main").await;
    let r = a
        .create_call(
            "VOICE",
            &host,
            &host_device,
            &[&a.w.recipient, &free],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(r.json["call"]["state"], "DIALING", "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    assert!(rings_for(&a, &free_device, call).await);
    assert!(!rings_for(&a, &callee, call).await);
    // The group call goes on; when it is over, the busy person has a missed call.
    let invitation = a.invitation_for(call, free_device.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.accept(call, invitation, &free_device, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let (_, version) = a.state(call).await;
    assert_eq!(a.command(call, "end", version).await.status, StatusCode::OK);
    assert_eq!(
        outcome(&a, busy_id, call).await,
        ("MISSED".to_string(), true)
    );

    // Everyone called is busy: the group call ends BUSY.
    let r = a
        .create_call(
            "VOICE",
            &host,
            &host_device,
            &[&free],
            &Uuid::new_v4().to_string(),
        )
        .await;
    let busy_too: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let invitation = a
        .invitation_for(busy_too, free_device.endpoint)
        .await
        .unwrap();
    let (_, version) = a.state(busy_too).await;
    let r = a.accept(busy_too, invitation, &free_device, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(busy_too).await.0, CallState::Answered);
    let caller = a.principal("group-caller").await;
    let caller_device = a.device(&caller, "group-caller-main").await;
    let r = a
        .create_call(
            "VOICE",
            &caller,
            &caller_device,
            &[&a.w.recipient, &free],
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.json["call"]["state"], "BUSY", "{}", r.text);
}
