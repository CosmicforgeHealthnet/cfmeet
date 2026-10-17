//! Composite layered authorization (Part H §71; ADR 0029 §1): credential → tenant /
//! application / environment status → principal (scoped resolution and category) →
//! Privacy Relay decision → routing / destination / block policy → spend → feature
//! permission → enterprise administration scope. Each case below makes exactly ONE layer
//! refuse while every other layer would allow, and proves the protected side effect did
//! NOT happen: no Call row, no Telephony command or originate, no spend reservation, no
//! Privacy Relay grant, no recording row, no enterprise configuration row. Passing one
//! layer never implies another.

mod common;

use common::NewPrincipal;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use calling::{
    calls::CallState,
    domain::{
        ApplicationStatus, EnvironmentKind, EnvironmentStatus, PrincipalKind, PrincipalStatus,
        TenantStatus,
    },
    enterprise::{policy::AdminCapability, EnterpriseFlags},
    media::provider::ProviderParticipant,
    platform::scopes::names,
    telephony::spend::SpendPolicy,
};
use common::{
    app::{AppWorld, Device},
    enterprise::{credential, EnterpriseWorld, RecordingCapture},
    privacy::{FakePrivacyRelay, Mode},
    public::{spend_policy, PublicWorld},
    telephony::leg_event,
    TestResponse,
};
use serde_json::json;
use uuid::Uuid;

fn code(r: &TestResponse) -> (StatusCode, &str) {
    (r.status, r.json["error"]["code"].as_str().unwrap_or(""))
}

async fn calls(w: &AppWorld) -> i64 {
    w.w.count("SELECT count(*) FROM calls").await
}

#[derive(Clone, Copy, Debug)]
enum Suspend {
    Tenant,
    Application,
    Environment,
}

impl Suspend {
    async fn set(self, w: &AppWorld, active: bool) {
        let p = &w.h().platform;
        let c = &w.w.consumer;
        match self {
            Suspend::Tenant => {
                let s = if active {
                    TenantStatus::Active
                } else {
                    TenantStatus::Suspended
                };
                p.set_tenant_status(c.tenant.id, s).await.unwrap();
            }
            Suspend::Application => {
                let s = if active {
                    ApplicationStatus::Active
                } else {
                    ApplicationStatus::Suspended
                };
                p.set_application_status(c.application.id, s).await.unwrap();
            }
            Suspend::Environment => {
                let s = if active {
                    EnvironmentStatus::Active
                } else {
                    EnvironmentStatus::Suspended
                };
                p.set_environment_status(c.env(EnvironmentKind::Production).id, s)
                    .await
                    .unwrap();
            }
        }
    }
}

/// A valid credential of a suspended scope: no new Call (normalized authentication
/// failure), no read either — but the live Call can still be ended (safety-only).
async fn suspended_scope_is_safety_only(link: Suspend) {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let (live, caller, _) = w.connected_call("VOICE").await;
    let before = calls(&w).await;
    link.set(&w, false).await;
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "suspended",
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::UNAUTHORIZED, "AUTHENTICATION_FAILED"),
        "{link:?}: {}",
        r.text
    );
    assert_eq!(calls(&w).await, before, "{link:?}: a Call was created");
    let r = w.get(&format!("/v1/calls/{live}")).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{link:?}");
    let r = w
        .post(
            "/v1/calling/endpoints",
            json!({ "principalId": w.w.initiator.id(), "endpointKey": "new-device",
                    "endpointKind": "MOBILE", "capabilities": { "audio": true, "video": true } }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{link:?}");
    // Safety: ending the existing live Call is still possible.
    let (_, version) = w.state(live).await;
    let r = w.command(live, "end", version).await;
    assert_eq!(r.status, StatusCode::OK, "{link:?}: {}", r.text);
    assert!(matches!(
        w.state(live).await.0,
        CallState::Ending | CallState::Ended
    ));
    link.set(&w, true).await;
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "reactivated",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{link:?}: {}", r.text);
}

#[tokio::test]
async fn a_suspended_tenant_creates_nothing_but_may_end_a_live_call() {
    suspended_scope_is_safety_only(Suspend::Tenant).await;
}

#[tokio::test]
async fn b_a_suspended_application_creates_nothing_but_may_end_a_live_call() {
    suspended_scope_is_safety_only(Suspend::Application).await;
}

#[tokio::test]
async fn c_a_suspended_environment_creates_nothing_but_may_end_a_live_call() {
    suspended_scope_is_safety_only(Suspend::Environment).await;
}

#[tokio::test]
async fn d_a_principal_of_another_environment_or_inactive_is_refused() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let caller = w.device(&w.w.initiator, "caller-main").await;
    w.device(&w.w.recipient, "callee-main").await;
    let h = w.h();
    // The same application's SANDBOX principal does not exist in PRODUCTION.
    let (sandbox, _) = h
        .platform
        .register_principal(
            w.w.consumer.env(EnvironmentKind::Sandbox).id,
            NewPrincipal::External {
                kind: PrincipalKind::ExternalApplicationPrincipal,
                external_reference: "sandbox-user".into(),
            },
        )
        .await
        .unwrap();
    // The principal layer alone: resolution in the credential's scope does not find it.
    let r = h
        .request(
            Method::POST,
            "/test/resolve",
            Some(w.secret()),
            Some(json!({ "id": sandbox.id })),
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
    let post = |body: serde_json::Value, key: &'static str| {
        let w = &w;
        async move {
            w.post_with("/v1/calls", body, &[("idempotency-key", key)])
                .await
        }
    };
    let r = post(
        json!({ "kind": "VOICE", "initiatorPrincipalId": w.w.initiator.id(),
                "initiatorEndpointId": caller.endpoint, "targetPrincipalIds": [sandbox.id] }),
        "d-target-env",
    )
    .await;
    assert_eq!(code(&r), (StatusCode::NOT_FOUND, "NOT_FOUND"), "{}", r.text);
    let r = post(
        json!({ "kind": "VOICE", "initiatorPrincipalId": sandbox.id,
                "initiatorEndpointId": caller.endpoint,
                "targetPrincipalIds": [w.w.recipient.id()] }),
        "d-initiator-env",
    )
    .await;
    assert_eq!(code(&r), (StatusCode::NOT_FOUND, "NOT_FOUND"), "{}", r.text);
    // Inactive principals: a suspended initiator is not authorized, a deactivated target
    // is unavailable.
    h.platform
        .set_principal_status(w.w.initiator.id(), PrincipalStatus::Suspended)
        .await
        .unwrap();
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "d-suspended",
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "CALL_NOT_AUTHORIZED"),
        "{}",
        r.text
    );
    h.platform
        .set_principal_status(w.w.initiator.id(), PrincipalStatus::Active)
        .await
        .unwrap();
    h.platform
        .set_principal_status(w.w.recipient.id(), PrincipalStatus::Deactivated)
        .await
        .unwrap();
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "d-deactivated",
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::CONFLICT, "TARGET_UNAVAILABLE"),
        "{}",
        r.text
    );
    assert_eq!(calls(&w).await, 0);
}

async fn protected_world() -> Option<(AppWorld, Arc<FakePrivacyRelay>, Device)> {
    let fake = Arc::new(FakePrivacyRelay::new(Vec::new()));
    let w = AppWorld::with_privacy_relay(fake.clone()).await?;
    fake.allow(w.w.initiator.id().as_uuid());
    let caller = w.device(&w.w.initiator, "caller-main").await;
    w.device(&w.w.recipient, "callee-main").await;
    Some((w, fake, caller))
}

#[tokio::test]
async fn e_a_principal_outside_the_privacy_relay_context_is_refused() {
    let Some((w, fake, caller)) = protected_world().await else {
        return;
    };
    // The recipient is a valid ACTIVE principal of the scope — but not authorized by the
    // context.
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "e-1",
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "CALL_NOT_AUTHORIZED"),
        "{}",
        r.text
    );
    assert_eq!(fake.requests().len(), 1, "Privacy Relay decided");
    assert!(fake.grants().is_empty());
    assert_eq!(calls(&w).await, 0);
    // Authorized by the context: the same request goes through.
    fake.allow(w.w.recipient.id().as_uuid());
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "e-2",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
}

#[tokio::test]
async fn f_a_direction_the_context_forbids_is_refused() {
    let Some((w, fake, caller)) = protected_world().await else {
        return;
    };
    fake.allow(w.w.recipient.id().as_uuid());
    fake.set_mode(Mode::Deny("DIRECTION_NOT_ALLOWED"));
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "f-1",
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "CALL_NOT_AUTHORIZED"),
        "{}",
        r.text
    );
    assert!(!r.text.contains("DIRECTION"), "the reason is not disclosed");
    assert!(fake.grants().is_empty());
    assert_eq!(calls(&w).await, 0);
    assert_eq!(w.w.count("SELECT count(*) FROM call_invitations").await, 0);
}

#[tokio::test]
async fn g_a_blocked_destination_is_refused_before_privacy_relay_and_spend() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let r = x
        .block(
            x.w.secret(),
            json!({ "principalId": x.w.w.initiator.id(), "blockedContactReferenceId": x.contact,
                    "reasonCode": "SPAM" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = x.call_to(x.contact, "g-1").await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "PUBLIC_ROUTE_DENIED"),
        "{}",
        r.text
    );
    assert!(x.privacy.requests().is_empty(), "Privacy Relay was asked");
    assert!(x.privacy.relay_identities.lock().unwrap().is_empty());
    assert!(x.telephony.originates().is_empty());
    assert_eq!(x.effects().await, (0, 0, 0));
}

#[tokio::test]
async fn h_exhausted_spend_is_refused_before_any_telephony_command() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    // Room for exactly one leg allowance today.
    x.set_spend(SpendPolicy {
        daily_limit_minor: 5_000,
        ..spend_policy()
    })
    .await;
    let ok = x.call_to(x.contact, "h-1").await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.text);
    assert_eq!(x.effects().await, (1, 1, 1));
    let r = x.call_to(x.contact, "h-2").await;
    assert_eq!(
        code(&r),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "TELECOM_SPEND_LIMIT_EXCEEDED"
        ),
        "{}",
        r.text
    );
    assert_eq!(x.effects().await, (1, 1, 1), "a second paid effect exists");
    assert_eq!(x.telephony.originates().len(), 1);
    assert_eq!(x.privacy.grants().len(), 1);
}

#[tokio::test]
async fn i_permission_to_call_is_not_permission_to_record() {
    let capture = Arc::new(RecordingCapture::default());
    let c = capture.clone();
    let Some(e) = EnterpriseWorld::configured(move |runtime| {
        runtime.enterprise_flags = EnterpriseFlags::ALL_ON;
        runtime.artifacts = Arc::new(cf_object_storage::MemoryObjectStore::new());
        runtime.capture = c;
    })
    .await
    else {
        return;
    };
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            e.a.w.scope(),
            vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageRecording,
            ],
        )
        .await
        .unwrap();
    let caller = e.member("rec-caller").await;
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let callee = e.member("rec-callee").await;
    let callee_phone = e.a.device(&callee, "callee-desk").await;
    e.extension("4410", "ENTERPRISE_MEMBER", callee.id()).await;
    let r = e
        .call_extension("VOICE", &caller, &caller_phone, "4410", "i-1")
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let inv =
        e.a.invitation_for(call, callee_phone.endpoint)
            .await
            .unwrap();
    let (_, v) = e.a.state(call).await;
    assert_eq!(
        e.a.accept(call, inv, &callee_phone, v).await.status,
        StatusCode::OK
    );
    for d in [&caller_phone, &callee_phone] {
        let s = e.a.session_on(call, d.endpoint).await.unwrap();
        e.a.connect(call, s).await;
    }
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
    // The caller's credential placed the Call; it holds no recording authority.
    let r = e
        .send(
            e.caller_token(),
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({ "principalId": caller.id() })),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    // A recording manager without a recording policy is refused too.
    let manager = credential(
        e.h(),
        &e.a.w.consumer,
        e.a.w.kind,
        &[
            names::ENTERPRISE_RECORDING_MANAGE,
            names::ENTERPRISE_RECORDING_READ,
        ],
    )
    .await;
    let r = e
        .send(
            manager.secret.expose_once(),
            Method::POST,
            &format!("/v1/calls/{call}/recordings"),
            Some(json!({ "principalId": caller.id() })),
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "RECORDING_NOT_AUTHORIZED"),
        "{}",
        r.text
    );
    assert_eq!(
        e.count(&format!(
            "SELECT count(*) FROM call_recordings WHERE call_id = '{call}'"
        ))
        .await,
        0
    );
    assert!(capture.started().is_empty(), "media capture started");
    assert_eq!(e.a.state(call).await.0, CallState::Connected);
}

#[tokio::test]
async fn j_calling_authority_never_configures_the_enterprise() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let member = e.member("j-member").await;
    let body = json!({ "extensionNumber": "4500", "displayLabel": "Front desk",
                       "destination": { "type": "ENTERPRISE_MEMBER", "id": member.id().to_string() } });
    for token in [
        e.caller_token(),
        e.reader.secret.expose_once(),
        e.a.secret(),
    ] {
        let r = e
            .send(
                token,
                Method::POST,
                "/v1/enterprise/extensions",
                Some(body.clone()),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text);
    }
    assert_eq!(
        e.count("SELECT count(*) FROM enterprise_extensions").await,
        0
    );
    // The administrator may.
    let r = e
        .admin(Method::POST, "/v1/enterprise/extensions", Some(body))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    assert_eq!(
        e.count("SELECT count(*) FROM enterprise_extensions").await,
        1
    );
}

#[tokio::test]
async fn k_a_provider_or_telephony_callback_never_creates_or_mutates_an_unauthorized_call() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let w = &x.w;
    w.device(&w.w.recipient, "callee-main").await;
    let r = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &x.caller,
            &[&w.w.recipient],
            "k-ringing",
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    let before = w.state(call).await;
    let snapshot = |x: &PublicWorld| {
        let conn = x.w.h().conn().clone();
        async move {
            let n = |sql: &'static str| {
                let conn = conn.clone();
                async move { common::scalar_i64(&conn, sql).await }
            };
            (
                n("SELECT count(*) FROM calls").await,
                n("SELECT count(*) FROM call_participants").await,
                n("SELECT count(*) FROM call_sessions WHERE state = 'CONNECTED'").await,
                n("SELECT count(*) FROM call_legs").await,
            )
        }
    };
    let rows = snapshot(&x).await;

    // Media provider (valid signature): a join in a room of a Call that does not exist,
    // and an unknown identity joining the real Call's room.
    let stranger = ProviderParticipant {
        identity: format!("cfm-{}", Uuid::new_v4().simple()),
        generation: None,
        permissions: calling::media::MediaPermissions::NONE,
        active: true,
        published_audio: true,
        published_video: false,
        joined_at: Some(chrono::Utc::now()),
    };
    for room in [AppWorld::room(Uuid::new_v4()), AppWorld::room(call)] {
        w.media().inject_room(&room);
        w.media().inject(&room, stranger.clone());
        let (auth, body) = w
            .media()
            .signed_event("participant_joined", &room, Some(&stranger));
        let r = w.callback(&auth, body).await;
        assert!(!r.status.is_server_error(), "{}", r.text);
    }
    // Telephony (valid service signature): legs and Calls it was never commanded for.
    for (call_id, leg) in [(Uuid::new_v4(), Uuid::new_v4()), (call, Uuid::new_v4())] {
        for event in ["LEG_ANSWERED", "LEG_CONNECTED"] {
            let r = x
                .as_telephony(
                    "/internal/v1/telephony/events",
                    leg_event(event, call_id, leg, Uuid::new_v4(), None, 1),
                )
                .await;
            assert_eq!(r.status, StatusCode::OK, "{}", r.text);
            assert_eq!(r.json["disposition"], "UNMATCHED", "{}", r.text);
        }
    }
    assert_eq!(
        snapshot(&x).await,
        rows,
        "a callback created or changed Call rows"
    );
    assert_eq!(w.state(call).await, before, "a callback mutated the Call");
    assert!(x.telephony.originates().is_empty());
    assert!(x.privacy.grants().is_empty());
}

#[tokio::test]
async fn l_an_inbound_public_offer_into_a_suspended_scope_creates_no_call() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    x.configure_owner().await;
    let recipient = x.w.w.recipient.clone();
    x.privacy.allow(recipient.id().as_uuid());
    x.w.device(&recipient, "callee-main").await;
    let offer = |x: &PublicWorld| x.offer(Uuid::new_v4(), x.contact, &[recipient.id().as_uuid()]);
    for link in [Suspend::Tenant, Suspend::Application, Suspend::Environment] {
        link.set(&x.w, false).await;
        let r = x
            .as_telephony("/internal/v1/telephony/inbound-calls", offer(&x))
            .await;
        assert!(!r.status.is_success(), "{link:?}: {}", r.text);
        assert_eq!(x.count("SELECT count(*) FROM calls").await, 0, "{link:?}");
        link.set(&x.w, true).await;
    }
    assert!(x.privacy.requests().is_empty(), "Privacy Relay was asked");
    // Active again, the same offer creates the Call: the refusals were the scope.
    let r = x
        .as_telephony("/internal/v1/telephony/inbound-calls", offer(&x))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(x.count("SELECT count(*) FROM calls").await, 1);
    let decisions = x.privacy.requests().len();
    // The owner credential revoked: no Call either.
    x.w.h()
        .platform
        .revoke_credential(x.w.w.token.credential.id)
        .await
        .unwrap();
    let r = x
        .as_telephony("/internal/v1/telephony/inbound-calls", offer(&x))
        .await;
    assert!(!r.status.is_success(), "{}", r.text);
    assert_eq!(x.count("SELECT count(*) FROM calls").await, 1);
    assert_eq!(x.privacy.requests().len(), decisions);
    assert!(x.telephony.originates().is_empty());
}

#[tokio::test]
async fn m_an_enterprise_sip_offer_into_a_suspended_scope_creates_no_call() {
    let Some(e) = EnterpriseWorld::new().await else {
        return;
    };
    let caller = e.member("sip-caller").await;
    let callee = e.member("sip-callee").await;
    e.a.device(&callee, "callee-desk").await;
    e.extension("4600", "ENTERPRISE_MEMBER", callee.id()).await;
    let r = e
        .admin(
            Method::POST,
            "/v1/enterprise/sip-devices",
            Some(
                json!({ "deviceRef": "dev-scope-01", "principalId": caller.id(),
                         "displayLabel": "Scope desk" }),
            ),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let credential = e.a.w.token.credential.id.as_uuid();
    common::exec(
        e.h().conn(),
        &format!(
            "INSERT INTO call_inbound_owners (tenant_id, application_id, environment_id, \
               service_account_id, credential_id, enabled, revision, created_at, updated_at) \
             SELECT tenant_id, application_id, environment_id, service_account_id, id, true, 1, \
               now(), now() FROM platform_api_credentials WHERE id = '{credential}'"
        ),
    )
    .await
    .unwrap();
    let env = e.a.w.consumer.env(e.a.w.kind).id.as_uuid();
    let offer = || calling::calls::app::enterprise::EnterpriseDialogOffer {
        schema_version: "1".into(),
        telephony_leg_id: Uuid::new_v4(),
        correlation_id: format!("corr_{}", Uuid::new_v4().simple()),
        scope: calling::calls::app::enterprise::OfferScope {
            tenant_id: e.a.w.consumer.tenant.id.as_uuid(),
            application_id: e.a.w.consumer.application.id.as_uuid(),
            environment_id: env,
        },
        device_ref: "dev-scope-01".into(),
        extension: "4600".into(),
        max_duration_seconds: 600,
    };
    for link in [Suspend::Tenant, Suspend::Application, Suspend::Environment] {
        link.set(&e.a, false).await;
        let refused = e.h().state.app.accept_enterprise_offer(&offer()).await;
        assert!(refused.is_err(), "{link:?}: {refused:?}");
        assert_eq!(e.count("SELECT count(*) FROM calls").await, 0, "{link:?}");
        link.set(&e.a, true).await;
    }
    // Active again: the same offer creates the Call (the refusals were the scope).
    let answer = e.h().state.app.accept_enterprise_offer(&offer()).await;
    assert!(answer.is_ok(), "{answer:?}");
    assert_eq!(e.count("SELECT count(*) FROM calls").await, 1);
}

#[tokio::test]
async fn n_a_service_account_principal_never_initiates_or_receives_an_app_call() {
    let Some(w) = AppWorld::new().await else {
        return;
    };
    let h = w.h();
    let (sa, _) = h
        .platform
        .register_principal(
            w.w.consumer.env(EnvironmentKind::Production).id,
            NewPrincipal::ServiceAccount(w.w.consumer.sa(EnvironmentKind::Production)),
        )
        .await
        .unwrap();
    assert_eq!(sa.kind, PrincipalKind::ServiceAccount);
    let callee = w.device(&w.w.recipient, "callee-main").await;
    let r = w
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": sa.id,
                    "initiatorEndpointId": callee.endpoint,
                    "targetPrincipalIds": [w.w.recipient.id()] }),
            &[("idempotency-key", "n-initiator")],
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "PRINCIPAL_NOT_ELIGIBLE"),
        "{}",
        r.text
    );
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let r = w
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": w.w.initiator.id(),
                    "initiatorEndpointId": caller.endpoint, "targetPrincipalIds": [sa.id] }),
            &[("idempotency-key", "n-target")],
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::CONFLICT, "TARGET_UNAVAILABLE"),
        "{}",
        r.text
    );
    assert_eq!(calls(&w).await, 0);
}

#[tokio::test]
async fn o_a_telephone_endpoint_principal_never_initiates_an_app_or_public_call() {
    let Some(x) = PublicWorld::new().await else {
        return;
    };
    let w = &x.w;
    let (phone, _) = w
        .h()
        .platform
        .register_principal(
            w.w.consumer.env(EnvironmentKind::Production).id,
            NewPrincipal::External {
                kind: PrincipalKind::TelephoneEndpoint,
                external_reference: Uuid::new_v4().to_string(),
            },
        )
        .await
        .unwrap();
    x.privacy.allow(phone.id.as_uuid());
    let r = w
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": phone.id,
                    "initiatorEndpointId": x.caller.endpoint,
                    "targetPrincipalIds": [w.w.recipient.id()] }),
            &[("idempotency-key", "o-app")],
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "PRINCIPAL_NOT_ELIGIBLE"),
        "{}",
        r.text
    );
    let r = w
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": phone.id,
                    "initiatorEndpointId": x.caller.endpoint,
                    "targetContactReference": x.contact,
                    "privacyRelaySessionId": x.session }),
            &[("idempotency-key", "o-public")],
        )
        .await;
    assert_eq!(
        code(&r),
        (StatusCode::FORBIDDEN, "PRINCIPAL_NOT_ELIGIBLE"),
        "{}",
        r.text
    );
    assert!(x.privacy.requests().is_empty());
    assert_eq!(x.effects().await, (0, 0, 0));
}
