//! Numbering service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use numbering::{
    config::{service_auth::ServiceAuthSettings, LogFormat, NumberingConfig},
    domain::{SharedClock, SystemClock},
    http,
    identity::PlatformAccessAuthority,
    infra::{idempotency, jobs::JobRunner},
    metrics,
    numbers::error::summarize_db_error,
    state::AppState,
};
use numbering_migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = match NumberingConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("numbering: configuration error: {err}");
            std::process::exit(2);
        }
    };
    let settings = match ServiceAuthSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("numbering: service authentication configuration error: {err}");
            std::process::exit(2);
        }
    };
    if !settings.has_platform_access() {
        eprintln!("numbering: CF_PLATFORM_ACCESS_BASE_URL is required: Platform Access verifies every customer credential");
        std::process::exit(2);
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("numbering=info,warn"));
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    // Fail closed on a missing / malformed key, trust bundle or target before touching the
    // network.
    let service_auth = match settings.load(Arc::new(cf_service_auth::SystemClock)) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "service authentication configuration error");
            std::process::exit(2);
        }
    };
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        trusted_issuers = service_auth.verifier().policy().grants().count(),
        "numbering configuration loaded"
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
            "cannot connect to the Numbering database ({})",
            summarize_db_error(&e)
        )
    })?;
    let pending = numbering_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Numbering migration(s) pending; run `cargo run -p numbering-migration -- up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let authority = Arc::new(PlatformAccessAuthority::new(service_auth.client()));
    let state = AppState::new(
        db.clone(),
        clock.clone(),
        authority,
        service_auth,
        config.workers,
    );

    // Worker foundation: lease recovery of interrupted jobs (UNKNOWN_OUTCOME parking for
    // provider-affecting ones) and idempotency-key expiry. Business job handlers are
    // registered by later workstreams; the outbox publisher starts once a consumer
    // transport is configured (events stay PENDING and visible in /ready until then).
    {
        let runner = Arc::new(JobRunner::new(
            db.clone(),
            clock.clone(),
            format!("numbering-{}", std::process::id()),
            config.workers.lease,
            config.workers.retry,
        ));
        tokio::spawn(runner.run(config.workers.interval));
        let db = db.clone();
        let clock = clock.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                if let Err(e) = idempotency::purge_expired(&db, clock.now(), 1_000).await {
                    tracing::warn!(summary = %summarize_db_error(&e), "idempotency purge failed");
                }
            }
        });
    }

    if let Some(address) = config.metrics_address {
        let router = metrics::router(state.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "numbering metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "numbering listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
