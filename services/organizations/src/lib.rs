//! CF Meet Organizations Service — the first-party business organization authority
//! (Blueprint v1.0 Sections 3–5, ADR 0056; contract `docs/api/organizations-internal-v1.md`).
//!
//! * [`http`]: probes, the served OpenAPI and the service-authenticated `/internal/v1` API
//!   (actor propagation from the Meeting Service relay, default-deny role matrix).
//! * [`domain`]: roles, capabilities and the authorization decision.
//! * [`import`]: the Meeting Service → Organizations migration import and reconciliation.
//! * [`outbox`] / [`worker`]: transactional outbox and the maintenance loop.
//! * [`policy`]: the typed organization policy (Section 6; restrictive only).
//! * [`provisioning`]: the Organizations to Platform Access scope saga (Section 7).
//!
//! It owns no account credential and no meeting, message or call data, stores users as
//! opaque ids and never reads another service's database.

pub mod config;
pub mod db;
pub mod domain;
pub mod error;
pub mod http;
pub mod idempotency;
pub mod import;
pub mod outbox;
pub mod policy;
pub mod provisioning;
pub mod state;
pub mod store;
pub mod validation;
pub mod worker;

use std::time::Duration;

use anyhow::anyhow;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

/// Connects to the Organizations Service's own database and refuses to run with pending
/// migrations (fail closed).
pub async fn connect(url: &str, max_connections: u32) -> anyhow::Result<DatabaseConnection> {
    let mut options = ConnectOptions::new(url.to_string());
    options
        .max_connections(max_connections)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(10))
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(300))
        .max_lifetime(Duration::from_secs(1800))
        .sqlx_logging(false);
    let db = Database::connect(options).await.map_err(|e| {
        anyhow!(
            "cannot connect to the Organizations database ({})",
            db::summarize_db_error(&e)
        )
    })?;
    let pending = pending_migrations(&db).await.map_err(|e| {
        anyhow!(
            "cannot read migration state ({})",
            db::summarize_db_error(&e)
        )
    })?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Organizations migration(s) pending; run `organizations-migration up` first",
            pending.len()
        ));
    }
    Ok(db)
}

/// The Organizations migrations not yet applied (read-only; works under the runtime role).
pub async fn pending_migrations(db: &DatabaseConnection) -> Result<Vec<String>, sea_orm::DbErr> {
    organizations_migration::pending_migrations(db).await
}

/// Resolves on SIGTERM (container stop) or Ctrl-C.
pub async fn shutdown_signal() {
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
        _ = interrupt => {}
        _ = terminate => {}
    }
    tracing::info!("shutdown signal received: draining");
}

/// Installs the tracing subscriber.
pub fn init_tracing(format: config::LogFormat) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("organizations=info,warn"));
    match format {
        config::LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        config::LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}
