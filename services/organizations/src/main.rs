//! Organizations Service API entrypoint (composition root). The maintenance loop
//! (`worker::run`) runs in the same process.

use std::sync::Arc;

use organizations::{config::OrganizationsConfig, http, state::AppState, worker};

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
    let notifications = config
        .notification_sink(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("notification publication configuration error", e));
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
                "notificationPublications": if notifications.is_some() { "enabled" } else { "disabled" },
                "provisioning": if config.provisioning.is_some() { "enabled" } else { "disabled" },
                "serviceBindings": if config.service_bindings_enabled { "enabled" } else { "disabled" },
                "appInstalls": if config.app_installs_enabled { "enabled" } else { "disabled" },
            })
        );
        return Ok(());
    }
    tracing::info!(
        runtime_environment = %config.runtime_environment,
        provisioning = platform_scopes.is_some(),
        service_bindings = config.service_bindings_enabled,
        event_destinations = ?dispatcher.destinations(),
        notification_publications = notifications.is_some(),
        "organizations configuration loaded"
    );
    if notifications.is_none() {
        tracing::info!("CF_NOTIFICATIONS_BASE_URL is not set: no notification is published");
    }
    let db = organizations::connect(config.database_url(), config.database_max_connections).await?;
    // Gap audit G19: the lifecycle switch needs the pending schema change (rollback-safe
    // releases keep it out of the migration history until it is turned on).
    if config.lifecycle_notifications
        && !organizations::notification_publications::lifecycle_types_admitted(&db).await?
    {
        fatal(
            "notification publication configuration error",
            "ORGANIZATIONS_LIFECYCLE_NOTIFICATIONS_ENABLED=true needs the pending migration \
             m20261118_000007_lifecycle_notification_types (see migration/pending/)",
        );
    }
    let worker_db = db.clone();
    let interval = config.worker_interval;
    let provisioning_enabled = platform_scopes.is_some();
    let platform_port = platform_scopes.clone();
    let event_destinations = dispatcher.destinations();
    let worker = tokio::spawn(worker::run_with(
        worker_db,
        dispatcher,
        platform_scopes,
        notifications,
        interval,
        organizations::shutdown_signal(),
    ));
    let app = http::app(
        AppState::new(db, verifier)
            .with_provisioning(provisioning_enabled)
            .with_platform(platform_port)
            .with_event_destinations(event_destinations)
            .with_app_installs(config.app_installs_enabled),
    );
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "organizations listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(organizations::shutdown_signal())
        .await?;
    worker.abort();
    Ok(())
}
