//! API Gateway entrypoint (composition root).

use std::sync::Arc;

use api_gateway::{
    config::{service_auth::ServiceAuthSettings, ApiGatewayConfig, LogFormat},
    http,
    identity::PlatformAccessAuthority,
    routing,
    state::AppState,
};
use tracing_subscriber::EnvFilter;

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
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("api_gateway=info,warn"));
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    let service_auth = settings
        .load(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let routes = routing::table_for(config.flags.public_routing, config.flags.websockets)
        .unwrap_or_else(|e| fatal("route catalogue error", e));
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
    let authority = Arc::new(PlatformAccessAuthority::new(service_auth.client()));
    let state = AppState::new(authority, service_auth, routes)
        .with_flags(config.flags)
        .with_quotas(config.quotas.clone())
        .with_upstream_timeout(config.upstream_timeout)
        .with_max_body_bytes(config.max_body_bytes);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "api gateway listening");
    axum::serve(listener, http::app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
