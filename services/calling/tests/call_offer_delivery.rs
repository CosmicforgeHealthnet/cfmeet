//! Ring-offer delivery truth and terminal closure (server bugs C1, C2, C4 of
//! 16 November 2026).
//!
//! * C1 — an offer is PENDING until the callee's device acknowledges receipt over its
//!   signaling credential; only then is it (and the Call) RINGING. A callee signed in
//!   nowhere never shows as ringing; several devices, reconnects and expiry behave.
//! * C2 — no ring offer stays open on an ending / terminal Call, and history never reports
//!   RINGING for a finished Call (also for rows written before the fix, which the expiry
//!   worker repairs).
//! * C4 — a refused signaling credential is a 401 with a stable, machine-readable code.

mod common;

use axum::http::{Method, StatusCode};
use calling::calls::CallState;
use common::app::{AppWorld, Device};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde_json::{json, Value};
use uuid::Uuid;

fn call_id(r: &common::TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

async fn offers(a: &AppWorld, call: Uuid) -> Vec<Value> {
    a.call_view(call).await["invitations"]
        .as_array()
        .unwrap()
        .clone()
}

async fn status_of(a: &AppWorld, call: Uuid, endpoint: Uuid) -> String {
    offers(a, call)
        .await
        .iter()
        .find(|i| i["endpointId"] == endpoint.to_string())
        .map(|i| i["status"].as_str().unwrap().to_string())
        .unwrap()
}

async fn recover(a: &AppWorld, device: &Device) -> Value {
    let token = a.signaling_token(device).await;
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json
}

async fn history_outcome(a: &AppWorld, principal: Uuid, call: Uuid) -> String {
    let r = a.get(&format!("/v1/calls?principalId={principal}")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    r.json["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["callId"] == call.to_string())
        .map(|e| e["participation"]["outcome"].as_str().unwrap().to_string())
        .unwrap()
}

async fn new_call(a: &AppWorld, caller: &Device, key: &str) -> Uuid {
    let r = a
        .create_call("VOICE", &a.w.initiator, caller, &[&a.w.recipient], key)
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    call_id(&r)
}

/// C1: a callee whose devices never receive the call (signed out / offline) never makes
/// the caller see "Ringing": the Call stays DIALING, the offer PENDING, and it expires into
/// NO_ANSWER / a missed call.
#[tokio::test]
async fn an_offer_nobody_received_never_rings() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c1-caller").await;
    let offline = a.device(&a.w.recipient, "c1-offline").await;
    let call = new_call(&a, &caller, "c1-offline").await;
    assert_eq!(a.state(call).await.0, CallState::Dialing);
    assert_eq!(status_of(&a, call, offline.endpoint).await, "PENDING");
    assert!(a.call_view(call).await["ringingAt"].is_null());
    // History while it is being offered: in progress / ringing, never answered.
    assert_eq!(
        history_outcome(&a, caller.principal, call).await,
        "IN_PROGRESS"
    );
    assert_eq!(
        history_outcome(&a, offline.principal, call).await,
        "RINGING"
    );
    a.h().clock.advance(chrono::Duration::seconds(46));
    a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);
    assert_eq!(status_of(&a, call, offline.endpoint).await, "EXPIRED");
    assert_eq!(history_outcome(&a, offline.principal, call).await, "MISSED");
    assert_eq!(
        history_outcome(&a, caller.principal, call).await,
        "NOT_ANSWERED"
    );
}

/// C1: each device acknowledges its own offer; the first acknowledgement moves the Call to
/// RINGING exactly once, a repeat is a no-op, and an un-acknowledged device stays PENDING.
#[tokio::test]
async fn each_device_acknowledges_its_own_offer_and_the_first_rings_the_call() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c1m-caller").await;
    let phone = a.device(&a.w.recipient, "c1m-phone").await;
    let browser = a.device(&a.w.recipient, "c1m-browser").await;
    let call = new_call(&a, &caller, "c1-multi").await;
    let phone_offer = a.invitation_for(call, phone.endpoint).await.unwrap();
    let browser_offer = a.invitation_for(call, browser.endpoint).await.unwrap();
    let recovered = recover(&a, &phone).await;
    assert_eq!(recovered["data"][0]["status"], "PENDING");
    assert_eq!(
        recovered["data"][0]["actionable"], true,
        "a PENDING offer can be answered"
    );

    // A device can never acknowledge another device's offer (indistinguishable from none).
    let r = a.acknowledge(&phone, browser_offer).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    assert_eq!(status_of(&a, call, browser.endpoint).await, "PENDING");

    let r = a.acknowledge(&phone, phone_offer).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["status"], "RINGING");
    assert_eq!(r.json["callState"], "RINGING");
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Ringing);
    assert_eq!(status_of(&a, call, phone.endpoint).await, "RINGING");
    assert_eq!(status_of(&a, call, browser.endpoint).await, "PENDING");

    // Repeating it, or the second device acknowledging, never transitions the Call again.
    let again = a.acknowledge(&phone, phone_offer).await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text);
    assert_eq!(again.json["ringingAt"], r.json["ringingAt"]);
    let second = a.acknowledge(&browser, browser_offer).await;
    assert_eq!(second.status, StatusCode::OK, "{}", second.text);
    assert_eq!(a.state(call).await, (CallState::Ringing, version));
    assert_eq!(status_of(&a, call, browser.endpoint).await, "RINGING");

    // Answering on one device: the other loses; acknowledging a finished offer is refused.
    let r = a.accept(call, phone_offer, &phone, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let late = a.acknowledge(&browser, browser_offer).await;
    assert_eq!(late.status, StatusCode::CONFLICT, "{}", late.text);
    assert_eq!(late.json["error"]["code"], "ANSWERED_ELSEWHERE");
}

/// C1: answering or declining is itself proof of receipt — a device that never sent an
/// acknowledgement (an older client, a push opened directly) can still answer.
#[tokio::test]
async fn answering_or_declining_a_pending_offer_acknowledges_it() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c1a-caller").await;
    let callee = a.device(&a.w.recipient, "c1a-callee").await;

    let call = new_call(&a, &caller, "c1-accept").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (state, version) = a.state(call).await;
    assert_eq!(state, CallState::Dialing);
    let r = a.accept(call, offer, &callee, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(status_of(&a, call, callee.endpoint).await, "ACCEPTED");
    assert_eq!(a.state(call).await.0, CallState::Answered);
    // Answering proved receipt: the Call keeps its ringing evidence.
    let view = a.call_view(call).await;
    assert!(!view["ringingAt"].is_null(), "{view}");

    let call = new_call(&a, &caller, "c1-decline").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.decline(call, offer, &callee, version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(status_of(&a, call, callee.endpoint).await, "DECLINED");
    assert_eq!(a.state(call).await.0, CallState::Declined);
}

/// C1: an acknowledgement after expiry, cancellation or reconnect follows server truth.
#[tokio::test]
async fn acknowledgements_after_expiry_cancel_and_reconnect() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c1e-caller").await;
    let callee = a.device(&a.w.recipient, "c1e-callee").await;

    // Expired: 410 even before the worker ran; the Call never rings.
    let call = new_call(&a, &caller, "c1-expired").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    a.h().clock.advance(chrono::Duration::seconds(46));
    let r = a.acknowledge(&callee, offer).await;
    assert_eq!(r.status, StatusCode::GONE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INVITATION_EXPIRED");
    assert_eq!(a.state(call).await.0, CallState::Dialing);
    // The refused acknowledgement's transaction may still be rolling back (its Call row
    // lock is released asynchronously); the worker skips locked Calls (SKIP LOCKED) and
    // picks this one up on a later round, so drive a bounded number of rounds.
    for _ in 0..100 {
        a.h().state.app.expire_due(100).await.unwrap();
        if a.state(call).await.0 == CallState::NoAnswer {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(a.state(call).await.0, CallState::NoAnswer);

    // Cancelled while PENDING: closed with the Call; acknowledging it is refused.
    let call = new_call(&a, &caller, "c1-cancelled").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    let r = a.command(call, "cancel", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(status_of(&a, call, callee.endpoint).await, "CANCELLED");
    let r = a.acknowledge(&callee, offer).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "INVITATION_NOT_ACTIONABLE");

    // Reconnect: the device comes back online with a NEW signaling credential, recovers the
    // still-live offer and acknowledges it then.
    let call = new_call(&a, &caller, "c1-reconnect").await;
    let offer = a.invitation_for(call, callee.endpoint).await.unwrap();
    let recovered = recover(&a, &callee).await;
    let live = recovered["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["invitationId"] == offer.to_string())
        .unwrap()
        .clone();
    assert_eq!(live["status"], "PENDING");
    let r = a.acknowledge(&callee, offer).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Ringing);
}

/// Reopens a closed offer behind the database guards: the state production rows were left
/// in before offers were closed with every ending transition (C2 repair input).
async fn reopen_offer_like_a_legacy_row(a: &AppWorld, offer: Uuid) {
    let txn = a.h().conn().begin().await.unwrap();
    for sql in [
        "SET LOCAL session_replication_role = replica".to_string(),
        format!(
            "UPDATE call_invitations SET status = 'RINGING', finished_at = NULL, cancelled_at = NULL, \
             terminal_reason = NULL WHERE id = '{offer}'"
        ),
    ] {
        txn.execute(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
}

/// C2: cancelling during ringing closes every offer in the same transaction; history says
/// MISSED, never RINGING — and an offer left open on a finished Call (legacy row) neither
/// rings in history nor in recovery, and the worker closes it with evidence.
#[tokio::test]
async fn a_cancelled_call_never_stays_ringing_in_history() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c2-caller").await;
    let phone = a.device(&a.w.recipient, "c2-phone").await;
    let browser = a.device(&a.w.recipient, "c2-browser").await;
    let call = new_call(&a, &caller, "c2-cancel").await;
    let offer = a.invitation_for(call, phone.endpoint).await.unwrap();
    assert_eq!(a.acknowledge(&phone, offer).await.status, StatusCode::OK);
    let (_, version) = a.state(call).await;
    let r = a.command(call, "cancel", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(a.state(call).await.0, CallState::Cancelled);
    for device in [&phone, &browser] {
        assert_eq!(status_of(&a, call, device.endpoint).await, "CANCELLED");
    }
    assert_eq!(history_outcome(&a, phone.principal, call).await, "MISSED");

    // A legacy row: the offer is open again although the Call is CANCELLED.
    reopen_offer_like_a_legacy_row(&a, offer).await;
    assert_eq!(status_of(&a, call, phone.endpoint).await, "RINGING");
    assert_eq!(
        history_outcome(&a, phone.principal, call).await,
        "MISSED",
        "history respects the Call's terminal state"
    );
    let recovered = recover(&a, &phone).await;
    let row = recovered["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["invitationId"] == offer.to_string())
        .unwrap()
        .clone();
    assert_eq!(row["status"], "CANCELLED", "{row}");
    assert_eq!(row["actionable"], false, "{row}");
    // The worker repairs it (CANCELLED, CALL_CANCELLED) and a second run finds nothing.
    let report = a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(report.finished_call_invitations_closed, 1);
    let closed = offers(&a, call)
        .await
        .into_iter()
        .find(|i| i["invitationId"] == offer.to_string())
        .unwrap();
    assert_eq!(closed["status"], "CANCELLED");
    assert_eq!(closed["terminalReason"], "CALL_CANCELLED");
    let report = a.h().state.app.expire_due(100).await.unwrap();
    assert_eq!(report.finished_call_invitations_closed, 0);
}

/// C2: ending an answered group call while a third participant is still being offered
/// closes that offer in the same transaction as the move to ENDING.
#[tokio::test]
async fn ending_a_call_closes_offers_still_ringing() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let carol = a.principal("c2-carol").await;
    let caller = a.device(&a.w.initiator, "c2e-caller").await;
    let bob = a.device(&a.w.recipient, "c2e-bob").await;
    let carol_d = a.device(&carol, "c2e-carol").await;
    let r = a
        .create_call(
            "VOICE",
            &a.w.initiator,
            &caller,
            &[&a.w.recipient, &carol],
            "c2-end",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    let carol_offer = a.invitation_for(call, carol_d.endpoint).await.unwrap();
    assert_eq!(
        a.acknowledge(&carol_d, carol_offer).await.status,
        StatusCode::OK
    );
    let bob_offer = a.invitation_for(call, bob.endpoint).await.unwrap();
    let (_, version) = a.state(call).await;
    assert_eq!(
        a.accept(call, bob_offer, &bob, version).await.status,
        StatusCode::OK
    );
    assert_eq!(status_of(&a, call, carol_d.endpoint).await, "RINGING");
    let (_, version) = a.state(call).await;
    let r = a.command(call, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(matches!(
        a.state(call).await.0,
        CallState::Ending | CallState::Ended
    ));
    assert_eq!(status_of(&a, call, carol_d.endpoint).await, "CANCELLED");
    assert_eq!(
        history_outcome(&a, carol.id().as_uuid(), call).await,
        "MISSED"
    );
}

/// C2: cancellation racing an answer — whichever wins, no offer stays open on a finished
/// Call and history never says RINGING for it.
#[tokio::test]
async fn cancel_racing_answers_leaves_no_open_offer() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let caller = a.device(&a.w.initiator, "c2r-caller").await;
    let phone = a.device(&a.w.recipient, "c2r-phone").await;
    let browser = a.device(&a.w.recipient, "c2r-browser").await;
    for round in 0..4 {
        let call = new_call(&a, &caller, &format!("c2-race-{round}")).await;
        let phone_offer = a.invitation_for(call, phone.endpoint).await.unwrap();
        let browser_offer = a.invitation_for(call, browser.endpoint).await.unwrap();
        let (_, version) = a.state(call).await;
        let (cancel, answer_1, answer_2) = tokio::join!(
            a.command(call, "cancel", version),
            a.accept(call, phone_offer, &phone, version),
            a.accept(call, browser_offer, &browser, version),
        );
        let winners = [&cancel, &answer_1, &answer_2]
            .iter()
            .filter(|r| r.status == StatusCode::OK)
            .count();
        assert!(
            winners >= 1,
            "{} {} {}",
            cancel.text,
            answer_1.text,
            answer_2.text
        );
        let (state, version) = a.state(call).await;
        // An answered Call is ended so the round finishes in a terminal state.
        if !state.is_terminal() {
            let r = a.command(call, "end", version).await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        }
        for offer in offers(&a, call).await {
            assert!(
                !["PENDING", "RINGING"].contains(&offer["status"].as_str().unwrap()),
                "round {round}: {offer}"
            );
        }
        assert_ne!(history_outcome(&a, phone.principal, call).await, "RINGING");
    }
}

/// C4: every refused signaling credential is a 401 with a stable code and a bearer
/// challenge — expired ones distinctly — so a client stops retrying the same token.
#[tokio::test]
async fn refused_signaling_credentials_are_401_with_stable_codes() {
    let Some(a) = AppWorld::new().await else {
        return;
    };
    let callee = a.device(&a.w.recipient, "c4-callee").await;
    let token = a.signaling_token(&callee).await;
    let mut tampered = token.clone();
    tampered.pop();
    tampered.push(if token.ends_with('A') { 'B' } else { 'A' });
    for (credential, code) in [
        (tampered.as_str(), "AUTHENTICATION_FAILED"),
        ("cfsig_not-a-credential", "AUTHENTICATION_FAILED"),
    ] {
        let r = a
            .as_endpoint(
                credential,
                Method::GET,
                "/v1/calling/invitations",
                None,
                &[],
            )
            .await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
        assert_eq!(r.json["error"]["code"], code, "{}", r.text);
        assert!(
            r.headers.contains_key("www-authenticate"),
            "a bearer challenge tells the client to authenticate again"
        );
    }
    // Revoked endpoint: the credential's authority is gone.
    let revoked = a.device(&a.w.recipient, "c4-revoked").await;
    let revoked_token = a.signaling_token(&revoked).await;
    let r = a
        .h()
        .request(
            Method::DELETE,
            &format!("/v1/calling/endpoints/{}", revoked.endpoint),
            Some(a.secret()),
            None,
        )
        .await;
    assert!(r.status.is_success(), "{}", r.text);
    let r = a
        .as_endpoint(
            &revoked_token,
            Method::GET,
            "/v1/calling/invitations",
            None,
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "AUTHENTICATION_FAILED");
    // Expired: a distinct code so the client mints a fresh credential.
    a.h().clock.advance(chrono::Duration::hours(2));
    let r = a
        .as_endpoint(&token, Method::GET, "/v1/calling/invitations", None, &[])
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "SIGNALING_CREDENTIAL_EXPIRED");
    let r = a
        .as_endpoint(
            &token,
            Method::POST,
            &format!("/v1/calling/invitations/{}/acknowledge", Uuid::new_v4()),
            Some(json!({})),
            &[],
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "SIGNALING_CREDENTIAL_EXPIRED");
}
