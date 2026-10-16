//! Telephony service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use sea_orm::{ConnectOptions, Database};
use telephony::{
    clock::{SharedClock, SystemClock},
    config::{ServiceAuthSettings, TelephonyConfig},
    error::summarize_db_error,
    internal,
    ports::{HttpCalling, HttpPrivacyRelay},
    providers::registry_from_database,
    routes,
    service::{ExecutionSettings, TelephonyService},
    sip::edge_from_env,
    state::AppState,
};
use telephony_migration::MigratorTrait;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = match TelephonyConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("telephony: configuration error: {err}");
            std::process::exit(2);
        }
    };
    let execution = match ExecutionSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("telephony: execution configuration error: {err}");
            std::process::exit(2);
        }
    };
    let settings = match ServiceAuthSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("telephony: service authentication configuration error: {err}");
            std::process::exit(2);
        }
    };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("telephony=info,warn"));
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
    let runtime_environment = service_auth.signer().runtime_environment();
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %runtime_environment,
        kid = %service_auth.signer().key_id(),
        trusted_issuers = service_auth.verifier().policy().grants().count(),
        calling_configured = settings.calling_base_url.is_some(),
        privacy_relay_configured = settings.privacy_relay_base_url.is_some(),
        "internal service authentication ready"
    );
    // The SIP / media edge (none = every SIP route fails safely; app RTC is unaffected).
    let edge = match edge_from_env(runtime_environment) {
        Ok(edge) => edge,
        Err(err) => {
            tracing::error!(error = %err, "SIP / media edge configuration error");
            std::process::exit(2);
        }
    };

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
            "cannot connect to the Telephony database ({})",
            summarize_db_error(&e).0
        )
    })?;
    let pending = telephony_migration::Migrator::get_pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e).0))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Telephony migration(s) pending; run `cargo run -p telephony-migration -- up` first",
            pending.len()
        ));
    }
    // Interconnects: configured provider rows + their adapters (credentials come from
    // secret files, never from the database).
    let providers = match registry_from_database(&db, runtime_environment, edge.clone()).await {
        Ok(p) => p,
        Err(err) => {
            tracing::error!(error = %err, "interconnect configuration error");
            std::process::exit(2);
        }
    };
    let clock: SharedClock = Arc::new(SystemClock);
    let flags = config.flags();
    tracing::info!(
        telephony_enabled = flags.telephony.enabled(),
        public_outbound_enabled = flags.public_outbound.enabled(),
        public_inbound_enabled = flags.public_inbound.enabled(),
        callback_relay_enabled = flags.callback_relay.enabled(),
        interconnects = providers.ids().count(),
        sip_edge = edge.name(),
        "telephony configuration"
    );
    let telephony = TelephonyService::new(db, clock, flags)
        .with_providers(providers)
        .with_edge(edge.clone())
        .with_privacy_relay(Arc::new(HttpPrivacyRelay::new(service_auth.clone())))
        .with_calling(Arc::new(HttpCalling::new(service_auth.clone())))
        .with_numbering(if settings.numbering_base_url.is_some() {
            Arc::new(telephony::ports::HttpNumbering::new(service_auth.clone()))
        } else {
            Arc::new(telephony::ports::UnconfiguredNumbering)
        })
        .with_settings(execution);
    // Edge events (inbound dialogs, call progress, application media joins).
    tokio::spawn(telephony.clone().run_edge_events());
    // Commands, outbox, reconciliation, usage, health: every instance (SKIP LOCKED).
    tokio::spawn(telephony.clone().run_workers(config.worker_interval));

    let app = routes::app(
        AppState::new(telephony.clone(), service_auth),
        internal::routes(),
    );
    if let Some(address) = config.metrics_address {
        let metrics = ::telephony::metrics::router(telephony.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "telephony metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, metrics).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "telephony listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
