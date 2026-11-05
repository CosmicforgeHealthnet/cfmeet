//! `security-scan-gateway`: see the crate documentation and `README.md`.

use std::sync::{Arc, RwLock};

use cf_service_auth::{
    services, ServiceId, ServiceTokenVerifier, SharedClock, SystemClock, TrustPolicy,
};
use ed25519_dalek::{pkcs8::DecodePrivateKey, SigningKey};
use security_scan_gateway::{
    clamd::Clamd,
    config::Config,
    metrics::Metrics,
    monitor_engine,
    registry::{Registry, ReplayGuard},
    App, EngineStatus,
};
use tokio::sync::Semaphore;

fn fatal(what: &str, error: impl std::fmt::Display) -> ! {
    tracing::error!(%error, "{what}");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = Config::from_env().unwrap_or_else(|e| fatal("invalid configuration", e));
    let me = ServiceId::parse(services::SECURITY_SCAN).expect("canonical identity");
    let policy = TrustPolicy::from_file(&config.trust_bundle_file, &me, config.environment)
        .unwrap_or_else(|e| fatal("CF_SERVICE_TRUST_BUNDLE_FILE", e));
    let registry_json = std::fs::read_to_string(&config.clients_file)
        .unwrap_or_else(|_| fatal("SCAN_GATEWAY_CLIENTS_FILE", "unreadable"));
    let registry = Registry::from_json(
        &registry_json,
        config.environment,
        &policy,
        config.backend.stream_max_bytes,
        config.backend.max_concurrency,
    )
    .unwrap_or_else(|e| fatal("SCAN_GATEWAY_CLIENTS_FILE", e));
    let pem = std::fs::read_to_string(&config.verdict_key_file)
        .unwrap_or_else(|_| fatal("SCAN_GATEWAY_VERDICT_KEY_FILE", "unreadable"));
    let verdict_key = SigningKey::from_pkcs8_pem(&pem)
        .unwrap_or_else(|_| fatal("SCAN_GATEWAY_VERDICT_KEY_FILE", "not a PKCS#8 Ed25519 key"));
    drop(pem);
    let clock: SharedClock = Arc::new(SystemClock);
    let products: Vec<String> = registry.products().map(str::to_string).collect();
    let app = Arc::new(App {
        environment: config.environment,
        verifier: ServiceTokenVerifier::new(policy, clock.clone()),
        registry,
        replay: ReplayGuard::default(),
        clamd: Clamd::new(
            config.backend.address.clone(),
            config.backend.connect_timeout,
        ),
        backend_slots: Arc::new(Semaphore::new(config.backend.max_concurrency)),
        engine: RwLock::new(EngineStatus::default()),
        engine_name: "clamav",
        max_signature_age: config.backend.max_signature_age,
        verdict_key_id: config.verdict_key_id.as_str().to_string(),
        verdict_key,
        scan_timeout: config.scan_timeout,
        queue_timeout: config.queue_timeout,
        metrics: Metrics::default(),
        clock,
    });
    let public = cf_scan_gateway_contract::public_key(&app.verdict_key_id, &app.verdict_key);
    tracing::info!(
        environment = app.environment.as_str(),
        products = ?products,
        backend_max_concurrency = config.backend.max_concurrency,
        stream_max_bytes = config.backend.stream_max_bytes,
        verdict_kid = %public.kid,
        verdict_public_key = %public.x,
        "security scan gateway starting"
    );
    tokio::spawn(monitor_engine(app.clone()));
    if let Some(addr) = config.metrics_bind {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .unwrap_or_else(|e| fatal("SCAN_GATEWAY_METRICS_ADDRESS", e));
        let router = app.metrics_router();
        tokio::spawn(async move { axum::serve(listener, router).await });
    }
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .unwrap_or_else(|e| fatal("SCAN_GATEWAY_BIND_ADDRESS", e));
    axum::serve(listener, app.router())
        .with_graceful_shutdown(async {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! {
                _ = term.recv() => {},
                _ = tokio::signal::ctrl_c() => {},
            }
        })
        .await
        .unwrap_or_else(|e| fatal("server", e));
}
