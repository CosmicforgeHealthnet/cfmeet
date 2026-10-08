//! Part D §§26–30 end to end: Calling and the real `privacy-relay` binary, each with its
//! own database, over signed internal HTTP (no shared database, no source imports).
//!
//! Requires `PRIVACY_RELAY_BIN` and `PRIVACY_RELAY_MIGRATION_BIN` (paths of the built
//! binaries) and `TEST_DATABASE_URL`; skipped otherwise (CI job `calling-e2e` of
//! `.github/workflows/privacy-relay.yml` sets them).

mod common;

use std::{path::PathBuf, sync::Arc, time::Duration};

use axum::http::{Method, StatusCode};
use calling::{calls::CallState, internal_auth::ServiceAuthRuntime, platform::scopes::names};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey, ServiceTokenSigner,
    ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use chrono::{SecondsFormat, Utc};
use common::{
    app::{AppWorld, Device},
    scalar_i64, TestDb, TestResponse,
};
use serde_json::{json, Value};
use tokio::{process::Child, task::JoinHandle};
use uuid::Uuid;

const ENV: RuntimeEnvironment = RuntimeEnvironment::Test;

struct Binaries {
    relay: PathBuf,
    migration: PathBuf,
}

fn binaries() -> Option<Binaries> {
    let relay = std::env::var_os("PRIVACY_RELAY_BIN")?;
    let migration = std::env::var_os("PRIVACY_RELAY_MIGRATION_BIN")?;
    Some(Binaries {
        relay: relay.into(),
        migration: migration.into(),
    })
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A running Privacy Relay process with its own database.
struct Relay {
    child: Child,
    _db: TestDb,
    dir: PathBuf,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Calling, served on a real port so Privacy Relay can deliver enforcement.
struct Stack {
    w: AppWorld,
    _relay: Relay,
    calling_port: u16,
    server: Option<JoinHandle<()>>,
    /// Customer credential with the privacy scopes.
    privacy: String,
}

impl Stack {
    async fn start() -> Option<Self> {
        let bins = binaries()?;
        let relay_db = TestDb::empty().await?;
        let dir = std::env::temp_dir().join(format!("cf-pr-e2e-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let calling_key = ServiceSigningKey::generate(KeyId::parse("calling-e2e-1").unwrap());
        let relay_key = ServiceSigningKey::generate(KeyId::parse("privacy-relay-e2e-1").unwrap());
        // Privacy Relay's own trust bundle: Calling with exactly its least-privilege grants.
        std::fs::write(
            dir.join("relay-trust.json"),
            common::trust_bundle_json(
                cf_service_auth::services::PRIVACY_RELAY,
                ENV,
                &[(
                    cf_service_auth::services::CALLING,
                    vec![calling_key.public_jwk()],
                    &[
                        "communication.authorize",
                        "communication-context.read",
                        "communication-context.write",
                        "relay-session.read",
                        "relay-session.write",
                    ],
                )],
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("relay-key.pem"),
            relay_key.to_pkcs8_pem().as_bytes(),
        )
        .unwrap();

        let migrated = std::process::Command::new(&bins.migration)
            .arg("up")
            .env("DATABASE_URL", &relay_db.url)
            .output()
            .expect("privacy-relay-migration runs");
        assert!(
            migrated.status.success(),
            "{}",
            String::from_utf8_lossy(&migrated.stderr)
        );

        let relay_port = free_port();
        let calling_port = free_port();
        let child = tokio::process::Command::new(&bins.relay)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("DATABASE_URL", &relay_db.url)
            .env(
                "PRIVACY_RELAY_BIND_ADDRESS",
                format!("127.0.0.1:{relay_port}"),
            )
            .env("PRIVACY_RELAY_ENABLED", "true")
            .env("PRIVACY_RELAY_WORKER_INTERVAL_MS", "200")
            .env("CF_RUNTIME_ENVIRONMENT", "test")
            .env("CF_SERVICE_AUTH_KEY_ID", "privacy-relay-e2e-1")
            .env(
                "CF_SERVICE_AUTH_PRIVATE_KEY_FILE",
                dir.join("relay-key.pem"),
            )
            .env("CF_SERVICE_TRUST_BUNDLE_FILE", dir.join("relay-trust.json"))
            .env(
                "CALLING_INTERNAL_BASE_URL",
                format!("http://127.0.0.1:{calling_port}"),
            )
            .env("CF_INTERNAL_HTTP_TIMEOUT_MS", "1000")
            .env("RUST_LOG", "privacy_relay=warn")
            .kill_on_drop(true)
            .spawn()
            .expect("privacy-relay starts");
        let relay = Relay {
            child,
            _db: relay_db,
            dir,
        };
        let relay_url = format!("http://127.0.0.1:{relay_port}");
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut healthy = false;
        for _ in 0..150 {
            if let Ok(r) = http.get(format!("{relay_url}/health")).send().await {
                if r.status().is_success() {
                    healthy = true;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(healthy, "privacy-relay became healthy");

        // Calling's runtime: its own key, a trust policy granting Privacy Relay only
        // enforcement, the wall clock (Privacy Relay verifies with the wall clock too).
        let clock: SharedClock = Arc::new(SystemClock);
        let policy = TrustPolicy::from_json(
            &common::trust_bundle_json(
                cf_service_auth::services::CALLING,
                ENV,
                &[(
                    cf_service_auth::services::PRIVACY_RELAY,
                    vec![relay_key.public_jwk()],
                    &["communication-authority.enforce"],
                )],
            ),
            &ServiceId::calling(),
            ENV,
        )
        .unwrap();
        let signer = Arc::new(ServiceTokenSigner::new(
            ServiceId::calling(),
            ENV,
            calling_key,
            TokenTtl::default(),
            clock.clone(),
        ));
        let mut targets = InternalTargets::new(ENV);
        targets
            .insert(ServiceId::privacy_relay(), &relay_url)
            .unwrap();
        let client = Arc::new(
            InternalServiceClient::new(signer.clone(), targets, Duration::from_secs(5)).unwrap(),
        );
        let runtime = ServiceAuthRuntime::new(
            signer,
            Arc::new(ServiceTokenVerifier::new(policy, clock)),
            client,
        )
        .unwrap();
        let (_, peers) = common::service_auth_fixture();
        let w = AppWorld::with_service_auth(runtime, peers, Utc::now()).await?;
        let privacy = w
            .h()
            .issue(
                w.w.consumer.sa(w.w.kind),
                &[
                    names::PRINCIPALS_READ,
                    names::PRIVACY_CONTEXTS_READ,
                    names::PRIVACY_CONTEXTS_WRITE,
                    names::PRIVACY_SESSIONS_READ,
                    names::PRIVACY_SESSIONS_WRITE,
                ],
            )
            .await
            .secret
            .expose_once()
            .to_string();
        let mut stack = Self {
            w,
            _relay: relay,
            calling_port,
            server: None,
            privacy,
        };
        stack.serve_calling().await;
        Some(stack)
    }

    async fn serve_calling(&mut self) {
        let mut listener = None;
        for _ in 0..50 {
            match tokio::net::TcpListener::bind(("127.0.0.1", self.calling_port)).await {
                Ok(l) => {
                    listener = Some(l);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        let listener = listener.expect("calling port");
        let app = self.w.h().router();
        self.server = Some(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
    }

    async fn stop_calling(&mut self) {
        if let Some(server) = self.server.take() {
            server.abort();
            let _ = server.await;
        }
    }

    async fn privacy(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        key: Option<&str>,
    ) -> TestResponse {
        let headers: Vec<(&str, &str)> = key
            .map(|k| vec![("idempotency-key", k)])
            .unwrap_or_default();
        self.w
            .h()
            .request_with(method, path, Some(&self.privacy), body, &headers)
            .await
    }

    async fn create_context(&self, subjects: &[Uuid]) -> Value {
        let now = Utc::now();
        let r = self
            .privacy(
                Method::POST,
                "/v1/privacy-relay/contexts",
                Some(json!({
                    "externalContextReference": format!("order-{}", Uuid::new_v4().simple()),
                    "participants": subjects.iter().map(|s| json!({ "principalId": s })).collect::<Vec<_>>(),
                    "allowedChannels": ["VOICE"],
                    "directionPolicy": "BIDIRECTIONAL",
                    "startsAt": (now - chrono::Duration::minutes(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                    "expiresAt": (now + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Secs, true),
                })),
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["context"].clone()
    }

    async fn create_session(&self, context: &str) -> Value {
        let r = self
            .privacy(
                Method::POST,
                &format!("/v1/privacy-relay/contexts/{context}/sessions"),
                Some(json!({ "relayStrategy": "APP_ONLY" })),
                Some(&Uuid::new_v4().to_string()),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        r.json["session"].clone()
    }

    async fn context(&self, context: &str) -> Value {
        let r = self
            .privacy(
                Method::GET,
                &format!("/v1/privacy-relay/contexts/{context}"),
                None,
                None,
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.text);
        r.json.clone()
    }

    async fn connected_protected(&self, session: Uuid) -> Uuid {
        let w = &self.w;
        let caller = w.device(&w.w.initiator, "caller-main").await;
        let callee: Device = w.device(&w.w.recipient, "callee-main").await;
        let r = w
            .create_protected_call(
                "VOICE",
                &w.w.initiator,
                &caller,
                &[&w.w.recipient],
                session,
                &Uuid::new_v4().to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let call: Uuid = r.json["call"]["callId"].as_str().unwrap().parse().unwrap();
        let invitation = w.invitation_for(call, callee.endpoint).await.unwrap();
        let (_, version) = w.state(call).await;
        assert_eq!(
            w.accept(call, invitation, &callee, version).await.status,
            StatusCode::OK
        );
        let s1 = w.session_on(call, caller.endpoint).await.unwrap();
        let s2 = w.session_on(call, callee.endpoint).await.unwrap();
        w.connect(call, s1).await;
        w.connect(call, s2).await;
        assert_eq!(w.state(call).await.0, CallState::Connected);
        call
    }

    async fn wait_for(&self, call: Uuid, state: CallState, within: Duration) {
        let deadline = tokio::time::Instant::now() + within;
        loop {
            let now = self.w.state(call).await.0;
            if now == state {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "call {call} is {now:?}, expected {state:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn principals(w: &AppWorld) -> (Uuid, Uuid) {
    (w.w.initiator.id().as_uuid(), w.w.recipient.id().as_uuid())
}

#[tokio::test]
async fn contexts_sessions_protected_calls_transfer_and_reassignment_across_services() {
    let Some(s) = Stack::start().await else {
        return;
    };
    let w = &s.w;
    let (a, b) = principals(w);
    let context = s.create_context(&[a, b]).await;
    let context_id = context["contextId"].as_str().unwrap().to_string();
    assert_eq!(
        (context["status"].as_str(), context["version"].as_i64()),
        (Some("ACTIVE"), Some(1))
    );
    let session = s.create_session(&context_id).await;
    let session_id: Uuid = session["relaySessionId"].as_str().unwrap().parse().unwrap();
    assert_eq!(session["participantScope"], "INHERITED");

    // A protected call is authorized by the real Privacy Relay and recorded by Calling.
    let call = s.connected_protected(session_id).await;
    assert_eq!(
        scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM call_privacy_authorizations WHERE call_id = '{call}' AND purpose = 'CALL_CREATE' AND context_version = 1 AND communication_context_ref = '{context_id}'")).await,
        1
    );
    let evidence = s
        .privacy(
            Method::GET,
            &format!("/v1/privacy-relay/contexts/{context_id}/evidence"),
            None,
            None,
        )
        .await;
    assert_eq!(evidence.status, StatusCode::OK, "{}", evidence.text);
    assert!(evidence.text.contains("AUTHORIZED"));

    // An outsider is not a participant: neither a new protected call nor a transfer.
    let outsider = w.principal("outsider-1").await;
    let outsider_device = w.device(&outsider, "outsider-main").await;
    let caller = w.device(&w.w.initiator, "caller-main").await;
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&outsider],
            session_id,
            "denied-1",
        )
        .await;
    assert_eq!(
        (r.status, r.json["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("CALL_NOT_AUTHORIZED")),
        "{}",
        r.text
    );
    let transferring = w.participant_of(call, b).await;
    let (_, version) = w.state(call).await;
    let r = w
        .transfer(call, transferring, &outsider, version, "transfer-1")
        .await;
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED", "{}", r.text);

    // The consumer adds the outsider (same context, new version); the inherited session
    // follows and the transfer is re-authorized.
    let o = outsider.id().as_uuid();
    let r = s
        .privacy(
            Method::PATCH,
            &format!("/v1/privacy-relay/contexts/{context_id}"),
            Some(json!({ "expectedVersion": 1, "participants": [{ "principalId": a }, { "principalId": b }, { "principalId": o }] })),
            Some("add-outsider"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["context"]["version"], 2);
    let r = w
        .transfer(call, transferring, &outsider, version, "transfer-2")
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM call_privacy_authorizations WHERE call_id = '{call}' AND purpose = 'CALL_TRANSFER' AND context_version = 2 AND target_principal_id = '{o}'")).await,
        1
    );
    let _ = outsider_device;

    // Reassignment: the outsider is removed. Privacy Relay delivers PARTICIPANT_REMOVED
    // enforcement and Calling terminates the protected call involving them.
    let r = s
        .privacy(
            Method::PATCH,
            &format!("/v1/privacy-relay/contexts/{context_id}"),
            Some(json!({ "expectedVersion": 2, "participants": [{ "principalId": a }, { "principalId": b }] })),
            Some("remove-outsider"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        r.json["enforcement"]["enforcementEventIds"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let state = w.state(call).await.0;
        if matches!(state, CallState::Ended | CallState::Cancelled) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "call still {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        scalar_i64(w.h().conn(), &format!("SELECT count(*) FROM call_privacy_enforcements WHERE communication_context_ref = '{context_id}' AND status = 'APPLIED' AND terminated_calls = 1")).await,
        1
    );

    // Revocation: new protected calls are refused; ordinary calls are unaffected.
    let current = s.context(&context_id).await["version"].as_i64().unwrap();
    let r = s
        .privacy(
            Method::POST,
            &format!("/v1/privacy-relay/contexts/{context_id}/revoke"),
            Some(json!({ "expectedVersion": current })),
            Some("revoke"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["context"]["status"], "REVOKED");
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            session_id,
            "after-revoke",
        )
        .await;
    assert_eq!(r.json["error"]["code"], "CALL_NOT_AUTHORIZED", "{}", r.text);
    let ordinary = w
        .create_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            "ordinary",
        )
        .await;
    assert_eq!(ordinary.status, StatusCode::CREATED, "{}", ordinary.text);

    // Privacy Relay itself refuses an unknown session and a foreign one alike.
    let r = w
        .create_protected_call(
            "VOICE",
            &w.w.initiator,
            &caller,
            &[&w.w.recipient],
            Uuid::new_v4(),
            "unknown",
        )
        .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "{}", r.text);
}

#[tokio::test]
async fn enforcement_is_retried_until_calling_is_reachable() {
    let Some(mut s) = Stack::start().await else {
        return;
    };
    let (a, b) = principals(&s.w);
    let context = s.create_context(&[a, b]).await;
    let context_id = context["contextId"].as_str().unwrap().to_string();
    let session = s.create_session(&context_id).await;
    let session_id: Uuid = session["relaySessionId"].as_str().unwrap().parse().unwrap();
    let call = s.connected_protected(session_id).await;

    // Calling's internal listener is down while the consumer revokes the context (the
    // public ingress still answers in-process).
    s.stop_calling().await;
    let r = s
        .privacy(
            Method::POST,
            &format!("/v1/privacy-relay/contexts/{context_id}/revoke"),
            Some(json!({ "expectedVersion": 1 })),
            Some("revoke"),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json["enforcement"]["status"], "PENDING", "{}", r.text);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.w.state(call).await.0, CallState::Connected);

    // Calling comes back: the durable job is delivered by the retry worker.
    s.serve_calling().await;
    s.wait_for(call, CallState::Ended, Duration::from_secs(20))
        .await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let r = s
            .privacy(
                Method::GET,
                &format!("/v1/privacy-relay/contexts/{context_id}"),
                None,
                None,
            )
            .await;
        assert_eq!(r.json["status"], "REVOKED");
        let replay = s
            .privacy(
                Method::POST,
                &format!("/v1/privacy-relay/contexts/{context_id}/revoke"),
                Some(json!({ "expectedVersion": 1 })),
                Some("revoke"),
            )
            .await;
        assert_eq!(
            replay
                .headers
                .get("idempotent-replayed")
                .map(|v| v.to_str().unwrap()),
            Some("true")
        );
        if replay.json["enforcement"]["status"] == "APPLIED" {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{}", replay.text);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // Exactly one application on Calling's side.
    assert_eq!(
        scalar_i64(s.w.h().conn(), &format!("SELECT count(*) FROM call_privacy_enforcements WHERE communication_context_ref = '{context_id}'")).await,
        1
    );
}
