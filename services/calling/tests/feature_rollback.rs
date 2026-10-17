//! §95 feature rollback: every server-side Calling switch is turned OFF while live state
//! exists (a redeploy of the same database with the switch off). The existing
//! authoritative state is intact and ends cleanly through the normal safety operations
//! (end, decline, cancel, reconciliation, leg release); only NEW calls of that capability
//! are refused, while unrelated calling continues.
//!
//! Switches: `CALLING_NEW_CALLS_ENABLED` (master), `CALLING_APP_CALLING_ENABLED`,
//! `CALLING_PUBLIC_OUTBOUND_ENABLED`, `CALLING_PUBLIC_INBOUND_ENABLED`,
//! `CALLING_CALLBACK_RELAY_ENABLED` and `ENTERPRISE_CALLING_ENABLED`. Voicemail and
//! recording switches are covered by `enterprise_voicemail.rs` /
//! `enterprise_recording.rs` (kill switch never blocks stop / finalize / delete).

mod common;

use axum::http::StatusCode;
use calling::{
    calls::{app::public::PublicNetwork, CallState},
    enterprise::EnterpriseFlags,
    routing::engine::TelecomFlags,
    state::CallingRuntime,
    telephony::LegControl,
};
use common::{public::PublicWorld, Harness, TestResponse};
use sea_orm::ConnectionTrait;
use serde_json::json;
use uuid::Uuid;

const ALL_ON: TelecomFlags = TelecomFlags {
    public_outbound: true,
    public_inbound: true,
    callback_relay: true,
};

/// Redeploys the world's Calling service over the SAME database, clock, media provider,
/// Privacy Relay and Telephony, with `adjust` applied to an all-on runtime.
fn redeploy(x: &mut PublicWorld, adjust: impl FnOnce(&mut CallingRuntime)) {
    let old = &x.w.w.h;
    let mut runtime = common::test_runtime(Some(old.media.clone()), old.bus.clone());
    runtime.privacy_relay = x.privacy.clone();
    runtime.public_network = PublicNetwork {
        synthetic_interconnect: false,
        telephony: x.telephony.clone(),
        media: Some(x.media.clone()),
        flags: ALL_ON,
    };
    runtime.enterprise_flags = EnterpriseFlags::ALL_ON;
    adjust(&mut runtime);
    let h = Harness::with_runtime(
        old.db.clone(),
        old.clock.clone(),
        old.media.clone(),
        old.bus.clone(),
        runtime,
    );
    x.w.w.h = h;
}

fn call_id(r: &TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

fn refused(r: &TestResponse) {
    assert!(
        r.status == StatusCode::UNPROCESSABLE_ENTITY || r.status == StatusCode::FORBIDDEN,
        "{}: {}",
        r.status,
        r.text
    );
    assert!(
        ["CAPABILITY_UNAVAILABLE", "PUBLIC_ROUTE_DENIED"]
            .contains(&r.json["error"]["code"].as_str().unwrap_or_default()),
        "{}",
        r.text
    );
}

/// A dialing app-to-phone call: (call, its Telephony leg).
async fn phone_call(x: &PublicWorld, key: &str) -> (Uuid, Uuid) {
    let r = x.call_to(x.contact, key).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    assert_eq!(x.w.state(call).await.0, CallState::Dialing);
    let leg: Uuid =
        x.w.h()
            .conn()
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                format!(
                "SELECT id FROM call_legs WHERE call_id = '{call}' AND leg_kind = 'PUBLIC_NETWORK'"
            ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by_index(0)
            .unwrap();
    (call, x.telephony.telephony_leg_of(leg).unwrap())
}

async fn cancel(x: &PublicWorld, call: Uuid) {
    let (_, v) = x.w.state(call).await;
    let r = x.w.command(call, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.w.state(call).await.0, CallState::Cancelled);
}

fn terminated(x: &PublicWorld, telephony_leg: Uuid) -> bool {
    x.telephony
        .controls()
        .iter()
        .any(|(leg, a, _)| *leg == telephony_leg && *a == LegControl::Terminate)
}

async fn end(x: &PublicWorld, call: Uuid) {
    let (_, v) = x.w.state(call).await;
    let r = x.w.command(call, "end", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.w.state(call).await.0, CallState::Ended);
}

/// A new application call is accepted and connects (unrelated calling continues).
async fn app_calling_works(x: &PublicWorld) {
    let (call, _, _) = x.w.connected_call("VOICE").await;
    end(x, call).await;
}

async fn new_app_call(x: &PublicWorld, key: &str) -> TestResponse {
    let caller = x.w.device(&x.w.w.initiator, "caller-main").await;
    x.w.device(&x.w.w.recipient, "callee-main").await;
    x.w.create_call("VOICE", &x.w.w.initiator, &caller, &[&x.w.w.recipient], key)
        .await
}

async fn callback(x: &PublicWorld, key: &str) -> TestResponse {
    let mine = x.privacy.add_contact("NG");
    x.callback_to(mine, x.contact, key).await
}

async fn inbound(x: &PublicWorld) -> TestResponse {
    let recipient = x.w.w.recipient.id().as_uuid();
    x.as_telephony(
        "/internal/v1/telephony/inbound-calls",
        x.offer(Uuid::new_v4(), x.contact, &[recipient]),
    )
    .await
}

async fn world() -> Option<PublicWorld> {
    let x = PublicWorld::new().await?;
    x.configure_owner().await;
    x.privacy.allow(x.w.w.recipient.id().as_uuid());
    x.w.device(&x.w.w.recipient, "callee-main").await;
    Some(x)
}

#[tokio::test]
async fn the_master_switch_refuses_every_new_call_and_lets_live_calls_end() {
    let Some(mut x) = world().await else {
        return;
    };
    let (connected, _, _) = x.w.connected_call("VOICE").await;
    let (ringing, _, callee) = x.w.ringing_call("VOICE").await;
    let (dialing, t) = phone_call(&x, "master-live").await;
    let calls = x.count("SELECT count(*) FROM calls").await;

    redeploy(&mut x, |rt| rt.app_config.new_calls_enabled = false);

    // Live state is intact…
    assert_eq!(x.w.state(connected).await.0, CallState::Connected);
    assert_eq!(x.w.state(ringing).await.0, CallState::Ringing);
    assert_eq!(x.w.state(dialing).await.0, CallState::Dialing);
    // …and every new call is refused, of every kind.
    refused(&new_app_call(&x, "master-app").await);
    refused(&x.call_to(x.contact, "master-phone").await);
    refused(&callback(&x, "master-callback").await);
    let r = inbound(&x).await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert_eq!(x.count("SELECT count(*) FROM calls").await, calls);
    // Safety operations keep working: decline, end, cancel (+ the paid leg's release),
    // and the workers.
    let invitation = x.w.invitation_for(ringing, callee.endpoint).await.unwrap();
    let (_, v) = x.w.state(ringing).await;
    assert_eq!(
        x.w.decline(ringing, invitation, &callee, v).await.status,
        StatusCode::OK
    );
    assert_eq!(x.w.state(ringing).await.0, CallState::Declined);
    end(&x, connected).await;
    cancel(&x, dialing).await;
    assert!(terminated(&x, t));
    let app = &x.w.h().state.app;
    app.reconcile_due(50).await.unwrap();
    app.expire_due(50).await.unwrap();
    app.dispatch_telephony_commands(50).await.unwrap();

    // Switched on again: calling resumes.
    redeploy(&mut x, |_| {});
    app_calling_works(&x).await;
}

#[tokio::test]
async fn the_app_calling_switch_refuses_new_app_calls_only() {
    let Some(mut x) = world().await else {
        return;
    };
    let (connected, _, _) = x.w.connected_call("VOICE").await;
    redeploy(&mut x, |rt| rt.app_config.app_calling_enabled = false);
    assert_eq!(x.w.state(connected).await.0, CallState::Connected);
    refused(&new_app_call(&x, "app-off").await);
    // Public calling is unrelated and continues.
    let (dialing, t) = phone_call(&x, "app-off-phone").await;
    end(&x, connected).await;
    cancel(&x, dialing).await;
    assert!(terminated(&x, t));
}

#[tokio::test]
async fn the_public_outbound_switch_refuses_new_phone_calls_only() {
    let Some(mut x) = world().await else {
        return;
    };
    let (live, t) = phone_call(&x, "out-live").await;
    redeploy(&mut x, |rt| rt.public_network.flags.public_outbound = false);
    assert_eq!(x.w.state(live).await.0, CallState::Dialing);
    let before = x.telephony.originates().len();
    refused(&x.call_to(x.contact, "out-off").await);
    assert_eq!(x.telephony.originates().len(), before, "nothing dialed");
    app_calling_works(&x).await;
    cancel(&x, live).await;
    assert!(terminated(&x, t), "the live paid leg is still released");
}

#[tokio::test]
async fn the_public_inbound_switch_refuses_new_inbound_calls_only() {
    let Some(mut x) = world().await else {
        return;
    };
    let r = inbound(&x).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let live: Uuid = r.json["callId"].as_str().unwrap().parse().unwrap();
    assert_eq!(x.w.state(live).await.0, CallState::Ringing);
    redeploy(&mut x, |rt| rt.public_network.flags.public_inbound = false);
    let calls = x.count("SELECT count(*) FROM calls").await;
    let r = inbound(&x).await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert_eq!(x.count("SELECT count(*) FROM calls").await, calls);
    app_calling_works(&x).await;
    // The live inbound call is declined by its recipient: it ends cleanly.
    let callee = x.w.device(&x.w.w.recipient, "callee-main").await;
    let invitation = x.w.invitation_for(live, callee.endpoint).await.unwrap();
    let (_, v) = x.w.state(live).await;
    assert_eq!(
        x.w.decline(live, invitation, &callee, v).await.status,
        StatusCode::OK
    );
    assert!(x.w.state(live).await.0.is_terminal());
}

#[tokio::test]
async fn the_callback_relay_switch_refuses_new_callbacks_only() {
    let Some(mut x) = world().await else {
        return;
    };
    let r = callback(&x, "cb-live").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let live = call_id(&r);
    let state = x.w.state(live).await.0;
    assert!(!state.is_terminal(), "{state}");
    redeploy(&mut x, |rt| rt.public_network.flags.callback_relay = false);
    let before = x.telephony.originates().len();
    refused(&callback(&x, "cb-off").await);
    assert_eq!(x.telephony.originates().len(), before);
    app_calling_works(&x).await;
    // Ordinary app-to-phone is a different capability and continues.
    let (phone, _) = phone_call(&x, "cb-off-phone").await;
    cancel(&x, phone).await;
    cancel(&x, live).await;
    // The requester's live (executed) leg is released.
    let executed: Vec<Uuid> = x
        .telephony
        .executions()
        .into_iter()
        .filter(|(_, _, command)| command["call"]["callId"] == json!(live))
        .map(|(_, leg, _)| leg)
        .collect();
    assert_eq!(executed.len(), 1, "only the requester's leg was dialed");
    assert!(terminated(&x, executed[0]));
}

/// An ENTERPRISE_MEMBER principal of the world's environment.
async fn member(
    x: &PublicWorld,
    reference: &str,
) -> calling::platform::principals::ResolvedPrincipal {
    let (view, _) =
        x.w.h()
            .platform
            .register_principal(
                x.w.w.consumer.env(x.w.w.kind).id,
                crate::common::NewPrincipal::External {
                    kind: calling::domain::PrincipalKind::EnterpriseMember,
                    external_reference: reference.to_string(),
                },
            )
            .await
            .unwrap();
    x.w.h()
        .state
        .principals
        .resolve(
            &x.w.w.ctx,
            &calling::platform::principals::PrincipalReference::Id(view.id),
        )
        .await
        .unwrap()
}

/// `POST /v1/calls` to extension 1002 as `from`.
async fn extension_call(
    x: &PublicWorld,
    token: &str,
    from: &calling::platform::principals::ResolvedPrincipal,
    phone: &common::app::Device,
    key: &str,
) -> TestResponse {
    x.w.h()
        .request_with(
            axum::http::Method::POST,
            "/v1/calls",
            Some(token),
            Some(json!({ "kind": "VOICE", "initiatorPrincipalId": from.id(),
                         "initiatorEndpointId": phone.endpoint, "targetExtension": "1002" })),
            &[("idempotency-key", key)],
        )
        .await
}

#[tokio::test]
async fn the_enterprise_switch_refuses_new_enterprise_calls_only() {
    use calling::platform::scopes::names;
    let Some(mut x) = world().await else {
        return;
    };
    common::enterprise::allow_enterprise_scopes(x.w.h(), &x.w.w.consumer).await;
    let token = common::enterprise::credential(
        x.w.h(),
        &x.w.w.consumer,
        x.w.w.kind,
        &[
            names::ENTERPRISE_CONFIG_WRITE,
            names::ENTERPRISE_CONFIG_READ,
            names::CALLS_WRITE,
            names::CALLS_READ,
            names::ENTERPRISE_CALLS_USE,
        ],
    )
    .await
    .secret
    .expose_once()
    .to_string();
    let alice = member(&x, "alice").await;
    let bob = member(&x, "bob").await;
    let r =
        x.w.h()
            .request(
                axum::http::Method::POST,
                "/v1/enterprise/extensions",
                Some(&token),
                Some(
                    json!({ "extensionNumber": "1002", "displayLabel": "Ext 1002",
                         "destination": { "type": "ENTERPRISE_MEMBER", "id": bob.id() } }),
                ),
            )
            .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let alice_phone = x.w.device(&alice, "alice-desk").await;
    x.w.device(&bob, "bob-desk").await;
    let r = extension_call(&x, &token, &alice, &alice_phone, "ent-live").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let live = call_id(&r);
    assert_eq!(x.w.state(live).await.0, CallState::Ringing);

    redeploy(&mut x, |rt| rt.enterprise_flags.enterprise_calling = false);
    refused(&extension_call(&x, &token, &alice, &alice_phone, "ent-off").await);
    app_calling_works(&x).await;
    // The live enterprise call is cancelled by its caller: it ends cleanly.
    let (_, v) = x.w.state(live).await;
    let r = x.w.command(live, "cancel", v).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.w.state(live).await.0, CallState::Cancelled);
}
