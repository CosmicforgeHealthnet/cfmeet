//! CF Meet Developer Platform — Part VI developer / enterprise control plane (foundation).
//!
//! * [`config`]: typed configuration (own database, flags) and internal service
//!   authentication.
//! * [`platform_access`]: the integration boundary to Platform Access, the ONLY authority
//!   for tenants, applications, environments, service accounts, credentials, scopes and
//!   principals. The Developer Platform references their canonical ids and never copies
//!   the hierarchy or verifies a credential.
//! * [`http`]: health / readiness, the served OpenAPI and the service-authenticated
//!   `/internal/v1` API (the API Gateway's forwarded requests).
//! * [`webhooks`]: Section 53 central customer webhooks (event ingestion, subscriptions,
//!   signed SSRF-safe delivery through the shared `cf_webhooks` primitives).
//! * [`worker`]: the Developer Platform's own background loop.
//!
//! It owns no communications data, no E2EE material and no product-specific endpoint, and
//! never reads another service's database.

pub mod config;
pub mod control;
pub mod db;
pub mod domain;
pub mod http;
pub mod platform_access;
pub mod session;
pub mod state;
pub mod webhooks;
pub mod worker;

use std::time::Duration;

use anyhow::anyhow;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

/// Connects to the Developer Platform's own database and refuses to run with pending
/// migrations (fail closed).
pub async fn connect(url: &str, max_connections: u32) -> anyhow::Result<DatabaseConnection> {
    use developer_platform_migration::MigratorTrait;
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
            "cannot connect to the Developer Platform database ({})",
            db::summarize_db_error(&e)
        )
    })?;
    let pending = developer_platform_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| {
            anyhow!(
                "cannot read migration state ({})",
                db::summarize_db_error(&e)
            )
        })?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Developer Platform migration(s) pending; run `cargo run -p developer-platform-migration -- up` first",
            pending.len()
        ));
    }
    Ok(db)
}

/// Installs the tracing subscriber.
pub fn init_tracing(format: config::LogFormat) {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("developer_platform=info,warn"));
    match format {
        config::LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        config::LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}
