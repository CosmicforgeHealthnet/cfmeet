//! Platform Access service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use platform_access::{
    config::{service_auth::ServiceAuthSettings, LogFormat, PlatformAccessConfig},
    domain::{SharedClock, SystemClock},
    http,
    identity::{
        authentication::CredentialAuthenticator, error::summarize_db_error, scopes::ScopeRegistry,
        PlatformService,
    },
    metrics,
    state::AppState,
};
use sea_orm::{ConnectOptions, Database};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let config = match PlatformAccessConfig::from_env() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("platform-access: configuration error: {err}");
            std::process::exit(2);
        }
    };
    let settings = match ServiceAuthSettings::from_env() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!("platform-access: service authentication configuration error: {err}");
            std::process::exit(2);
        }
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("platform_access=info,warn"));
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    // Fail closed on a missing / malformed key or trust bundle before touching the network.
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
        verifier_key_version = config.keyring.current_version(),
        "platform access configuration loaded"
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
            "cannot connect to the Platform Access database ({})",
            summarize_db_error(&e)
        )
    })?;
    let pending = platform_access_migration::pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Platform Access migration(s) pending; run `cargo run -p platform-access-migration -- up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let keyring = Arc::new(config.keyring);
    let registry = Arc::new(ScopeRegistry::standard());
    let platform = PlatformService::new(
        db.clone(),
        clock.clone(),
        keyring.clone(),
        registry.clone(),
        config.credential_policy,
    );
    let authenticator = CredentialAuthenticator::new(db, keyring, registry, clock.clone())
        .with_failure_policy(config.auth_failure_policy);
    tracing::info!(
        end_users = config.build.end_users_enabled,
        user_tokens = config.build.user_tokens_enabled,
        environment_origins = config.build.environment_origins_enabled,
        meeting_platform = config.build.meeting_platform_enabled,
        oauth = config.build.oauth_enabled,
        oauth_pairwise_key = if config.build.oauth_pairwise_key.is_some() { "configured" } else { "derived" },
        user_token_kid = config
            .build
            .user_token_keys
            .as_ref()
            .map(|k| k.kid())
            .unwrap_or(""),
        "platform build features"
    );
    let state = AppState::new(platform, authenticator, service_auth).with_build(config.build);

    // Failed-authentication windows are bounded: prune windows older than an hour.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let before = clock.now() - chrono::Duration::hours(1);
                if let Err(e) = state.authenticator.prune_failure_windows(before).await {
                    tracing::warn!(summary = %summarize_db_error(&e), "failure-window pruning failed");
                }
                // ADR 0062: user-token mint windows are hourly; keep the last two.
                let mint_before = clock.now() - chrono::Duration::hours(2);
                if let Err(e) = platform_access::identity::end_users::prune_mint_windows(
                    state.platform.db(),
                    mint_before,
                )
                .await
                {
                    tracing::warn!(summary = %summarize_db_error(&e), "mint-window pruning failed");
                }
                // ADR 0064: expired authorization codes can never be used; keep a day.
                let code_before = clock.now() - chrono::Duration::days(1);
                if let Err(e) =
                    platform_access::identity::oauth::prune_codes(state.platform.db(), code_before)
                        .await
                {
                    tracing::warn!(summary = %summarize_db_error(&e), "OAuth code pruning failed");
                }
            }
        });
    }

    if let Some(address) = config.metrics_address {
        let router = metrics::router(state.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "platform access metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "platform access listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
