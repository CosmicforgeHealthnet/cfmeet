//! Privacy Relay service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use privacy_relay::{
    clock::{SharedClock, SystemClock},
    config::{ContactVaultSettings, FeatureFlag, RelayConfig, ServiceAuthSettings},
    contact_vault::ContactVault,
    enforcement::HttpCallingEnforcer,
    error::summarize_db_error,
    internal, routes,
    service::RelayService,
    state::AppState,
};
use privacy_relay_migration::MigratorTrait;
use sea_orm::{ConnectOptions, Database};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = match RelayConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("privacy-relay: configuration error: {err}");
            std::process::exit(2);
        }
    };
    let settings = match ServiceAuthSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("privacy-relay: service authentication configuration error: {err}");
            std::process::exit(2);
        }
    };
    let vault_settings = match ContactVaultSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("privacy-relay: contact vault configuration error: {err}");
            std::process::exit(2);
        }
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("privacy_relay=info,warn"));
    if config.json_logs {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init();
    } else {
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }
    // Fail closed on a missing / malformed key or trust bundle before touching the network.
    let service_auth = match settings.load(Arc::new(cf_service_auth::SystemClock)) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "service authentication configuration error");
            std::process::exit(2);
        }
    };
    // Contact Vault key material and delegation trust: fail closed BEFORE serving.
    let vault_material = match vault_settings.load(
        service_auth.signer().runtime_environment(),
        service_auth.verifier().policy(),
        Arc::new(cf_service_auth::SystemClock),
    ) {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(error = %err, "contact vault configuration error");
            std::process::exit(2);
        }
    };
    tracing::info!(
        contact_vault_enabled = vault_settings.enabled,
        contact_vault_keys_configured = vault_material.keys.is_some(),
        contact_delegation_configured = vault_material.delegation.is_some(),
        "contact vault configuration"
    );
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        kid = %service_auth.signer().key_id(),
        trusted_issuers = service_auth.verifier().policy().grants().count(),
        calling_enforcement_configured = settings.calling_base_url.is_some(),
        "internal service authentication ready"
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
            "cannot connect to the Privacy Relay database ({})",
            summarize_db_error(&e).0
        )
    })?;
    let pending = privacy_relay_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e).0))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Privacy Relay migration(s) pending; run `cargo run -p privacy-relay-migration -- up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let feature = FeatureFlag::new(config.enabled);
    tracing::info!(
        privacy_relay_enabled = feature.enabled(),
        "privacy relay feature flag"
    );
    let relay = RelayService::new(
        db,
        clock.clone(),
        feature,
        service_auth.signer().runtime_environment(),
        Arc::new(HttpCallingEnforcer::new(service_auth.client().clone())),
    );
    // Expiry and enforcement retry on every instance (SKIP LOCKED makes them safe).
    tokio::spawn(relay.clone().run_workers(config.worker_interval));

    let contacts = ContactVault::new(
        relay.db().clone(),
        clock.clone(),
        FeatureFlag::new(vault_settings.enabled),
        vault_material.keys,
    );
    // Key-rotation re-encryption runs on every instance (SKIP LOCKED batches).
    tokio::spawn(privacy_relay::workers::run_contact_workers(
        contacts.clone(),
        config.worker_interval,
    ));
    let state =
        AppState::new(relay, service_auth).with_contact_vault(contacts, vault_material.delegation);
    let app = routes::app(state, internal::routes());
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "privacy relay service listening");
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
