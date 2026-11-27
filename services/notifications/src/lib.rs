//! CF Meet Unified Notification Service (docs/audits/notification-service; roadmap
//! N2–N13): the single notification delivery authority.
//!
//! * [`ingest`] — I1, the responsibility boundary (validation, dedupe, HELD / shadow).
//! * [`processor`] — W1: recipients from the owners (X1–X5), the precedence engine
//!   ([`policy`]) over the preference authority ([`preferences`]), rendering
//!   ([`templates`]) and deliveries per service-side routing mode ([`routing`]).
//! * [`email`] — the email ledger and lane workers (Resend / SMTP / none; X6/X7 leases).
//! * [`push`] — push endpoints, the push dispatcher and the I3 call fast lane.
//! * [`inbox`] — the in-app inbox (U1–U5).
//! * [`partner`] — partner push (ADR 0063, `NOTIFICATIONS_PARTNER_PUSH_ENABLED`): the
//!   provider vault, `platform-principal` registrations, call fast lane and recipients.
//! * [`http`] — probes, metrics, the Resend callback and the internal APIs (I1–I12).
//!
//! Boundaries: Calling owns ringing and signaling, Account owns codes and their
//! verification, Messaging owns E2EE content and conversation authorization, the Developer
//! Platform owns customer webhooks. This service never stores plaintext codes, tokens,
//! addresses or push handles at rest outside sealed columns.

pub mod authorities;
pub mod catalogue;
pub mod config;
pub mod crypto;
pub mod db;
pub mod email;
pub mod error;
pub mod http;
pub mod inbox;
pub mod ingest;
pub mod metrics;
pub mod partner;
pub mod policy;
pub mod preference_copy;
pub mod preferences;
pub mod processor;
pub mod push;
pub mod routing;
pub mod state;
pub mod templates;
pub mod workers;

use std::time::Duration;

use anyhow::anyhow;
use sea_orm::{ConnectOptions, Database, DatabaseConnection};

/// Connects to the Notification Service's own database and refuses to run with pending
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
            "cannot connect to the Notifications database ({})",
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
            "{} Notifications migration(s) pending; run `notifications-migration up` first",
            pending.len()
        ));
    }
    Ok(db)
}

/// The Notifications migrations not yet applied (read-only; works under the runtime role).
pub async fn pending_migrations(db: &DatabaseConnection) -> Result<Vec<String>, sea_orm::DbErr> {
    notifications_migration::pending_migrations(db).await
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
        .unwrap_or_else(|_| EnvFilter::new("notifications=info,warn"));
    match format {
        config::LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        config::LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}
