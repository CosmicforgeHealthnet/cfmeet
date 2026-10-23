//! Developer Platform API entrypoint (composition root).

use std::sync::Arc;

use developer_platform::{
    config::{service_auth::ServiceAuthSettings, DeveloperPlatformConfig},
    http,
    platform_access::HttpPlatformAccess,
    state::AppState,
};

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("developer-platform: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config =
        DeveloperPlatformConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    if !settings.has_platform_access() {
        fatal(
            "configuration error",
            "CF_PLATFORM_ACCESS_BASE_URL is required: Platform Access is the identity authority",
        );
    }
    developer_platform::init_tracing(config.log_format);
    // Fail closed on malformed keys, bundles or targets before touching the network.
    let service_auth = settings
        .load(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        gateway_api = config.flags.gateway_api,
        "developer platform configuration loaded"
    );
    let db =
        developer_platform::connect(config.database_url(), config.database_max_connections).await?;
    let state = AppState::new(db, service_auth.clone())
        .with_platform_access(Arc::new(HttpPlatformAccess::new(service_auth.client())))
        .with_flags(config.flags);
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "developer platform listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
