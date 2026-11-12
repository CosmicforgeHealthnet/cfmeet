//! Developer Platform API entrypoint (composition root).

use std::sync::Arc;

use developer_platform::{
    config::{service_auth::ServiceAuthSettings, DeveloperPlatformConfig},
    http,
    platform_access::HttpPlatformAccess,
    session::MeetingServiceSessions,
    state::AppState,
    webhooks::WebhookService,
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
    // Fail closed on malformed keys, bundles or targets before touching the network.
    let check_only = std::env::args().any(|a| a == "--check-config");
    if !check_only {
        developer_platform::init_tracing(config.log_format);
    }
    let service_auth = settings
        .load(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    if check_only {
        // Part VI §21 deployment preflight: the configuration (and every key / bundle
        // file it names) is valid in THIS environment; nothing is connected or served.
        println!(
            "{}",
            serde_json::json!({
                "service": "developer-platform",
                "configuration": "valid",
                "runtimeEnvironment": service_auth.signer().runtime_environment().as_str(),
                "controls": {
                    "DEVELOPER_PLATFORM_GATEWAY_API_ENABLED": config.flags.gateway_api,
                    "DEVELOPER_PLATFORM_PRODUCTION_ACCESS_WORKFLOW_ENABLED": config.flags.production_access_workflow,
                    "DEVELOPER_PLATFORM_PRODUCTION_AUTO_APPROVAL": config.flags.production_auto_approval,
                    "DEVELOPER_PLATFORM_WEBHOOKS_ENABLED": config.webhooks.enabled,
                    "DEVELOPER_PLATFORM_WEBHOOK_DELIVERY_MODE": config.webhooks.mode.as_str(),
                    "DEVELOPER_PLATFORM_WEBHOOK_MUTATIONS_FROZEN": config.webhooks.mutations_frozen,
                },
                "webhookMigrationTransferKeyConfigured": config.webhooks.transfer_key.is_some(),
            })
        );
        return Ok(());
    }
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        gateway_api = config.flags.gateway_api,
        webhooks = config.webhooks.enabled,
        webhook_mode = config.webhooks.mode.as_str(),
        webhook_mutations_frozen = config.webhooks.mutations_frozen,
        webhook_migration_transfer_key_id = config
            .webhooks
            .transfer_key
            .as_deref()
            .map(|k| k.current_kid()),
        "developer platform configuration loaded"
    );
    let db =
        developer_platform::connect(config.database_url(), config.database_max_connections).await?;
    let mut state = AppState::new(db, service_auth.clone())
        .with_platform_access(Arc::new(HttpPlatformAccess::new(service_auth.client())))
        .with_flags(config.flags)
        .with_production_reviewers(config.production_reviewers.clone());
    if config.webhooks.enabled {
        developer_platform::webhooks::require_signable(&state.db, &config.webhooks)
            .await
            .unwrap_or_else(|e| fatal("webhook configuration error", e));
        let service = WebhookService::new(state.db.clone(), config.webhooks.clone());
        state = state.with_webhooks(service);
    }
    if settings.has_target(cf_service_auth::services::CALLING) {
        state = state.with_calling_export(Arc::new(
            developer_platform::webhooks::migration::port::HttpCallingMigration::new(
                service_auth.client(),
            ),
        ));
    }
    if settings.has_target(cf_service_auth::services::MEETING_SERVICE) {
        state = state.with_sessions(Arc::new(MeetingServiceSessions::new(service_auth.client())));
    } else {
        tracing::warn!(
            "CF_MEETING_SERVICE_BASE_URL is not set: Developer Console sessions are refused"
        );
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "developer platform listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(developer_platform::shutdown_signal())
        .await?;
    Ok(())
}
