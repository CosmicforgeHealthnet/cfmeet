//! Messaging service entrypoint (composition root).

use std::{sync::Arc, time::Duration};

use anyhow::anyhow;
use messaging::{
    config::{service_auth::ServiceAuthSettings, LogFormat, MessagingConfig},
    db::summarize_db_error,
    domain::{SharedClock, SystemClock},
    http,
    identity::{
        cache::{CachingAuthority, PlatformAccessFeed},
        PlatformAccessAuthority, PlatformAccessPrincipals,
    },
    metrics,
    state::AppState,
};
use sea_orm::{ConnectOptions, Database};
use tracing_subscriber::EnvFilter;

fn fatal(what: &str, err: impl std::fmt::Display) -> ! {
    eprintln!("messaging: {what}: {err}");
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let settings = ServiceAuthSettings::from_env()
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let config = MessagingConfig::from_env(settings.runtime_environment)
        .unwrap_or_else(|e| fatal("configuration error", e));
    if !settings.has_platform_access() {
        fatal(
            "configuration error",
            "CF_PLATFORM_ACCESS_BASE_URL is required: Platform Access verifies every customer credential",
        );
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("messaging=info,warn"));
    match config.log_format {
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .json()
            .init(),
        LogFormat::Pretty => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    // Fail closed on malformed keys, bundles or targets before touching the network.
    // SEC-012: with the realtime Redis configured, the service-token replay guard is
    // shared by every replica (process-local otherwise).
    let replay_guard = config.realtime_redis_url.as_ref().map(|url| {
        use secrecy::ExposeSecret;
        Arc::new(
            messaging::config::replay_guard::RedisReplayGuard::new(
                url.expose_secret(),
                cf_service_auth::InMemoryReplayGuard::process_default(),
            )
            .unwrap_or_else(|e| fatal("MESSAGING_REALTIME_REDIS_URL", e)),
        ) as Arc<dyn cf_service_auth::ReplayGuard>
    });
    let service_auth = settings
        .load_with_replay_guard(Arc::new(cf_service_auth::SystemClock), replay_guard)
        .unwrap_or_else(|e| fatal("service authentication configuration error", e));
    let tree_heads = Arc::new(
        config
            .transparency
            .load()
            .unwrap_or_else(|e| fatal("key-transparency configuration error", e)),
    );
    let witnesses = Arc::new(
        config
            .transparency
            .load_witnesses()
            .unwrap_or_else(|e| fatal("key-transparency witness configuration error", e)),
    );
    let runtime = service_auth.signer().runtime_environment();
    if !config.devices.enabled {
        tracing::warn!("E2EE device registration and KeyPackage publication are disabled (MESSAGING_E2EE_DEVICES_ENABLED)");
    }
    if let Some(release) = &config.devices.production_release {
        // Never a certification claim: the record states the actual assessment status.
        tracing::warn!(
            decision_id = %release.decision_id,
            independent_assessment = %release.independent_assessment,
            accepted_residual_risks = ?release.accepted_residual_risks,
            "production personal E2EE enabled by owner release decision"
        );
    }
    tracing::info!(
        service = %service_auth.signer().issuer(),
        runtime_environment = %runtime,
        e2ee_devices = config.devices.enabled,
        auth_cache_ttl_seconds = config.auth_cache.ttl.as_secs(),
        "messaging configuration loaded"
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
            "cannot connect to the Messaging database ({})",
            summarize_db_error(&e)
        )
    })?;
    let pending = messaging_migration::pending_migrations(&db)
        .await
        .map_err(|e| anyhow!("cannot read migration state ({})", summarize_db_error(&e)))?;
    if !pending.is_empty() {
        return Err(anyhow!(
            "{} Messaging migration(s) pending; run `messaging-migration up` first",
            pending.len()
        ));
    }

    let clock: SharedClock = Arc::new(SystemClock);
    let authority = Arc::new(CachingAuthority::new(
        Arc::new(PlatformAccessAuthority::new(service_auth.client())),
        config.auth_cache.ttl,
    ));
    if !config.auth_cache.ttl.is_zero() {
        tokio::spawn(authority.clone().watch(
            Arc::new(PlatformAccessFeed::new(service_auth.client())),
            config.auth_cache.feed_interval,
        ));
    }
    let managed_keys = match &config.managed_keyring_file {
        Some(path) => Some(Arc::new(
            messaging::messages::managed::ManagedKeyring::load(path)
                .unwrap_or_else(|e| fatal("MESSAGING_MANAGED_KEYRING_FILE", e)),
        )),
        None => {
            tracing::warn!(
                "no managed key ring is configured: MANAGED message sending is disabled"
            );
            None
        }
    };
    let personal_scope = cf_communications_policy::Scope::from_env().map_err(anyhow::Error::msg)?;
    let personal = personal_scope
        .map(|scope| cf_communications_policy::PolicyClient::new(service_auth.client(), scope));
    if personal.is_some() && !messaging_migration::communications_ready(&db).await? {
        anyhow::bail!("Communications integration migration is required");
    }
    // CF Meet chat remediation M1: CF Meet's own personal application environment gets
    // the first-party policy baseline (attachments on, 25 MiB, every kind). No other
    // application environment is touched, and its own application policy still wins.
    if let Some(scope) = &personal_scope {
        messaging::conversations::policy::register_baseline(
            &db,
            scope.tenant_id,
            scope.application_id,
            scope.environment_id,
            messaging::conversations::policy::ApplicationBaseline::CfMeetFirstParty,
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| anyhow!("cannot register the CF Meet policy baseline ({e})"))?;
        tracing::info!(
            environment_id = %scope.environment_id,
            "CF Meet first-party messaging policy baseline registered"
        );
    }
    let mut state = AppState::new(db.clone(), clock, authority, service_auth.clone())
        .with_communications(personal)
        .with_principals(Arc::new(PlatformAccessPrincipals::new(
            service_auth.client(),
        )))
        .with_devices(config.devices)
        .with_conversations(config.conversations)
        .with_workers(config.workers)
        .with_tree_head_signer(tree_heads)
        .with_witnesses(witnesses.clone());
    state.managed_keys = managed_keys;
    // Protected attachment storage and self-hosted scanning (Part F). Both fail closed
    // when unconfigured: no upload intent is issued, no MANAGED scan reports CLEAN.
    state.attachments = config.attachments;
    match &config.attachment_store {
        Some(s3) => match cf_object_storage::S3ObjectStore::new(s3.clone()) {
            Ok(store) => state.storage = Arc::new(store),
            Err(e) => anyhow::bail!("MESSAGING_ATTACHMENTS_S3_*: {e}"),
        },
        None => {
            tracing::warn!("no attachment object storage is configured: attachments are disabled");
        }
    }
    match (&config.clamav_address, &config.scan_gateway) {
        (_, Some(gateway)) => {
            state.scanner = Arc::new(
                messaging::attachments::scanner::GatewayScanner::from_files(
                    &gateway.url,
                    &gateway.verdict_keys_file,
                    service_auth.shared_signer(),
                    gateway.timeout,
                )
                .unwrap_or_else(|e| fatal("MESSAGING_SCAN_GATEWAY_*", e)),
            );
            // warn: always visible; messaging-configure reads it to prove the active scanner.
            tracing::warn!(
                "MANAGED attachments are scanned through the shared security scan gateway"
            );
        }
        (Some(a), None) => {
            state.scanner = Arc::new(messaging::attachments::scanner::ClamAvScanner::new(
                a.clone(),
                std::time::Duration::from_secs(60),
            ))
        }
        (None, None) => {
            tracing::warn!("no attachment scanner is configured: MANAGED attachments requiring scanning stay pending");
        }
    }
    // Realtime gateway runtime (Part E).
    let realtime = {
        use messaging::realtime::{broker, ephemeral, push, token::ClientTokenKeys, Realtime};
        use secrecy::ExposeSecret;
        let keys = match &config.client_token_key_file {
            Some(path) => Some(
                ClientTokenKeys::load(path)
                    .unwrap_or_else(|e| fatal("MESSAGING_CLIENT_TOKEN_SIGNING_KEY_FILE", e)),
            ),
            None => {
                tracing::warn!(
                    "no client-token signing key is configured: realtime connections are disabled"
                );
                None
            }
        };
        let (broker, presence): (Arc<dyn broker::Broker>, Arc<dyn ephemeral::PresenceStore>) =
            match &config.realtime_redis_url {
                Some(url) => (
                    Arc::new(
                        broker::RedisBroker::new(url.expose_secret())
                            .unwrap_or_else(|e| fatal("MESSAGING_REALTIME_REDIS_URL", e)),
                    ),
                    Arc::new(
                        ephemeral::RedisPresence::new(url.expose_secret())
                            .unwrap_or_else(|e| fatal("MESSAGING_REALTIME_REDIS_URL", e)),
                    ),
                ),
                None => {
                    tracing::warn!("no realtime Redis is configured: single-instance in-process fan-out and presence");
                    (
                        Arc::new(broker::LocalBroker::default()),
                        Arc::new(ephemeral::MemoryPresence::default()),
                    )
                }
            };
        let local = match config.push_provider {
            messaging::config::PushProviderKind::LocalTest => {
                Some(Arc::new(push::LocalTestPush::default()))
            }
            messaging::config::PushProviderKind::None => None,
        };
        let adapters = messaging::realtime::push_adapters::PushAdapters::build(
            &config.push_adapters,
            runtime.allows_insecure_transport(),
            local,
        )
        .map_err(|e| anyhow::anyhow!("push adapter configuration: {e}"))?;
        tracing::info!(platforms = ?adapters.configured(), "push adapters");
        let push_provider: Arc<dyn push::PushProvider> = Arc::new(adapters);
        let rt = Arc::new(Realtime::new(
            config.realtime.clone(),
            keys,
            broker,
            presence,
            push_provider,
        ));
        rt.start();
        rt
    };
    let mut state = state.with_realtime(realtime.clone());
    state.kt_checkpoint = std::env::var("MESSAGING_KT_CHECKPOINT_FILE")
        .ok()
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from);
    // Part V H SMS Bridge: the SMS Gateway is the only SMS transport authority.
    let sms_configured = settings
        .targets
        .iter()
        .any(|(s, _)| s.as_str() == cf_service_auth::services::SMS_GATEWAY);
    let mut state = if sms_configured {
        let client = state.service_auth.client();
        state.with_sms_transport(Arc::new(messaging::sms_bridge::HttpSmsTransport::new(
            client,
        )))
    } else {
        tracing::warn!("CF_SMS_GATEWAY_BASE_URL is not set: SMS bridges are refused (in-app messaging unaffected)");
        state
    };
    // Part V J: key-transparency rollback detection against an operator checkpoint kept
    // outside the database (and its backups).
    match std::env::var("MESSAGING_KT_CHECKPOINT_FILE") {
        Ok(path) if !path.is_empty() => {
            // Verify the configured checkpoint before accepting protected requests.
            // A fault engages existing onboarding/proof controls; other Messaging
            // reads and independently authorized services remain available.
            if let Err(error) = messaging::operations::kt_monitor_once(&db, state.now(), std::path::Path::new(&path)).await {
                tracing::warn!(%error, "key-transparency startup verification failed");
            }
            tokio::spawn(messaging::operations::run_kt_monitor(
                db.clone(),
                state.clock.clone(),
                std::path::PathBuf::from(path),
                Duration::from_secs(60),
            ));
        }
        _ => tracing::warn!("MESSAGING_KT_CHECKPOINT_FILE is not set: key-transparency rollback detection relies on external auditors only"),
    }
    // Part V I: customer webhooks through the existing platform webhook system (Calling);
    // Part VI §53: and / or directly to the Developer Platform's central event ingestion.
    let has_target = |service: &str| settings.targets.iter().any(|(s, _)| s.as_str() == service);
    let destination = config.public_events;
    if destination
        .destinations()
        .contains(&messaging::webhook_relay::EventDestination::DeveloperPlatform)
        && !has_target(cf_service_auth::services::DEVELOPER_PLATFORM)
    {
        anyhow::bail!(
            "MESSAGING_PUBLIC_EVENTS_DESTINATION={} requires CF_DEVELOPER_PLATFORM_BASE_URL",
            destination.as_str()
        );
    }
    let webhook_sinks: Vec<(
        messaging::webhook_relay::EventDestination,
        Arc<dyn messaging::webhook_relay::PlatformEventSink>,
    )> = destination
        .destinations()
        .iter()
        .map(|d| {
            let sink: Arc<dyn messaging::webhook_relay::PlatformEventSink> = match d {
                messaging::webhook_relay::EventDestination::Calling
                    if has_target(cf_service_auth::services::CALLING) =>
                {
                    Arc::new(messaging::webhook_relay::CallingPlatformEvents::new(
                        state.service_auth.client(),
                    ))
                }
                messaging::webhook_relay::EventDestination::Calling => {
                    tracing::warn!(
                        "CF_CALLING_BASE_URL is not set: webhook-relayed events wait PENDING"
                    );
                    Arc::new(messaging::webhook_relay::UnconfiguredSink)
                }
                messaging::webhook_relay::EventDestination::DeveloperPlatform => {
                    Arc::new(messaging::webhook_relay::DeveloperPlatformEvents::new(
                        state.service_auth.client(),
                    ))
                }
            };
            (*d, sink)
        })
        .collect();
    tracing::info!(
        public_events_destination = destination.as_str(),
        "webhook relay receivers"
    );
    // ADR 0064 (CF Meet Apps): OAuth credentials and app bots (default off). Bot events
    // travel only to the Developer Platform's central event ingestion.
    {
        let developer_platform = destination
            .destinations()
            .contains(&messaging::webhook_relay::EventDestination::DeveloperPlatform);
        let apps =
            messaging::apps::AppsSettings::from_env(|k| std::env::var(k).ok(), developer_platform)
                .map_err(|e| anyhow!("apps configuration: {e}"))?;
        if apps.enabled {
            tracing::info!(
                bot_events = apps.bot_events,
                "CF Meet Apps are enabled (MESSAGING_APPS_ENABLED)"
            );
            if !apps.bot_events {
                tracing::warn!("MESSAGING_APPS_ENABLED is set but MESSAGING_PUBLIC_EVENTS_DESTINATION does not include developer-platform: bot events are not recorded");
            }
        }
        state.apps = apps;
    }
    // ADR 0062 / 0063 (CF Meet Platform "Build"): partner session devices, partner push
    // and MESSAGE_SENT facts, each behind its own default-off flag.
    let partner_settings = messaging::partner::PartnerSettings::from_env(|k| std::env::var(k).ok())
        .map_err(|e| anyhow!("partner configuration: {e}"))?;
    messaging::partner::install_process_flags(&partner_settings);
    {
        let mut partner = messaging::partner::Partner::new(
            partner_settings,
            Arc::new(messaging::partner::PlatformAccessPartners::new(
                state.service_auth.client(),
            )),
        );
        if partner_settings.push_enabled {
            if !has_target(cf_service_auth::services::NOTIFICATIONS) {
                anyhow::bail!(
                    "MESSAGING_PARTNER_PUSH_ENABLED=true needs CF_NOTIFICATIONS_BASE_URL"
                );
            }
            partner = partner.with_push(
                Arc::new(
                    messaging::partner::push::NotificationServicePartnerRegistrations::new(
                        state.service_auth.client(),
                    ),
                ),
                Arc::new(
                    messaging::notification_publications::NotificationServiceEvents::new(
                        state.service_auth.client(),
                    ),
                ),
            );
        }
        state.partner = Arc::new(partner);
        if partner_settings.session_devices_enabled {
            tokio::spawn(messaging::partner::revocation::run(
                state.clone(),
                Duration::from_secs(15),
            ));
            tracing::info!(
                "partner session devices are enabled (MESSAGING_PARTNER_SESSION_DEVICES_ENABLED)"
            );
        }
        if partner_settings.push_enabled {
            tokio::spawn(messaging::partner::push::run(
                state.clone(),
                Duration::from_secs(2),
            ));
            tracing::info!("partner push is enabled (MESSAGING_PARTNER_PUSH_ENABLED)");
        }
    }
    // Part VI §55: attachment STORAGE_BYTE_TIME usage facts to the Developer Platform.
    match cf_usage_facts::exporter::ExportSettings::from_env(|k| std::env::var(k).ok()) {
        Ok(s) if s.enabled && has_target(cf_service_auth::services::DEVELOPER_PLATFORM) => {
            let mut exporter = cf_usage_facts::exporter::Exporter::new(
                Arc::new(cf_usage_facts::exporter::DeveloperPlatformSink::new(
                    state.service_auth.client(),
                )),
                s,
            )
            .with_intervals(Arc::new(messaging::usage_export::AttachmentStorage::new(
                state.db.clone(),
            )));
            // ADR 0062: MESSAGE_SENT facts, only once the usage catalogue knows the type.
            if partner_settings.message_sent_export_enabled {
                if messaging::usage_export::MessageSent::catalogued() {
                    exporter = exporter.with_events(Arc::new(
                        messaging::usage_export::MessageSent::new(state.db.clone()),
                    ));
                    tracing::info!(
                        "MESSAGE_SENT usage facts are exported to the Developer Platform"
                    );
                } else {
                    tracing::warn!("MESSAGING_MESSAGE_SENT_EXPORT_ENABLED is set but the usage catalogue does not know MESSAGE_SENT: not exported");
                }
            }
            tokio::spawn(Arc::new(exporter).run());
            tracing::info!("attachment storage usage facts are exported to the Developer Platform");
        }
        Ok(s) if s.enabled => tracing::warn!(
            "CFMEET_USAGE_EXPORT_ENABLED needs CF_DEVELOPER_PLATFORM_BASE_URL: usage export off"
        ),
        Ok(_) => {}
        Err(e) => anyhow::bail!("usage export configuration: {e}"),
    }
    tokio::spawn(messaging::webhook_relay::run(
        state.clone(),
        webhook_sinks,
        config.workers.interval,
    ));
    // Notification Service N9 (D-1b copy path): the push-registration facade. Off unless
    // MESSAGING_PUSH_REGISTRATION_MIRROR=on; then the service target and CF Meet's personal
    // scope are required (only first-party registrations are ever mirrored).
    match messaging::push_mirror::interval_from_env(|k| std::env::var(k).ok()) {
        Err(e) => anyhow::bail!("push-registration mirror configuration: {e}"),
        Ok(None) => {}
        Ok(Some(interval)) => {
            if !has_target(cf_service_auth::services::NOTIFICATIONS) {
                anyhow::bail!(
                    "MESSAGING_PUSH_REGISTRATION_MIRROR=on needs CF_NOTIFICATIONS_BASE_URL"
                );
            }
            let Some(scope) = personal_scope else {
                anyhow::bail!(
                    "MESSAGING_PUSH_REGISTRATION_MIRROR=on needs CF Meet's personal scope (CF_COMMUNICATIONS_PERSONAL_*)"
                );
            };
            let mirror = std::sync::Arc::new(messaging::push_mirror::PushMirror {
                scope,
                sink: std::sync::Arc::new(
                    messaging::push_mirror::NotificationServiceRegistrations::new(
                        state.service_auth.client(),
                    ),
                ),
            });
            state.push_mirror = Some(mirror.clone());
            tokio::spawn(messaging::push_mirror::run(state.clone(), mirror, interval));
            tracing::info!(
                interval_seconds = interval.as_secs(),
                "push registrations are mirrored to the Notification Service"
            );
        }
    }
    // Gap audit S1: organization conversations' wake-ups through the Notification Service.
    // Off unless MESSAGING_ORGANIZATION_WAKEUPS=on; then the service target, CF Meet's
    // personal scope (to tell an organization's scope from it) and the routing-fence
    // history (decisions and publications) are required.
    if messaging::notifications::organization_wakeups_from_env(|k| std::env::var(k).ok())
        .map_err(anyhow::Error::msg)?
    {
        if !has_target(cf_service_auth::services::NOTIFICATIONS) {
            anyhow::bail!("MESSAGING_ORGANIZATION_WAKEUPS=on needs CF_NOTIFICATIONS_BASE_URL");
        }
        if personal_scope.is_none() {
            anyhow::bail!(
                "MESSAGING_ORGANIZATION_WAKEUPS=on needs CF Meet's personal scope (CF_COMMUNICATIONS_PERSONAL_*)"
            );
        }
        if !messaging_migration::notification_fence_ready(&state.db).await? {
            anyhow::bail!(
                "MESSAGING_ORGANIZATION_WAKEUPS=on needs the notification-fence migration history"
            );
        }
        state.organization_wakeups = true;
        tracing::info!("organization conversation wake-ups go to the Notification Service");
    }
    // Notification Service N7: the relay of Messaging's publications (shadow copies in
    // SHADOW, publications of NEW decisions) to I1. Off unless the service is configured:
    // the routing mode alone decides what is enqueued, and nothing is ever sent without it.
    if has_target(cf_service_auth::services::NOTIFICATIONS) {
        tokio::spawn(messaging::notification_publications::run(
            state.clone(),
            Arc::new(
                messaging::notification_publications::NotificationServiceEvents::new(
                    state.service_auth.client(),
                ),
            ),
            config.workers.interval,
        ));
        tracing::info!("notification publications are relayed to the Notification Service");
        // "Added to a group" notices (catalogue #14, GROUP_INVITATIONS): written and relayed
        // only with the service configured (no legacy path, no routing fence).
        state.member_notices = true;
        tokio::spawn(messaging::member_notifications::run(
            state.clone(),
            Arc::new(
                messaging::notification_publications::NotificationServiceEvents::new(
                    state.service_auth.client(),
                ),
            ),
            config.workers.interval,
        ));
    } else {
        tracing::info!(
            "CF_NOTIFICATIONS_BASE_URL is not set: notification publications are not relayed"
        );
    }
    // ADR 0068 A7: owner events to the HQ admin bridge (abuse.report.changed.v1,
    // incident.changed.v1). Off unless CF_ADMIN_BRIDGE_BASE_URL is set: then they are
    // written in the change's transaction and relayed at least once.
    if has_target(cf_service_auth::services::ADMIN_BRIDGE) {
        messaging::bridge_events::install(true);
        tokio::spawn(messaging::bridge_events::run(
            state.clone(),
            Arc::new(messaging::bridge_events::AdminBridgeEvents::new(
                state.service_auth.client(),
            )),
            config.workers.interval,
        ));
        tracing::info!("owner events are published to the admin bridge");
    } else {
        tracing::info!(
            "CF_ADMIN_BRIDGE_BASE_URL is not set: no owner events are stored or published"
        );
    }
    tokio::spawn(messaging::conversations::service::run_expiry(
        state.clone(),
        config.conversations.expiry_interval,
    ));
    // Committed events -> realtime broker pointers and push jobs (leased, multi-instance).
    let publisher = Arc::new(messaging::events::outbox::OutboxPublisher::new(
        db.clone(),
        Arc::new(messaging::realtime::fanout::RealtimeTransport::new(
            state.clone(),
        )),
        state.clock.clone(),
        messaging::events::outbox::PublisherSettings {
            lease: config.workers.lease,
            max_attempts: config.workers.outbox_max_attempts,
            retry: messaging::events::RetryPolicy::default(),
            batch: 500,
        },
        state.runtime_environment().to_string(),
    ));
    tokio::spawn(
        publisher.run_with_wakeup(
            config
                .realtime
                .publisher_interval
                .max(std::time::Duration::from_millis(50)),
            realtime.outbox_wakeup.clone(),
        ),
    );
    // MSG-018: publishes coalesced read / delivery receipts when their window ends.
    tokio::spawn(messaging::messages::receipts::run(
        state.clone(),
        config
            .realtime
            .publisher_interval
            .max(std::time::Duration::from_millis(50)),
    ));
    tokio::spawn(messaging::realtime::push::run(
        state.clone(),
        config.workers.interval,
    ));
    if witnesses.is_configured() {
        tracing::info!(
            witnesses = witnesses.endpoints.len(),
            quorum = witnesses.quorum,
            "key-transparency witnesses configured"
        );
        tokio::spawn(messaging::transparency::witness::run(
            state.clone(),
            witnesses.clone(),
            config.transparency.witness_interval,
        ));
    } else {
        tracing::warn!(
            "no key-transparency witnesses configured: tree heads are served UNWITNESSED"
        );
    }
    tokio::spawn(messaging::compliance::run(
        state.clone(),
        std::time::Duration::from_secs(60),
    ));
    tokio::spawn(messaging::attachments::lifecycle::run(
        state.clone(),
        std::time::Duration::from_secs(30),
    ));
    tokio::spawn(messaging::sms_bridge::run(
        state.clone(),
        config.workers.interval,
    ));
    tokio::spawn(messaging::workers::run_maintenance(
        state.clone(),
        std::time::Duration::from_secs(30),
    ));

    if let Some(address) = config.metrics_address {
        let router = metrics::router(state.clone());
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(address = %address, "messaging metrics listening");
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router).await {
                tracing::warn!(error = %e, "metrics listener stopped");
            }
        });
    }
    let app = http::app(state);
    let listener = tokio::net::TcpListener::bind(config.bind_address).await?;
    tracing::info!(address = %config.bind_address, "messaging listening");
    let drain = config.realtime.drain;
    let hub = realtime.hub.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown().await;
            // Stop admitting connections, tell every connection to reconnect elsewhere
            // (jittered), and give them the drain period before the process exits.
            hub.drain();
            let deadline = tokio::time::Instant::now() + drain;
            while hub.connections() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await?;
    Ok(())
}

/// Ctrl-C or SIGTERM (container stop / rolling deploy) drains in-flight requests.
async fn shutdown() {
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
        _ = interrupt => {},
        _ = terminate => {},
    }
    tracing::info!("messaging shutting down");
}
