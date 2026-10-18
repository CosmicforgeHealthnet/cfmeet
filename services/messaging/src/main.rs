//! Messaging service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use messaging::{
    config::{service_auth::ServiceAuthSettings, LogFormat, MessagingConfig},
    db::summarize_db_error,
    domain::{SharedClock, SystemClock},
    http,
    identity::{
        cache::{CachingAuthority, PlatformAccessFeed},
        PlatformAccessAuthority, PlatformAccessPrincipals,
    },
    metrics,
    state::AppState,
};
use messaging_migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use tracing_subscriber::EnvFilter;

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("messaging: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let config = MessagingConfig::from_env(settings.runtime_environment)
        .unwrap_or_else(|e| fatal("configuration error", e));
    if !settings.has_platform_access() {
        fatal(
            "configuration error",
            "CF_PLATFORM_ACCESS_BASE_URL is required: Platform Access verifies every customer credential",
        );
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("messaging=info,warn"));
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
    let tree_heads = Arc::new(
        config
            .transparency
            .load()
            .unwrap_or_else(|e| fatal("key-transparency configuration error", e)),
    );
    let runtime = service_auth.signer().runtime_environment();
    if !config.devices.enabled {
        tracing::warn!("E2EE device registration and KeyPackage publication are disabled (MESSAGING_E2EE_DEVICES_ENABLED)");
    }
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %runtime,
        e2ee_devices = config.devices.enabled,
        auth_cache_ttl_seconds = config.auth_cache.ttl.as_secs(),
        "messaging configuration loaded"
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
            "cannot connect to the Messaging database ({})",
            summarize_db_error(&e)
        )
    })?;
    let pending = messaging_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Messaging migration(s) pending; run `messaging-migration up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let authority = Arc::new(CachingAuthority::new(
        Arc::new(PlatformAccessAuthority::new(service_auth.client())),
        config.auth_cache.ttl,
    ));
    if !config.auth_cache.ttl.is_zero() {
        tokio::spawn(authority.clone().watch(
            Arc::new(PlatformAccessFeed::new(service_auth.client())),
            config.auth_cache.feed_interval,
        ));
    }
    let managed_keys = match &config.managed_keyring_file {
        Some(path) => Some(Arc::new(
            messaging::messages::managed::ManagedKeyring::load(path)
                .unwrap_or_else(|e| fatal("MESSAGING_MANAGED_KEYRING_FILE", e)),
        )),
        None => {
            tracing::warn!(
                "no managed key ring is configured: MANAGED message sending is disabled"
            );
            None
        }
    };
    let mut state = AppState::new(db.clone(), clock, authority, service_auth.clone())
        .with_principals(Arc::new(PlatformAccessPrincipals::new(
            service_auth.client(),
        )))
        .with_devices(config.devices)
        .with_conversations(config.conversations)
        .with_workers(config.workers)
        .with_tree_head_signer(tree_heads);
    state.managed_keys = managed_keys;
    tokio::spawn(messaging::conversations::service::run_expiry(
        state.clone(),
        config.conversations.expiry_interval,
    ));
    // No event consumer exists yet (realtime gateway / webhooks are later workstreams):
    // the outbox publisher is not started and events stay PENDING (readiness, metrics).
    tracing::info!(
        "no Messaging event consumer is configured: domain events stay PENDING in the outbox"
    );

    if let Some(address) = config.metrics_address {
        let router = metrics::router(state.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "messaging metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "messaging listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Ctrl-C or SIGTERM (container stop / rolling deploy) drains in-flight requests.
async fn shutdown() {
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
        _ = interrupt => {},
        _ = terminate => {},
    }
    tracing::info!("messaging shutting down");
}
