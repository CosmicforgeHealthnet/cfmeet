//! Numbering service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use numbering::{
    config::{
        kyc::{KycConfig, VerifierChoice},
        providers::ProviderConfig,
        service_auth::ServiceAuthSettings,
        LogFormat, NumberingConfig,
    },
    domain::{SharedClock, SystemClock},
    http,
    identity::PlatformAccessAuthority,
    infra::{idempotency, jobs::JobRunner},
    metrics,
    numbers::{
        error::summarize_db_error,
        kyc::{
            verifier::{DeterministicKycVerifier, KycVerifier, ManualKycVerifier},
            KycService, VerificationHandler,
        },
    },
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
    // KYC / eligibility (fail closed: the deterministic verifier is refused in staging /
    // production; evidence is refused without a protected store and key).
    let kyc_config = match KycConfig::from_lookup(
        |key| std::env::var(key).ok(),
        settings.runtime_environment,
        None,
    ) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("numbering: KYC configuration error: {err}");
            std::process::exit(2);
        }
    };
    let provider_config = match ProviderConfig::from_lookup(
        |key| std::env::var(key).ok(),
        settings.runtime_environment,
    ) {
        Ok(c) => c,
        Err(err) => {
            eprintln!("numbering: provider configuration error: {err}");
            std::process::exit(2);
        }
    };
    let stewardship_settings =
        match numbering::config::stewardship::from_lookup(|key| std::env::var(key).ok()) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("numbering: stewardship configuration error: {err}");
                std::process::exit(2);
            }
        };
    let acquisition_settings =
        match numbering::config::acquisition::from_lookup(|key| std::env::var(key).ok()) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("numbering: acquisition configuration error: {err}");
                std::process::exit(2);
            }
        };
    let grant_settings = match numbering::config::grants::GrantSettings::from_lookup(|key| {
        std::env::var(key).ok()
    }) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("numbering: routing grant configuration error: {err}");
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
    let grant_signer = match &grant_settings {
        None => {
            tracing::warn!("no routing grant key is configured: routing grants are refused (503)");
            None
        }
        Some(g) => match g.load(&settings, Arc::new(cf_service_auth::SystemClock)) {
            Ok(signer) => Some(signer),
            Err(err) => {
                tracing::error!(error = %err, "routing grant configuration error");
                std::process::exit(2);
            }
        },
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
    let principals = Arc::new(numbering::identity::PlatformAccessPrincipals::new(
        service_auth.client(),
    ));
    let mut state = AppState::new(
        db.clone(),
        clock.clone(),
        authority,
        service_auth,
        config.workers,
    );
    if let Some(signer) = grant_signer {
        state = state.with_grants(signer, Arc::new(cf_service_auth::SystemClock));
    }
    let verifier: Arc<dyn KycVerifier> = match kyc_config.verifier {
        VerifierChoice::Manual => Arc::new(ManualKycVerifier),
        VerifierChoice::Deterministic => {
            tracing::warn!("the deterministic KYC verifier is active (test-only, never an external KYC provider)");
            Arc::new(DeterministicKycVerifier::new())
        }
    };
    if !kyc_config.vault.is_configured() {
        tracing::warn!(
            "no protected KYC evidence store is configured: evidence submissions are refused"
        );
    }
    let kyc = KycService::new(
        state.numbers.clone(),
        kyc_config.vault.clone(),
        verifier,
        kyc_config.settings,
    );
    let state = state.with_kyc(kyc.clone(), kyc_config.eligibility.clone());
    let runtime = state.service_auth.signer().runtime_environment();
    let providers = match provider_config.registry(runtime, clock.clone()) {
        Ok(registry) => registry.map(|r| {
            numbering::providers::commands::ProviderCommands::new(
                state.numbers.clone(),
                r,
                Default::default(),
            )
        }),
        Err(err) => {
            tracing::error!(error = %err, "provider configuration error");
            std::process::exit(2);
        }
    };
    let state = match &providers {
        Some(p) => state.with_providers(p.clone()),
        None => state,
    }
    .with_acquisition_settings(acquisition_settings)
    .with_principals(principals)
    .with_stewardship_settings(stewardship_settings);
    let acquisition = Arc::new(state.acquisition());
    let stewardship = Arc::new(state.stewardship());

    // Part VI §53: the Developer Platform consumes the outbox (public lifecycle events of
    // customer scopes; everything else is acknowledged without leaving Numbering).
    {
        use numbering::infra::{
            outbox::{OutboxPublisher, PublisherSettings},
            platform_events::{DeveloperPlatformEvents, PublicEventsDestination},
        };
        let dp_configured = settings
            .targets
            .iter()
            .any(|(s, _)| s.as_str() == cf_service_auth::services::DEVELOPER_PLATFORM);
        match config.public_events {
            PublicEventsDestination::DeveloperPlatform if dp_configured => {
                let publisher = Arc::new(OutboxPublisher::new(
                    db.clone(),
                    Arc::new(DeveloperPlatformEvents::new(state.service_auth.client())),
                    clock.clone(),
                    PublisherSettings {
                        lease: config.workers.lease,
                        max_attempts: config.workers.outbox_max_attempts,
                        retry: config.workers.retry,
                        batch: 100,
                    },
                    runtime.as_str(),
                ));
                tokio::spawn(publisher.run(config.workers.interval));
                tracing::info!(
                    "public number lifecycle events are published to the Developer Platform"
                );
            }
            PublicEventsDestination::DeveloperPlatform => tracing::warn!(
                "CF_DEVELOPER_PLATFORM_BASE_URL is not set: outbox events stay PENDING"
            ),
            PublicEventsDestination::None => tracing::info!(
                "NUMBERING_PUBLIC_EVENTS_DESTINATION=none: outbox events stay PENDING"
            ),
        }
    }

    // Worker foundation: lease recovery of interrupted jobs (UNKNOWN_OUTCOME parking for
    // provider-affecting ones) and idempotency-key expiry. Business job handlers are
    // registered by later workstreams; the outbox publisher (above) starts once its
    // consumer is configured (events stay PENDING and visible in /ready until then).
    {
        let mut runner = JobRunner::new(
            db.clone(),
            clock.clone(),
            format!("numbering-{}", std::process::id()),
            config.workers.lease,
            config.workers.retry,
        )
        .with_handler(Arc::new(VerificationHandler { kyc: kyc.clone() }))
        .with_handler(Arc::new(
            numbering::acquisition::reservations::ExpiryHandler {
                acquisition: acquisition.clone(),
            },
        ));
        if let Some(p) = &providers {
            runner = runner
                .with_handler(Arc::new(numbering::providers::commands::DispatchHandler {
                    commands: p.clone(),
                }))
                .with_handler(Arc::new(numbering::providers::commands::ReconcileHandler {
                    commands: p.clone(),
                }));
        }
        let runner = Arc::new(runner);
        // Provider commands interrupted mid-dispatch become UNKNOWN and are reconciled.
        if let Some(p) = providers.clone() {
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(60));
                loop {
                    ticker.tick().await;
                    let ctx = numbering::domain::RequestContext::generate();
                    if let Err(e) = p.recover_interrupted(&ctx, 500).await {
                        tracing::warn!(error = %e, "provider command recovery failed");
                    }
                }
            });
        }
        tokio::spawn(runner.run(config.workers.interval));
        // Reservation expiry (leased jobs) and owed provider-hold releases; search quota
        // windows are forgotten once they ended.
        {
            let acquisition = acquisition.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(15));
                loop {
                    ticker.tick().await;
                    let ctx = numbering::domain::RequestContext::generate();
                    if let Err(e) = acquisition.sweep_reservations(&ctx, 500).await {
                        tracing::warn!(error = %e, "reservation sweep failed");
                    }
                    let before =
                        acquisition.numbers().clock().now() - chrono::Duration::minutes(10);
                    if let Err(e) = acquisition.purge_search_windows(before).await {
                        tracing::warn!(error = %e, "search quota purge failed");
                    }
                    if let Err(e) = acquisition.numbers().purge_expired_grants().await {
                        tracing::warn!(error = %e, "routing grant purge failed");
                    }
                }
            });
        }
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
        // Lifecycle stewardship (Workstreams P–X): suspension / restoration and release
        // steps, quarantine completion, relay lease expiry, usage accrual, port status and
        // the recurring provider drift reconciliation. Each task is idempotent and
        // multi-instance safe (row locks / SKIP LOCKED / unique source keys).
        {
            let stewardship = stewardship.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(30));
                let mut sweeps: u64 = 0;
                loop {
                    ticker.tick().await;
                    let ctx = numbering::domain::RequestContext::generate();
                    if let Err(e) = stewardship.run_workers(&ctx, 200).await {
                        tracing::warn!(error = %e, "stewardship workers failed");
                    }
                    sweeps += 1;
                    if sweeps.is_multiple_of(10) {
                        if let Err(e) = stewardship.reconcile_lifecycle(&ctx, 200).await {
                            tracing::warn!(error = %e, "lifecycle reconciliation failed");
                        }
                        let cutoff =
                            stewardship.numbers().clock().now() - chrono::Duration::days(2);
                        if let Err(e) = stewardship.finalize_usage(cutoff).await {
                            tracing::warn!(error = %e, "usage finalization failed");
                        }
                    }
                }
            });
        }
        // Capability evidence past its expiry is already not granting (resolution reads
        // expiry); the sweep records it durably and emits the change event.
        let numbers = state.numbers.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let ctx = numbering::domain::RequestContext::generate();
                if let Err(e) = numbers.expire_capabilities(&ctx, 500).await {
                    tracing::warn!(error = %e, "capability expiry sweep failed");
                }
            }
        });
        // KYC lifecycle sweeps: approval expiry and abandonment, revalidation scheduling,
        // evidence retention.
        let kyc = kyc.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(60));
            loop {
                ticker.tick().await;
                let ctx = numbering::domain::RequestContext::generate();
                if let Err(e) = kyc.expire_due(&ctx, 500).await {
                    tracing::warn!(error = %e, "KYC expiry sweep failed");
                }
                if let Err(e) = kyc.schedule_revalidations(&ctx, 500).await {
                    tracing::warn!(error = %e, "KYC revalidation sweep failed");
                }
                if let Err(e) = kyc.purge_expired_evidence(500).await {
                    tracing::warn!(error = %e, "KYC evidence retention sweep failed");
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
