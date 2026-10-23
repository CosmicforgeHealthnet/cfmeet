//! A mock platform-event receiver (Calling's or the Developer Platform's
//! `POST /internal/v1/platform/events`, Part VI §53) for publication tests.
//!
//! It verifies the REAL service token against a receiver-authoritative trust bundle that
//! grants exactly one permission (`platform-event.publish` on Calling,
//! `developer-event.publish` on the Developer Platform) to the producer's key, refuses an
//! envelope whose `producer_service` is not the authenticated caller, de-duplicates on
//! (producer, `event_id`) like both real receivers, and records every accepted envelope.
//! Scripted statuses make it fail (transiently or for good) on demand.
#![allow(dead_code)]

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use cf_service_auth::{
    client::{InternalServiceClient, InternalTargets},
    InternalPermission, KeyId, RuntimeEnvironment, ServiceId, ServiceSigningKey,
    ServiceTokenSigner, ServiceTokenVerifier, SharedClock, SystemClock, TokenTtl, TrustPolicy,
};
use serde_json::{json, Value};

#[derive(Default)]
struct Inner {
    /// Accepted envelopes (first acceptance of each event id).
    accepted: Vec<Value>,
    /// Every authenticated request (envelope, correlation header), accepted or not.
    attempts: Vec<(Value, String)>,
    /// Statuses to answer the next requests with (FIFO; empty: accept).
    script: Vec<(u16, &'static str)>,
    seen_ids: BTreeSet<String>,
    unauthenticated: usize,
}

#[derive(Clone)]
pub struct Receiver {
    pub url: String,
    pub service: ServiceId,
    inner: Arc<Mutex<Inner>>,
}

struct Ctx {
    verifier: ServiceTokenVerifier,
    permission: InternalPermission,
    inner: Arc<Mutex<Inner>>,
}

async fn ingest(
    State(ctx): State<Arc<Ctx>>,
    headers: HeaderMap,
    Json(envelope): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let error = |status: StatusCode, code: &str| {
        (
            status,
            Json(json!({ "error": { "code": code, "message": code } })),
        )
    };
    let Ok(caller) = ctx.verifier.verify_headers(&headers) else {
        ctx.inner.lock().unwrap().unauthenticated += 1;
        return error(StatusCode::UNAUTHORIZED, "SERVICE_AUTHENTICATION_FAILED");
    };
    if !caller.has_permission(ctx.permission) {
        return error(StatusCode::FORBIDDEN, "SERVICE_PERMISSION_DENIED");
    }
    let correlation = caller.correlation_id().as_str().to_string();
    let mut inner = ctx.inner.lock().unwrap();
    inner.attempts.push((envelope.clone(), correlation));
    if envelope["producer_service"].as_str() != Some(caller.authenticated_service().as_str()) {
        return error(StatusCode::FORBIDDEN, "PRODUCER_MISMATCH");
    }
    if !inner.script.is_empty() {
        let (status, code) = inner.script.remove(0);
        if !(200..300).contains(&status) {
            return error(StatusCode::from_u16(status).unwrap(), code);
        }
    }
    let id = envelope["event_id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    if inner.seen_ids.insert(id) {
        inner.accepted.push(envelope);
        (StatusCode::OK, Json(json!({ "status": "ACCEPTED" })))
    } else {
        (StatusCode::OK, Json(json!({ "status": "DUPLICATE" })))
    }
}

impl Receiver {
    /// Starts a receiver running as `service`, trusting `producer`'s `key` with
    /// `permission` only.
    pub async fn start(
        service: ServiceId,
        permission: &str,
        producer: &ServiceId,
        key: &ServiceSigningKey,
        environment: RuntimeEnvironment,
    ) -> Self {
        let bundle = json!({
            "bundle_version": 1,
            "receiver": service.as_str(),
            "runtime_environment": environment,
            "trusted_issuers": [{
                "issuer": producer.as_str(),
                "audience": service.as_str(),
                "runtime_environment": environment,
                "keys": [key.public_jwk()],
                "permissions": [permission],
            }],
        })
        .to_string();
        let policy = TrustPolicy::from_json(&bundle, &service, environment).unwrap();
        let clock: SharedClock = Arc::new(SystemClock);
        let inner = Arc::new(Mutex::new(Inner::default()));
        let ctx = Arc::new(Ctx {
            verifier: ServiceTokenVerifier::new(policy, clock),
            permission: InternalPermission::parse(permission).unwrap(),
            inner: inner.clone(),
        });
        let app = Router::new()
            .route("/internal/v1/platform/events", post(ingest))
            .with_state(ctx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self {
            url,
            service,
            inner,
        }
    }

    /// The Developer Platform's ingestion (`developer-event.publish`).
    pub async fn developer_platform(
        producer: &ServiceId,
        key: &ServiceSigningKey,
        environment: RuntimeEnvironment,
    ) -> Self {
        Self::start(
            ServiceId::developer_platform(),
            "developer-event.publish",
            producer,
            key,
            environment,
        )
        .await
    }

    /// Calling's platform-event ingress (`platform-event.publish`).
    pub async fn calling(
        producer: &ServiceId,
        key: &ServiceSigningKey,
        environment: RuntimeEnvironment,
    ) -> Self {
        Self::start(
            ServiceId::calling(),
            "platform-event.publish",
            producer,
            key,
            environment,
        )
        .await
    }

    pub fn accepted(&self) -> Vec<Value> {
        self.inner.lock().unwrap().accepted.clone()
    }

    pub fn attempts(&self) -> Vec<(Value, String)> {
        self.inner.lock().unwrap().attempts.clone()
    }

    pub fn unauthenticated(&self) -> usize {
        self.inner.lock().unwrap().unauthenticated
    }

    /// Answers the next requests with these statuses (and error codes).
    pub fn script(&self, outcomes: &[(u16, &'static str)]) {
        self.inner
            .lock()
            .unwrap()
            .script
            .extend_from_slice(outcomes);
    }
}

/// A fresh signing key.
pub fn signing_key(kid: &str) -> ServiceSigningKey {
    ServiceSigningKey::generate(KeyId::parse(kid).unwrap())
}

/// The producer's outbound client, with base URLs for the given receivers.
pub fn producer_client(
    producer: ServiceId,
    key: ServiceSigningKey,
    environment: RuntimeEnvironment,
    receivers: &[&Receiver],
) -> Arc<InternalServiceClient> {
    let clock: SharedClock = Arc::new(SystemClock);
    let signer = Arc::new(ServiceTokenSigner::new(
        producer,
        environment,
        key,
        TokenTtl::default(),
        clock,
    ));
    let mut targets = InternalTargets::new(environment);
    for r in receivers {
        targets.insert(r.service.clone(), &r.url).unwrap();
    }
    Arc::new(
        InternalServiceClient::new(signer, targets, std::time::Duration::from_secs(5)).unwrap(),
    )
}
