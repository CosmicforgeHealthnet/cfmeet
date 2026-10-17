//! Application-calling fixtures (Part C §20–§25): endpoints, calls created through the
//! public API, ring offers, acceptance, media credentials and simulated client joins on
//! the test media provider (signed callbacks go through the real callback route).

use axum::http::{Method, StatusCode};
use calling::{
    calls::{ids::CallId, CallState},
    media::provider::{MediaResourceName, ProviderParticipant},
    platform::principals::ResolvedPrincipal,
};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{calls::CallWorld, fake_media::FakeMediaProvider, Harness, TestResponse};

/// A CallWorld plus application-calling helpers.
pub struct AppWorld {
    pub w: CallWorld,
}

/// A registered endpoint of a principal.
#[derive(Debug, Clone)]
pub struct Device {
    pub principal: Uuid,
    pub endpoint: Uuid,
}

impl AppWorld {
    pub async fn new() -> Option<Self> {
        Some(Self {
            w: CallWorld::new().await?,
        })
    }

    /// A world whose Calling deployment uses `privacy` as its Privacy Relay authority.
    pub async fn with_privacy_relay(
        privacy: std::sync::Arc<dyn calling::privacy_relay::PrivacyRelayAuthority>,
    ) -> Option<Self> {
        use std::sync::Arc;
        let db = Arc::new(super::TestDb::new().await?);
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = super::test_runtime(Some(media.clone()), bus.clone());
        runtime.privacy_relay = privacy;
        let h = Harness::with_runtime(
            db,
            calling::domain::ManualClock::new(super::t0()),
            media,
            bus,
            runtime,
        );
        let w =
            CallWorld::in_harness(h, "acme", calling::domain::EnvironmentKind::Production).await;
        Some(Self { w })
    }

    /// A world whose Calling deployment executes public-network legs through `telephony`
    /// (and attaches bridges with `bridge`), with Privacy Relay authority `privacy` and
    /// every telecom switch ON. The Production environment is live-telecom allowed.
    pub async fn with_public_network(
        privacy: std::sync::Arc<dyn calling::privacy_relay::PrivacyRelayAuthority>,
        telephony: std::sync::Arc<dyn calling::telephony::TelephonyPort>,
        bridge: Option<std::sync::Arc<dyn calling::telephony::media::PublicMediaGateway>>,
    ) -> Option<Self> {
        Self::with_public_network_configured(privacy, telephony, bridge, |_| {}).await
    }

    /// [`with_public_network`](Self::with_public_network) whose runtime is further
    /// adjusted by `configure` (e.g. the compliance overlay or abuse thresholds).
    pub async fn with_public_network_configured(
        privacy: std::sync::Arc<dyn calling::privacy_relay::PrivacyRelayAuthority>,
        telephony: std::sync::Arc<dyn calling::telephony::TelephonyPort>,
        bridge: Option<std::sync::Arc<dyn calling::telephony::media::PublicMediaGateway>>,
        configure: impl FnOnce(&mut calling::state::CallingRuntime),
    ) -> Option<Self> {
        Self::with_public_network_in(
            calling::domain::EnvironmentKind::Production,
            privacy,
            telephony,
            bridge,
            configure,
        )
        .await
    }

    /// As [`Self::with_public_network_configured`] in an environment of `kind` (only a
    /// PRODUCTION environment is allowed live telecom).
    pub async fn with_public_network_in(
        kind: calling::domain::EnvironmentKind,
        privacy: std::sync::Arc<dyn calling::privacy_relay::PrivacyRelayAuthority>,
        telephony: std::sync::Arc<dyn calling::telephony::TelephonyPort>,
        bridge: Option<std::sync::Arc<dyn calling::telephony::media::PublicMediaGateway>>,
        configure: impl FnOnce(&mut calling::state::CallingRuntime),
    ) -> Option<Self> {
        use std::sync::Arc;
        let db = Arc::new(super::TestDb::new().await?);
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = super::test_runtime(Some(media.clone()), bus.clone());
        runtime.privacy_relay = privacy;
        runtime.public_network = calling::calls::app::public::PublicNetwork {
            synthetic_interconnect: false,
            telephony,
            media: bridge,
            flags: calling::routing::engine::TelecomFlags {
                public_outbound: true,
                public_inbound: true,
                callback_relay: true,
            },
        };
        configure(&mut runtime);
        let h = Harness::with_runtime(
            db,
            calling::domain::ManualClock::new(chrono::Utc::now()),
            media,
            bus,
            runtime,
        );
        let w = CallWorld::in_harness(h, "acme", kind).await;
        if kind == calling::domain::EnvironmentKind::Production {
            w.h.platform
                .set_live_telecom_allowed(
                    w.consumer
                        .env(calling::domain::EnvironmentKind::Production)
                        .id,
                    true,
                )
                .await
                .unwrap();
        }
        Some(Self { w })
    }

    /// A world whose Calling deployment reaches a Privacy Relay over HTTP at `base_url`
    /// (Calling's real `HttpPrivacyRelay` adapter, signed service tokens).
    pub async fn with_privacy_relay_at(base_url: &str) -> Option<Self> {
        let (service_auth, peers) = super::service_auth_fixture_targeting(&[(
            cf_service_auth::ServiceId::privacy_relay(),
            base_url,
        )]);
        Self::with_service_auth(service_auth, peers, super::t0()).await
    }

    /// A world over an explicit service-auth runtime whose Privacy Relay authority is the
    /// HTTP adapter of that runtime; the Calling clock starts at `now`.
    pub async fn with_service_auth(
        service_auth: calling::internal_auth::ServiceAuthRuntime,
        peers: super::Peers,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<Self> {
        Self::with_service_auth_for(
            service_auth,
            peers,
            now,
            None,
            calling::domain::TenantClassification::External,
        )
        .await
    }

    /// [`with_service_auth`](Self::with_service_auth) for a tenant of `classification`,
    /// optionally minting Contact Vault access tokens with `contact_delegation`.
    pub async fn with_service_auth_for(
        service_auth: calling::internal_auth::ServiceAuthRuntime,
        peers: super::Peers,
        now: chrono::DateTime<chrono::Utc>,
        contact_delegation: Option<std::sync::Arc<cf_service_auth::ContactDelegationSigner>>,
        classification: calling::domain::TenantClassification,
    ) -> Option<Self> {
        use std::sync::Arc;
        let db = Arc::new(super::TestDb::new().await?);
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = super::test_runtime(Some(media.clone()), bus.clone());
        runtime.privacy_relay = Arc::new(calling::privacy_relay::HttpPrivacyRelay::new(
            service_auth.clone(),
        ));
        let push = Arc::new(super::RecordingPush::default());
        runtime.push = push.clone();
        runtime.contact_delegation = contact_delegation;
        let h = Harness::with_service_auth(
            db,
            calling::domain::ManualClock::new(now),
            media,
            bus,
            runtime,
            push,
            service_auth,
            peers,
        );
        let consumer = h.consumer("acme", classification).await;
        let w = CallWorld::for_consumer(h, consumer, calling::domain::EnvironmentKind::Production)
            .await;
        Some(Self { w })
    }

    /// A world whose Calling deployment reaches Privacy Relay AND Telephony over HTTP
    /// (the real adapters, signed service tokens), with every telecom switch ON and the
    /// Production environment live-telecom allowed: the cross-service end to end stack.
    pub async fn with_service_auth_public_network(
        service_auth: calling::internal_auth::ServiceAuthRuntime,
        peers: super::Peers,
        now: chrono::DateTime<chrono::Utc>,
        contact_delegation: Option<std::sync::Arc<cf_service_auth::ContactDelegationSigner>>,
    ) -> Option<Self> {
        Self::with_service_auth_public_network_configured(
            service_auth,
            peers,
            now,
            contact_delegation,
            |_| {},
        )
        .await
    }

    /// [`with_service_auth_public_network`](Self::with_service_auth_public_network) whose
    /// runtime is further adjusted by `configure` (e.g. protected artifact storage).
    pub async fn with_service_auth_public_network_configured(
        service_auth: calling::internal_auth::ServiceAuthRuntime,
        peers: super::Peers,
        now: chrono::DateTime<chrono::Utc>,
        contact_delegation: Option<std::sync::Arc<cf_service_auth::ContactDelegationSigner>>,
        configure: impl FnOnce(&mut calling::state::CallingRuntime),
    ) -> Option<Self> {
        use std::sync::Arc;
        let db = Arc::new(super::TestDb::new().await?);
        let media = Arc::new(FakeMediaProvider::default());
        let bus = Arc::new(calling::signaling::InMemorySignalBus::new());
        let mut runtime = super::test_runtime(Some(media.clone()), bus.clone());
        runtime.privacy_relay = Arc::new(calling::privacy_relay::HttpPrivacyRelay::new(
            service_auth.clone(),
        ));
        runtime.public_network = calling::calls::app::public::PublicNetwork {
            synthetic_interconnect: false,
            telephony: Arc::new(calling::telephony::HttpTelephony::new(service_auth.clone())),
            media: None,
            flags: calling::routing::engine::TelecomFlags {
                public_outbound: true,
                public_inbound: true,
                callback_relay: true,
            },
        };
        let push = Arc::new(super::RecordingPush::default());
        runtime.push = push.clone();
        runtime.contact_delegation = contact_delegation;
        configure(&mut runtime);
        let h = Harness::with_service_auth(
            db,
            calling::domain::ManualClock::new(now),
            media,
            bus,
            runtime,
            push,
            service_auth,
            peers,
        );
        let consumer = h
            .consumer("acme", calling::domain::TenantClassification::External)
            .await;
        let w = CallWorld::for_consumer(h, consumer, calling::domain::EnvironmentKind::Production)
            .await;
        w.h.platform
            .set_live_telecom_allowed(
                w.consumer
                    .env(calling::domain::EnvironmentKind::Production)
                    .id,
                true,
            )
            .await
            .unwrap();
        Some(Self { w })
    }

    /// `POST /v1/calls` of a PROTECTED call under `session`.
    pub async fn create_protected_call(
        &self,
        kind: &str,
        initiator: &ResolvedPrincipal,
        from: &Device,
        targets: &[&ResolvedPrincipal],
        session: Uuid,
        key: &str,
    ) -> TestResponse {
        self.post_with(
            "/v1/calls",
            json!({
                "kind": kind,
                "initiatorPrincipalId": initiator.id(),
                "initiatorEndpointId": from.endpoint,
                "targetPrincipalIds": targets.iter().map(|t| t.id()).collect::<Vec<_>>(),
                "privacyRelaySessionId": session,
            }),
            &[("idempotency-key", key)],
        )
        .await
    }

    pub fn h(&self) -> &Harness {
        &self.w.h
    }

    pub fn media(&self) -> &FakeMediaProvider {
        &self.w.h.media
    }

    pub fn secret(&self) -> &str {
        self.w.secret()
    }

    pub async fn post(&self, path: &str, body: Value) -> TestResponse {
        self.h()
            .request(Method::POST, path, Some(self.secret()), Some(body))
            .await
    }

    pub async fn post_with(
        &self,
        path: &str,
        body: Value,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.h()
            .request_with(Method::POST, path, Some(self.secret()), Some(body), headers)
            .await
    }

    pub async fn get(&self, path: &str) -> TestResponse {
        self.h()
            .request(Method::GET, path, Some(self.secret()), None)
            .await
    }

    pub async fn principal(&self, reference: &str) -> ResolvedPrincipal {
        self.w.principal(reference).await
    }

    /// Registers an endpoint (audio + video) for `principal`.
    pub async fn device(&self, principal: &ResolvedPrincipal, key: &str) -> Device {
        let r = self
            .post(
                "/v1/calling/endpoints",
                json!({
                    "principalId": principal.id(),
                    "endpointKey": key,
                    "endpointKind": "MOBILE",
                    "capabilities": { "audio": true, "video": true },
                }),
            )
            .await;
        assert!(
            r.status == StatusCode::CREATED || r.status == StatusCode::OK,
            "{}",
            r.text
        );
        Device {
            principal: principal.id().as_uuid(),
            endpoint: r.json["endpointId"].as_str().unwrap().parse().unwrap(),
        }
    }

    /// `POST /v1/calls` (VOICE / VIDEO) from `initiator` on `from` to `targets`.
    pub async fn create_call(
        &self,
        kind: &str,
        initiator: &ResolvedPrincipal,
        from: &Device,
        targets: &[&ResolvedPrincipal],
        key: &str,
    ) -> TestResponse {
        self.post_with(
            "/v1/calls",
            json!({
                "kind": kind,
                "initiatorPrincipalId": initiator.id(),
                "initiatorEndpointId": from.endpoint,
                "targetPrincipalIds": targets.iter().map(|t| t.id()).collect::<Vec<_>>(),
            }),
            &[("idempotency-key", key)],
        )
        .await
    }

    pub async fn call_view(&self, call: Uuid) -> Value {
        let r = self.get(&format!("/v1/calls/{call}")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        r.json
    }

    pub async fn state(&self, call: Uuid) -> (CallState, i64) {
        let c = self.w.call(CallId::from_uuid(call)).await;
        (c.state, c.version)
    }

    /// Live (RINGING) invitations of `call` for `endpoint`.
    pub async fn invitation_for(&self, call: Uuid, endpoint: Uuid) -> Option<Uuid> {
        self.call_view(call).await["invitations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["endpointId"] == endpoint.to_string() && i["status"] == "RINGING")
            .map(|i| i["invitationId"].as_str().unwrap().parse().unwrap())
    }

    pub async fn accept(
        &self,
        call: Uuid,
        invitation: Uuid,
        device: &Device,
        version: i64,
    ) -> TestResponse {
        self.post(
            &format!("/v1/calls/{call}/accept"),
            json!({ "invitationId": invitation, "endpointId": device.endpoint, "expectedVersion": version }),
        )
        .await
    }

    pub async fn decline(
        &self,
        call: Uuid,
        invitation: Uuid,
        device: &Device,
        version: i64,
    ) -> TestResponse {
        self.post(
            &format!("/v1/calls/{call}/decline"),
            json!({ "invitationId": invitation, "endpointId": device.endpoint, "expectedVersion": version }),
        )
        .await
    }

    pub async fn command(&self, call: Uuid, action: &str, version: i64) -> TestResponse {
        self.post(
            &format!("/v1/calls/{call}/{action}"),
            json!({ "expectedVersion": version }),
        )
        .await
    }

    pub async fn media_credential(&self, call: Uuid, session: Uuid) -> TestResponse {
        self.post(
            &format!("/v1/calls/{call}/sessions/{session}/media-credentials"),
            json!({}),
        )
        .await
    }

    /// The session of `participant_principal` on `endpoint` in `call`.
    pub async fn session_on(&self, call: Uuid, endpoint: Uuid) -> Option<Uuid> {
        self.call_view(call).await["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["endpointId"] == endpoint.to_string() && s["state"] != "ENDED")
            .map(|s| s["sessionId"].as_str().unwrap().parse().unwrap())
            .next()
    }

    pub fn room(call: Uuid) -> String {
        MediaResourceName::for_call(CallId::from_uuid(call))
            .as_str()
            .to_string()
    }

    /// Delivers a signed provider callback through the real callback route.
    pub async fn callback(&self, authorization: &str, body: Vec<u8>) -> TestResponse {
        let body: Value = serde_json::from_slice(&body).unwrap();
        self.h()
            .request_with(
                Method::POST,
                "/internal/provider/media/events",
                None,
                Some(body),
                &[("authorization", authorization)],
            )
            .await
    }

    /// A client joins with a media credential; the provider then reports the join (signed
    /// callback). Returns the provider participant.
    pub async fn join(&self, call: Uuid, token: &str) -> ProviderParticipant {
        let participant = self.media().join(token).expect("client joins");
        let (auth, body) =
            self.media()
                .signed_event("participant_joined", &Self::room(call), Some(&participant));
        let r = self.callback(&auth, body).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        self.media()
            .participant(&Self::room(call), &participant.identity)
            .unwrap_or(participant)
    }

    /// Credential + join for a session; returns the provider participant.
    pub async fn connect(&self, call: Uuid, session: Uuid) -> ProviderParticipant {
        let r = self.media_credential(call, session).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        self.join(call, r.json["token"].as_str().unwrap()).await
    }

    /// A ringing one-to-one VOICE call between the world's initiator and recipient, each
    /// with one device: (call id, initiator device, recipient device).
    pub async fn ringing_call(&self, kind: &str) -> (Uuid, Device, Device) {
        // One stable device per party: helpers never pile up endpoints past the ringing cap.
        let caller = self.device(&self.w.initiator, "caller-main").await;
        let callee = self.device(&self.w.recipient, "callee-main").await;
        let r = self
            .create_call(
                kind,
                &self.w.initiator,
                &caller,
                &[&self.w.recipient],
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        assert_eq!(r.json["call"]["state"], "RINGING", "{}", r.text);
        (
            r.json["call"]["callId"].as_str().unwrap().parse().unwrap(),
            caller,
            callee,
        )
    }

    /// A CONNECTED one-to-one call with both parties admitted in the media resource.
    pub async fn connected_call(&self, kind: &str) -> (Uuid, Device, Device) {
        let (call, caller, callee) = self.ringing_call(kind).await;
        let invitation = self.invitation_for(call, callee.endpoint).await.unwrap();
        let (_, version) = self.state(call).await;
        let r = self.accept(call, invitation, &callee, version).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        let caller_session = self.session_on(call, caller.endpoint).await.unwrap();
        let callee_session = self.session_on(call, callee.endpoint).await.unwrap();
        self.connect(call, caller_session).await;
        self.connect(call, callee_session).await;
        assert_eq!(self.state(call).await.0, CallState::Connected);
        (call, caller, callee)
    }

    /// A short-lived signaling credential for `device`.
    pub async fn signaling_token(&self, device: &Device) -> String {
        let r = self
            .post(
                "/v1/calling/signaling-credentials",
                json!({ "principalId": device.principal, "endpointId": device.endpoint }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["token"].as_str().unwrap().to_string()
    }

    /// A request authenticated with a signaling credential.
    pub async fn as_endpoint(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
        headers: &[(&str, &str)],
    ) -> TestResponse {
        self.h()
            .request_with(method, path, Some(token), body, headers)
            .await
    }

    /// `POST /v1/calls/{call}/transfer` of `participant`'s place to `target`.
    pub async fn transfer(
        &self,
        call: Uuid,
        participant: Uuid,
        target: &ResolvedPrincipal,
        version: i64,
        key: &str,
    ) -> TestResponse {
        self.post_with(
            &format!("/v1/calls/{call}/transfer"),
            json!({
                "transferringParticipantId": participant,
                "targetPrincipalId": target.id(),
                "expectedVersion": version,
            }),
            &[("idempotency-key", key)],
        )
        .await
    }

    /// The (active) participant of `principal` in `call`.
    pub async fn participant_of(&self, call: Uuid, principal: Uuid) -> Uuid {
        self.call_view(call).await["participants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["principalId"] == principal.to_string() && p["endedAt"].is_null())
            .map(|p| p["participantId"].as_str().unwrap().parse().unwrap())
            .expect("participant")
    }
}
