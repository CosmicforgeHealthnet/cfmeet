//! Roadmap N9: Calling's `NotificationsPushTransport` for the Notification Service call
//! fast lane (I3). Only first-party CF Meet recipients (NATIVE_CF_USER `cfmeet-user:`
//! principals of FIRST_PARTY tenants) are sent — a customer application's
//! `pushTargetRef` never leaves Calling; ringing never waits for push (fire and forget,
//! bounded); one alert per (call, push target). Real PostgreSQL for the recipient facts.
mod common;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::http::StatusCode;
use calling::{
    domain::{EnvironmentKind, PrincipalKind, TenantClassification},
    platform::principals::{PrincipalReference, ResolvedPrincipal},
    signaling::{CallPushSink, NotificationsPushTransport, PushDispatcher, PushOffer},
};
use common::{
    app::{AppWorld, Device},
    calls::CallWorld,
    fake_media::FakeMediaProvider,
    Harness, NewPrincipal, TestDb,
};
use serde_json::{json, Value};
use uuid::Uuid;

/// The service's I3, recording bodies; optionally slow.
#[derive(Default)]
struct FakeI3 {
    bodies: Mutex<Vec<Value>>,
    delay: Mutex<Option<Duration>>,
}

#[async_trait]
impl CallPushSink for FakeI3 {
    async fn call_push(&self, body: Value) -> Result<Value, String> {
        let delay = *self.delay.lock().unwrap();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.bodies.lock().unwrap().push(body);
        Ok(json!({ "results": [] }))
    }
}

fn offer(call: Uuid, target: Uuid, account: Option<&str>, expires_in: i64) -> PushOffer {
    PushOffer {
        push_target_ref: target,
        payload: json!({
            "schema": "cfmeet.calling.push-invitation", "schemaVersion": 1, "callId": call,
            "invitationId": Uuid::new_v4(), "callKind": "VOICE", "caller": {},
            "issuedAt": chrono::Utc::now().to_rfc3339(),
            "expiresAt": (chrono::Utc::now() + chrono::Duration::seconds(expires_in)).to_rfc3339(),
        }),
        call_id: call,
        first_party_account: account.map(str::to_string),
    }
}

async fn settle(i3: &FakeI3, n: usize) {
    for _ in 0..100 {
        if i3.bodies.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
}

#[tokio::test]
async fn only_first_party_offers_are_sent_once_per_call_and_target() {
    let i3 = Arc::new(FakeI3::default());
    let t = NotificationsPushTransport::new(i3.clone(), Duration::from_millis(1000));
    let account = format!("cfmeet-user:{}", Uuid::new_v4());
    let (call, target) = (Uuid::new_v4(), Uuid::new_v4());
    // A customer application's push target: never sent.
    t.dispatch_offer(offer(call, Uuid::new_v4(), None, 30))
        .await;
    t.dispatch(Uuid::new_v4(), json!({})).await;
    // First-party: sent once; a repeat for the same (call, target) is dropped.
    t.dispatch_offer(offer(call, target, Some(&account), 30))
        .await;
    t.dispatch_offer(offer(call, target, Some(&account), 30))
        .await;
    // Another device of the same call, and the same device for another call: sent.
    t.dispatch_offer(offer(call, Uuid::new_v4(), Some(&account), 30))
        .await;
    t.dispatch_offer(offer(Uuid::new_v4(), target, Some(&account), 30))
        .await;
    // An already expired invitation: nothing.
    t.dispatch_offer(offer(Uuid::new_v4(), target, Some(&account), -1))
        .await;
    settle(&i3, 3).await;
    let bodies = i3.bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3, "{bodies:#?}");
    let b = &bodies[0];
    assert_eq!(b["pushTargetRefs"], json!([target]));
    assert_eq!(b["recipientAccountRef"], account);
    assert_eq!(b["expiresAt"], b["payload"]["expiresAt"]);
    assert_eq!(b["payload"]["schema"], "cfmeet.calling.push-invitation");
}

#[tokio::test]
async fn a_slow_service_never_delays_ringing() {
    let i3 = Arc::new(FakeI3::default());
    *i3.delay.lock().unwrap() = Some(Duration::from_secs(30));
    let t = NotificationsPushTransport::new(i3.clone(), Duration::from_millis(100));
    let started = Instant::now();
    t.dispatch_offer(offer(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Some(&format!("cfmeet-user:{}", Uuid::new_v4())),
        30,
    ))
    .await;
    // Fire and forget: the caller returns at once …
    assert!(started.elapsed() < Duration::from_millis(50));
    // … and the background call is abandoned at the timeout (never completes).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(i3.bodies.lock().unwrap().is_empty());
}

/// A NATIVE_CF_USER principal with a CF Meet account reference.
async fn native(a: &AppWorld, reference: &str) -> ResolvedPrincipal {
    let h = a.h();
    let (view, _) = h
        .platform
        .register_principal(
            a.w.consumer.env(a.w.kind).id,
            NewPrincipal::External {
                kind: PrincipalKind::NativeCfUser,
                external_reference: reference.to_string(),
            },
        )
        .await
        .unwrap();
    h.state
        .principals
        .resolve(&a.w.ctx, &PrincipalReference::Id(view.id))
        .await
        .unwrap()
}

async fn world(classification: TenantClassification) -> Option<AppWorld> {
    let db = Arc::new(TestDb::new().await?);
    let media = Arc::new(FakeMediaProvider::default());
    let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
    let runtime = common::test_runtime(Some(media.clone()), bus.clone());
    let h = Harness::with_runtime(
        db,
        calling::domain::ManualClock::new(common::t0()),
        media,
        bus,
        runtime,
    );
    let consumer = h.consumer("cosmicforge", classification).await;
    Some(AppWorld {
        w: CallWorld::for_consumer(h, consumer, EnvironmentKind::Production).await,
    })
}

async fn endpoint(a: &AppWorld, p: &ResolvedPrincipal, key: &str, push: Uuid) -> Device {
    let r = a
        .post(
            "/v1/calling/endpoints",
            json!({
                "principalId": p.id(), "endpointKey": key, "endpointKind": "MOBILE",
                "capabilities": { "audio": true, "video": true }, "pushTargetRef": push,
            }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    Device {
        principal: p.id().as_uuid(),
        endpoint: r.json["endpointId"].as_str().unwrap().parse().unwrap(),
    }
}

#[tokio::test]
async fn offers_carry_the_account_only_for_first_party_recipients() {
    // FIRST_PARTY tenant: a CF Meet account gets its reference; an application principal
    // and a look-alike reference do not.
    let Some(a) = world(TenantClassification::FirstParty).await else {
        return;
    };
    let caller = a.w.initiator.clone();
    let from = a.device(&caller, "caller-1").await;
    let account = format!("cfmeet-user:{}", Uuid::new_v4());
    let bob = native(&a, &account).await;
    let app_user = a.w.recipient.clone();
    let fake = native(&a, "cfmeet-user:not-a-uuid").await;
    let (t_bob, t_app, t_fake) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    endpoint(&a, &bob, "bob-1", t_bob).await;
    endpoint(&a, &app_user, "app-1", t_app).await;
    endpoint(&a, &fake, "fake-1", t_fake).await;
    let r = a
        .create_call(
            "VOICE",
            &caller,
            &from,
            &[&bob, &app_user, &fake],
            "n9-push",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let offers = a.h().push.offers();
    let of = |t: Uuid| offers.iter().find(|o| o.push_target_ref == t).cloned();
    let bob_offer = of(t_bob).expect("bob was offered");
    assert_eq!(
        bob_offer.first_party_account.as_deref(),
        Some(account.as_str())
    );
    assert_eq!(
        bob_offer.call_id.to_string(),
        r.json["call"]["callId"].as_str().unwrap()
    );
    assert_eq!(of(t_app).unwrap().first_party_account, None);
    assert_eq!(of(t_fake).unwrap().first_party_account, None);

    // EXTERNAL tenant: even a cfmeet-user-looking NATIVE principal is not first-party.
    let Some(e) = world(TenantClassification::External).await else {
        return;
    };
    let caller = e.w.initiator.clone();
    let from = e.device(&caller, "caller-1").await;
    let lookalike = native(&e, &format!("cfmeet-user:{}", Uuid::new_v4())).await;
    let target = Uuid::new_v4();
    endpoint(&e, &lookalike, "x-1", target).await;
    let r = e
        .create_call("VOICE", &caller, &from, &[&lookalike], "n9-push-ext")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let offers = e.h().push.offers();
    let o = offers.iter().find(|o| o.push_target_ref == target).unwrap();
    assert_eq!(o.first_party_account, None);
}
