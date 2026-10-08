//! Calling service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use calling::{
    calls::{app::AppCallingConfig, CallServiceConfig},
    config::CallingConfig,
    domain::{SharedClock, SystemClock},
    internal_auth::config::ServiceAuthSettings,
    media::MediaSettings,
    platform::{error::summarize_db_error, scopes::ScopeRegistry},
    privacy_relay::{HttpPrivacyRelay, PrivacyRelayAuthority, UnconfiguredPrivacyRelay},
    routes,
    signaling::{NoPushTransport, PgNotifySignalBus, RealtimeSignalBus, SignalingKeys},
    state::{AppState, CallingRuntime},
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
    let service_auth_settings = match ServiceAuthSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("calling: service authentication configuration error: {err}");
            std::process::exit(2);
        }
    };
    telemetry::init(config.log_format);
    // Fail closed on a missing / malformed key or trust bundle before touching the network.
    let service_auth = match service_auth_settings.load(Arc::new(cf_service_auth::SystemClock)) {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "service authentication configuration error");
            std::process::exit(2);
        }
    };
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %service_auth.signer().runtime_environment(),
        kid = %service_auth.signer().key_id(),
        trusted_issuers = service_auth.verifier().policy().grants().count(),
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

    // Application calling (Part C §20–§25): no fallback signaling key or provider
    // credential exists; an invalid configuration stops the service before it serves.
    let lookup = |key: &str| std::env::var(key).ok();
    let signaling_keys = match SignalingKeys::from_lookup(&lookup) {
        Ok(keys) => Arc::new(keys),
        Err(err) => {
            tracing::error!(error = %err, "signaling configuration error");
            std::process::exit(2);
        }
    };
    let media_settings = match MediaSettings::from_lookup(&lookup) {
        Ok(settings) => settings,
        Err(err) => {
            tracing::error!(error = %err, "media provider configuration error");
            std::process::exit(2);
        }
    };
    let media = match media_settings.build() {
        Ok(media) => media,
        Err(err) => {
            tracing::error!(error = %err, "media provider configuration error");
            std::process::exit(2);
        }
    };
    let app_config = match AppCallingConfig::from_lookup(&lookup) {
        Ok(config) => config,
        Err(err) => {
            tracing::error!(error = %err, "application calling configuration error");
            std::process::exit(2);
        }
    };
    tracing::info!(
        application_media = media.provider.is_some(),
        "application calling configured"
    );
    // Cross-instance realtime fanout over PostgreSQL LISTEN / NOTIFY.
    let bus: Arc<dyn RealtimeSignalBus> = Arc::new(
        PgNotifySignalBus::start(db.clone())
            .await
            .map_err(|e| anyhow!("cannot start the realtime signal listener ({e})"))?,
    );

    // Privacy Relay (Part D): protected calls and the public ingress need it; ordinary
    // application calling never does. Without CF_PRIVACY_RELAY_BASE_URL both fail closed.
    let privacy_relay_configured = service_auth_settings
        .targets
        .iter()
        .any(|(service, _)| service.as_str() == cf_service_auth::services::PRIVACY_RELAY);
    let privacy_relay: Arc<dyn PrivacyRelayAuthority> = if privacy_relay_configured {
        Arc::new(HttpPrivacyRelay::new(service_auth.clone()))
    } else {
        Arc::new(UnconfiguredPrivacyRelay)
    };
    tracing::info!(
        privacy_relay_configured,
        "privacy relay authority configured"
    );

    // Contact Vault delegated access (§31): a dedicated key, never the service-auth key.
    let contact_delegation =
        match calling::privacy_relay::contacts::ContactDelegationSettings::from_lookup(&lookup)
            .and_then(|s| {
                s.load(
                    service_auth.signer().runtime_environment(),
                    &service_auth.signer().public_jwk(),
                    Arc::new(cf_service_auth::SystemClock),
                )
            }) {
            Ok(signer) => signer.map(Arc::new),
            Err(err) => {
                tracing::error!(error = %err, "contact delegation configuration error");
                std::process::exit(2);
            }
        };
    tracing::info!(
        contact_delegation_configured = contact_delegation.is_some(),
        "contact vault delegated access configured"
    );

    // Telephony (Part E): public-network execution. Without CF_TELEPHONY_BASE_URL no
    // public route exists; every telecom switch defaults OFF; app RTC never depends on it.
    let telephony_configured = service_auth_settings
        .targets
        .iter()
        .any(|(service, _)| service.as_str() == cf_service_auth::services::TELEPHONY);
    let telecom_flags = match calling::calls::app::public::telecom_flags_from_lookup(&lookup) {
        Ok(flags) => flags,
        Err(err) => {
            tracing::error!(error = %err, "telecom configuration error");
            std::process::exit(2);
        }
    };
    let public_network = calling::calls::app::public::PublicNetwork {
        telephony: if telephony_configured {
            Arc::new(calling::telephony::HttpTelephony::new(service_auth.clone()))
        } else {
            Arc::new(calling::telephony::UnconfiguredTelephony)
        },
        media: media.public_bridge.clone(),
        flags: telecom_flags,
    };
    tracing::info!(
        telephony_configured,
        public_outbound_enabled = telecom_flags.public_outbound,
        public_inbound_enabled = telecom_flags.public_inbound,
        callback_relay_enabled = telecom_flags.callback_relay,
        public_media_bridge = media.public_bridge.is_some(),
        "public-network calling configured"
    );

    let clock: SharedClock = Arc::new(SystemClock);
    let state = AppState::new(
        db,
        clock.clone(),
        Arc::new(config.keyring),
        Arc::new(ScopeRegistry::standard()),
        config.limit_defaults,
        service_auth,
        CallingRuntime {
            signaling_keys,
            media: media.provider,
            media_gateway: media.gateway,
            credential_policy: media_settings.credential_policy,
            bus,
            push: Arc::new(NoPushTransport),
            app_config,
            call_config: CallServiceConfig::default(),
            privacy_relay,
            contact_delegation,
            public_network,
        },
    );

    // Durable invitation expiry and media reconciliation on every instance (row locks /
    // SKIP LOCKED leases make the sweeps multi-instance safe).
    let workers = (*state.app).clone();
    tokio::spawn(workers.run_workers(Duration::from_secs(1), Duration::from_secs(2)));

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

    // Public customer API (call reads, creation, control, endpoints, signaling credentials,
    // the Privacy Relay ingress) and the internal API (Privacy Relay enforcement), which
    // like every /internal/v1/* path requires service authentication.
    let app = routes::app(state, routes::public_api(), routes::internal_api());
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
