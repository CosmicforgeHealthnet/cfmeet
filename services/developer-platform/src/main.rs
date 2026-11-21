//! Developer Platform API entrypoint (composition root).

use std::sync::Arc;

use developer_platform::{
    config::{service_auth::ServiceAuthSettings, DeveloperPlatformConfig},
    http,
    notification_publications::Publishing,
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
                    "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE":
                        if config.notification_routes.production_access { "notification-service" } else { "legacy" },
                    "DEVELOPER_PLATFORM_NOTIFICATIONS_CREDENTIAL_ROUTE":
                        if config.notification_routes.credential_lifecycle { "notification-service" } else { "off" },
                    "DEVELOPER_PLATFORM_USERS_API_ENABLED": config.build.users_api,
                    "DEVELOPER_PLATFORM_ALLOWED_ORIGINS_ENABLED": config.build.allowed_origins,
                    "DEVELOPER_PLATFORM_TEST_TOKENS_ENABLED": config.build.test_tokens,
                    "DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED": config.build.push_credentials,
                    "DEVELOPER_PLATFORM_CREATION_CAPS_ENABLED": config.build.creation_caps,
                    "DEVELOPER_PLATFORM_MAX_WORKSPACES_PER_OWNER": config.build.max_workspaces_per_owner,
                    "DEVELOPER_PLATFORM_MAX_PROJECTS_PER_WORKSPACE": config.build.max_projects_per_workspace,
                    "DEVELOPER_PLATFORM_APPS_ENABLED": config.build.apps,
                    "DEVELOPER_PLATFORM_OAUTH_ENABLED": config.build.oauth,
                    "DEVELOPER_PLATFORM_APP_DIRECTORY_ENABLED": config.build.app_directory,
                    "DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED": config.build.install_events,
                    "DEVELOPER_PLATFORM_APP_REVIEWERS": config.app_reviewers.len(),
                },
                "notificationsSealingKeyConfigured": config.notifications_sealing_key.is_some(),
                "notificationServiceConfigured": settings.has_target(cf_service_auth::services::NOTIFICATIONS),
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
        .with_build(config.build)
        .with_production_reviewers(config.production_reviewers.clone())
        .with_oauth(Arc::new(
            developer_platform::apps::port::HttpPlatformOAuth::new(service_auth.client()),
        ))
        .with_app_settings(developer_platform::apps::AppSettings {
            reviewers: config.app_reviewers.clone(),
            endpoints: config.oauth_endpoints.clone(),
            ..Default::default()
        });
    if config.webhooks.enabled {
        developer_platform::webhooks::require_signable(&state.db, &config.webhooks)
            .await
            .unwrap_or_else(|e| fatal("webhook configuration error", e));
        let service = WebhookService::new(state.db.clone(), config.webhooks.clone())
            .with_install_events(config.build.install_events);
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
        state = state
            .with_sessions(Arc::new(MeetingServiceSessions::new(service_auth.client())))
            .with_account_profiles(Arc::new(
                developer_platform::apps::port::HttpAccountProfiles::new(service_auth.client()),
            ));
    } else {
        tracing::warn!(
            "CF_MEETING_SERVICE_BASE_URL is not set: Developer Console sessions are refused"
        );
    }
    // ADR 0063 N-P1: push credential uploads need the Notification Service AND its public
    // sealing key (secrets are sealed on receipt); otherwise they answer 503.
    if config.build.push_credentials {
        match (
            settings.has_target(cf_service_auth::services::NOTIFICATIONS),
            config.notifications_sealing_key.clone(),
        ) {
            (true, Some(sealing_key)) => {
                state = state.with_push(developer_platform::push_providers::PushProviders {
                    port: Arc::new(developer_platform::push_providers::HttpPushProviders::new(
                        service_auth.client(),
                    )),
                    sealing_key,
                });
            }
            _ => tracing::warn!(
                "DEVELOPER_PLATFORM_PUSH_CREDENTIALS_ENABLED needs CF_NOTIFICATIONS_BASE_URL and \
                 CF_NOTIFICATIONS_SEALING_KEY: push credential uploads are unavailable"
            ),
        }
    }
    // Notification Service N7 / N11: production-access decisions are enqueued for the
    // Notification Service only on the explicit route AND with CF_NOTIFICATIONS_BASE_URL.
    let notifications_configured = settings.has_target(cf_service_auth::services::NOTIFICATIONS);
    if config.notification_routes.production_access {
        if notifications_configured {
            state = state.with_notifications(Publishing {
                production_access: true,
            });
            tracing::warn!(
                "production-access decisions are published to the Notification Service: the \
                 operator-console decision email (Meeting Service developer-access-decision) must be off"
            );
        } else {
            tracing::warn!(
                "DEVELOPER_PLATFORM_NOTIFICATIONS_PRODUCTION_ACCESS_ROUTE=notification-service needs \
                 CF_NOTIFICATIONS_BASE_URL: production-access decisions are not published"
            );
        }
    }
    // ADR 0068 A4: secrets produced by admin bridge commands are delivered by the
    // Developer Platform itself (sealed to the Notification Service), never returned to HQ.
    match (
        notifications_configured,
        config.notifications_sealing_key.clone(),
    ) {
        (true, Some(sealing_key)) => {
            state = state.with_owner_delivery(
                developer_platform::control::owner_delivery::OwnerDelivery { sealing_key },
            );
        }
        _ => tracing::info!(
            "owner-delivered secrets need CF_NOTIFICATIONS_BASE_URL and CF_NOTIFICATIONS_SEALING_KEY:              the admin bridge's first-party workspace registration is unavailable"
        ),
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    if config.production_access_withheld {
        tracing::warn!(
            "production-access submissions stay disabled: no named reviewer \
             (DEVELOPER_PLATFORM_PRODUCTION_REVIEWERS)"
        );
    }
    if config.auto_approval_refused {
        tracing::warn!("automatic production approval is not permitted and was ignored");
    }
    tracing::info!(address = %config.bind_address, "developer platform listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(developer_platform::shutdown_signal())
        .await?;
    Ok(())
}
