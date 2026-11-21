//! Developer Platform worker entrypoint: its own background loop, separate from the API
//! process (independently scalable and restartable).

use developer_platform::config::{service_auth::ServiceAuthSettings, DeveloperPlatformConfig};

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("developer-platform-worker: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config =
        DeveloperPlatformConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    // The worker signs as the Developer Platform too: validate the same identity inputs.
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    developer_platform::init_tracing(config.log_format);
    let runtime = settings.runtime_environment.as_str().to_string();
    let db =
        developer_platform::connect(config.database_url(), config.database_max_connections).await?;
    let webhooks = if config.webhooks.enabled {
        developer_platform::webhooks::require_signable(&db, &config.webhooks)
            .await
            .unwrap_or_else(|e| fatal("webhook configuration error", e));
        Some(
            developer_platform::webhooks::WebhookService::new(db.clone(), config.webhooks.clone())
                .with_install_events(config.build.install_events),
        )
    } else {
        None
    };
    tracing::info!(instance = %config.worker_instance_id, runtime_environment = %runtime,
        interval_ms = config.worker_interval.as_millis() as u64,
        webhooks = config.webhooks.enabled, webhook_mode = config.webhooks.mode.as_str(),
        "developer platform worker started");
    // SIGTERM / Ctrl-C: the delivery being sent is finished and recorded, the rest of the
    // claimed batch is released, then the loop exits (no abandoned send to re-deliver).
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        developer_platform::shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let webhooks = webhooks.map(|w| w.with_stop(stop_rx.clone()));
    // Section 15: with Platform Access configured the worker also settles organization
    // creations and renames that did not complete there (ORG-014 / ORG-013).
    let notifications_configured = settings.has_target(cf_service_auth::services::NOTIFICATIONS);
    let bridge_configured = settings.has_target(cf_service_auth::services::ADMIN_BRIDGE);
    let runtime_auth = (settings.has_platform_access()
        || notifications_configured
        || bridge_configured)
        .then(|| {
            settings
                .load(std::sync::Arc::new(cf_service_auth::SystemClock))
                .unwrap_or_else(|e| fatal("service authentication configuration error", e))
        });
    let platform: Option<
        std::sync::Arc<dyn developer_platform::platform_access::PlatformAccessPort>,
    > = if let Some(runtime_auth) = runtime_auth
        .as_ref()
        .filter(|_| settings.has_platform_access())
    {
        Some(std::sync::Arc::new(
            developer_platform::platform_access::HttpPlatformAccess::new(runtime_auth.client()),
        ))
    } else {
        tracing::warn!(
            "CF_PLATFORM_ACCESS_BASE_URL is not set: organization reconciliation is disabled"
        );
        None
    };
    // Section 15 (PLT-008) credential lifecycle webhooks (with webhooks and Platform
    // Access) and the DATA-017 report-only reference reconciliation.
    // Notification Service N7 / N11: the publication relay (and the credential notice
    // sweep, when routed) runs only with CF_NOTIFICATIONS_BASE_URL; unset, nothing is
    // enqueued or sent (today's behaviour).
    let notifications = match runtime_auth.as_ref().filter(|_| notifications_configured) {
        Some(runtime_auth) => {
            let expiry_notices = config.notification_routes.credential_lifecycle;
            tracing::info!(
                expiry_notices,
                "Developer Platform publications are relayed to the Notification Service"
            );
            Some(developer_platform::worker::NotificationJobs {
                sink: std::sync::Arc::new(
                    developer_platform::notification_publications::NotificationServiceEvents::new(
                        runtime_auth.client(),
                    ),
                ),
                credential_notices: expiry_notices,
            })
        }
        None => {
            tracing::info!(
                "CF_NOTIFICATIONS_BASE_URL is not set: nothing is published to the Notification Service"
            );
            None
        }
    };
    let jobs = developer_platform::worker::PeriodicJobs {
        credential_lifecycle: config.credential_lifecycle.clone(),
        reference_reconciliation_interval: config.reference_reconciliation_interval,
        reference_reconciliation_remote_limit: config.reference_reconciliation_remote_limit,
        notifications,
        // ADR 0064: the APP_GRANT projection (Platform Access) when install events are on.
        app_grants: match (
            config.build.install_events,
            runtime_auth
                .as_ref()
                .filter(|_| settings.has_platform_access()),
        ) {
            (true, Some(runtime_auth)) => Some(developer_platform::worker::AppGrantJobs {
                port: std::sync::Arc::new(developer_platform::apps::port::HttpPlatformOAuth::new(
                    runtime_auth.client(),
                )),
            }),
            (true, None) => {
                tracing::warn!(
                    "DEVELOPER_PLATFORM_INSTALL_EVENTS_ENABLED needs CF_PLATFORM_ACCESS_BASE_URL: \
                     the app grant projection is disabled"
                );
                None
            }
            (false, _) => None,
        },
        // ADR 0068 A7: owner events to the admin bridge (off unless CF_ADMIN_BRIDGE_BASE_URL).
        bridge_events: runtime_auth
            .as_ref()
            .filter(|_| bridge_configured)
            .map(|runtime_auth| {
                tracing::info!("owner events are posted to the admin bridge");
                developer_platform::worker::BridgeEventJobs {
                    sink: std::sync::Arc::new(
                        developer_platform::bridge_events::HttpBridgeEvents::new(
                            runtime_auth.client(),
                        ),
                    ),
                }
            }),
        retention: match developer_platform::retention::RetentionSettings::from_lookup(
            &|k: &str| std::env::var(k).ok(),
        ) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "retention configuration error");
                std::process::exit(2);
            }
        },
    };
    let mut stopped = stop_rx;
    developer_platform::worker::run_with_jobs(
        db,
        config.worker_instance_id,
        runtime,
        config.worker_interval,
        webhooks,
        platform,
        jobs,
        async move {
            let _ = stopped.wait_for(|s| *s).await;
        },
    )
    .await;
    tracing::info!("developer platform worker stopped");
    Ok(())
}
