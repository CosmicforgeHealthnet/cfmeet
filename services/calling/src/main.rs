//! Calling service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use axum::Router;
use calling::{
    config::CallingConfig,
    domain::{SharedClock, SystemClock},
    platform::{error::summarize_db_error, scopes::ScopeRegistry},
    routes,
    state::AppState,
    telemetry,
};
use calling_migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};

/// Rate windows older than this are pruned (windows are one minute long).
const RATE_WINDOW_RETENTION: chrono::Duration = chrono::Duration::minutes(5);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = match CallingConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("calling: configuration error: {err}");
            std::process::exit(2);
        }
    };
    telemetry::init(config.log_format);

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
            "cannot connect to the Calling database ({})",
            summarize_db_error(&e)
        )
    })?;

    let pending = calling_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Calling migration(s) pending; run `cargo run -p calling-migration -- up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let state = AppState::new(
        db,
        clock.clone(),
        Arc::new(config.keyring),
        Arc::new(ScopeRegistry::standard()),
        config.limit_defaults,
    );

    let pruner = state.rate_limiter.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Err(err) = pruner.prune(clock.now() - RATE_WINDOW_RETENTION).await {
                tracing::warn!(summary = %summarize_db_error(&err), "rate window pruning failed");
            }
        }
    });

    // The Calling business API (Part C) is not implemented yet: no protected routes.
    let app = routes::app(state, Router::new());
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "calling service listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
