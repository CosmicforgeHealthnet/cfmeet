//! # CosmicForge shared security scan gateway
//!
//! One authenticated internal endpoint, `POST /internal/v1/scans`, in front of an EXISTING
//! ClamAV engine. The gateway loads no signatures, stores no file, keeps no copy and no
//! reusable credential after a scan, and returns a signed verdict bound to the caller, the
//! request and the SHA-256 of the exact bytes the engine received.
//!
//! Order of checks (nothing is read from the body before authorization):
//!
//! 1. service token verified against the receiver-authoritative trust bundle (issuer, key,
//!    audience, runtime environment, lifetime, request/correlation binding);
//! 2. `security-scan.submit` granted and the issuer registered (its product and limits);
//! 3. the token id was never used before (single-use tokens);
//! 4. declared size, content digest header and per-product rate budget;
//! 5. a per-product slot, then a slot at the shared engine (bounded waiting);
//! 6. a reachable engine with fresh signatures;
//! 7. streaming to clamd `INSTREAM` while hashing, under one scan deadline.
//!
//! Every failure is a non-definitive verdict (never `CLEAN`).

pub mod clamd;
pub mod config;
pub mod metrics;
pub mod registry;

use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use axum::{
    body::Body,
    extract::State,
    http::{header, HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use cf_scan_gateway_contract::{
    self as contract, EngineFacts, Reason, ScanResponse, Verdict, CONTENT_SHA256_HEADER,
    CONTRACT_VERSION, SCAN_PATH,
};
use cf_service_auth::{InternalPermission, RuntimeEnvironment, ServiceTokenVerifier, SharedClock};
use ed25519_dalek::SigningKey;
use futures_util::StreamExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::{
    clamd::{Clamd, EngineVersion, Reply},
    metrics::Metrics,
    registry::{Client, Registry, ReplayGuard},
};

/// How long an engine probe stays valid for admission.
pub const ENGINE_STATUS_TTL: Duration = Duration::from_secs(120);
/// Engine probe interval.
pub const ENGINE_PROBE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default)]
pub struct EngineStatus {
    pub version: Option<EngineVersion>,
    pub checked_at: Option<Instant>,
}

pub struct App {
    pub environment: RuntimeEnvironment,
    pub verifier: ServiceTokenVerifier,
    pub registry: Registry,
    pub replay: ReplayGuard,
    pub clamd: Clamd,
    pub backend_slots: Arc<Semaphore>,
    pub engine: RwLock<EngineStatus>,
    pub engine_name: &'static str,
    pub max_signature_age: Duration,
    pub verdict_key: SigningKey,
    pub verdict_key_id: String,
    pub scan_timeout: Duration,
    pub queue_timeout: Duration,
    pub metrics: Metrics,
    pub clock: SharedClock,
}

/// Engine admission decision.
enum EngineGate {
    Ready(EngineVersion, u64),
    Unavailable(Reason),
}

impl App {
    /// Probes the engine (`VERSION`) and records the result.
    pub async fn refresh_engine(&self) {
        let version = self.clamd.version().await.ok();
        let now = self.clock.now_unix();
        self.metrics.set(
            "cosmicforge_scan_backend_up",
            &[],
            version.is_some() as u8 as f64,
        );
        if let Some(v) = &version {
            self.metrics.set(
                "cosmicforge_scan_signature_age_seconds",
                &[],
                (now - v.signature_time).max(0) as f64,
            );
            self.metrics.set(
                "cosmicforge_scan_signature_version",
                &[],
                v.signature_version as f64,
            );
        }
        let mut status = self.engine.write().expect("engine status");
        status.version = version;
        status.checked_at = Some(Instant::now());
    }

    fn engine_gate(&self) -> EngineGate {
        let status = self.engine.read().expect("engine status").clone();
        let fresh = status
            .checked_at
            .is_some_and(|t| t.elapsed() <= ENGINE_STATUS_TTL);
        let Some(version) = status.version.filter(|_| fresh) else {
            return EngineGate::Unavailable(Reason::BackendUnavailable);
        };
        let age = self.clock.now_unix() - version.signature_time;
        // A database dated in the future is as untrustworthy as a stale one.
        if age < -3600 || age > self.max_signature_age.as_secs() as i64 {
            return EngineGate::Unavailable(Reason::SignaturesStale);
        }
        let age = age.max(0) as u64;
        EngineGate::Ready(version, age)
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/health", get(|| async { Json(json!({"status": "ok"})) }))
            .route("/ready", get(ready))
            .route(SCAN_PATH, post(scan))
            .with_state(self.clone())
    }

    pub fn metrics_router(self: &Arc<Self>) -> Router {
        Router::new()
            .route(
                "/metrics",
                get(|State(app): State<Arc<App>>| async move { app.metrics.render() }),
            )
            .with_state(self.clone())
    }
}

/// Background engine monitor.
pub async fn monitor_engine(app: Arc<App>) {
    loop {
        app.refresh_engine().await;
        tokio::time::sleep(ENGINE_PROBE_INTERVAL).await;
    }
}

async fn ready(State(app): State<Arc<App>>) -> Response {
    match app.engine_gate() {
        EngineGate::Ready(v, age) => Json(json!({
            "status": "ready",
            "engine": app.engine_name,
            "engineVersion": v.engine,
            "signatureVersion": v.signature_version,
            "signatureAgeSeconds": age,
        }))
        .into_response(),
        EngineGate::Unavailable(reason) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"status": "not_ready", "reason": reason.as_str()})),
        )
            .into_response(),
    }
}

fn refuse(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error": {"code": code}}))).into_response()
}

struct Inflight<'a> {
    app: &'a App,
    product: &'a str,
}

impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.app.metrics.add(
            "cosmicforge_scan_inflight",
            &[("product", self.product)],
            -1.0,
        );
    }
}

struct Outcome {
    verdict: Verdict,
    reason: Option<Reason>,
    finding: Option<String>,
    sha256: String,
    bytes: u64,
    engine: Option<(EngineVersion, u64)>,
}

impl Outcome {
    fn of(verdict: Verdict, reason: Reason) -> Self {
        Self {
            verdict,
            reason: Some(reason),
            finding: None,
            sha256: String::new(),
            bytes: 0,
            engine: None,
        }
    }
}

async fn scan(
    State(app): State<Arc<App>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let started = Instant::now();
    // SEC-012: bound to this request (method + path; the scan route is not nested) and
    // single-use; the registry's own per-client replay check below stays.
    let verified = app
        .verifier
        .verify_request(&method, uri.path(), &headers)
        .await;
    let ctx = match verified {
        Ok(ctx) => ctx,
        Err(e) => {
            app.metrics.inc(
                "cosmicforge_scan_auth_failures_total",
                &[("reason", e.failure.as_str())],
                1,
            );
            tracing::warn!(target: "audit", event = "scan.refused", failure = e.failure.as_str());
            return refuse(StatusCode::UNAUTHORIZED, "UNAUTHENTICATED");
        }
    };
    if ctx
        .require_permission(InternalPermission::SecurityScanSubmit)
        .is_err()
    {
        app.metrics.inc(
            "cosmicforge_scan_auth_failures_total",
            &[("reason", "permission")],
            1,
        );
        tracing::warn!(target: "audit", event = "scan.refused", failure = "permission", issuer = %ctx.authenticated_service());
        return refuse(StatusCode::FORBIDDEN, "FORBIDDEN");
    }
    let Some(client) = app.registry.get(ctx.authenticated_service()) else {
        app.metrics.inc(
            "cosmicforge_scan_auth_failures_total",
            &[("reason", "unregistered")],
            1,
        );
        tracing::warn!(target: "audit", event = "scan.refused", failure = "unregistered", issuer = %ctx.authenticated_service());
        return refuse(StatusCode::FORBIDDEN, "FORBIDDEN");
    };
    let now = app.clock.now_unix();
    if !app.replay.first_use(
        client.issuer.as_str(),
        ctx.token_id().as_str(),
        ctx.expires_at(),
        now,
    ) {
        app.metrics.inc(
            "cosmicforge_scan_auth_failures_total",
            &[("reason", "replay")],
            1,
        );
        tracing::warn!(target: "audit", event = "scan.refused", failure = "replay", issuer = %client.issuer);
        return refuse(StatusCode::UNAUTHORIZED, "UNAUTHENTICATED");
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type != contract::BODY_CONTENT_TYPE {
        return refuse(StatusCode::UNSUPPORTED_MEDIA_TYPE, "UNSUPPORTED_MEDIA_TYPE");
    }
    let expected = headers
        .get(CONTENT_SHA256_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| contract::is_sha256_hex(v))
        .map(str::to_string);
    let Some(expected) = expected else {
        return refuse(StatusCode::BAD_REQUEST, "INVALID_CONTENT_DIGEST");
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let Some(declared) = declared else {
        return refuse(StatusCode::LENGTH_REQUIRED, "LENGTH_REQUIRED");
    };

    let outcome = admit_and_scan(&app, &client, body, declared, &expected).await;
    respond(
        &app,
        &client,
        ctx.request_id().as_str(),
        ctx.correlation_id().as_str(),
        outcome,
        started,
    )
}

async fn admit_and_scan(
    app: &App,
    client: &Client,
    body: Body,
    declared: u64,
    expected: &str,
) -> Outcome {
    if declared == 0 {
        return Outcome::of(Verdict::Rejected, Reason::EmptyContent);
    }
    if declared > client.max_bytes {
        return Outcome::of(Verdict::Rejected, Reason::TooLarge);
    }
    if !client.admit(Instant::now()) {
        return Outcome::of(Verdict::Rejected, Reason::RateLimited);
    }
    // Per-product slot first, so one product waiting for the engine can never hold
    // another product's capacity; then the shared engine slot.
    let Ok(Ok(_own)) =
        tokio::time::timeout(app.queue_timeout, client.slots.clone().acquire_owned()).await
    else {
        return Outcome::of(Verdict::Unavailable, Reason::Overloaded);
    };
    let Ok(Ok(_engine)) =
        tokio::time::timeout(app.queue_timeout, app.backend_slots.clone().acquire_owned()).await
    else {
        return Outcome::of(Verdict::Unavailable, Reason::Overloaded);
    };
    let (version, age) = match app.engine_gate() {
        EngineGate::Ready(v, age) => (v, age),
        EngineGate::Unavailable(reason) => return Outcome::of(Verdict::Unavailable, reason),
    };
    app.metrics.add(
        "cosmicforge_scan_inflight",
        &[("product", client.product.as_str())],
        1.0,
    );
    let _inflight = Inflight {
        app,
        product: &client.product,
    };
    let mut outcome = match tokio::time::timeout(
        app.scan_timeout,
        stream_to_engine(app, body, declared, expected),
    )
    .await
    {
        Ok(o) => o,
        Err(_) => Outcome::of(Verdict::Timeout, Reason::ScanTimeout),
    };
    if outcome.verdict.is_definitive() {
        outcome.engine = Some((version, age));
    }
    outcome
}

async fn stream_to_engine(app: &App, body: Body, declared: u64, expected: &str) -> Outcome {
    let mut session = match app.clamd.start_instream().await {
        Ok(s) => s,
        Err(_) => return Outcome::of(Verdict::Unavailable, Reason::BackendUnavailable),
    };
    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    let mut frames = body.into_data_stream();
    let mut engine_closed = false;
    while let Some(frame) = frames.next().await {
        let Ok(frame) = frame else {
            return Outcome::of(Verdict::Rejected, Reason::Truncated);
        };
        total += frame.len() as u64;
        if total > declared {
            return Outcome::of(Verdict::Rejected, Reason::LengthMismatch);
        }
        hasher.update(&frame);
        if session.send(&frame).await.is_err() {
            engine_closed = true;
            break;
        }
    }
    let reply = if engine_closed {
        match session.reply().await {
            Ok(Reply::SizeLimit) => return Outcome::of(Verdict::Rejected, Reason::TooLarge),
            _ => return Outcome::of(Verdict::Unavailable, Reason::BackendUnavailable),
        }
    } else {
        if total != declared {
            return Outcome::of(Verdict::Rejected, Reason::Truncated);
        }
        match session.finish().await {
            Ok(r) => r,
            Err(_) => return Outcome::of(Verdict::Unavailable, Reason::BackendUnavailable),
        }
    };
    let sha256 = hex::encode(hasher.finalize());
    let outcome = |verdict, reason, finding| Outcome {
        verdict,
        reason,
        finding,
        sha256: sha256.clone(),
        bytes: total,
        engine: None,
    };
    if sha256 != expected {
        return outcome(Verdict::Rejected, Some(Reason::DigestMismatch), None);
    }
    match reply {
        Reply::Clean => outcome(Verdict::Clean, None, None),
        Reply::Infected(name) => outcome(Verdict::Infected, None, Some(name)),
        Reply::SizeLimit => outcome(Verdict::Rejected, Some(Reason::TooLarge), None),
        Reply::Error => outcome(Verdict::Error, Some(Reason::BackendError), None),
        Reply::Malformed => outcome(Verdict::Error, Some(Reason::MalformedBackendReply), None),
    }
}

fn respond(
    app: &App,
    client: &Client,
    request_id: &str,
    correlation_id: &str,
    o: Outcome,
    started: Instant,
) -> Response {
    let elapsed = started.elapsed().as_secs_f64();
    let mut r = ScanResponse {
        contract_version: CONTRACT_VERSION.into(),
        request_id: request_id.into(),
        client_id: client.issuer.as_str().into(),
        product: client.product.clone(),
        environment: app.environment.as_str().into(),
        verdict: o.verdict,
        reason: o.reason,
        finding: o.finding,
        sha256: o.sha256,
        bytes: o.bytes,
        engine: o.engine.map(|(v, age)| EngineFacts {
            name: app.engine_name.into(),
            version: v.engine,
            signature_version: v.signature_version,
            signature_age_seconds: age,
        }),
        scanned_at: app.clock.now_unix(),
        verdict_key_id: None,
        verdict_signature: None,
    };
    if r.verdict.is_definitive()
        && contract::sign(&mut r, &app.verdict_key_id, &app.verdict_key).is_err()
    {
        // Unsignable (e.g. an engine field with a newline): never return it as definitive.
        r.verdict = Verdict::Error;
        r.reason = Some(Reason::MalformedBackendReply);
        r.finding = None;
        r.engine = None;
        r.verdict_key_id = None;
    }
    let reason = r.reason.map(|x| x.as_str()).unwrap_or("NONE");
    app.metrics.inc(
        "cosmicforge_scan_requests_total",
        &[
            ("product", client.product.as_str()),
            ("verdict", r.verdict.as_str()),
            ("reason", reason),
        ],
        1,
    );
    app.metrics.inc(
        "cosmicforge_scan_bytes_total",
        &[("product", client.product.as_str())],
        r.bytes,
    );
    app.metrics.observe(&client.product, elapsed);
    tracing::info!(
        target: "audit",
        event = "scan.completed",
        request_id,
        correlation_id,
        client = client.issuer.as_str(),
        product = client.product.as_str(),
        verdict = r.verdict.as_str(),
        reason,
        finding = r.finding.as_deref().unwrap_or(""),
        bytes = r.bytes,
        signature_version = r.engine.as_ref().map(|e| e.signature_version).unwrap_or(0),
        duration_ms = (elapsed * 1000.0) as u64,
    );
    let status =
        StatusCode::from_u16(r.verdict.http_status(r.reason)).unwrap_or(StatusCode::BAD_GATEWAY);
    (status, Json(r)).into_response()
}
