//! API Gateway entrypoint (composition root).

use std::sync::Arc;

use api_gateway::{
    catalogue,
    config::{service_auth::ServiceAuthSettings, ApiGatewayConfig, LogFormat},
    http,
    identity::PlatformAccessAuthority,
    quota::{DeveloperPlatformQuotas, QuotaEngine},
    state::AppState,
};
use tracing_subscriber::EnvFilter;

/// Resolves on SIGTERM (container stop) or Ctrl-C: in-flight requests drain.
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received: draining");
}

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("api-gateway: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = ApiGatewayConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    config
        .quotas
        .validate_for(settings.runtime_environment)
        .unwrap_or_else(|e| fatal("configuration error", e));
    let check_only = std::env::args().any(|a| a == "--check-config");
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("api_gateway=info,warn"));
    match (check_only, config.log_format) {
        (true, _) => {}
        (false, LogFormat::Json) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        (false, LogFormat::Pretty) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    let service_auth = settings
        .load(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let routes = catalogue::table_for(
        config.flags.public_routing,
        config.flags.websockets,
        &config.controls,
    )
    .unwrap_or_else(|e| fatal("route catalogue error", e));
    if check_only {
        // Part VI §21 deployment preflight: configuration, key file and route catalogue
        // are valid in THIS environment; nothing is connected or served.
        println!(
            "{}",
            serde_json::json!({
                "service": "api-gateway",
                "configuration": "valid",
                "runtimeEnvironment": service_auth.signer().runtime_environment().as_str(),
                "routes": routes.len(),
                "controls": {
                    "API_GATEWAY_PUBLIC_ROUTING_ENABLED": config.flags.public_routing,
                    "API_GATEWAY_WEBSOCKETS_ENABLED": config.flags.websockets,
                    "API_GATEWAY_CENTRAL_WEBHOOKS_ENABLED": config.controls.central_webhooks,
                    "API_GATEWAY_CENTRAL_USAGE_ENABLED": config.controls.central_usage,
                    "API_GATEWAY_DISABLED_DOMAINS": config.controls.disabled_domains.iter().collect::<Vec<_>>(),
                },
                "quotaStoreConfigured": config.quotas.configured(),
                "quotaStoreTimeoutMs": config.quotas.redis_timeout.as_millis() as u64,
            })
        );
        return Ok(());
    }
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        public_routing = config.flags.public_routing,
        websockets = config.flags.websockets,
        routes = routes.len(),
        quotas = config.quotas.configured(),
        "api gateway configuration loaded"
    );
    if !config.flags.public_routing {
        tracing::warn!(
            "API_GATEWAY_PUBLIC_ROUTING_ENABLED is off: every customer path is NOT_FOUND"
        );
    }
    let authority = Arc::new(
        PlatformAccessAuthority::new(service_auth.client())
            .with_health_probe(service_auth.authority_base()),
    );
    // Platform request quotas: shared Redis counters when configured (per-instance
    // otherwise), limits and durable daily accounting from the Developer Platform.
    let mut engine = QuotaEngine::new(
        None,
        config.quotas.key_prefix.clone(),
        Arc::new(DeveloperPlatformQuotas::new(service_auth.client())),
    );
    if let Some(url) = config.quotas.redis_url() {
        let client = redis::Client::open(url)
            .unwrap_or_else(|_| fatal("configuration error", "API_GATEWAY_REDIS_URL"));
        engine = engine.with_store(client, config.quotas.redis_timeout);
    }
    let quota = Arc::new(engine);
    if config.quotas.configured() && !quota.connect_store().await {
        tracing::warn!(
            "quota store unreachable at startup: per-instance counters until it recovers"
        );
    }
    tokio::spawn(quota.clone().run_store_reconnector());
    tokio::spawn(quota.clone().run_flusher(config.quota_flush_interval));
    let state = AppState::new(authority, service_auth, routes)
        .with_flags(config.flags)
        .with_controls(config.controls.clone())
        .with_quotas(config.quotas.clone())
        .with_quota_engine(quota)
        .with_upstream_timeout(config.upstream_timeout)
        .with_max_body_bytes(config.max_body_bytes)
        .with_header_limits(config.max_header_bytes, config.max_headers)
        .with_ws_max_message_bytes(config.ws_max_message_bytes)
        .with_cors_allowed_origins(config.cors_allowed_origins.clone());
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "api gateway listening");
    axum::serve(listener, http::app(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}
