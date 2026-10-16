//! §87 failure matrix: every external dependency of Calling fails independently, and only
//! the capability that depends on it fails closed or degrades — application (app-to-app)
//! and enterprise internal calling keep working, and no paid effect is ever produced
//! without its authority.
//!
//! | dependency down                         | dependent capability                    | expected                                              |
//! |-----------------------------------------|-----------------------------------------|-------------------------------------------------------|
//! | carrier / route (refused or no route)   | app-to-phone                            | refused (no route) / FAILED (refused), spend released |
//! | SIP edge / Telephony unavailable        | app-to-phone execution                  | DIALING, retried, never a failure or a 2nd paid leg   |
//! | RTC media provider failing              | app / enterprise media (not phone-only) | waits in ROUTING, nothing rings; callback relay works |
//! | Privacy Relay unavailable               | protected / public calls                | 503 fail closed, nothing persisted or dialed          |
//! | Contact Vault unavailable (via Relay)   | protected contact resolution            | ORIGINATE held (no grant → nothing sent)              |
//! | spend authority unprovable (DB fault)   | paid public legs                        | refused before any paid command                       |
//! | realtime fanout down                    | live signals                            | lost; authoritative state and calling unaffected      |

mod common;

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use axum::http::StatusCode;
use calling::{
    calls::CallState,
    media::MediaProviderError,
    platform::principals::ResolvedPrincipal,
    routing::engine::Evidence,
    signaling::{
        bus::{SignalBusError, SignalSubscription},
        InMemorySignalBus, RealtimeSignalBus, SignalEnvelope,
    },
};
use common::{
    app::Device,
    enterprise::EnterpriseWorld,
    fake_media::FakeOp,
    privacy::{FakePrivacyRelay, Mode},
    public::PublicWorld,
    telephony::{Answer, FakeTelephony},
    TestResponse,
};
use serde_json::json;
use uuid::Uuid;

/// A public-network world wrapped as an enterprise world: app-to-phone, callback relay,
/// app-to-app and extension 1001 → 1002 calling on ONE deployment.
struct Matrix {
    e: EnterpriseWorld,
    privacy: Arc<FakePrivacyRelay>,
    telephony: Arc<FakeTelephony>,
    contact: Uuid,
    session: Uuid,
    caller: Device,
    alice: ResolvedPrincipal,
    alice_phone: Device,
}

async fn matrix(configure: impl FnOnce(&mut calling::state::CallingRuntime)) -> Option<Matrix> {
    let x = PublicWorld::configured(configure).await?;
    let PublicWorld {
        w,
        privacy,
        telephony,
        contact,
        session,
        caller,
        ..
    } = x;
    let e = EnterpriseWorld::around(w).await;
    let alice = e.member("alice").await;
    let bob = e.member("bob").await;
    e.extension("1001", "ENTERPRISE_MEMBER", alice.id()).await;
    e.extension("1002", "ENTERPRISE_MEMBER", bob.id()).await;
    let alice_phone = e.a.device(&alice, "alice-desk").await;
    e.a.device(&bob, "bob-mobile").await;
    Some(Matrix {
        e,
        privacy,
        telephony,
        contact,
        session,
        caller,
        alice,
        alice_phone,
    })
}

impl Matrix {
    async fn count(&self, sql: &str) -> i64 {
        self.e.count(sql).await
    }

    /// `POST /v1/calls` from the initiator to a protected phone contact.
    async fn phone_call(&self, key: &str) -> TestResponse {
        self.e
            .a
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "initiatorPrincipalId": self.e.a.w.initiator.id(),
                    "initiatorEndpointId": self.caller.endpoint,
                    "targetContactReference": self.contact,
                    "privacyRelaySessionId": self.session,
                }),
                &[("idempotency-key", key)],
            )
            .await
    }

    /// Paid effects so far: (Telephony executions, spend reservations still RESERVED).
    async fn paid(&self) -> (usize, i64) {
        (
            self.telephony.executions().len(),
            self.count("SELECT count(*) FROM call_spend_reservations WHERE status = 'RESERVED'")
                .await,
        )
    }

    /// Application (app-to-app) calling and enterprise internal calling work.
    async fn unrelated_calling_works(&self, tag: &str) {
        let (call, _, _) = self.e.a.connected_call("VOICE").await;
        let (_, v) = self.e.a.state(call).await;
        assert_eq!(
            self.e.a.command(call, "end", v).await.status,
            StatusCode::OK,
            "{tag}: app-to-app"
        );
        let r = self
            .e
            .call_extension(
                "VOICE",
                &self.alice,
                &self.alice_phone,
                "1002",
                &format!("{tag}-ext"),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{tag}: {}", r.text);
        assert_eq!(
            r.json["call"]["state"], "RINGING",
            "{tag}: enterprise internal"
        );
        assert!(r.json["liveInvitationCount"].as_i64().unwrap() >= 1);
    }
}

fn call_id(r: &TestResponse) -> Uuid {
    r.json["call"]["callId"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn carrier_or_route_down_refuses_public_calls_only() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    // No usable route: refused before anything is reserved or dialed.
    m.telephony.set_routes(Evidence::Known(vec![]));
    let r = m.phone_call("route-down").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PUBLIC_ROUTE_UNAVAILABLE");
    assert_eq!(m.paid().await, (0, 0));
    assert!(m.telephony.originates().is_empty());
    m.unrelated_calling_works("route-down").await;

    // The carrier refuses the execution: the Call fails, the reservation is released.
    m.telephony.set_routes(common::telephony::healthy_routes());
    m.telephony.script(&[Answer::Refuse("ROUTE_UNAVAILABLE")]);
    let r = m.phone_call("carrier-refuses").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    assert_eq!(m.e.a.state(call).await.0, CallState::Failed);
    assert_eq!(m.paid().await, (0, 0), "no paid leg, reservation released");
    m.unrelated_calling_works("carrier-refuses").await;
}

#[tokio::test]
async fn sip_edge_or_telephony_unavailable_holds_execution_without_inventing_an_outcome() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    m.telephony.script(&vec![Answer::Unavailable; 5]);
    let r = m.phone_call("edge-down").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    // Unknown, not failed: the Call waits, the ORIGINATE is retried later (same id).
    assert_eq!(m.e.a.state(call).await.0, CallState::Dialing);
    assert_eq!(
        m.count(&format!(
            "SELECT count(*) FROM call_telephony_commands WHERE call_id = '{call}' \
             AND kind = 'ORIGINATE' AND status = 'PENDING' AND last_error_code = 'TELEPHONY_UNAVAILABLE'"
        ))
        .await,
        1
    );
    assert!(m.telephony.executions().is_empty());
    m.unrelated_calling_works("edge-down").await;
}

#[tokio::test]
async fn a_failing_rtc_provider_degrades_media_calls_while_phone_only_calls_work() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    let provider = &m.e.a.h().media;
    provider.fail_next(FakeOp::Ensure, MediaProviderError::Unavailable, 100);
    // App-to-app: nothing rings without a media resource (no half-made Call).
    let (call, _, _) = {
        let caller = m.e.a.device(&m.e.a.w.initiator, "caller-main").await;
        m.e.a.device(&m.e.a.w.recipient, "callee-main").await;
        let r =
            m.e.a
                .create_call(
                    "VOICE",
                    &m.e.a.w.initiator,
                    &caller,
                    &[&m.e.a.w.recipient],
                    "rtc-down",
                )
                .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        (call_id(&r), (), ())
    };
    assert_eq!(m.e.a.state(call).await.0, CallState::Routing);
    assert_eq!(
        m.count(&format!(
            "SELECT count(*) FROM call_invitations WHERE call_id = '{call}'"
        ))
        .await,
        0
    );
    // Phone-to-phone (callback relay) needs no RTC media: it proceeds.
    let mine = m.privacy.add_contact("NG");
    let r =
        m.e.a
            .post_with(
                "/v1/calls",
                json!({
                    "kind": "VOICE",
                    "callbackRequested": true,
                    "initiatorPrincipalId": m.e.a.w.initiator.id(),
                    "initiatorContactReference": mine,
                    "targetContactReference": m.contact,
                    "privacyRelaySessionId": m.session,
                }),
                &[("idempotency-key", "rtc-down-callback")],
            )
            .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        m.telephony.executions().len(),
        1,
        "the requester's leg is dialed"
    );
    // The provider recovers: application and enterprise calling work again.
    provider.clear_failures();
    m.unrelated_calling_works("rtc-recovered").await;
}

#[tokio::test]
async fn privacy_relay_unavailable_fails_protected_calls_closed() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    let calls_before = m.count("SELECT count(*) FROM calls").await;
    m.privacy.set_mode(Mode::Unavailable);
    let r = m.phone_call("relay-down").await;
    assert_eq!(r.status, StatusCode::SERVICE_UNAVAILABLE, "{}", r.text);
    assert_eq!(r.json["error"]["code"], "PRIVACY_AUTHORIZATION_UNAVAILABLE");
    assert_eq!(m.count("SELECT count(*) FROM calls").await, calls_before);
    assert_eq!(m.paid().await, (0, 0));
    m.unrelated_calling_works("relay-down").await;
}

#[tokio::test]
async fn contact_vault_unavailable_holds_the_originate_before_any_paid_command() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    *m.privacy.grants_unavailable.lock().unwrap() = true;
    let r = m.phone_call("vault-down").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call = call_id(&r);
    // Authorized, but the protected contact cannot be resolved: nothing is sent.
    assert!(m.telephony.originates().is_empty());
    assert_eq!(
        m.count(&format!(
            "SELECT count(*) FROM call_telephony_commands WHERE call_id = '{call}' \
             AND kind = 'ORIGINATE' AND status = 'PENDING' AND last_error_code = 'GRANT_UNAVAILABLE'"
        ))
        .await,
        1
    );
    m.unrelated_calling_works("vault-down").await;
    // Contact Vault returns: the held ORIGINATE goes out once.
    *m.privacy.grants_unavailable.lock().unwrap() = false;
    m.e.a.h().clock.advance(chrono::Duration::seconds(5));
    m.e.a
        .h()
        .state
        .app
        .dispatch_telephony_commands(50)
        .await
        .unwrap();
    assert_eq!(m.telephony.originates().len(), 1);
}

#[tokio::test]
async fn unprovable_spend_authority_refuses_before_any_paid_command() {
    let Some(m) = matrix(|_| {}).await else {
        return;
    };
    // A database fault inside the spend reservation (the authority cannot be proven).
    common::exec(
        m.e.h().conn(),
        "CREATE FUNCTION test_spend_fault() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'spend store fault' USING ERRCODE = 'internal_error'; END; $$; \
         CREATE TRIGGER test_spend_fault BEFORE INSERT ON call_spend_reservations \
         FOR EACH ROW EXECUTE FUNCTION test_spend_fault();",
    )
    .await
    .unwrap();
    let calls_before = m.count("SELECT count(*) FROM calls").await;
    let r = m.phone_call("spend-fault").await;
    assert!(r.status.is_server_error(), "{}: {}", r.status, r.text);
    assert!(m.telephony.originates().is_empty(), "no paid command");
    assert_eq!(
        m.count("SELECT count(*) FROM call_telephony_commands")
            .await,
        0
    );
    assert_eq!(m.count("SELECT count(*) FROM calls").await, calls_before);
    m.unrelated_calling_works("spend-fault").await;
    common::exec(
        m.e.h().conn(),
        "DROP TRIGGER test_spend_fault ON call_spend_reservations; DROP FUNCTION test_spend_fault();",
    )
    .await
    .unwrap();
    let r = m.phone_call("spend-ok").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(m.telephony.originates().len(), 1);
}

/// The realtime path is down: every publish fails.
struct DownBus {
    inner: InMemorySignalBus,
    attempts: AtomicUsize,
}

#[async_trait::async_trait]
impl RealtimeSignalBus for DownBus {
    async fn publish(&self, _envelope: SignalEnvelope) -> Result<(), SignalBusError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(SignalBusError::Unavailable)
    }

    fn subscribe(&self, endpoint_id: Uuid) -> SignalSubscription {
        self.inner.subscribe(endpoint_id)
    }
}

#[tokio::test]
async fn realtime_fanout_down_loses_signals_but_never_calling() {
    let down = Arc::new(DownBus {
        inner: InMemorySignalBus::new(),
        attempts: AtomicUsize::new(0),
    });
    let bus = down.clone();
    let Some(m) = matrix(move |rt| rt.bus = bus).await else {
        return;
    };
    m.unrelated_calling_works("fanout-down").await;
    let r = m.phone_call("fanout-down-phone").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(m.telephony.executions().len(), 1);
    assert!(
        down.attempts.load(Ordering::SeqCst) > 0,
        "signals were attempted"
    );
}
