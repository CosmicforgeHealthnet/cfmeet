//! A fully provisioned Calling deployment for certification tests (Part K): one database,
//! the scripted Privacy Relay and Telephony behind deterministic gates, public-network
//! calling switched on (live-telecom PRODUCTION environment, NG routing and spend
//! policies), enterprise calling with artifact storage and media capture, customer
//! webhooks with a sealed key ring, and every scope the platform registry offers allowed
//! for the application. Further deployments ("instances") can be started over the same
//! database with the same external fakes — e.g. after a restore, or as a second worker
//! instance.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    routing::post,
    Router,
};
use calling::{
    calls::app::enterprise::CapturedMedia,
    domain::{EnvironmentKind, ManualClock},
    platform::scopes::names,
    signaling::InMemorySignalBus,
    state::CallingRuntime,
    webhooks::WebhookSettings,
};
use cf_object_storage::MemoryObjectStore;
use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{
    app::{AppWorld, Device},
    calls::CallWorld,
    enterprise::{allow_enterprise_scopes, EnterpriseWorld, RecordingCapture},
    fake_media::FakeMediaProvider,
    gates::{GatedPrivacy, GatedTelephony, Timeline},
    privacy::FakePrivacyRelay,
    public::{public_policy, spend_policy},
    telephony::{FakePublicMedia, FakeTelephony},
    Harness, TestDb, TestResponse, TEST_PEPPER,
};

/// A deterministic, non-weak 32-byte test-only webhook key.
pub fn webhook_key(seed: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add((i as u8).wrapping_mul(7));
    }
    k
}

/// Webhook settings for local receivers (plain HTTP, loopback allowed).
pub fn webhook_settings() -> WebhookSettings {
    WebhookSettings {
        secret_box: Some(Arc::new(SecretBox::from_bytes(&webhook_key(42)).unwrap())),
        policy: EndpointPolicy {
            allow_insecure_http: true,
            allow_private_networks: true,
        },
        request_timeout: std::time::Duration::from_secs(1),
        max_attempts: 3,
    }
}

/// Every scope the application may grant in these tests.
pub const ALL_TEST_SCOPES: &[&str] = &[
    names::PRINCIPALS_READ,
    names::PRINCIPALS_WRITE,
    names::CALLS_READ,
    names::CALLS_WRITE,
    names::PRIVACY_CONTEXTS_READ,
    names::PRIVACY_CONTEXTS_WRITE,
    names::PRIVACY_SESSIONS_READ,
    names::PRIVACY_SESSIONS_WRITE,
    names::PRIVACY_CONTACTS_READ,
    names::PRIVACY_CONTACTS_WRITE,
    names::PRIVACY_CONTACTS_VERIFY,
    names::PRIVACY_EVIDENCE_READ,
    names::ENTERPRISE_CONFIG_READ,
    names::ENTERPRISE_CONFIG_WRITE,
    names::ENTERPRISE_CALLS_USE,
    names::ENTERPRISE_VOICEMAIL_READ,
    names::ENTERPRISE_VOICEMAIL_MANAGE,
    names::ENTERPRISE_RECORDING_MANAGE,
    names::ENTERPRISE_RECORDING_READ,
    names::WEBHOOKS_READ,
    names::WEBHOOKS_MANAGE,
    names::USAGE_READ,
];

/// The external systems of a deployment (shared by every instance over one database).
#[derive(Clone)]
pub struct Externals {
    pub timeline: Arc<Timeline>,
    pub privacy: Arc<FakePrivacyRelay>,
    pub gated_privacy: Arc<GatedPrivacy>,
    pub telephony: Arc<FakeTelephony>,
    pub gated_telephony: Arc<GatedTelephony>,
    pub public_media: Arc<FakePublicMedia>,
    pub media: Arc<FakeMediaProvider>,
    pub bus: Arc<InMemorySignalBus>,
    pub store: Arc<MemoryObjectStore>,
    pub capture: Arc<RecordingCapture>,
}

impl Externals {
    pub fn new() -> Self {
        let timeline = Arc::new(Timeline::default());
        let privacy = Arc::new(FakePrivacyRelay::new(Vec::new()));
        let telephony = Arc::new(FakeTelephony::healthy());
        Self {
            gated_privacy: Arc::new(GatedPrivacy::new(privacy.clone(), timeline.clone())),
            gated_telephony: Arc::new(GatedTelephony::new(telephony.clone(), timeline.clone())),
            timeline,
            privacy,
            telephony,
            public_media: Arc::new(FakePublicMedia::default()),
            media: Arc::new(FakeMediaProvider::default()),
            bus: Arc::new(InMemorySignalBus::new()),
            store: Arc::new(MemoryObjectStore::new()),
            capture: Arc::new(RecordingCapture::default()),
        }
    }

    /// The Calling runtime wired to these externals.
    pub fn runtime(&self) -> CallingRuntime {
        let mut runtime = super::test_runtime(Some(self.media.clone()), self.bus.clone());
        runtime.privacy_relay = self.gated_privacy.clone();
        runtime.public_network = calling::calls::app::public::PublicNetwork {
            telephony: self.gated_telephony.clone(),
            media: Some(self.public_media.clone()),
            flags: calling::routing::engine::TelecomFlags {
                public_outbound: true,
                public_inbound: true,
                callback_relay: true,
            },
        };
        runtime.artifacts = self.store.clone();
        runtime.capture = self.capture.clone();
        runtime.webhooks = webhook_settings();
        runtime
    }

    /// A Calling instance (its own state, router and workers) over `db`.
    pub fn instance(&self, db: Arc<TestDb>, clock: ManualClock) -> Harness {
        Harness::with_runtime(
            db,
            TEST_PEPPER,
            clock,
            self.media.clone(),
            self.bus.clone(),
            self.runtime(),
        )
    }
}

impl Default for Externals {
    fn default() -> Self {
        Self::new()
    }
}

/// The provisioned deployment.
pub struct Deployment {
    pub e: EnterpriseWorld,
    pub x: Externals,
    /// A verified NG contact Privacy Relay knows.
    pub contact: Uuid,
    pub session: Uuid,
    /// The initiator's device.
    pub caller: Device,
}

impl Deployment {
    pub async fn new() -> Option<Self> {
        let x = Externals::new();
        let db = Arc::new(TestDb::new().await?);
        // Real time: Privacy Relay decisions carry real validity windows.
        let h = x.instance(db, ManualClock::new(chrono::Utc::now()));
        let w = CallWorld::in_harness(h, "acme", EnvironmentKind::Production).await;
        let env = w.consumer.env(EnvironmentKind::Production).id;
        w.h.platform
            .set_live_telecom_allowed(env, true)
            .await
            .unwrap();
        for scope in ALL_TEST_SCOPES {
            w.h.platform
                .grant_application_scope(w.consumer.application.id, scope)
                .await
                .unwrap();
        }
        let e = EnterpriseWorld::around(AppWorld { w }).await;
        x.privacy.allow(e.a.w.initiator.id().as_uuid());
        let contact = x.privacy.add_contact("NG");
        let caller = e.a.device(&e.a.w.initiator, "caller-main").await;
        let d = Self {
            e,
            x,
            contact,
            session: Uuid::new_v4(),
            caller,
        };
        let scope = d.e.a.w.scope();
        calling::routing::store::store_policy(
            d.h().conn(),
            scope.tenant_id().as_uuid(),
            scope.application_id().as_uuid(),
            scope.environment_id().as_uuid(),
            &public_policy(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        calling::telephony::spend::store_policy(
            d.h().conn(),
            calling::telephony::spend::SpendScope {
                tenant_id: scope.tenant_id().as_uuid(),
                application_id: scope.application_id().as_uuid(),
                environment_id: scope.environment_id().as_uuid(),
            },
            &spend_policy(),
            chrono::Utc::now(),
        )
        .await
        .unwrap();
        Some(d)
    }

    pub fn h(&self) -> &Harness {
        self.e.h()
    }

    pub fn a(&self) -> &AppWorld {
        &self.e.a
    }

    /// A second Calling instance over this deployment's database (same externals, same
    /// clock).
    pub fn instance(&self) -> Harness {
        self.x.instance(self.h().db.clone(), self.h().clock.clone())
    }

    /// A credential with `scopes` and generous limits.
    pub async fn credential(
        &self,
        scopes: &[&str],
    ) -> calling::platform::credentials::IssuedCredential {
        super::enterprise::credential(self.h(), &self.e.a.w.consumer, self.e.a.w.kind, scopes).await
    }

    /// `POST /v1/calls` from the initiator to a protected contact (APP_TO_PSTN).
    pub async fn call_contact(&self, key: &str) -> TestResponse {
        self.a()
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

    /// The public leg and its latest attempt of `call`.
    pub async fn public_leg(&self, call: Uuid) -> (Uuid, Uuid) {
        use sea_orm::ConnectionTrait;
        let row = self
            .h()
            .conn()
            .query_one(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT l.id, a.id FROM call_legs l JOIN call_attempts a ON a.leg_id = l.id \
                 WHERE l.call_id = $1 AND l.leg_kind = 'PUBLIC_NETWORK' \
                 ORDER BY a.attempt_number DESC LIMIT 1",
                vec![call.into()],
            ))
            .await
            .unwrap()
            .expect("a public leg");
        (
            row.try_get_by_index(0).unwrap(),
            row.try_get_by_index(1).unwrap(),
        )
    }

    /// Grants the enterprise capabilities voicemail and recording administration need.
    pub async fn enable_artifacts(&self) {
        use calling::enterprise::policy::AdminCapability;
        allow_enterprise_scopes(self.h(), &self.e.a.w.consumer).await;
        self.h()
            .state
            .enterprise
            .operator_set_capabilities(
                self.e.a.w.scope(),
                vec![
                    AdminCapability::ManageRouting,
                    AdminCapability::ManageVoicemail,
                    AdminCapability::ManageRecording,
                    AdminCapability::ManagePublicForwarding,
                ],
            )
            .await
            .unwrap();
    }

    /// An enterprise member with a desk phone.
    pub async fn desk(
        &self,
        name: &str,
    ) -> (calling::platform::principals::ResolvedPrincipal, Device) {
        let principal = self.e.member(name).await;
        let phone = self.e.a.device(&principal, &format!("{name}-desk")).await;
        (principal, phone)
    }

    /// A READY voicemail message: `owner` gets a mailbox, a NO_ANSWER forwarding rule and
    /// extension `number`; `caller` leaves a message. Returns (call, message, mailbox).
    pub async fn voicemail(
        &self,
        manager: &str,
        owner: &calling::platform::principals::ResolvedPrincipal,
        caller: &(calling::platform::principals::ResolvedPrincipal, Device),
        number: &str,
    ) -> (Uuid, Uuid, Uuid) {
        let r = self
            .e
            .send(
                manager,
                Method::POST,
                "/v1/enterprise/voicemail/mailboxes",
                Some(
                    json!({ "owner": { "type": "PRINCIPAL", "id": owner.id().to_string() },
                             "displayLabel": format!("Box {number}"), "maxMessageSeconds": 120,
                             "retentionDays": 30 }),
                ),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let mailbox: Uuid = r.json["mailboxId"].as_str().unwrap().parse().unwrap();
        let r = self
            .e
            .admin(
                Method::POST,
                "/v1/enterprise/forwarding-rules",
                Some(json!({ "principalId": owner.id(), "condition": "NO_ANSWER",
                             "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } })),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        self.e
            .extension(number, "ENTERPRISE_MEMBER", owner.id())
            .await;
        let r = self
            .e
            .call_extension(
                "VOICE",
                &caller.0,
                &caller.1,
                number,
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        self.h().clock.advance(chrono::Duration::seconds(46));
        self.h().state.app.expire_due(100).await.unwrap();
        let message: Uuid = super::enterprise::strings(
            self.h().conn(),
            &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{call}'"),
        )
        .await
        .pop()
        .expect("a voicemail message")
        .parse()
        .unwrap();
        self.h()
            .state
            .app
            .deliver_capture(message, call, captured(&wav(1500)))
            .await
            .unwrap();
        (call, message, mailbox)
    }
}

/// A short 8 kHz mono WAV tone.
pub fn wav(ms: u32) -> Vec<u8> {
    let samples = 8 * ms;
    let data_len = samples * 2;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&8000u32.to_le_bytes());
    out.extend_from_slice(&16000u32.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..samples {
        let v: i16 = if (i / 10) % 2 == 0 { 800 } else { -800 };
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn captured(bytes: &[u8]) -> CapturedMedia {
    CapturedMedia {
        content_type: "audio/wav".into(),
        duration_ms: 1500,
        sha256: cf_object_storage::sha256_hex(bytes),
        bytes: bytes.to_vec(),
    }
}

/// What a local webhook receiver saw.
#[derive(Debug, Clone)]
pub struct Received {
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

/// A local HTTP webhook receiver (always 200).
#[derive(Clone)]
pub struct Receiver {
    seen: Arc<std::sync::Mutex<Vec<Received>>>,
    pub url: String,
}

async fn receive(
    State(seen): State<Arc<std::sync::Mutex<Vec<Received>>>>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    seen.lock().unwrap().push(Received {
        headers,
        body: body.to_vec(),
    });
    StatusCode::OK
}

impl Receiver {
    pub async fn start() -> Self {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/hook", post(receive))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            seen,
            url: format!("http://127.0.0.1:{port}/hook"),
        }
    }

    pub fn seen(&self) -> Vec<Received> {
        self.seen.lock().unwrap().clone()
    }

    /// The delivered event ids (from the signed body), in arrival order.
    pub fn event_ids(&self) -> Vec<String> {
        self.seen()
            .iter()
            .filter_map(|r| {
                serde_json::from_slice::<Value>(&r.body)
                    .ok()
                    .and_then(|v| v["id"].as_str().map(str::to_string))
            })
            .collect()
    }
}
