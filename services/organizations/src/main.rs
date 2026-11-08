//! Organizations Service API entrypoint (composition root). The maintenance loop
//! (`worker::run`) runs in the same process.

use std::sync::Arc;

use organizations::{
    config::OrganizationsConfig, http, state::AppState, worker,
};

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("organizations: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config =
        OrganizationsConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    let check_only = std::env::args().any(|a| a == "--check-config");
    if !check_only {
        organizations::init_tracing(config.log_format);
    }
    // Fail closed on a malformed or foreign trust bundle before touching the network.
    let verifier = config
        .verifier(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let platform_scopes = config
        .platform_scopes(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("provisioning configuration error", e));
    let dispatcher = config
        .event_dispatcher(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("event delivery configuration error", e));
    if check_only {
        println!(
            "{}",
            serde_json::json!({
                "service": "organizations",
                "configuration": "valid",
                "runtimeEnvironment": config.runtime_environment.as_str(),
                "bindAddress": config.bind_address.to_string(),
                "outboxDelivery": if dispatcher.enabled() { "enabled" } else { "disabled" },
                "eventDestinations": dispatcher.destinations(),
                "provisioning": if config.provisioning.is_some() { "enabled" } else { "disabled" },
            })
        );
        return Ok(());
    }
    tracing::info!(
        runtime_environment = %config.runtime_environment,
        provisioning = platform_scopes.is_some(),
        event_destinations = ?dispatcher.destinations(),
        "organizations configuration loaded"
    );
    let db = organizations::connect(config.database_url(), config.database_max_connections).await?;
    let worker_db = db.clone();
    let interval = config.worker_interval;
    let provisioning_enabled = platform_scopes.is_some();
    let worker = tokio::spawn(worker::run_with(
        worker_db,
        dispatcher,
        platform_scopes,
        interval,
        organizations::shutdown_signal(),
    ));
    let app = http::app(AppState::new(db, verifier).with_provisioning(provisioning_enabled));
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "organizations listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(organizations::shutdown_signal())
        .await?;
    worker.abort();
    Ok(())
}
