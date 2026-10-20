//! I. Secret leakage: raw credentials never appear in error bodies, Debug output or
//! structured logs.

mod common;

use common::RotateCredential;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::http::{Method, StatusCode};
use calling::{
    config::CallingConfig,
    domain::{EnvironmentKind, TenantClassification},
    platform::scopes::names,
};
use common::{secret_part, Harness, TEST_PEPPER};
use serde_json::json;
use tracing_subscriber::{fmt::MakeWriter, layer::SubscriberExt};

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The log-capture test installs a thread-scoped subscriber, but tracing caches each
/// callsite's interest process-wide: a callsite first hit by a concurrently running test
/// while that subscriber is being installed can stay cached as "never" and escape the
/// capture. The tests of this binary therefore run one at a time.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Capture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn structured_logs_never_contain_credentials_or_authorization_headers() {
    let _serial = SERIAL.lock().await;
    let Some(h) = Harness::new().await else {
        return;
    };
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(capture.clone()),
    );
    let _guard = tracing::subscriber::set_default(subscriber);

    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h
        .issue(c.sa(EnvironmentKind::Production), &[names::PRINCIPALS_READ])
        .await;
    let token = issued.secret.expose_once().to_string();
    // Change the last secret character to a DIFFERENT hex digit (replacing the tail with a
    // fixed "0000" was the same token whenever the random secret already ended so).
    let last = if token.ends_with('0') { '1' } else { '0' };
    let wrong = format!("{}{last}", &token[..token.len() - 1]);
    assert_ne!(wrong, token);

    assert_eq!(h.whoami(&token).await.status, StatusCode::OK);
    assert_eq!(h.whoami(&wrong).await.status, StatusCode::UNAUTHORIZED);
    let scope_denied = h
        .request(Method::GET, "/test/calls-write", Some(&token), None)
        .await;
    assert_eq!(scope_denied.status, StatusCode::FORBIDDEN);
    let rotated = h
        .platform
        .rotate_credential(issued.credential.id, RotateCredential::default())
        .await
        .unwrap();
    let rotated_token = rotated.secret.expose_once().to_string();
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    assert_eq!(h.whoami(&token).await.status, StatusCode::UNAUTHORIZED);

    let logs = capture.text();
    // Logging actually happened, with operator context. (Issuance, rotation and revocation
    // are logged by Platform Access, in its own process; Calling logs the projection
    // and the authentication outcomes.)
    assert!(logs.contains("platform authentication rejected"), "{logs}");
    assert!(logs.contains("secret_mismatch"));
    assert!(logs.contains(&issued.credential.public_id));
    assert!(logs.contains("req_"));
    // After authentication the request span carries the platform identifiers.
    let scope_line = logs
        .lines()
        .find(|l| l.contains("scope check denied"))
        .expect("scope denial logged");
    for id in [
        c.tenant.id.to_string(),
        c.application.id.to_string(),
        c.env(EnvironmentKind::Production).id.to_string(),
        c.sa(EnvironmentKind::Production).to_string(),
        issued.credential.id.to_string(),
    ] {
        assert!(scope_line.contains(&id), "{scope_line}");
    }
    assert!(scope_line.contains("\"request_id\":\"req_"), "{scope_line}");

    for secret in [&token, &wrong, &rotated_token] {
        assert!(!logs.contains(secret.as_str()), "raw credential in logs");
        assert!(!logs.contains(secret_part(secret)), "secret part in logs");
    }
    assert!(!logs.to_lowercase().contains("bearer "));
    assert!(!logs.to_lowercase().contains("authorization"));
    assert!(!logs.contains(TEST_PEPPER));
}

#[tokio::test]
async fn error_bodies_never_echo_presented_credentials() {
    let _serial = SERIAL.lock().await;
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once().to_string();
    // A DIFFERENT last secret digit (a fixed "ffff" tail equals the token 1 in 65536 times).
    let last = if token.ends_with('f') { 'e' } else { 'f' };
    let wrong = format!("{}{last}", &token[..token.len() - 1]);
    assert_ne!(wrong, token);
    for presented in [&token, &wrong] {
        for (method, path) in [
            (Method::GET, "/test/calls-write"),
            (Method::POST, "/test/resolve"),
            (Method::POST, "/test/context"),
        ] {
            let response = h
                .request(
                    method,
                    path,
                    Some(presented),
                    Some(json!({ "credential": presented })),
                )
                .await;
            assert!(
                !response.text.contains(secret_part(presented)),
                "{path}: {}",
                response.text
            );
            for value in response.headers.values() {
                assert!(!value
                    .to_str()
                    .unwrap_or("")
                    .contains(secret_part(presented)));
            }
        }
    }
    h.platform
        .revoke_credential(issued.credential.id)
        .await
        .unwrap();
    let revoked = h.whoami(&token).await;
    assert!(!revoked.text.contains(secret_part(&token)));
    assert!(!revoked.text.to_lowercase().contains("revoked"));
}

#[tokio::test]
async fn debug_output_of_secret_bearing_types_is_redacted() {
    let _serial = SERIAL.lock().await;
    let Some(h) = Harness::new().await else {
        return;
    };
    let c = h.consumer("acme", TenantClassification::External).await;
    let issued = h.issue(c.sa(EnvironmentKind::Production), &[]).await;
    let token = issued.secret.expose_once().to_string();
    for rendered in [
        format!("{issued:?}"),
        format!("{issued:#?}"),
        format!("{:?}", issued.secret),
    ] {
        assert!(!rendered.contains(secret_part(&token)), "{rendered}");
        assert!(rendered.contains("[REDACTED]"));
    }
    // Authentication errors and contexts carry no secret either.
    // Change the last secret character to a DIFFERENT hex digit (replacing the tail with a
    // fixed "0000" was the same token whenever the random secret already ended so).
    let last = if token.ends_with('0') { '1' } else { '0' };
    let wrong = format!("{}{last}", &token[..token.len() - 1]);
    assert_ne!(wrong, token);
    let err = h.authenticate(&wrong).await.unwrap_err();
    assert!(!format!("{err:?}").contains(secret_part(&wrong)));
    let ctx = h.ctx(&token).await;
    assert!(!format!("{ctx:?}").contains(secret_part(&token)));

    // Configuration Debug never shows the database password; a pepper is refused outright
    // (it is Platform Access's) without echoing it.
    let config = CallingConfig::from_lookup(|key| match key {
        "DATABASE_URL" => Some("postgres://calling:db-password-123@db/calling".into()),
        _ => None,
    })
    .unwrap();
    let rendered = format!("{config:?}");
    assert!(!rendered.contains("db-password-123"));
    // (The refusal sees variable names only, never values.)
    let refused =
        calling::config::refuse_moved_settings(["API_CREDENTIAL_PEPPER".to_string()]).unwrap_err();
    assert!(!format!("{refused} {refused:?}").contains(TEST_PEPPER));
}

// ---------------------------------------------------------------------------
// Part H §72.7: runtime secrets of every class, under TRACE capture of all crates.
// ---------------------------------------------------------------------------

/// Installs the TRACE / all-crates JSON capture for this thread (the test runtime).
fn capture_logs() -> (Capture, tracing::subscriber::DefaultGuard) {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(capture.clone()),
    );
    (capture, tracing::subscriber::set_default(subscriber))
}

/// Every response a test saw (label, status line + headers + body), for "only the one
/// intended response carries it" checks.
#[derive(Default)]
struct Seen(Vec<(String, String)>);

impl Seen {
    fn keep(&mut self, label: impl Into<String>, r: &common::TestResponse) -> serde_json::Value {
        let headers: Vec<String> = r
            .headers
            .iter()
            .map(|(k, v)| format!("{k}: {}", v.to_str().unwrap_or("")))
            .collect();
        self.0.push((
            label.into(),
            format!("{}\n{}\n{}", r.status, headers.join("\n"), r.text),
        ));
        r.json.clone()
    }

    /// Labels of the responses that contain `needle`.
    fn containing(&self, needle: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(_, text)| text.contains(needle))
            .map(|(label, _)| label.clone())
            .collect()
    }
}

/// `+` followed by eight or more digits: an E.164 telephone number.
fn contains_e164(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len()).any(|i| {
        bytes[i] == b'+'
            && bytes[i + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count()
                >= 8
    })
}

#[tokio::test]
async fn runtime_secrets_never_reach_logs_or_any_other_response() {
    use base64::Engine;
    use calling::{
        calls::app::enterprise::CapturedMedia,
        enterprise::policy::AdminCapability,
        webhooks::{worker::LocalWebhookTransport, WebhookSettings},
    };
    use cf_object_storage::MemoryObjectStore;
    use cf_webhooks::{endpoint::EndpointPolicy, secret_box::SecretBox};
    use common::enterprise::{credential, id_of, strings, EnterpriseWorld, RecordingCapture};
    use uuid::Uuid;

    let _serial = SERIAL.lock().await;
    let (capture, guard) = capture_logs();
    let mut seen = Seen::default();

    // Deployment secrets of this world.
    let box_key: [u8; 32] = std::array::from_fn(|i| (i as u8).wrapping_mul(29).wrapping_add(17));
    let box_key_b64 = base64::engine::general_purpose::STANDARD.encode(box_key);
    let box_key_hex = hex::encode(box_key);
    let store = Arc::new(MemoryObjectStore::new());
    let rec_capture = Arc::new(RecordingCapture::default());
    let (s, c) = (store.clone(), rec_capture.clone());
    let Some(e) = EnterpriseWorld::configured(move |rt| {
        rt.artifacts = s;
        rt.capture = c;
        rt.webhooks = WebhookSettings {
            secret_box: Some(Arc::new(SecretBox::from_bytes(&box_key).unwrap())),
            policy: EndpointPolicy {
                allow_insecure_http: true,
                allow_private_networks: true,
            },
            request_timeout: std::time::Duration::from_secs(2),
            max_attempts: 3,
        };
    })
    .await
    else {
        return;
    };
    for scope in [names::WEBHOOKS_READ, names::WEBHOOKS_MANAGE] {
        e.h()
            .platform
            .grant_application_scope(e.a.w.consumer.application.id, scope)
            .await
            .unwrap();
    }
    e.h()
        .state
        .enterprise
        .operator_set_capabilities(
            e.a.w.scope(),
            vec![
                AdminCapability::ManageRouting,
                AdminCapability::ManageVoicemail,
                AdminCapability::ManageRecording,
            ],
        )
        .await
        .unwrap();
    let mut scopes: Vec<&str> = common::enterprise::ENTERPRISE_SCOPES.to_vec();
    scopes.extend([
        names::WEBHOOKS_READ,
        names::WEBHOOKS_MANAGE,
        names::CALLS_READ,
        names::CALLS_WRITE,
    ]);
    let admin = credential(e.h(), &e.a.w.consumer, e.a.w.kind, &scopes).await;
    let token = admin.secret.expose_once().to_string();
    let send = |method: Method, path: String, body: Option<serde_json::Value>| {
        let token = token.clone();
        let e = &e;
        async move { e.send(&token, method, &path, body).await }
    };

    // 1. Webhook signing secrets: returned once at creation and once per rotation.
    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sink = received.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/hook",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                let sink = sink.clone();
                async move {
                    sink.lock().unwrap().push(format!("{headers:?}\n{body}"));
                    StatusCode::OK
                }
            }),
        );
        axum::serve(listener, app).await.unwrap();
    });
    let r = send(
        Method::POST,
        "/v1/webhooks".into(),
        Some(json!({ "url": format!("http://127.0.0.1:{port}/hook"), "eventTypes": ["call.requested"] })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let created = seen.keep("webhook create", &r);
    let first_secret = created["signingSecret"].as_str().unwrap().to_string();
    assert!(first_secret.starts_with("whsec_"));
    let webhook = created["webhook"]["webhookId"]
        .as_str()
        .unwrap()
        .to_string();
    for (label, method, path, body) in [
        (
            "webhook get",
            Method::GET,
            format!("/v1/webhooks/{webhook}"),
            None,
        ),
        (
            "webhook list",
            Method::GET,
            "/v1/webhooks".to_string(),
            None,
        ),
        (
            "webhook patch",
            Method::PATCH,
            format!("/v1/webhooks/{webhook}"),
            Some(json!({ "expectedVersion": 1, "description": "renamed" })),
        ),
    ] {
        let r = send(method, path, body).await;
        assert!(r.status.is_success(), "{label}: {}", r.text);
        seen.keep(label, &r);
    }
    let r = send(
        Method::POST,
        format!("/v1/webhooks/{webhook}/rotate-secret"),
        Some(json!({ "expectedVersion": 2 })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let rotated_secret = seen.keep("webhook rotate", &r)["signingSecret"]
        .as_str()
        .unwrap()
        .to_string();
    // A signed delivery under the rotated secret.
    e.a.ringing_call("VOICE").await;
    let webhooks = &e.h().state.webhooks;
    webhooks
        .publish_outbox_once(&LocalWebhookTransport::new((**webhooks).clone()), 1000)
        .await
        .unwrap();
    assert!(webhooks.deliver_due_once(100).await.unwrap().succeeded > 0);
    let r = send(
        Method::GET,
        format!("/v1/webhooks/{webhook}/deliveries"),
        None,
    )
    .await;
    seen.keep("webhook deliveries", &r);
    let r = send(Method::GET, format!("/v1/webhooks/{webhook}"), None).await;
    seen.keep("webhook get after rotation", &r);

    // 2. Signaling credential (minted with the signaling key).
    let device = e.a.device(&e.a.w.initiator, "leak-desk").await;
    let r =
        e.a.post(
            "/v1/calling/signaling-credentials",
            json!({ "principalId": device.principal, "endpointId": device.endpoint }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let signaling_token = seen.keep("signaling credential", &r)["token"]
        .as_str()
        .unwrap()
        .to_string();

    // 3. Voicemail and recording media access: the signed URL / token exactly once.
    let caller = e
        .member(&format!("caller-{}", Uuid::new_v4().simple()))
        .await;
    let callee = e
        .member(&format!("callee-{}", Uuid::new_v4().simple()))
        .await;
    let owner = e
        .member(&format!("owner-{}", Uuid::new_v4().simple()))
        .await;
    let caller_phone = e.a.device(&caller, "caller-desk").await;
    let callee_phone = e.a.device(&callee, "callee-desk").await;
    e.a.device(&owner, "owner-desk").await;
    e.extension("4300", "ENTERPRISE_MEMBER", callee.id()).await;
    e.extension("4301", "ENTERPRISE_MEMBER", owner.id()).await;
    let r = send(
        Method::POST,
        "/v1/enterprise/voicemail/mailboxes".into(),
        Some(
            json!({ "owner": { "type": "PRINCIPAL", "id": owner.id().to_string() },
                      "displayLabel": "Leak box", "maxMessageSeconds": 120, "retentionDays": 30 }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let mailbox = id_of(&r.json, "mailboxId");
    let r = send(
        Method::POST,
        "/v1/enterprise/forwarding-rules".into(),
        Some(json!({ "principalId": owner.id(), "condition": "NO_ANSWER",
                      "destination": { "type": "VOICEMAIL", "id": mailbox.to_string() } })),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let r = send(
        Method::POST,
        "/v1/enterprise/recording-policies".into(),
        Some(
            json!({ "name": "Leak recording", "mode": "MANUAL_ALLOWED", "noticeRequired": false,
                      "consentParticipants": "NONE", "jurisdictionRef": "NG-NDPA-2023/v2",
                      "retentionDays": 30, "maxRecordingSeconds": 600, "isDefault": true }),
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let audio = b"RIFF$\0\0\0WAVEfmt leak-test-audio".to_vec();
    let media = |bytes: &[u8]| CapturedMedia {
        content_type: "audio/wav".into(),
        duration_ms: 1000,
        sha256: cf_object_storage::sha256_hex(bytes),
        bytes: bytes.to_vec(),
    };
    // Recording of a connected Call.
    let r = e
        .call_extension(
            "VOICE",
            &caller,
            &caller_phone,
            "4300",
            &Uuid::new_v4().to_string(),
        )
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
    let r = send(
        Method::POST,
        format!("/v1/calls/{call}/recordings"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let recording = id_of(&r.json["recording"], "recordingId");
    let (_, v) = e.a.state(call).await;
    e.a.command(call, "end", v).await;
    e.h().state.app.expire_due(100).await.unwrap();
    e.h()
        .state
        .app
        .deliver_capture(recording, call, media(&audio))
        .await
        .unwrap();
    // Voicemail of an unanswered Call.
    let r = e
        .call_extension(
            "VOICE",
            &caller,
            &caller_phone,
            "4301",
            &Uuid::new_v4().to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let vm_call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
    e.h().clock.advance(chrono::Duration::seconds(46));
    e.h().state.app.expire_due(100).await.unwrap();
    let message: Uuid = strings(
        e.h().conn(),
        &format!("SELECT id::text FROM voicemail_messages WHERE call_id = '{vm_call}'"),
    )
    .await
    .pop()
    .unwrap()
    .parse()
    .unwrap();
    e.h()
        .state
        .app
        .deliver_capture(message, vm_call, media(&audio))
        .await
        .unwrap();
    let object_keys = store.keys();
    assert_eq!(object_keys.len(), 2);
    let r = send(
        Method::POST,
        format!("/v1/enterprise/recordings/{recording}/media-access"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let recording_url = seen.keep("recording media access", &r)["url"]
        .as_str()
        .unwrap()
        .to_string();
    let r = send(
        Method::POST,
        format!("/v1/enterprise/voicemail/messages/{message}/media-access"),
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let voicemail_url = seen.keep("voicemail media access", &r)["url"]
        .as_str()
        .unwrap()
        .to_string();
    for (label, path) in [
        (
            "recording get",
            format!("/v1/enterprise/recordings/{recording}"),
        ),
        (
            "recording audit",
            format!("/v1/enterprise/recordings/{recording}/access-audit"),
        ),
        (
            "voicemail get",
            format!("/v1/enterprise/voicemail/messages/{message}"),
        ),
        ("call recordings", format!("/v1/calls/{call}/recordings")),
        ("enterprise audit", "/v1/enterprise/audit".to_string()),
    ] {
        let r = send(Method::GET, path, None).await;
        assert!(r.status.is_success(), "{label}: {}", r.text);
        seen.keep(label, &r);
    }

    // 4. Contact Resolution Grant secret: Calling receives one per paid submission and
    //    hands it to Telephony only (the fake Privacy Relay / Telephony).
    let privacy = Arc::new(common::privacy::FakePrivacyRelay::new(Vec::new()));
    let telephony = Arc::new(common::telephony::FakeTelephony::healthy());
    let public = common::app::AppWorld::with_public_network(
        privacy.clone(),
        telephony.clone(),
        Some(Arc::new(common::telephony::FakePublicMedia::default())),
    )
    .await
    .unwrap();
    privacy.allow(public.w.initiator.id().as_uuid());
    let contact = privacy.add_contact("NG");
    let scope = public.w.scope();
    calling::routing::store::store_policy(
        public.h().conn(),
        scope.tenant_id().as_uuid(),
        scope.application_id().as_uuid(),
        scope.environment_id().as_uuid(),
        &calling::routing::policy::RoutingPolicy {
            allowed_route_classes: vec![
                calling::routing::RoutingClass::AppRtc,
                calling::routing::RoutingClass::AppToPstn,
            ],
            public_calling_enabled: true,
            destinations: calling::routing::policy::DestinationPolicy {
                allow: vec!["NG".into()],
                deny: vec![],
                default: calling::routing::policy::CountryDecision::Deny,
            },
            ..Default::default()
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    calling::telephony::spend::store_policy(
        public.h().conn(),
        calling::telephony::spend::SpendScope {
            tenant_id: scope.tenant_id().as_uuid(),
            application_id: scope.application_id().as_uuid(),
            environment_id: scope.environment_id().as_uuid(),
        },
        &calling::telephony::spend::SpendPolicy {
            currency: "NGN".into(),
            leg_allowance_minor: 5_000,
            max_leg_seconds: 3600,
            daily_limit_minor: 50_000,
            monthly_limit_minor: 500_000,
            max_concurrent_legs: 5,
            max_originations_per_minute: 20,
            max_per_party_per_hour: 10,
            max_per_principal_per_hour: None,
            max_per_service_account_per_hour: None,
            max_per_context_per_hour: None,
            enabled: true,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let public_caller = public.device(&public.w.initiator, "public-caller").await;
    let r = public
        .post_with(
            "/v1/calls",
            json!({ "kind": "VOICE", "initiatorPrincipalId": public.w.initiator.id(),
                     "initiatorEndpointId": public_caller.endpoint,
                     "targetContactReference": contact,
                     "privacyRelaySessionId": Uuid::new_v4() }),
            &[("idempotency-key", "leak-public")],
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
    let public_call = seen.keep("public call create", &r)["call"]["callId"]
        .as_str()
        .unwrap()
        .to_string();
    let r = public.get(&format!("/v1/calls/{public_call}")).await;
    seen.keep("public call get", &r);
    let originates = telephony.originates();
    let grant_secret = originates[0].1["destination"]["grantSecret"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(grant_secret.len() >= 32);

    // 5. LiveKit API secret and media-edge signing key (adapter built from config): a
    //    join credential is minted and a provider call fails (unreachable API).
    const LIVEKIT_SECRET: &str = "leak-test-livekit-api-secret-5c2e9a7f31d04b68";
    const EDGE_KEY: &str = "leak-test-media-edge-signing-key-8e1f6b2d4a9c07";
    let settings = calling::media::MediaSettings::from_lookup(&|k: &str| {
        match k {
            "CALLING_MEDIA_PROVIDER" => Some("livekit"),
            "CALLING_LIVEKIT_URL" => Some("ws://127.0.0.1:9"),
            "CALLING_LIVEKIT_API_URL" => Some("http://127.0.0.1:9"),
            "CALLING_LIVEKIT_API_KEY" => Some("leakkey"),
            "CALLING_LIVEKIT_API_SECRET" => Some(LIVEKIT_SECRET),
            "CALLING_MEDIA_GATEWAY_URL" => Some("ws://127.0.0.1:9/v1/calling/media"),
            "CALLING_MEDIA_EDGE_SIGNING_KEY" => Some(EDGE_KEY),
            "CALLING_LIVEKIT_API_TIMEOUT_MS" => Some("200"),
            _ => None,
        }
        .map(str::to_string)
    })
    .unwrap();
    let runtime = settings.build().unwrap();
    let provider = runtime.provider.clone().expect("livekit provider");
    let session = calling::media::provider::MediaSessionRef {
        name: calling::media::provider::MediaResourceName::for_call(
            calling::calls::ids::CallId::from_uuid(Uuid::new_v4()),
        ),
    };
    let issued = provider
        .issue_credentials(&calling::media::provider::MediaCredentialRequest {
            session: session.clone(),
            identity: "cfmid_leak".into(),
            generation: 1,
            ttl: chrono::Duration::minutes(5),
        })
        .await
        .unwrap();
    let media_token = issued.token.expose().to_string();
    let failure = provider
        .create_or_ensure_session(
            &session,
            &calling::media::provider::MediaSessionSpec {
                max_participants: 2,
                empty_timeout: chrono::Duration::minutes(1),
            },
        )
        .await
        .unwrap_err();
    tracing::warn!(error = %failure, debug = ?failure, issued = ?issued,
                   "media provider failure (test)");
    drop(guard);

    let logs = capture.text();
    // Logging really happened on these paths.
    for expected in [
        "webhook subscription created",
        "call recording ready",
        "voicemail message ready",
        "media provider failure (test)",
    ] {
        assert!(logs.contains(expected), "{expected} not logged");
    }
    let url_token = |url: &str| {
        url.trim_start_matches("memory://object/")
            .split('?')
            .next()
            .unwrap()
            .to_string()
    };
    // (secret, the ONE response allowed to carry it, if any)
    let secrets: Vec<(&str, String, Option<&str>)> = vec![
        (
            "webhook signing secret",
            first_secret.clone(),
            Some("webhook create"),
        ),
        (
            "rotated webhook signing secret",
            rotated_secret.clone(),
            Some("webhook rotate"),
        ),
        ("webhook secret-box key (base64)", box_key_b64.clone(), None),
        ("webhook secret-box key (hex)", box_key_hex.clone(), None),
        (
            "signaling signing key",
            common::TEST_SIGNALING_KEY.to_string(),
            None,
        ),
        (
            "signaling credential",
            signaling_token.clone(),
            Some("signaling credential"),
        ),
        (
            "recording signed url",
            recording_url.clone(),
            Some("recording media access"),
        ),
        (
            "recording url token",
            url_token(&recording_url),
            Some("recording media access"),
        ),
        (
            "voicemail signed url",
            voicemail_url.clone(),
            Some("voicemail media access"),
        ),
        (
            "voicemail url token",
            url_token(&voicemail_url),
            Some("voicemail media access"),
        ),
        ("grant secret", grant_secret.clone(), None),
        ("livekit api secret", LIVEKIT_SECRET.to_string(), None),
        ("media edge key", EDGE_KEY.to_string(), None),
        ("media join token", media_token.clone(), None),
        ("pepper", TEST_PEPPER.to_string(), None),
    ];
    for (what, secret, intended) in &secrets {
        assert!(secret.len() >= 16, "{what}");
        assert!(
            !logs.contains(secret.as_str()),
            "{what} appears in the logs"
        );
        let carriers = seen.containing(secret);
        match intended {
            Some(only) => assert_eq!(carriers, [only.to_string()], "{what} in other responses"),
            None => assert!(carriers.is_empty(), "{what} in responses: {carriers:?}"),
        }
        // Customer endpoints receive signatures, never a secret.
        for request in received.lock().unwrap().iter() {
            assert!(
                !request.contains(secret.as_str()),
                "{what} sent to the webhook endpoint"
            );
        }
    }
    // Object keys are never logged nor returned either.
    for key in &object_keys {
        assert!(!logs.contains(key.as_str()), "object key in logs");
        assert!(seen.containing(key).is_empty(), "object key in a response");
    }
    // No telephone number anywhere (Calling only ever holds opaque contact references).
    assert!(!contains_e164(&logs), "a telephone number was logged");
    for (label, text) in &seen.0 {
        assert!(!contains_e164(text), "a telephone number in {label}");
    }
    assert!(!received.lock().unwrap().is_empty());
}
