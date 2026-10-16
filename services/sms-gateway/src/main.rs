//! SMS Gateway service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use sea_orm::{ConnectOptions, Database};
use sms_gateway::{
    config::{
        read_key_file, service_auth::ServiceAuthSettings, LogFormat, ProviderChoice,
        SmsGatewayConfig,
    },
    db::summarize_db_error,
    domain::{SharedClock, SystemClock},
    http,
    identity::{PlatformAccessAuthority, PlatformAccessPrincipals},
    infra::{
        outbox::{OutboxPublisher, PublisherSettings},
        platform_events::PlatformEventTransport,
    },
    metrics,
    numbering::{HttpNumbering, NumberingPort, UnconfiguredNumbering},
    providers::{deterministic::DeterministicSmsProvider, guard_runtime, SharedProvider},
    sms::protection::Protection,
    state::AppState,
};
use sms_gateway_migration::MigratorTrait;
use tracing_subscriber::EnvFilter;

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("sms-gateway: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = SmsGatewayConfig::from_env().unwrap_or_else(|e| fatal("configuration error", e));
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    if !settings.has_platform_access() {
        fatal(
            "configuration error",
            "CF_PLATFORM_ACCESS_BASE_URL is required: Platform Access verifies every customer credential",
        );
    }
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("sms_gateway=info,warn"));
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    // Fail closed on malformed keys, bundles or targets before touching the network.
    let service_auth = settings
        .load(Arc::new(cf_service_auth::SystemClock))
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let protection = Protection::load(&config.protection)
        .unwrap_or_else(|e| fatal("content protection configuration error", e));
    let runtime = service_auth.signer().runtime_environment();
    let provider: Option<SharedProvider> = match &config.provider {
        ProviderChoice::None => {
            tracing::warn!(
                "no SMS provider is configured: sending is refused (PROVIDER_UNAVAILABLE)"
            );
            None
        }
        ProviderChoice::Deterministic {
            callback_secret_file,
        } => {
            let secret = zeroize::Zeroizing::new(
                read_key_file(
                    "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE",
                    callback_secret_file,
                )
                .unwrap_or_else(|e| fatal("provider configuration error", e)),
            );
            if secret.len() < 32 {
                fatal(
                    "provider configuration error",
                    "SMS_GATEWAY_PROVIDER_CALLBACK_SECRET_FILE must hold at least 32 bytes",
                );
            }
            let provider = DeterministicSmsProvider::new("deterministic", &secret);
            guard_runtime(&provider, runtime)
                .unwrap_or_else(|e| fatal("provider configuration error", e));
            tracing::warn!(
                "the deterministic SMS provider is active (local / test only, never a carrier)"
            );
            Some(Arc::new(provider))
        }
    };
    let has_target = |service: &str| settings.targets.iter().any(|(s, _)| s.as_str() == service);
    let numbering: Arc<dyn NumberingPort> = if has_target(cf_service_auth::services::NUMBERING) {
        Arc::new(HttpNumbering::new(service_auth.client()))
    } else {
        tracing::warn!("CF_NUMBERING_BASE_URL is not set: every SMS operation fails closed");
        Arc::new(UnconfiguredNumbering)
    };
    let publish_events = has_target(cf_service_auth::services::CALLING);
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %runtime,
        provider = provider.as_ref().map(|p| p.id().to_string()).unwrap_or_else(|| "none".into()),
        publish_events,
        "sms gateway configuration loaded"
    );

    let mut options = ConnectOptions::new(config.database_url().to_string());
    options
        .max_connections(config.database_max_connections)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .sqlx_logging(false);
    let db = Database::connect(options).await.map_err(|e| {
        anyhow!(
            "cannot connect to the SMS Gateway database ({})",
            summarize_db_error(&e)
        )
    })?;
    let pending = sms_gateway_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} SMS Gateway migration(s) pending; run `cargo run -p sms-gateway-migration -- up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let authority = Arc::new(PlatformAccessAuthority::new(service_auth.client()));
    let principals = Arc::new(PlatformAccessPrincipals::new(service_auth.client()));
    let mut state = AppState::new(
        db.clone(),
        clock.clone(),
        authority,
        service_auth.clone(),
        protection,
    )
    .with_numbering(numbering)
    .with_principals(principals)
    .with_transport(config.transport)
    .with_controls(config.controls.clone())
    .with_workers(config.workers);
    if let Some(p) = provider {
        state = state.with_provider(p);
    }

    // Event publication to the platform's customer webhook delivery (Calling).
    if publish_events {
        let publisher = Arc::new(OutboxPublisher::new(
            db.clone(),
            Arc::new(PlatformEventTransport::new(service_auth.client())),
            clock.clone(),
            PublisherSettings {
                lease: config.workers.lease,
                max_attempts: config.workers.outbox_max_attempts,
                retry: config.workers.retry,
                batch: 100,
            },
            runtime.as_str(),
        ));
        tokio::spawn(publisher.run(config.workers.interval));
    } else {
        tracing::warn!("CF_CALLING_BASE_URL is not set: events stay PENDING in the outbox");
    }
    tokio::spawn(sms_gateway::sms::workers::run(state.clone()));

    if let Some(address) = config.metrics_address {
        let router = metrics::router(state.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "sms gateway metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "sms gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
